use super::*;

const COMMANDS: &[(&str, &str)] = &[
    ("voice", "Configure hold-Space dictation and API key"),
    ("compact", "Compact the current conversation"),
    ("model", "Choose a model"),
    ("effort", "Choose the reasoning level"),
    ("skills", "Browse skills for this workspace"),
    ("status", "Show session, workspace and permissions"),
    ("diff", "Show current session changes"),
    ("new", "Launch a new coding session"),
    ("rename", "Rename this session"),
    ("help", "Search keyboard shortcuts"),
    ("actions", "Open difu session controls"),
];

impl Ui {
    pub(super) fn open_commands(&mut self, skills_only: bool) {
        self.drilled = true;
        self.focus = Focus::Composer;
        self.modal = Some(Modal::Commands {
            query: Editor::default(),
            selected: 0,
            skills_only,
            files_only: false,
        });
        if let Some(id) = self.selected.clone()
            && !self.skills.contains_key(&id)
        {
            self.load_skills(false);
        }
    }
    pub(super) fn open_paths(&mut self) {
        self.drilled = true;
        self.focus = Focus::Composer;
        self.modal = Some(Modal::Commands {
            query: Editor::default(),
            selected: 0,
            skills_only: false,
            files_only: true,
        });
        self.load_paths();
    }
    fn load_paths(&mut self) {
        if let Some(id) = self.selected.clone()
            && self.paths_loading.insert(id.clone())
        {
            self.task(
                Task::WorkspacePaths(id.clone()),
                Request::WorkspacePaths { id },
                false,
            );
        }
    }
    fn load_skills(&mut self, force: bool) {
        if let Some(id) = self.selected.clone()
            && self.skills_loading.as_ref() != Some(&id)
        {
            self.skills_loading = Some(id.clone());
            self.task(
                Task::Skills(id.clone()),
                Request::Skills { id, force },
                false,
            );
        }
    }
    fn command_entries(&self) -> Vec<(String, String, Option<Skill>)> {
        let Some(Modal::Commands {
            query,
            skills_only,
            files_only,
            ..
        }) = &self.modal
        else {
            return Vec::new();
        };
        let query = query.text().to_lowercase();
        let query = query.trim_start_matches('/');
        let mut result = Vec::new();
        if *files_only {
            if let Some(paths) = self
                .selected
                .as_ref()
                .and_then(|id| self.workspace_paths.get(id))
            {
                for path in paths
                    .iter()
                    .filter(|path| path.to_lowercase().contains(query))
                {
                    result.push((
                        format!("@{path}"),
                        if path.ends_with('/') {
                            "Folder".into()
                        } else {
                            "File".into()
                        },
                        None,
                    ));
                }
            }
            return result;
        }
        if !skills_only {
            for (name, description) in COMMANDS {
                if format!("{name} {description}").contains(query) {
                    result.push((format!("/{name}"), (*description).into(), None));
                }
            }
        }
        if let Some((skills, _)) = self.selected.as_ref().and_then(|id| self.skills.get(id)) {
            for skill in skills.iter().filter(|s| s.enabled) {
                if format!(
                    "{} {} {}",
                    skill.name,
                    skill.description,
                    skill.path.display()
                )
                .to_lowercase()
                .contains(query)
                {
                    result.push((
                        format!("${}", skill.name),
                        format!("{} · {}", skill.description, skill.path.display()),
                        Some(skill.clone()),
                    ));
                }
            }
        }
        result
    }
    pub(super) fn command_key(&mut self, key: KeyEvent) {
        let entries = self.command_entries();
        let Some(Modal::Commands {
            query, selected, ..
        }) = &mut self.modal
        else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.modal = None,
            KeyCode::Backspace if query.chars.is_empty() => self.modal = None,
            KeyCode::Up if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                *selected = selected.saturating_sub(1)
            }
            KeyCode::Down if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                *selected = (*selected + 1).min(entries.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                let index = *selected;
                self.run_command(index);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                query.clear();
                *selected = 0;
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if matches!(
                    self.modal,
                    Some(Modal::Commands {
                        files_only: true,
                        ..
                    })
                ) {
                    self.load_paths();
                } else {
                    self.load_skills(true);
                }
            }
            _ => {
                query.key(key);
                *selected = 0;
            }
        }
    }
    pub(super) fn run_command(&mut self, index: usize) {
        let Some((name, _, skill)) = self.command_entries().get(index).cloned() else {
            return;
        };
        if matches!(
            self.modal,
            Some(Modal::Commands {
                files_only: true,
                ..
            })
        ) {
            if let Some(id) = self.selected.clone() {
                let text = if name.chars().any(char::is_whitespace) {
                    format!("@{:?} ", name.trim_start_matches('@'))
                } else {
                    format!("{name} ")
                };
                self.positions.entry(id).or_default().draft.insert(&text);
                self.modal = None;
                self.focus = Focus::Composer;
            }
            return;
        }
        if let Some(skill) = skill {
            if let Some(id) = self.selected.clone() {
                let position = self.positions.entry(id).or_default();
                position.draft.insert(&format!("${} ", skill.name));
                position.skills.retain(|s| s.path != skill.path);
                position.skills.push(skill);
                self.focus = Focus::Composer;
                self.drilled = true;
                self.modal = None;
            }
            return;
        }
        match name.as_str() {
            "/voice" => self.open_voice(),
            "/compact" => self.control(Control::Compact),
            "/model" => self.open_model(0),
            "/effort" => self.open_model(1),
            "/skills" => self.open_commands(true),
            "/status" => self.modal = Some(Modal::Status),
            "/diff" => {
                self.changes_visible = true;
                self.drilled = true;
                self.changes_at = None;
                self.save_visibility();
                self.modal = None;
            }
            "/new" => self.launch(),
            "/rename" => self.menu_action(3),
            "/help" => self.modal = Some(Modal::Help(Default::default())),
            "/actions" => {
                self.modal = Some(Modal::Menu {
                    query: Editor::default(),
                    selected: 0,
                })
            }
            _ => {}
        }
    }
    pub(super) fn commands_height(&self) -> u16 {
        self.command_entries().len().clamp(1, 8) as u16 + 4
    }
    pub(super) fn draw_commands(&mut self, frame: &mut Frame, area: Rect) {
        let entries = self.command_entries();
        let Some(Modal::Commands {
            query,
            selected,
            skills_only,
            files_only,
        }) = &self.modal
        else {
            return;
        };
        let selected = *selected;
        let mut input = self
            .selected
            .as_ref()
            .and_then(|id| self.positions.get(id))
            .map(|p| p.draft.clone())
            .unwrap_or_default();
        let start = input.cursor;
        let trigger = if *files_only {
            '@'
        } else if *skills_only {
            '$'
        } else {
            '/'
        };
        input.insert(&format!("{trigger}{}", query.text()));
        input.cursor = start
            .saturating_add(1 + query.cursor)
            .min(input.chars.len());
        input.anchor = query.anchor.map(|n| start.saturating_add(n + 1));
        let input_height = area.height.min(3);
        let available = area.height.saturating_sub(input_height + 1) as usize;
        let start = selected.saturating_sub(available.saturating_sub(1));
        for (row, (index, (name, description, _))) in entries
            .iter()
            .enumerate()
            .skip(start)
            .take(available)
            .enumerate()
        {
            let rect = Rect::new(area.x, area.y + row as u16, area.width, 1);
            frame.render_widget(
                Paragraph::new(format!(
                    "{} {name:12} {description}",
                    if index == selected { "›" } else { " " }
                ))
                .style(Style::default().fg(if index == selected {
                    ACCENT
                } else {
                    TEXT
                })),
                rect,
            );
            self.hits.push((rect, Action::Command(index)));
        }
        let loading =
            self.skills_loading.as_ref() == self.selected.as_ref() && self.selected.is_some();
        let message = if *files_only {
            if self
                .selected
                .as_ref()
                .is_some_and(|id| self.paths_loading.contains(id))
            {
                "Loading workspace files…".into()
            } else if entries.is_empty() {
                "No matching files or folders".into()
            } else {
                "↑↓ choose · Enter insert path · Ctrl+R reload files · Esc close".into()
            }
        } else if loading {
            "Loading workspace skills…".into()
        } else if let Some((_, errors)) = self
            .selected
            .as_ref()
            .and_then(|id| self.skills.get(id))
            .filter(|(_, errors)| !errors.is_empty())
        {
            format!("Skill discovery: {}", errors.join("; "))
        } else if entries.is_empty() {
            "No matching commands or skills".into()
        } else {
            "↑↓ choose · Enter select · Ctrl+R reload skills · Esc close".into()
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(DIM)),
            Rect::new(
                area.x,
                area.bottom().saturating_sub(input_height + 1),
                area.width,
                1,
            ),
        );
        editor(
            frame,
            Rect::new(
                area.x,
                area.bottom().saturating_sub(input_height),
                area.width,
                input_height,
            ),
            "Message",
            &input,
            true,
        );
    }
    pub(super) fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let Some(s) = self.selected.as_ref().and_then(|id| self.sessions.get(id)) else {
            return;
        };
        let text = format!(
            "{}\n\nModel: {} · {}\nState: {:?}\nWorkspace: {}\nBranch: {}\nCodex thread: {}\n\nPermissions\n{}\n\nUses the installed Codex configuration, native workspace instructions, skills and enabled local memory.\n\nEsc Close",
            s.title,
            s.model.as_deref().unwrap_or("Inherited"),
            s.effort.as_deref().unwrap_or("Inherited"),
            s.status,
            s.workspace
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            s.branch.as_deref().unwrap_or("Existing directory"),
            s.thread_id.as_deref().unwrap_or("Connecting"),
            serde_json::to_string_pretty(&s.permissions).unwrap_or_default()
        );
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(TEXT)),
            area,
        );
    }
}
