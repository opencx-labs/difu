use super::*;
use crate::shortcuts::{Bindings, contexts::*, ids::*};
use std::path::PathBuf;

type Loaded = std::result::Result<(PathBuf, String), String>;

pub struct Settings {
    pub repository: Editor,
    pub text: Editor,
    pub field: usize,
    root: Option<PathBuf>,
    saved: String,
    candidates: Vec<PathBuf>,
    selected: usize,
    loading: Option<mpsc::Receiver<Loaded>>,
    error: Option<String>,
    return_to: Option<Box<defaults::Settings>>,
}

impl Settings {
    fn options(&self) -> Vec<PathBuf> {
        let query = self.repository.text().to_lowercase();
        self.candidates
            .iter()
            .filter(|path| path.to_string_lossy().to_lowercase().contains(&query))
            .cloned()
            .collect()
    }
    fn load(&mut self, storage: Storage, path: PathBuf) {
        if self.text.text() != self.saved {
            if let Some(root) = &self.root {
                self.repository = Editor::from(root.to_string_lossy().as_ref());
            }
            self.error =
                Some("Save or cancel these changes before choosing another repository".into());
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.loading = Some(rx);
        self.error = None;
        self.root = None;
        std::thread::spawn(move || {
            let result = super::super::rules::load(&storage, &path).map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
        });
    }
}

impl Ui {
    pub(super) fn open_rules(&mut self, path: Option<PathBuf>) {
        let config = match self.storage.load_config() {
            Ok(config) => config,
            Err(error) => {
                self.notice = Some((format!("Cannot load repository rules: {error:#}"), true));
                return;
            }
        };
        let path = path
            .or_else(|| {
                self.selected
                    .as_ref()
                    .and_then(|id| self.sessions.get(id))
                    .map(|s| s.job.root().clone())
            })
            .or_else(|| config.agent_defaults.repository.clone())
            .or_else(|| std::env::current_dir().ok());
        let mut candidates: Vec<_> = config
            .repository_rules
            .keys()
            .cloned()
            .chain(config.repositories.values().cloned())
            .chain(config.agent_defaults.repository)
            .chain(self.summaries.iter().filter_map(|s| s.repository.clone()))
            .collect();
        candidates.sort();
        candidates.dedup();
        let return_to = match self.modal.take() {
            Some(Modal::AgentDefaults(form)) => Some(form),
            _ => None,
        };
        let mut form = Settings {
            repository: Editor::from(
                path.as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .as_str(),
            ),
            text: Editor::default(),
            field: 0,
            root: None,
            saved: String::new(),
            candidates,
            selected: 0,
            loading: None,
            error: None,
            return_to,
        };
        if let Some(path) = path {
            form.load(self.storage.clone(), path);
        }
        self.modal = Some(Modal::RepositoryRules(Box::new(form)));
    }
    pub(super) fn tick_rules(&mut self) {
        let Some(Modal::RepositoryRules(form)) = &mut self.modal else {
            return;
        };
        let Some(result) = form.loading.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            return;
        };
        crate::repaint::request();
        form.loading = None;
        match result {
            Ok((root, text)) => {
                form.repository = Editor::from(root.to_string_lossy().as_ref());
                form.root = Some(root);
                form.text = Editor::from(text.as_str());
                form.saved = text;
                form.field = 1;
            }
            Err(error) => form.error = Some(error),
        }
    }
    pub(super) fn close_rules(&mut self) {
        if let Some(Modal::RepositoryRules(form)) = self.modal.take() {
            self.modal = form.return_to.map(Modal::AgentDefaults);
        }
    }
    pub(super) fn save_rules(&mut self) {
        let select_hint = Bindings::new(&self.shortcut_config)
            .primary_hint_in(NAVIGATION_ACTIVATE, AGENT_RULES_REPOSITORY);
        let Some(Modal::RepositoryRules(form)) = &mut self.modal else {
            return;
        };
        let Some(root) = &form.root else { return };
        if form.repository.text() != root.to_string_lossy() {
            form.error = Some(format!(
                "Press {select_hint} to select the repository first"
            ));
            return;
        }
        let text = form.text.text();
        let result = self.storage.update_config(|config| {
            if text.trim().is_empty() {
                config.repository_rules.remove(root);
            } else {
                config.repository_rules.insert(root.clone(), text);
            }
        });
        match result {
            Ok(_) => {
                self.close_rules();
                self.notice = Some(("Repository rules saved for new sessions".into(), false));
            }
            Err(error) => form.error = Some(format!("Cannot save rules: {error:#}")),
        }
    }
    pub(super) fn rules_option(&mut self, index: usize) {
        if let Some(Modal::RepositoryRules(form)) = &mut self.modal
            && let Some(path) = form.options().get(index).cloned()
        {
            form.load(self.storage.clone(), path);
        }
    }
    pub(super) fn rules_paste(&mut self, text: &str) {
        if let Some(Modal::RepositoryRules(form)) = &mut self.modal
            && form.loading.is_none()
        {
            if form.field == 0 {
                form.repository.insert(text);
                form.selected = 0;
            } else if form.field == 1 && form.root.is_some() {
                form.text.insert(text);
            }
        }
    }
    pub(super) fn rules_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.close_rules();
            return;
        }
        let action = self.shortcut(key);
        if action == Some(RULES_SAVE) {
            self.save_rules();
            return;
        }
        let Some(Modal::RepositoryRules(form)) = &mut self.modal else {
            return;
        };
        if form.loading.is_some() {
            return;
        }
        match action {
            Some(NAVIGATION_ACTIVATE) => match form.field {
                0 => {
                    let path = form
                        .options()
                        .get(form.selected)
                        .cloned()
                        .unwrap_or_else(|| PathBuf::from(form.repository.text()));
                    form.load(self.storage.clone(), path);
                }
                2 => self.save_rules(),
                3 => self.close_rules(),
                _ => {}
            },
            Some(RULES_REPOSITORY_PREVIOUS | RULES_REPOSITORY_NEXT) => {
                form.selected = if action == Some(RULES_REPOSITORY_PREVIOUS) {
                    form.selected.saturating_sub(1)
                } else {
                    (form.selected + 1).min(form.options().len().saturating_sub(1))
                };
            }
            Some(NAVIGATION_NEXT_FOCUS | NAVIGATION_PREVIOUS_FOCUS) => {
                form.field = (form.field
                    + if action == Some(NAVIGATION_NEXT_FOCUS) {
                        1
                    } else {
                        3
                    })
                    % 4;
            }
            _ if form.field == 0 => {
                form.repository.key(key);
                form.selected = 0;
            }
            _ if form.field == 1 && form.root.is_some() => form.text.key(key),
            _ => {}
        }
    }
    pub(super) fn draw_rules(&mut self, frame: &mut Frame, area: Rect) {
        let Some(Modal::RepositoryRules(form)) = &self.modal else {
            return;
        };
        let bindings = Bindings::new(&self.shortcut_config);
        let repository_label = format!(
            "Repository · {}/{} select · {} opens",
            bindings.primary_hint_in(RULES_REPOSITORY_PREVIOUS, AGENT_RULES_REPOSITORY),
            bindings.primary_hint_in(RULES_REPOSITORY_NEXT, AGENT_RULES_REPOSITORY),
            bindings.primary_hint_in(NAVIGATION_ACTIVATE, AGENT_RULES_REPOSITORY)
        );
        let save_label = format!(
            "Save · {}",
            bindings.primary_hint_in(RULES_SAVE, self.shortcut_context())
        );
        let repository = Rect::new(area.x, area.y, area.width, 3).intersection(area);
        editor(
            frame,
            repository,
            &repository_label,
            &form.repository,
            form.field == 0,
        );
        self.hits.push((repository, Action::RulesField(0)));
        let options = form.options();
        let count = if form.field == 0 {
            options.len().min(5).min(usize::from(area.height / 3))
        } else {
            0
        };
        let start = form.selected.saturating_sub(count.saturating_sub(1));
        for (index, path) in options.iter().enumerate().skip(start).take(count) {
            let rect = Rect::new(area.x, area.y + 3 + (index - start) as u16, area.width, 1)
                .intersection(area);
            frame.render_widget(
                Paragraph::new(path.to_string_lossy().into_owned())
                    .style(crate::ui::option_style(index == form.selected)),
                rect,
            );
            self.hits.push((rect, Action::RulesOption(index)));
        }
        let top = area.y + 3 + count as u16;
        let text = Rect::new(
            area.x,
            top,
            area.width,
            area.bottom().saturating_sub(top + 3),
        )
        .intersection(area);
        editor(
            frame,
            text,
            "Rules",
            &form.text,
            form.field == 1 && form.root.is_some(),
        );
        self.hits.push((text, Action::RulesField(1)));
        let footer = area.bottom().saturating_sub(3);
        frame.render_widget(
            Paragraph::new(
                "Included in the initial instructions for new sessions in this repository.",
            )
            .style(Style::default().fg(DIM)),
            Rect::new(area.x, footer, area.width, 1),
        );
        for (field, label) in [(2, save_label.as_str()), (3, "Cancel · Esc")] {
            let rect = Rect::new(
                area.x + (field - 2) as u16 * (area.width / 2),
                footer + 1,
                area.width / 2,
                1,
            );
            frame.render_widget(
                Paragraph::new(label).style(crate::ui::option_style(form.field == field)),
                rect,
            );
            self.hits.push((
                rect,
                if field == 2 {
                    Action::RulesSave
                } else {
                    Action::RulesCancel
                },
            ));
        }
        if let Some(error) = &form.error {
            frame.render_widget(
                Paragraph::new(error.as_str()).style(Style::default().fg(RED)),
                Rect::new(area.x, footer + 2, area.width, 1),
            );
        } else if form.loading.is_some() {
            frame.render_widget(
                Paragraph::new("Loading repository rules…").style(Style::default().fg(DIM)),
                Rect::new(area.x, footer + 2, area.width, 1),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn multiline_rules_save_cancel_and_clear_without_changing_other_settings() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().to_owned();
        let storage = Storage {
            config: root.join("config.json"),
            cache: root.join("cache"),
        };
        let mut config = Config::default();
        config.pinned_sessions.insert("pinned".into());
        storage.save_config(&config)?;
        let mut ui = Ui::new(storage.clone(), &config);
        let form = |text: &str| {
            Modal::RepositoryRules(Box::new(Settings {
                repository: Editor::from(root.to_string_lossy().as_ref()),
                text: Editor::from(text),
                field: 1,
                root: Some(root.clone()),
                saved: text.into(),
                candidates: vec![root.clone()],
                selected: 0,
                loading: None,
                error: None,
                return_to: None,
            }))
        };
        ui.modal = Some(form(""));
        ui.rules_paste("First rule");
        ui.rules_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        ui.rules_paste("Second rule");
        ui.rules_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SUPER));
        assert!(ui.modal.is_none());
        let config = storage.load_config()?;
        assert_eq!(
            config.repository_rules.get(&root).map(String::as_str),
            Some("First rule\nSecond rule")
        );
        assert!(config.pinned_sessions.contains("pinned"));
        ui.modal = Some(form("First rule\nSecond rule"));
        ui.rules_paste("Unsaved");
        ui.rules_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            storage.load_config()?.repository_rules,
            config.repository_rules
        );
        ui.modal = Some(form(""));
        ui.rules_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
        assert!(storage.load_config()?.repository_rules.is_empty());
        Ok(())
    }
}

#[cfg(test)]
mod shortcut_regressions {
    use super::*;
    use anyhow::{Context, Result};
    fn setup(ui: &mut Ui, root: &std::path::Path, field: usize) {
        ui.modal = Some(Modal::RepositoryRules(Box::new(Settings {
            repository: Editor::from(root.to_string_lossy().as_ref()),
            text: Editor::from("saved rule"),
            field,
            root: Some(root.into()),
            saved: "saved rule".into(),
            candidates: vec![root.join("one"), root.join("two")],
            selected: 0,
            loading: None,
            error: None,
            return_to: None,
        })));
    }
    fn form(ui: &Ui) -> Result<&Settings> {
        let Some(Modal::RepositoryRules(f)) = &ui.modal else {
            anyhow::bail!("rules closed")
        };
        Ok(f)
    }
    #[test]
    fn rules_focus_navigation_and_repository_selection_are_contextual() -> Result<()> {
        for field in 0..4 {
            for (action, code, mods, want) in [
                (
                    NAVIGATION_NEXT_FOCUS,
                    KeyCode::Tab,
                    KeyModifiers::NONE,
                    (field + 1) % 4,
                ),
                (
                    NAVIGATION_PREVIOUS_FOCUS,
                    KeyCode::BackTab,
                    KeyModifiers::SHIFT,
                    (field + 3) % 4,
                ),
            ] {
                for (phase, custom) in shortcut_tests::BINDING_PHASES {
                    let (mut ui, dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                    setup(&mut ui, dir.path(), field);
                    if custom {
                        ui.key(KeyEvent::new(code, mods));
                        assert_eq!(form(&ui)?.field, field);
                    }
                    ui.key(KeyEvent::new(
                        if custom { KeyCode::F(12) } else { code },
                        if custom { KeyModifiers::NONE } else { mods },
                    ));
                    assert_eq!(form(&ui)?.field, want);
                }
            }
        }
        for (action, code, start, want) in [
            (RULES_REPOSITORY_PREVIOUS, KeyCode::Up, 1, 0),
            (RULES_REPOSITORY_NEXT, KeyCode::Down, 0, 1),
        ] {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                setup(&mut ui, dir.path(), 0);
                let Some(Modal::RepositoryRules(f)) = &mut ui.modal else {
                    anyhow::bail!("rules")
                };
                f.repository = Editor::default();
                f.selected = start;
                if custom {
                    ui.key(KeyEvent::new(code, KeyModifiers::NONE));
                    assert_eq!(form(&ui)?.selected, 0, "removed navigation remains native");
                    if let Some(Modal::RepositoryRules(f)) = &mut ui.modal {
                        f.selected = start;
                    }
                }
                ui.key(KeyEvent::new(
                    if custom { KeyCode::F(12) } else { code },
                    KeyModifiers::NONE,
                ));
                assert_eq!(form(&ui)?.selected, want);
                let Some(Modal::RepositoryRules(f)) = &mut ui.modal else {
                    anyhow::bail!("rules")
                };
                f.field = 1;
                assert_eq!(ui.shortcut_context(), AGENT_RULES);
                assert!(
                    !crate::shortcuts::Bindings::new(&ui.shortcut_config).matches(
                        action,
                        ui.shortcut_context(),
                        KeyEvent::new(
                            if custom { KeyCode::F(12) } else { code },
                            KeyModifiers::NONE
                        )
                    )
                );
            }
        }
        Ok(())
    }
    #[test]
    fn rules_save_predicate_replaces_all_control_or_super_combinations() -> Result<()> {
        for field in 0..4 {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                for bits in 1u8..64 {
                    if bits & 9 == 0 {
                        continue;
                    }
                    let (mut ui, dir, _) = shortcut_tests::binding_fixture(RULES_SAVE, phase)?;
                    setup(&mut ui, dir.path(), field);
                    let mut mods = KeyModifiers::NONE;
                    for (bit, modifier) in [
                        (1, KeyModifiers::CONTROL),
                        (2, KeyModifiers::ALT),
                        (4, KeyModifiers::SHIFT),
                        (8, KeyModifiers::SUPER),
                        (16, KeyModifiers::HYPER),
                        (32, KeyModifiers::META),
                    ] {
                        if bits & bit != 0 {
                            mods |= modifier;
                        }
                    }
                    if custom {
                        ui.key(KeyEvent::new(KeyCode::Enter, mods));
                        assert!(ui.modal.is_some());
                        assert!(ui.storage.load_config()?.repository_rules.is_empty());
                        setup(&mut ui, dir.path(), field);
                    }
                    ui.key(KeyEvent::new(
                        if custom {
                            KeyCode::F(12)
                        } else {
                            KeyCode::Enter
                        },
                        if custom { KeyModifiers::NONE } else { mods },
                    ));
                    assert!(ui.modal.is_none(), "{field} {phase} {mods:?}");
                    assert_eq!(
                        ui.storage
                            .load_config()?
                            .repository_rules
                            .get(dir.path())
                            .map(String::as_str),
                        Some("saved rule")
                    );
                }
            }
        }
        Ok(())
    }
    #[test]
    fn rules_activation_keeps_multiline_input_and_repository_and_dirty_guards() -> Result<()> {
        for field in [0, 1, 2, 3] {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, dir, _) = shortcut_tests::binding_fixture(NAVIGATION_ACTIVATE, phase)?;
                setup(&mut ui, dir.path(), field);
                if custom {
                    ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    assert!(ui.modal.is_some());
                    assert!(form(&ui)?.loading.is_none());
                    setup(&mut ui, dir.path(), field);
                }
                ui.key(KeyEvent::new(
                    if custom {
                        KeyCode::F(12)
                    } else {
                        KeyCode::Enter
                    },
                    KeyModifiers::NONE,
                ));
                match field {
                    0 => assert!(form(&ui)?.loading.is_some()),
                    1 => {
                        assert_eq!(
                            form(&ui)?.text.text(),
                            if custom { "saved rule" } else { "saved rule\n" }
                        );
                        assert!(ui.storage.load_config()?.repository_rules.is_empty());
                    }
                    2 => assert!(ui.modal.is_none()),
                    3 => {
                        assert!(ui.modal.is_none());
                        assert!(ui.storage.load_config()?.repository_rules.is_empty());
                    }
                    _ => anyhow::bail!("field"),
                }
            }
        }
        let (mut ui, dir, _) = shortcut_tests::fixture(Some(NAVIGATION_ACTIVATE))?;
        setup(&mut ui, dir.path(), 0);
        let Some(Modal::RepositoryRules(f)) = &mut ui.modal else {
            anyhow::bail!("rules")
        };
        f.text.insert("unsaved");
        ui.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
        assert!(form(&ui)?.loading.is_none());
        assert!(
            form(&ui)?
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Save or cancel"))
        );
        assert_eq!(form(&ui)?.text.text(), "saved ruleunsaved");
        let Some(Modal::RepositoryRules(f)) = &mut ui.modal else {
            anyhow::bail!("rules")
        };
        f.repository.insert("changed");
        ui.save_rules();
        assert!(
            form(&ui)?
                .error
                .as_deref()
                .is_some_and(|e| e.contains("select the repository"))
        );
        assert!(
            form(&ui)?
                .error
                .as_deref()
                .is_some_and(|e| e.contains("F12")),
            "recovery uses the actual activation binding"
        );
        Ok(())
    }
    #[test]
    fn rules_save_failure_loading_and_escape_keep_native_safety() -> Result<()> {
        let (mut ui, dir, _) = shortcut_tests::fixture(Some(RULES_SAVE))?;
        setup(&mut ui, dir.path(), 1);
        std::fs::write(&ui.storage.config, "{malformed")?;
        ui.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
        assert_eq!(form(&ui)?.text.text(), "saved rule");
        assert!(form(&ui)?.error.is_some());
        assert_eq!(std::fs::read_to_string(&ui.storage.config)?, "{malformed");
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(ui.modal.is_none());
        setup(&mut ui, dir.path(), 1);
        let (_tx, rx) = mpsc::channel();
        let Some(Modal::RepositoryRules(f)) = &mut ui.modal else {
            anyhow::bail!("rules")
        };
        f.loading = Some(rx);
        f.root = None;
        ui.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(form(&ui)?.text.text(), "saved rule");
        ui.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
        assert!(ui.modal.is_some());
        Ok(())
    }
    #[test]
    fn rules_fields_and_buttons_preserve_copy_early_globals_and_editing() -> Result<()> {
        for field in 0..4 {
            shortcut_tests::owned_modal_context(
                |ui| {
                    setup(ui, std::path::Path::new("/temporary/repository"), field);
                    if let Some(Modal::RepositoryRules(f)) = &mut ui.modal {
                        let editor = if field == 0 {
                            Some(&mut f.repository)
                        } else if field == 1 {
                            Some(&mut f.text)
                        } else {
                            None
                        };
                        if let Some(editor) = editor {
                            *editor = Editor::from("focused");
                            editor.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
                        }
                    }
                },
                match field {
                    0 => AGENT_RULES_REPOSITORY,
                    1 => AGENT_RULES,
                    _ => AGENT_RULES_BUTTON,
                },
                (field < 2).then_some("d"),
            )?;
        }
        Ok(())
    }
    #[test]
    fn rules_native_hints_use_repository_navigation_focus_and_save_bindings() -> Result<()> {
        for width in [80, 160] {
            for action in [
                RULES_SAVE,
                RULES_REPOSITORY_PREVIOUS,
                RULES_REPOSITORY_NEXT,
                NAVIGATION_NEXT_FOCUS,
                NAVIGATION_ACTIVATE,
            ] {
                let (mut ui, dir, _) = shortcut_tests::fixture(Some(action))?;
                setup(&mut ui, dir.path(), 0);
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 44))?;
                terminal.draw(|frame| ui.draw(frame))?;
                let text = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(text.contains("F12"), "{} {width}: {text}", action.0);
            }
        }
        Ok(())
    }

    #[test]
    fn rules_visible_save_cancel_and_blank_targets_survive_activation_remap() -> Result<()> {
        for width in [40, 80, 160] {
            let (mut ui, dir, _) = shortcut_tests::fixture(Some(RULES_SAVE))?;
            setup(&mut ui, dir.path(), 1);
            let click = shortcut_tests::native_target(&mut ui, width, "Save · F12")?;
            ui.mouse(click);
            assert!(ui.modal.is_none());
            assert_eq!(
                ui.storage
                    .load_config()?
                    .repository_rules
                    .get(dir.path())
                    .map(String::as_str),
                Some("saved rule")
            );
            let (mut ui, dir, _) = shortcut_tests::fixture(Some(NAVIGATION_ACTIVATE))?;
            setup(&mut ui, dir.path(), 1);
            let click = shortcut_tests::native_target(&mut ui, width, "Cancel · Esc")?;
            ui.mouse(click);
            assert!(ui.modal.is_none());
            assert!(ui.storage.load_config()?.repository_rules.is_empty());
            let (mut ui, dir, _) = shortcut_tests::fixture(Some(NAVIGATION_ACTIVATE))?;
            setup(&mut ui, dir.path(), 1);
            let blank = shortcut_tests::native_blank_below(&mut ui, width, "Save · Ctrl")?;
            ui.mouse(blank);
            assert!(ui.modal.is_some());
            assert!(ui.storage.load_config()?.repository_rules.is_empty());
        }
        Ok(())
    }
    #[test]
    fn rules_visible_repository_option_loads_the_selected_temporary_repository() -> Result<()> {
        for width in [40, 80, 160] {
            let (mut ui, dir, _) = shortcut_tests::fixture(Some(NAVIGATION_ACTIVATE))?;
            let root = dir.path().join("repository");
            std::fs::create_dir(&root)?;
            crate::process::checked(
                crate::repo::git(&root).args(["init", "-b", "main"]),
                &crate::process::Cancel::default(),
            )?;
            let root = root.canonicalize()?;
            ui.storage.update_config(|config| {
                config
                    .repository_rules
                    .insert(root.clone(), "selected repository rules".into());
            })?;
            setup(&mut ui, &root, 0);
            let Some(Modal::RepositoryRules(f)) = &mut ui.modal else {
                anyhow::bail!("rules")
            };
            f.candidates = vec![root.clone()];
            f.repository = Editor::default();
            // Long temporary paths are clipped on narrow screens; use the visible
            // prefix to locate the actual rendered option, not an invented rectangle.
            let label = root.to_string_lossy();
            let prefix = label.get(..16).context("path prefix")?;
            let click = shortcut_tests::native_target(&mut ui, width, prefix)?;
            ui.mouse(click);
            let deadline = Instant::now() + Duration::from_secs(3);
            while form(&ui)?.loading.is_some() {
                ui.tick_rules();
                anyhow::ensure!(Instant::now() < deadline, "rules load timeout");
                thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(form(&ui)?.root.as_ref(), Some(&root));
            assert_eq!(form(&ui)?.text.text(), "selected repository rules");
            assert_eq!(form(&ui)?.field, 1);
        }
        Ok(())
    }
}
