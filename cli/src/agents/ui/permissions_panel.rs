//! Provider-native permissions, fetched off the terminal thread.
use super::*;
use crate::agents::permissions::{self, CodexPreset, Scope, SettingKind, SettingsEdit, Snapshot};
use crate::shortcuts::ids::*;
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicU64, Ordering};

static REQUEST: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Mode,
    Denied,
    Allow,
    Ask,
    Deny,
    Auto,
    Workspace,
}
impl Tab {
    fn label(self) -> &'static str {
        match self {
            Self::Mode => "Mode",
            Self::Denied => "Recently denied",
            Self::Allow => "Allow",
            Self::Ask => "Ask",
            Self::Deny => "Deny",
            Self::Auto => "Auto mode",
            Self::Workspace => "Workspace",
        }
    }
    fn kinds(self) -> &'static [SettingKind] {
        match self {
            Self::Allow => &[SettingKind::Allow],
            Self::Ask => &[SettingKind::Ask],
            Self::Deny => &[SettingKind::Deny],
            Self::Auto => &[
                SettingKind::AutoAllow,
                SettingKind::AutoSoftDeny,
                SettingKind::AutoHardDeny,
                SettingKind::AutoEnvironment,
            ],
            Self::Workspace => &[SettingKind::Directory],
            _ => &[],
        }
    }
}
#[derive(Clone, Copy)]
enum Item {
    Mode(usize),
    Preset(usize),
    Row(usize),
    Denial(usize),
}
struct Form {
    kind: SettingKind,
    scope: Scope,
    before: Option<String>,
    revision: Option<String>,
    value: Editor,
    field: usize,
    delete: bool,
}
impl Form {
    fn cycle_scope(&mut self, reverse: bool) {
        if self.before.is_some() || is_auto(self.kind) {
            return;
        }
        self.scope = match (self.scope, reverse) {
            (Scope::Session, false) | (Scope::Local, true) => Scope::Project,
            (Scope::Project, false) | (Scope::User, true) => Scope::Local,
            (Scope::Local, false) | (Scope::Session, true) => Scope::User,
            _ => Scope::Session,
        };
    }
    fn cycle_kind(&mut self, reverse: bool) {
        if self.before.is_some() || !is_auto(self.kind) {
            return;
        }
        let kinds = Tab::Auto.kinds();
        let index = kinds.iter().position(|k| *k == self.kind).unwrap_or(0);
        let index = if reverse {
            (index + kinds.len() - 1) % kinds.len()
        } else {
            (index + 1) % kinds.len()
        };
        if let Some(kind) = kinds.get(index) {
            self.kind = *kind;
        }
    }
    fn edit(&self, data: &Snapshot) -> Result<SettingsEdit> {
        let value = self.value.text();
        anyhow::ensure!(
            self.delete || !value.trim().is_empty(),
            "Enter a rule, directory or environment description before saving"
        );
        let revision = self
            .revision
            .clone()
            .or_else(|| data.revisions.get(&self.scope).cloned())
            .context("This settings source is unavailable; refresh permissions before saving")?;
        Ok(SettingsEdit {
            kind: self.kind,
            scope: self.scope,
            before: self.before.clone(),
            after: (!self.delete).then_some(value),
            revision,
        })
    }
}
fn is_auto(kind: SettingKind) -> bool {
    Tab::Auto.kinds().contains(&kind)
}

fn mode_label(mode: Option<&str>, provider: provider::Provider) -> &str {
    match mode {
        Some("default") => {
            if provider == provider::Provider::Claude {
                "Manual"
            } else {
                "Regular"
            }
        }
        Some("acceptEdits") => "Accept edits",
        Some("plan") => "Plan",
        Some("bypassPermissions") => "Bypass",
        Some("auto") => "Auto",
        Some("dontAsk") => "Don't ask",
        Some(mode) => mode,
        None => "Inherited",
    }
}
fn connection_failure(error: Option<&str>) -> bool {
    error.is_some_and(|error| {
        error.contains("channel is empty")
            || error.contains("sending half is closed")
            || error.contains("No conversation found")
    })
}
fn readable_error(error: &str, refresh: &str) -> String {
    if error.contains("channel is empty") || error.contains("sending half is closed") {
        format!("Agent connection closed. Press {refresh} to reconnect.")
    } else if error.contains("No conversation found") {
        format!("Agent session could not be resumed. Press {refresh} to reconnect.")
    } else {
        let first = provider_error_text(error)
            .lines()
            .next()
            .unwrap_or("Turn failed")
            .trim();
        format!("{first} · {refresh} retry")
    }
}
pub(super) fn composer_status(session: &Session, binding: &str) -> Line<'static> {
    let state = &session.permission_state;
    let mut spans = vec![Span::styled(
        mode_label(state.mode.as_deref(), session.provider).to_owned(),
        Style::default().fg(ACCENT),
    )];
    if session.provider == provider::Provider::Codex {
        spans.push(Span::styled(
            format!(
                " · {}",
                permissions::detected_preset(&session.permissions).map_or(
                    if session.permissions.is_null() {
                        "Inherited"
                    } else {
                        "Custom"
                    },
                    |p| p.label()
                )
            ),
            Style::default().fg(DIM),
        ));
    }
    if session.status == Status::Failed {
        spans.push(Span::styled(
            if connection_failure(session.error.as_deref()) {
                " · reconnect needed"
            } else {
                " · Turn failed"
            },
            Style::default().fg(RED),
        ));
    } else if session.status == Status::Interrupted {
        spans.push(Span::styled(" · Interrupted", Style::default().fg(DIM)));
    } else if let Some(target) = &state.pending_mode {
        spans.push(Span::styled(
            if state.reload_pending {
                format!(
                    " · switching to {}…",
                    mode_label(Some(target), session.provider)
                )
            } else {
                format!(
                    " · {} next turn",
                    mode_label(Some(target), session.provider)
                )
            },
            Style::default().fg(DIM),
        ));
    } else if let Some(preset) = state.pending_preset {
        spans.push(Span::styled(
            format!(" · {} next turn", preset.label()),
            Style::default().fg(DIM),
        ));
    } else if state.reload_pending {
        spans.push(Span::styled(
            " · settings pending",
            Style::default().fg(DIM),
        ));
    }
    spans.push(Span::styled(
        format!("  {binding} · /permissions"),
        Style::default().fg(DIM),
    ));
    Line::from(spans)
}
fn mode_description(id: &str, provider: provider::Provider) -> &'static str {
    match id {
        "default" if provider == provider::Provider::Claude => {
            "Ask before actions that need approval."
        }
        "default" => "Work with the current approval policy.",
        "acceptEdits" => "Approve file edits; ask for other actions.",
        "plan" => "Explore and plan before making changes.",
        "bypassPermissions" => "Skip permission checks for this session.",
        "auto" => "Let Claude review actions automatically.",
        "dontAsk" => "Decline actions that require approval.",
        _ => "",
    }
}

pub struct State {
    id: String,
    data: Snapshot,
    error: Option<String>,
    loading: bool,
    request: u64,
    tab: Tab,
    query: Editor,
    searching: bool,
    selected: usize,
    form: Option<Form>,
    preserve_form: bool,
}
impl State {
    fn new(id: String, provider: provider::Provider) -> Self {
        Self {
            id,
            data: Snapshot {
                provider,
                ..Snapshot::default()
            },
            error: None,
            loading: false,
            request: 0,
            tab: Tab::Mode,
            query: Editor::default(),
            searching: false,
            selected: 0,
            form: None,
            preserve_form: false,
        }
    }
    fn tabs(&self) -> &'static [Tab] {
        if self.data.provider == provider::Provider::Claude {
            &[
                Tab::Mode,
                Tab::Denied,
                Tab::Allow,
                Tab::Ask,
                Tab::Deny,
                Tab::Auto,
                Tab::Workspace,
            ]
        } else {
            &[Tab::Mode]
        }
    }
    fn items(&self) -> Vec<Item> {
        let query = self.query.text().to_lowercase();
        let matches = |text: String| text.to_lowercase().contains(&query);
        match self.tab {
            Tab::Mode => self
                .data
                .modes
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    matches(format!(
                        "{} {} {}",
                        c.label,
                        c.description,
                        c.reason.as_deref().unwrap_or_default()
                    ))
                })
                .map(|(i, _)| Item::Mode(i))
                .chain(
                    self.data
                        .presets
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| {
                            matches(format!(
                                "{} {} {}",
                                c.label,
                                c.description,
                                c.reason.as_deref().unwrap_or_default()
                            ))
                        })
                        .map(|(i, _)| Item::Preset(i)),
                )
                .collect(),
            Tab::Denied => self
                .data
                .denials
                .iter()
                .enumerate()
                .filter(|(_, d)| {
                    matches(format!(
                        "{} {} {}",
                        d.tool,
                        d.detail,
                        d.reason.as_deref().unwrap_or_default()
                    ))
                })
                .map(|(i, _)| Item::Denial(i))
                .collect(),
            tab => self
                .data
                .rows
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    tab.kinds().contains(&r.kind)
                        && matches(format!(
                            "{} {} {} {}",
                            r.kind.label(),
                            r.value,
                            r.source,
                            r.scope.map_or("Managed", |s| s.label())
                        ))
                })
                .map(|(i, _)| Item::Row(i))
                .collect(),
        }
    }
    fn begin_edit(&mut self, new: bool) {
        if self.loading {
            return;
        }
        self.error = None;
        if new {
            let Some(kind) = self.tab.kinds().first().copied() else {
                return;
            };
            self.form = Some(Form {
                kind,
                scope: if is_auto(kind) {
                    Scope::User
                } else {
                    Scope::Session
                },
                before: None,
                revision: None,
                value: Editor::default(),
                field: 2,
                delete: false,
            });
        } else if let Some(Item::Row(index)) = self.items().get(self.selected)
            && let Some(row) = self.data.rows.get(*index)
        {
            if !row.editable || row.scope.is_none() {
                self.error = Some(format!("This entry is read-only. Source: {}", row.source));
                return;
            }
            self.form = Some(Form {
                kind: row.kind,
                scope: row.scope.unwrap_or(Scope::Session),
                before: Some(row.value.clone()),
                revision: Some(row.revision.clone()),
                value: Editor::from(row.value.as_str()),
                field: 2,
                delete: false,
            });
        }
    }
    pub(super) fn finish(
        &mut self,
        id: &str,
        request: u64,
        result: std::result::Result<Reply, String>,
    ) {
        if self.id != id || self.request != request {
            return;
        }
        self.loading = false;
        match result {
            Ok(Reply::Permissions { snapshot }) => {
                self.data = snapshot;
                self.error = None;
                if self.preserve_form {
                    if let Some(form) = &mut self.form
                        && let Some(before) = &form.before
                    {
                        if let Some(row) = self.data.rows.iter().find(|row| {
                            row.kind == form.kind
                                && row.scope == Some(form.scope)
                                && row.value == *before
                                && row.editable
                        }) {
                            form.revision = Some(row.revision.clone());
                        } else {
                            self.error = Some("This entry changed or became read-only. Cancel and inspect the refreshed source; your draft is preserved.".into());
                        }
                    }
                } else {
                    self.form = None;
                }
                self.selected = self.selected.min(self.items().len().saturating_sub(1));
            }
            Ok(_) => {
                self.error = Some(
                    "Unexpected permissions response; restart Difu's agent service after updating"
                        .into(),
                )
            }
            Err(error) => self.error = Some(error),
        }
        self.preserve_form = false;
    }
    pub(super) fn shortcut_context(&self) -> crate::shortcuts::Context {
        use crate::shortcuts::contexts::*;
        if let Some(form) = &self.form {
            if form.delete {
                AGENT_PERMISSION_DELETE
            } else {
                match form.field {
                    0 | 1 => AGENT_PERMISSION_CHOICE,
                    2 if form.kind == SettingKind::AutoEnvironment => AGENT_PERMISSION_MULTILINE,
                    2 => AGENT_PERMISSION_FORM,
                    _ => AGENT_PERMISSION_BUTTON,
                }
            }
        } else if self.searching {
            AGENT_PERMISSION_SEARCH
        } else {
            AGENT_PERMISSIONS
        }
    }
    pub(super) fn editor(&self) -> Option<&Editor> {
        self.form
            .as_ref()
            .filter(|f| f.field == 2 && !f.delete)
            .map(|f| &f.value)
            .or_else(|| self.searching.then_some(&self.query))
    }
    pub(super) fn paste(&mut self, value: &str) {
        if self.loading {
            return;
        }
        if let Some(form) = self.form.as_mut().filter(|f| f.field == 2 && !f.delete) {
            form.value.insert(value);
        } else if self.searching {
            self.query.insert(value);
            self.selected = 0;
        }
    }
}

impl Ui {
    pub(super) fn open_permissions(&mut self) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        if !matches!(session.job, Job::Coding(_)) {
            self.notice = Some(("Permissions are available for coding agents".into(), true));
            return;
        }
        self.modal = Some(Modal::Permissions(Box::new(State::new(
            id,
            session.provider,
        ))));
        self.request_permissions(permissions::Action::Refresh);
    }
    fn request_permissions(&mut self, action: permissions::Action) {
        let Some(Modal::Permissions(state)) = &mut self.modal else {
            return;
        };
        if state.loading {
            return;
        }
        state.preserve_form =
            matches!(action, permissions::Action::Refresh) && state.form.is_some();
        state.loading = true;
        state.error = None;
        state.request = REQUEST.fetch_add(1, Ordering::Relaxed);
        let (id, request) = (state.id.clone(), state.request);
        self.task(
            Task::Permissions(id.clone(), request),
            Request::Permissions { id, action },
            false,
        );
    }
    pub(super) fn cycle_permission_mode(&mut self) {
        let Some(id) = self.selected.clone().filter(|id| {
            self.sessions
                .get(id)
                .is_some_and(|s| matches!(s.job, Job::Coding(_)))
        }) else {
            return;
        };
        self.task(
            Task::Permissions(id.clone(), 0),
            Request::Permissions {
                id,
                action: permissions::Action::CycleMode,
            },
            false,
        );
    }
    pub(super) fn tick_permissions(&mut self) {
        // Normal session watches carry mode/pending status. Refresh settings only
        // on request; a native settings inspection may run provider subprocesses.
        if let Some(Modal::Permissions(state)) = &mut self.modal
            && !state.loading
            && let Some(session) = self.sessions.get(&state.id)
        {
            if let Some(mode) = &session.permission_state.mode {
                state.data.active_mode = Some(mode.clone());
            }
            state
                .data
                .pending_mode
                .clone_from(&session.permission_state.pending_mode);
            state.data.active_preset = permissions::detected_preset(&session.permissions);
            state.data.pending_preset = session.permission_state.pending_preset;
            state
                .data
                .denials
                .clone_from(&session.permission_state.denials);
            state.data.reload_pending = session.permission_state.reload_pending;
        }
    }
    pub(super) fn finish_permissions(
        &mut self,
        id: &str,
        request: u64,
        result: std::result::Result<Reply, String>,
    ) {
        if let Ok(Reply::Permissions { snapshot }) = &result
            && let Some(session) = self.sessions.get_mut(id)
        {
            session
                .permission_state
                .mode
                .clone_from(&snapshot.active_mode);
            session
                .permission_state
                .pending_mode
                .clone_from(&snapshot.pending_mode);
            session.permission_state.pending_preset = snapshot.pending_preset;
            session.permission_state.reload_pending = snapshot.reload_pending;
        }
        if let Some(Modal::Permissions(state)) = &mut self.modal
            && state.id == id
            && state.request == request
        {
            state.finish(id, request, result);
        } else if request == 0
            && let Err(error) = result
        {
            self.notice = Some((error, true));
        }
        self.task(
            Task::Read(id.to_owned()),
            Request::Read {
                id: id.to_owned(),
                version: None,
            },
            false,
        );
    }
    pub(super) fn permissions_field(&mut self, index: usize) {
        if let Some(Modal::Permissions(state)) = &mut self.modal
            && !state.loading
            && let Some(form) = &mut state.form
        {
            form.field = index.min(3);
        }
    }
    pub(super) fn permissions_tab(&mut self, index: usize) {
        if let Some(Modal::Permissions(state)) = &mut self.modal
            && state.form.is_none()
            && let Some(tab) = state.tabs().get(index).copied()
        {
            state.tab = tab;
            state.selected = 0;
            state.query.clear();
            state.searching = false;
        }
    }
    pub(super) fn permissions_select(&mut self, index: Option<usize>) {
        let Some(Modal::Permissions(state)) = &mut self.modal else {
            return;
        };
        if state.loading || state.form.is_some() {
            return;
        }
        if let Some(index) = index {
            state.selected = index;
        }
        let item = state.items().get(state.selected).copied();
        let action = match item {
            Some(Item::Mode(i)) => state
                .data
                .modes
                .get(i)
                .map(|c| (c, permissions::Action::SetMode { mode: c.id.clone() })),
            Some(Item::Preset(i)) => state.data.presets.get(i).and_then(|c| {
                let preset = match c.id.as_str() {
                    "Ask" => CodexPreset::Ask,
                    "Auto" => CodexPreset::Auto,
                    "Full" => CodexPreset::Full,
                    "ReadOnly" => CodexPreset::ReadOnly,
                    _ => return None,
                };
                Some((c, permissions::Action::SetPreset { preset }))
            }),
            Some(Item::Row(_)) => {
                state.begin_edit(false);
                return;
            }
            _ => None,
        };
        if let Some((choice, action)) = action {
            if choice.available {
                self.request_permissions(action);
            } else {
                state.error = Some(choice.reason.clone().unwrap_or_else(|| {
                    "This option is unavailable for the current provider".into()
                }));
            }
        }
    }
    pub(super) fn permissions_scroll(&mut self, down: bool) {
        let Some(Modal::Permissions(state)) = &mut self.modal else {
            return;
        };
        let editor = if let Some(form) = &mut state.form {
            if state.loading || form.delete || form.field != 2 {
                return;
            }
            Some(&mut form.value)
        } else if state.searching {
            Some(&mut state.query)
        } else {
            None
        };
        if let Some(editor) = editor {
            // Wheel motion inside native text fields remains Editor navigation.
            for _ in 0..3 {
                editor.key(KeyEvent::new(
                    if down { KeyCode::Down } else { KeyCode::Up },
                    KeyModifiers::NONE,
                ));
            }
        } else {
            state.selected = if down {
                state
                    .selected
                    .saturating_add(3)
                    .min(state.items().len().saturating_sub(1))
            } else {
                state.selected.saturating_sub(3)
            };
        }
    }
    pub(super) fn permissions_save(&mut self) {
        let Some(Modal::Permissions(state)) = &mut self.modal else {
            return;
        };
        if state.loading {
            return;
        }
        let Some(form) = &state.form else {
            return;
        };
        match form.edit(&state.data) {
            Ok(edit) => self.request_permissions(permissions::Action::Edit { edit }),
            Err(error) => state.error = Some(error.to_string()),
        }
    }
    pub(super) fn permissions_search(&mut self) {
        if let Some(Modal::Permissions(state)) = &mut self.modal
            && state.form.is_none()
        {
            state.searching = true;
        }
    }
    pub(super) fn permissions_key(&mut self, key: KeyEvent) {
        let shortcut = self.shortcut(key);
        let Some(Modal::Permissions(state)) = &mut self.modal else {
            return;
        };
        if let Some(form) = &mut state.form {
            if key.code == KeyCode::Esc {
                if !state.loading {
                    state.form = None;
                    state.error = None;
                }
                return;
            }
            if state.loading {
                return;
            }
            if shortcut == Some(PERMISSIONS_REFRESH_FORM) {
                self.request_permissions(permissions::Action::Refresh);
                return;
            }
            if form.delete {
                if shortcut == Some(NAVIGATION_ACTIVATE) {
                    self.permissions_save();
                }
                return;
            }
            match shortcut {
                Some(NAVIGATION_NEXT_FOCUS | NAVIGATION_PREVIOUS_FOCUS) => {
                    form.field = (form.field
                        + if shortcut == Some(NAVIGATION_PREVIOUS_FOCUS) {
                            3
                        } else {
                            1
                        })
                        % 4;
                }
                Some(PERMISSIONS_PREVIOUS_VALUE | PERMISSIONS_NEXT_VALUE) => {
                    if form.field == 0 {
                        form.cycle_kind(shortcut == Some(PERMISSIONS_PREVIOUS_VALUE));
                    } else if form.field == 1 {
                        form.cycle_scope(shortcut == Some(PERMISSIONS_PREVIOUS_VALUE));
                    }
                }
                Some(PERMISSIONS_SUBMIT) => self.permissions_save(),
                Some(PERMISSIONS_ACTIVATE) if form.field == 3 => self.permissions_save(),
                Some(PERMISSIONS_ACTIVATE) if form.field == 2 => form.field = 3,
                _ if form.field == 2 => {
                    // This is native field editing, never an action shortcut.
                    if form.kind == SettingKind::AutoEnvironment
                        && key.code == KeyCode::Enter
                        && key.modifiers == KeyModifiers::SHIFT
                    {
                        form.value.insert("\n");
                    } else {
                        form.value.key(key);
                    }
                }
                _ => {}
            }
            return;
        }
        if state.searching {
            match shortcut {
                _ if key.code == KeyCode::Esc => state.searching = false,
                Some(PERMISSIONS_FINISH_SEARCH) => state.searching = false,
                _ => {
                    state.query.key(key);
                    state.selected = 0;
                }
            }
            return;
        }
        match shortcut {
            _ if key.code == KeyCode::Esc => self.modal = None,
            Some(PERMISSIONS_SEARCH) => state.searching = true,
            Some(PERMISSIONS_REFRESH) => self.request_permissions(permissions::Action::Refresh),
            Some(PERMISSIONS_PREVIOUS_TAB | PERMISSIONS_NEXT_TAB) => {
                let index = state
                    .tabs()
                    .iter()
                    .position(|tab| *tab == state.tab)
                    .unwrap_or(0);
                let len = state.tabs().len();
                let reverse = shortcut == Some(PERMISSIONS_PREVIOUS_TAB);
                self.permissions_tab(if reverse {
                    (index + len - 1) % len
                } else {
                    (index + 1) % len
                });
            }
            Some(NAVIGATION_UP) => state.selected = state.selected.saturating_sub(1),
            Some(NAVIGATION_DOWN) => {
                state.selected = (state.selected + 1).min(state.items().len().saturating_sub(1))
            }
            Some(NAVIGATION_PAGE_UP) => state.selected = state.selected.saturating_sub(5),
            Some(NAVIGATION_PAGE_DOWN) => {
                state.selected = (state.selected + 5).min(state.items().len().saturating_sub(1))
            }
            Some(NAVIGATION_FIRST) => state.selected = 0,
            Some(NAVIGATION_LAST) => state.selected = state.items().len().saturating_sub(1),
            Some(PERMISSIONS_ADD) => state.begin_edit(true),
            Some(PERMISSIONS_EDIT) => state.begin_edit(false),
            Some(PERMISSIONS_DELETE) if !state.loading => {
                state.begin_edit(false);
                if let Some(form) = &mut state.form {
                    form.delete = true;
                }
            }
            Some(NAVIGATION_ACTIVATE) => self.permissions_select(None),
            _ => {}
        }
    }
    pub(super) fn draw_permissions_overlay(&mut self, frame: &mut Frame) {
        let full = frame.area();
        let desired = match &self.modal {
            Some(Modal::Permissions(state)) if state.form.is_some() => 23,
            Some(Modal::Permissions(state)) if state.tab == Tab::Mode => (state.items().len()
                as u16
                + 12
                + u16::from(state.error.is_some()) * 3
                + u16::from(state.data.message.is_some()) * 2)
                .min(24),
            Some(Modal::Permissions(state)) => (state.items().len() as u16 * 3 + 9).clamp(14, 30),
            _ => 18,
        };
        let width = full.width.saturating_sub(6).min(90);
        let height = full.height.saturating_sub(4).min(desired);
        let rect = Rect::new(
            full.x + full.width.saturating_sub(width) / 2,
            full.y + full.height.saturating_sub(height) / 3,
            width,
            height,
        );
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Block::default()
                .style(Style::default().bg(PANEL).fg(TEXT))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(BORDER)),
            rect,
        );
        self.hits.clear();
        self.draw_permissions(
            frame,
            Rect::new(
                rect.x + rect.width.min(2),
                rect.y + rect.height.min(1),
                rect.width.saturating_sub(4),
                rect.height.saturating_sub(2),
            ),
        );
    }
    pub(super) fn draw_permissions(&mut self, frame: &mut Frame, area: Rect) {
        let bindings = crate::shortcuts::Bindings::new(&self.shortcut_config);
        let hint = |id| bindings.primary_hint_in(id, crate::shortcuts::contexts::AGENT_PERMISSIONS);
        let field_hint =
            |id| bindings.primary_hint_in(id, crate::shortcuts::contexts::AGENT_PERMISSION_FORM);
        let choice_hint =
            |id| bindings.primary_hint_in(id, crate::shortcuts::contexts::AGENT_PERMISSION_CHOICE);
        let Some(Modal::Permissions(state)) = &mut self.modal else {
            return;
        };
        if area.is_empty() {
            return;
        }
        let mut lines = vec![Line::styled(
            format!(
                "{} permissions",
                if state.data.provider == provider::Provider::Claude {
                    "Claude"
                } else {
                    "Codex"
                }
            ),
            Style::default()
                .fg(ACCENT)
                .add_modifier(ratatui::style::Modifier::BOLD),
        )];
        let mut tabs = Vec::new();
        let mut x = area.x;
        for (i, tab) in state.tabs().iter().enumerate() {
            let label = format!(" {} ", tab.label());
            let width = unicode_width::UnicodeWidthStr::width(label.as_str()) as u16;
            if x < area.right() {
                self.hits.push((
                    Rect::new(
                        x,
                        area.y.saturating_add(1),
                        width.min(area.right().saturating_sub(x)),
                        1,
                    ),
                    Action::PermissionTab(i),
                ));
            }
            x = x.saturating_add(width);
            tabs.push(Span::styled(
                label,
                Style::default().fg(if *tab == state.tab { ACCENT } else { DIM }),
            ));
        }
        lines.push(Line::from(tabs));
        let failure = self
            .sessions
            .get(&state.id)
            .filter(|session| session.status == Status::Failed)
            .map(|session| session.error.as_deref().unwrap_or("Turn failed"));
        let interrupted = self
            .sessions
            .get(&state.id)
            .is_some_and(|session| session.status == Status::Interrupted);
        let failed = failure.is_some() || state.error.is_some() || interrupted;
        let mut current = format!(
            "Current: {}",
            mode_label(state.data.active_mode.as_deref(), state.data.provider)
        );
        if state.data.provider == provider::Provider::Codex {
            current.push_str(&format!(
                " · {}",
                state.data.active_preset.map_or("Custom", |p| p.label())
            ));
        }
        lines.push(Line::styled(current, Style::default().fg(DIM)));
        if let Some(error) = state.error.as_deref().or(failure) {
            usage::text_lines(
                &readable_error(error, &hint(PERMISSIONS_REFRESH)),
                area.width,
                Style::default().fg(RED),
                &mut lines,
            );
        } else if interrupted {
            lines.push(Line::styled("Interrupted", Style::default().fg(DIM)));
        } else if state.loading {
            lines.push(Line::styled("Updating…", Style::default().fg(ACCENT)));
        } else if state.data.reload_pending {
            let text = if let Some(mode) = &state.data.pending_mode {
                format!(
                    "Switching to {}…",
                    mode_label(Some(mode), state.data.provider)
                )
            } else {
                "Settings update pending".into()
            };
            lines.push(Line::styled(text, Style::default().fg(ACCENT)));
        } else if let Some(mode) = &state.data.pending_mode {
            lines.push(Line::styled(
                format!(
                    "{} · next turn",
                    mode_label(Some(mode), state.data.provider)
                ),
                Style::default().fg(DIM),
            ));
        } else if let Some(preset) = state.data.pending_preset {
            lines.push(Line::styled(
                format!("{} · next turn", preset.label()),
                Style::default().fg(DIM),
            ));
        }
        if let Some(message) = &state.data.message {
            usage::text_lines(message, area.width, Style::default().fg(DIM), &mut lines);
        }
        if let Some(form) = &state.form {
            lines.push(Line::default());
            if form.delete {
                lines.push(Line::styled(
                    format!("Delete {} from {}?", form.kind.label(), form.scope.label()),
                    Style::default().fg(RED),
                ));
                usage::text_lines(
                    form.before.as_deref().unwrap_or_default(),
                    area.width,
                    Style::default().fg(TEXT),
                    &mut lines,
                );
            } else {
                lines.push(Line::raw(if form.before.is_some() {
                    "Edit permission entry"
                } else {
                    "Add permission entry"
                }));
                self.hits.push((
                    Rect::new(
                        area.x,
                        area.y.saturating_add(lines.len() as u16),
                        area.width,
                        1,
                    ),
                    Action::PermissionField(0),
                ));
                lines.push(Line::styled(
                    format!(
                        "{} Kind: {}{}",
                        if form.field == 0 { ">" } else { " " },
                        form.kind.label(),
                        if is_auto(form.kind) && form.before.is_none() {
                            format!(
                                " · {}/{} change",
                                choice_hint(PERMISSIONS_PREVIOUS_VALUE),
                                choice_hint(PERMISSIONS_NEXT_VALUE)
                            )
                        } else {
                            String::new()
                        }
                    ),
                    Style::default().fg(if form.field == 0 { ACCENT } else { TEXT }),
                ));
                self.hits.push((
                    Rect::new(
                        area.x,
                        area.y.saturating_add(lines.len() as u16),
                        area.width,
                        1,
                    ),
                    Action::PermissionField(1),
                ));
                lines.push(Line::styled(
                    format!(
                        "{} Scope: {}{}",
                        if form.field == 1 { ">" } else { " " },
                        form.scope.label(),
                        if form.before.is_none() && !is_auto(form.kind) {
                            format!(
                                " · {}/{} change",
                                choice_hint(PERMISSIONS_PREVIOUS_VALUE),
                                choice_hint(PERMISSIONS_NEXT_VALUE)
                            )
                        } else {
                            " · source fixed".into()
                        }
                    ),
                    Style::default().fg(if form.field == 1 { ACCENT } else { TEXT }),
                ));
                if let Some(before) = &form.before {
                    if let Some(row) = state.data.rows.iter().find(|r| {
                        r.kind == form.kind && r.scope == Some(form.scope) && &r.value == before
                    }) {
                        usage::text_lines(
                            &format!("Source: {}", row.source),
                            area.width,
                            Style::default().fg(DIM),
                            &mut lines,
                        );
                    }
                } else {
                    lines.push(Line::styled(
                        match form.scope {
                            Scope::Session => "This session only",
                            Scope::Project => {
                                "Project .claude/settings.json · shared with this project"
                            }
                            Scope::Local => {
                                "Native repository .claude/settings.local.json · local settings"
                            }
                            Scope::User => "User Claude settings · applies across projects",
                        },
                        Style::default().fg(DIM),
                    ));
                }
                let head = lines.len() as u16;
                frame.render_widget(Paragraph::new(lines), area);
                let edit_rect = Rect::new(
                    area.x,
                    area.y.saturating_add(head),
                    area.width,
                    area.height
                        .saturating_sub(head + 4)
                        .clamp(1, 8)
                        .min(area.height.saturating_sub(head)),
                );
                self.hits.push((edit_rect, Action::PermissionField(2)));
                editor(
                    frame,
                    edit_rect,
                    if form.kind == SettingKind::AutoEnvironment {
                        "Environment description"
                    } else {
                        "Rule / directory"
                    },
                    &form.value,
                    form.field == 2,
                );
                let foot_y = edit_rect.bottom().min(area.bottom().saturating_sub(1));
                if foot_y < area.bottom() {
                    self.hits.push((
                        Rect::new(area.x, foot_y, area.width.min(7), 1),
                        Action::PermissionSave,
                    ));
                    frame.render_widget(
                        Paragraph::new(format!(
                            "{} Save · {} save · Esc cancel\n{}/{} fields · {} accept\n{} refresh{}",
                            if form.field == 3 { ">" } else { " " },
                            field_hint(PERMISSIONS_SUBMIT),
                            field_hint(NAVIGATION_NEXT_FOCUS),
                            field_hint(NAVIGATION_PREVIOUS_FOCUS),
                            field_hint(PERMISSIONS_ACTIVATE),
                            field_hint(PERMISSIONS_REFRESH_FORM),
                            if form.kind == SettingKind::AutoEnvironment {
                                " · Shift+Enter newline"
                            } else {
                                ""
                            }
                        ))
                        .style(Style::default().fg(if form.field == 3 {
                            ACCENT
                        } else {
                            DIM
                        })),
                        Rect::new(
                            area.x,
                            foot_y,
                            area.width,
                            area.bottom().saturating_sub(foot_y).min(3),
                        ),
                    );
                }
                return;
            }
        } else {
            if state.searching || !state.query.text().is_empty() {
                self.hits.push((
                    Rect::new(
                        area.x,
                        area.y.saturating_add(lines.len() as u16),
                        area.width,
                        1,
                    ),
                    Action::PermissionSearch,
                ));
                lines.push(Line::styled(
                    format!("Search: {}", state.query.text()),
                    Style::default().fg(ACCENT),
                ));
            }
            lines.push(Line::default());
            let items = state.items();
            if items.is_empty() {
                let text = if !state.query.text().is_empty() {
                    "No permissions match this search."
                } else if state.tab == Tab::Denied {
                    "No denied tools observed in this Difu session."
                } else if state.tab == Tab::Mode {
                    &format!(
                        "Provider permissions have not been reported yet. Press {} to refresh.",
                        hint(PERMISSIONS_REFRESH)
                    )
                } else {
                    &format!(
                        "No entries in this category. Press {} to add one.",
                        hint(PERMISSIONS_ADD)
                    )
                };
                usage::text_lines(text, area.width, Style::default().fg(DIM), &mut lines);
                if state.tab == Tab::Denied {
                    lines.push(Line::styled(
                        "This is observed session history, not all provider denials.",
                        Style::default().fg(DIM),
                    ));
                }
            }
            let available = usize::from(area.height.saturating_sub(lines.len() as u16 + 3));
            let mut entries = Vec::new();
            for (index, item) in items.iter().enumerate() {
                let mut lines = Vec::new();
                let selected = index == state.selected;
                let color = if selected { ACCENT } else { TEXT };
                let prefix = if selected { "> " } else { "  " };
                match *item {
                    Item::Mode(i) | Item::Preset(i) => {
                        let preset = matches!(item, Item::Preset(_));
                        if let Some(choice) = if preset {
                            state.data.presets.get(i)
                        } else {
                            state.data.modes.get(i)
                        } {
                            let current = if preset {
                                state
                                    .data
                                    .active_preset
                                    .is_some_and(|p| format!("{p:?}") == choice.id)
                            } else {
                                state.data.active_mode.as_deref() == Some(choice.id.as_str())
                            };
                            let pending = !failed
                                && if preset {
                                    state
                                        .data
                                        .pending_preset
                                        .is_some_and(|p| format!("{p:?}") == choice.id)
                                } else {
                                    state.data.pending_mode.as_deref() == Some(choice.id.as_str())
                                };
                            if preset && i == 0 {
                                lines.push(Line::styled(
                                    "Approval policy",
                                    Style::default().fg(DIM),
                                ));
                            }
                            let label = if preset {
                                choice.label.as_str()
                            } else {
                                mode_label(Some(&choice.id), state.data.provider)
                            };
                            lines.push(Line::styled(
                                format!(
                                    "{prefix}{label}{}{}{}",
                                    if current { " · current" } else { "" },
                                    if pending {
                                        if state.data.reload_pending {
                                            " · switching"
                                        } else {
                                            " · next turn"
                                        }
                                    } else {
                                        ""
                                    },
                                    if !choice.available {
                                        " · unavailable"
                                    } else {
                                        ""
                                    }
                                ),
                                Style::default().fg(color),
                            ));
                            if selected {
                                let description = choice.reason.as_deref().unwrap_or_else(|| {
                                    if preset {
                                        match choice.id.as_str() {
                                            "Ask" => "Ask before actions outside the workspace.",
                                            "Auto" => "Let Codex review approval requests.",
                                            "Full" => "Allow actions without sandbox restrictions.",
                                            "ReadOnly" => {
                                                "Inspect files without editing the workspace."
                                            }
                                            _ => choice.description.as_str(),
                                        }
                                    } else {
                                        mode_description(&choice.id, state.data.provider)
                                    }
                                });
                                if !description.is_empty() {
                                    usage::text_lines(
                                        &format!("  {description}"),
                                        area.width,
                                        Style::default().fg(DIM),
                                        &mut lines,
                                    );
                                }
                            }
                        }
                    }
                    Item::Row(i) => {
                        if let Some(row) = state.data.rows.get(i) {
                            usage::text_lines(
                                &format!(
                                    "{prefix}{}{}: {}",
                                    row.kind.label(),
                                    if row.editable { "" } else { " · read-only" },
                                    row.value
                                ),
                                area.width,
                                Style::default().fg(color),
                                &mut lines,
                            );
                            usage::text_lines(
                                &format!(
                                    "  {} · {}",
                                    row.scope.map_or("Managed / built-in", |s| s.label()),
                                    row.source
                                ),
                                area.width,
                                Style::default().fg(DIM),
                                &mut lines,
                            );
                        }
                    }
                    Item::Denial(i) => {
                        if let Some(denial) = state.data.denials.get(i) {
                            usage::text_lines(
                                &format!("{prefix}{}: {}", denial.tool, denial.detail),
                                area.width,
                                Style::default().fg(color),
                                &mut lines,
                            );
                            if let Some(reason) = &denial.reason {
                                usage::text_lines(
                                    &format!("  {reason}"),
                                    area.width,
                                    Style::default().fg(DIM),
                                    &mut lines,
                                );
                            }
                        }
                    }
                }
                if state.tab != Tab::Mode {
                    lines.push(Line::default());
                }
                entries.push(lines);
            }
            // Count wrapped lines so long sources cannot scroll the selected row away.
            let mut start = state.selected.min(entries.len().saturating_sub(1));
            let mut used = entries.get(start).map_or(0, Vec::len);
            while start > 0 {
                let previous = entries.get(start - 1).map_or(0, Vec::len);
                if used.saturating_add(previous) > available {
                    break;
                }
                used = used.saturating_add(previous);
                start -= 1;
            }
            for (index, entry) in entries.into_iter().enumerate().skip(start) {
                let row_y = area.y.saturating_add(lines.len() as u16);
                let remaining = usize::from(area.height.saturating_sub(lines.len() as u16 + 3));
                if remaining == 0 {
                    break;
                }
                let height = entry.len().min(remaining) as u16;
                self.hits.push((
                    Rect::new(area.x, row_y, area.width, height),
                    Action::PermissionItem(index),
                ));
                lines.extend(entry.into_iter().take(remaining));
            }
        }
        let footer = if state.form.is_some() {
            self.hits.push((
                Rect::new(
                    area.x,
                    area.bottom().saturating_sub(1),
                    area.width.min(12),
                    1,
                ),
                Action::PermissionSave,
            ));
            format!(
                "{} Delete · {} refresh · Esc Cancel",
                bindings.primary_hint_in(
                    NAVIGATION_ACTIVATE,
                    crate::shortcuts::contexts::AGENT_PERMISSION_DELETE
                ),
                bindings.primary_hint_in(
                    PERMISSIONS_REFRESH_FORM,
                    crate::shortcuts::contexts::AGENT_PERMISSION_DELETE
                )
            )
        } else if state.searching {
            format!(
                "Type to filter · {} finish · Esc cancel",
                bindings.primary_hint_in(
                    PERMISSIONS_FINISH_SEARCH,
                    crate::shortcuts::contexts::AGENT_PERMISSION_SEARCH
                )
            )
        } else {
            let edits = if state.tab.kinds().is_empty() {
                String::new()
            } else {
                format!(
                    " · {} add · {} edit · {} delete",
                    hint(PERMISSIONS_ADD),
                    hint(PERMISSIONS_EDIT),
                    hint(PERMISSIONS_DELETE)
                )
            };
            format!(
                "{}/{} tabs · {}/{} select · {} apply{} · {} search · {} refresh · Esc close",
                hint(PERMISSIONS_PREVIOUS_TAB),
                hint(PERMISSIONS_NEXT_TAB),
                hint(NAVIGATION_UP),
                hint(NAVIGATION_DOWN),
                hint(NAVIGATION_ACTIVATE),
                edits,
                hint(PERMISSIONS_SEARCH),
                hint(PERMISSIONS_REFRESH)
            )
        };
        frame.render_widget(
            Paragraph::new(lines),
            Rect::new(area.x, area.y, area.width, area.height.saturating_sub(3)),
        );
        frame.render_widget(
            Paragraph::new(footer)
                .wrap(ratatui::widgets::Wrap { trim: true })
                .style(Style::default().fg(DIM)),
            Rect::new(
                area.x,
                area.bottom().saturating_sub(3),
                area.width,
                area.height.min(3),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn render(ui: &mut Ui, width: u16, height: u16) -> Result<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
        terminal.draw(|frame| ui.draw_permissions_overlay(frame))?;
        let buffer = terminal.backend().buffer();
        Ok((0..height)
            .map(|y| {
                (0..width)
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }
    #[test]
    fn permission_kind_selectors_and_save_errors_retain_the_real_form_guards() -> Result<()> {
        for (action, code, want) in [
            (
                PERMISSIONS_PREVIOUS_VALUE,
                KeyCode::Left,
                SettingKind::AutoEnvironment,
            ),
            (
                PERMISSIONS_NEXT_VALUE,
                KeyCode::Right,
                SettingKind::AutoSoftDeny,
            ),
        ] {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                let mut state = field_state(Some(0), false, false);
                state.form.as_mut().context("form")?.kind = SettingKind::AutoAllow;
                ui.modal = Some(Modal::Permissions(Box::new(state)));
                if custom {
                    ui.key(KeyEvent::new(code, KeyModifiers::NONE));
                    assert!(
                        matches!(&ui.modal, Some(Modal::Permissions(s)) if s.form.as_ref().is_some_and(|f| f.kind == SettingKind::AutoAllow))
                    );
                }
                ui.key(KeyEvent::new(
                    if custom { KeyCode::F(12) } else { code },
                    KeyModifiers::NONE,
                ));
                assert!(
                    matches!(&ui.modal, Some(Modal::Permissions(s)) if s.form.as_ref().is_some_and(|f| f.kind == want))
                );
            }
        }
        let (mut ui, _dir, _) = shortcut_tests::fixture(Some(PERMISSIONS_SUBMIT))?;
        let mut state = field_state(Some(2), false, false);
        state.form.as_mut().context("form")?.value.clear();
        ui.modal = Some(Modal::Permissions(Box::new(state)));
        ui.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
        assert!(
            matches!(&ui.modal, Some(Modal::Permissions(s)) if !s.loading && s.error.as_deref().is_some_and(|e| e.contains("before saving")))
        );
        if let Some(Modal::Permissions(state)) = &mut ui.modal {
            state
                .form
                .as_mut()
                .context("form")?
                .value
                .insert("kept draft");
        }
        ui.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
        assert!(
            matches!(&ui.modal, Some(Modal::Permissions(s)) if !s.loading && s.error.as_deref().is_some_and(|e| e.contains("source is unavailable")) && s.editor().is_some_and(|e| e.text() == "kept draft"))
        );
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            matches!(&ui.modal, Some(Modal::Permissions(s)) if s.form.is_none() && s.error.is_none())
        );
        Ok(())
    }

    #[test]
    fn unregistered_modified_navigation_stays_in_the_native_editor() -> Result<()> {
        let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
        let mut state = list_state();
        state.selected = 5;
        ui.modal = Some(Modal::Permissions(Box::new(state)));
        ui.key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL));
        assert!(matches!(&ui.modal, Some(Modal::Permissions(s)) if s.selected == 5));
        ui.modal = Some(Modal::Permissions(Box::new(field_state(
            Some(2),
            false,
            false,
        ))));
        ui.key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL));
        ui.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert!(
            matches!(&ui.modal, Some(Modal::Permissions(s)) if s.editor().is_some_and(|e| e.text() == "zvalue"))
        );
        Ok(())
    }

    #[test]
    fn permission_enter_and_shift_enter_keep_single_line_accept_and_multiline_editing() -> Result<()>
    {
        for (kind, code, modifiers, want_field, want_text) in [
            (
                SettingKind::Allow,
                KeyCode::Enter,
                KeyModifiers::SHIFT,
                3,
                "value",
            ),
            (
                SettingKind::AutoEnvironment,
                KeyCode::Enter,
                KeyModifiers::SHIFT,
                2,
                "value\n",
            ),
            (
                SettingKind::AutoEnvironment,
                KeyCode::Enter,
                KeyModifiers::NONE,
                3,
                "value",
            ),
        ] {
            let (mut ui, _dir, _) = shortcut_tests::fixture(None)?;
            let mut state = field_state(Some(2), false, false);
            state.form.as_mut().context("form")?.kind = kind;
            ui.modal = Some(Modal::Permissions(Box::new(state)));
            ui.key(KeyEvent::new(code, modifiers));
            let Some(Modal::Permissions(state)) = &ui.modal else {
                anyhow::bail!("panel")
            };
            let form = state.form.as_ref().context("form")?;
            assert_eq!(form.field, want_field);
            assert_eq!(form.value.text(), want_text);
        }
        Ok(())
    }

    fn field_state(field: Option<usize>, searching: bool, delete: bool) -> State {
        let mut state = list_state();
        state.searching = searching;
        if let Some(field) = field {
            state.begin_edit(true);
            if let Some(form) = &mut state.form {
                form.field = if field == 4 { 2 } else { field };
                if field == 4 {
                    form.kind = SettingKind::AutoEnvironment;
                }
                form.value = Editor::from("value");
                form.delete = delete;
            }
        }
        state
    }

    #[test]
    fn permission_contexts_keep_early_main_actions_and_reject_stored_typing_overrides() -> Result<()>
    {
        use crate::shortcuts::{contexts::*, ids::*};
        for (field, searching, delete, context) in [
            (None, false, false, AGENT_PERMISSIONS),
            (None, true, false, AGENT_PERMISSION_SEARCH),
            (Some(0), false, false, AGENT_PERMISSION_CHOICE),
            (Some(1), false, false, AGENT_PERMISSION_CHOICE),
            (Some(2), false, false, AGENT_PERMISSION_FORM),
            (Some(4), false, false, AGENT_PERMISSION_MULTILINE),
            (Some(3), false, false, AGENT_PERMISSION_BUTTON),
            (Some(2), false, true, AGENT_PERMISSION_DELETE),
        ] {
            for (action, code) in [
                (AGENTS_SIDEBAR, KeyCode::Char('[')),
                (AGENTS_CHANGES, KeyCode::Char('d')),
                (AGENTS_MODELS, KeyCode::Char('m')),
                (QUESTION_OPEN, KeyCode::Up),
            ] {
                for custom in [false, true] {
                    let (mut ui, _dir, _) = shortcut_tests::fixture(custom.then_some(action))?;
                    ui.model_completion.options.push(crate::model::ModelInfo {
                        id: "fixture-model".into(),
                        name: "Fixture model".into(),
                        efforts: vec!["medium".into()],
                    });
                    ui.modal = Some(Modal::Permissions(Box::new(field_state(
                        field, searching, delete,
                    ))));
                    assert_eq!(ui.shortcut_context(), context);
                    if custom {
                        ui.key(KeyEvent::new(code, KeyModifiers::ALT));
                        assert_eq!(ui.shortcut_context(), context);
                        assert!(ui.list_visible && !ui.changes_visible);
                    }
                    ui.key(KeyEvent::new(
                        if custom { KeyCode::F(12) } else { code },
                        if custom {
                            KeyModifiers::NONE
                        } else {
                            KeyModifiers::ALT
                        },
                    ));
                    match action {
                        AGENTS_SIDEBAR => assert!(!ui.list_visible),
                        AGENTS_CHANGES => assert!(ui.changes_visible),
                        AGENTS_MODELS => assert!(matches!(ui.modal, Some(Modal::ModelPicker(_)))),
                        QUESTION_OPEN => assert!(matches!(ui.modal, Some(Modal::Pending { .. }))),
                        _ => anyhow::bail!("action"),
                    }
                }
            }
            let (mut ui, _dir, mut config) = shortcut_tests::fixture(None)?;
            config.preferences.keyboard.bindings.insert(
                SETTINGS_OPEN.0.into(),
                crate::shortcuts::key(crate::settings::BindingKey::Character('z'), 0),
            );
            ui.apply_settings(&config);
            ui.modal = Some(Modal::Permissions(Box::new(field_state(
                field, searching, delete,
            ))));
            let mut shell = crate::shell::Shell::new(ui.storage.clone(), config, None);
            shell.agents = ui;
            shell.agents_active = true;
            shell.key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
            let editing = searching || (matches!(field, Some(2 | 4)) && !delete);
            assert_eq!(shell.settings.is_none(), editing, "{context:?}");
            if editing {
                let Some(Modal::Permissions(state)) = &shell.agents.modal else {
                    anyhow::bail!("typing opened action")
                };
                assert!(state.editor().context("editor")?.text().ends_with('z'));
            }
        }
        Ok(())
    }

    #[test]
    fn permission_copy_uses_only_the_focused_field_and_preserves_selection() -> Result<()> {
        for (field, searching, delete) in [
            (None, false, false),
            (None, true, false),
            (Some(0), false, false),
            (Some(1), false, false),
            (Some(2), false, false),
            (Some(4), false, false),
            (Some(3), false, false),
            (Some(2), false, true),
        ] {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, _dir, _) = shortcut_tests::binding_fixture(SELECTION_COPY, phase)?;
                let mut state = field_state(field, searching, delete);
                state.query = Editor::from("query");
                if let Some(editor) = if searching {
                    Some(&mut state.query)
                } else {
                    state
                        .form
                        .as_mut()
                        .filter(|f| f.field == 2 && !f.delete)
                        .map(|f| &mut f.value)
                } {
                    editor.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
                }
                ui.positions
                    .get_mut("one")
                    .context("draft")?
                    .draft
                    .key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
                ui.modal = Some(Modal::Permissions(Box::new(state)));
                if custom {
                    ui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER));
                    assert!(ui.clipboard.is_none());
                }
                ui.key(KeyEvent::new(
                    if custom {
                        KeyCode::F(12)
                    } else {
                        KeyCode::Char('c')
                    },
                    if custom {
                        KeyModifiers::NONE
                    } else {
                        KeyModifiers::SUPER
                    },
                ));
                let want = if searching {
                    Some("y")
                } else if matches!(field, Some(2 | 4)) && !delete {
                    Some("e")
                } else {
                    None
                };
                assert_eq!(ui.clipboard.as_deref(), want);
                ui.clipboard = None;
                ui.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
                assert_eq!(ui.clipboard.as_deref(), want);
                assert_eq!(
                    ui.positions.get("one").context("draft")?.draft.text(),
                    "unsent draft"
                );
            }
        }
        Ok(())
    }

    fn list_state() -> State {
        let mut state = State::new("one".into(), provider::Provider::Claude);
        state.tab = Tab::Allow;
        state.data.rows = (0..20)
            .map(|i| permissions::SettingRow {
                kind: SettingKind::Allow,
                scope: Some(Scope::Project),
                value: format!("Read({i})"),
                editable: true,
                source: "fixture".into(),
                revision: "r1".into(),
            })
            .collect();
        state
    }

    #[test]
    fn permission_shared_list_navigation_replaces_literal_arrows_and_aliases() -> Result<()> {
        for (action, aliases, start, want) in [
            (NAVIGATION_UP, vec![KeyCode::Up, KeyCode::Char('k')], 5, 4),
            (
                NAVIGATION_DOWN,
                vec![KeyCode::Down, KeyCode::Char('j')],
                5,
                6,
            ),
            (NAVIGATION_PAGE_UP, vec![KeyCode::PageUp], 8, 3),
            (NAVIGATION_PAGE_DOWN, vec![KeyCode::PageDown], 5, 10),
            (NAVIGATION_FIRST, vec![KeyCode::Home], 5, 0),
            (NAVIGATION_LAST, vec![KeyCode::End], 5, 19),
        ] {
            for alias in aliases {
                for (phase, custom) in shortcut_tests::BINDING_PHASES {
                    let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                    let mut state = list_state();
                    state.selected = start;
                    ui.modal = Some(Modal::Permissions(Box::new(state)));
                    if custom {
                        ui.key(KeyEvent::new(alias, KeyModifiers::NONE));
                        assert!(
                            matches!(&ui.modal, Some(Modal::Permissions(s)) if s.selected == start)
                        );
                    }
                    ui.key(KeyEvent::new(
                        if custom { KeyCode::F(12) } else { alias },
                        KeyModifiers::NONE,
                    ));
                    assert!(matches!(&ui.modal, Some(Modal::Permissions(s)) if s.selected == want));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn permission_mouse_wheels_and_clicks_survive_keyboard_rebinding() -> Result<()> {
        let (mut ui, _dir, mut config) = shortcut_tests::fixture(Some(NAVIGATION_DOWN))?;
        for id in [NAVIGATION_UP, PERMISSIONS_SEARCH, PERMISSIONS_SUBMIT] {
            config.preferences.keyboard.bindings.insert(
                id.0.into(),
                crate::shortcuts::key(crate::settings::BindingKey::Function(11), 0),
            );
        }
        ui.apply_settings(&config);
        ui.modal = Some(Modal::Permissions(Box::new(list_state())));
        let wheel = |kind| MouseEvent {
            kind,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        ui.mouse(wheel(MouseEventKind::ScrollDown));
        assert!(matches!(&ui.modal, Some(Modal::Permissions(s)) if s.selected == 3));
        ui.mouse(wheel(MouseEventKind::ScrollUp));
        assert!(matches!(&ui.modal, Some(Modal::Permissions(s)) if s.selected == 0));
        ui.hits = vec![(Rect::new(1, 1, 2, 1), Action::PermissionSearch)];
        ui.mouse(wheel(MouseEventKind::Down(MouseButton::Left)));
        assert!(matches!(&ui.modal, Some(Modal::Permissions(s)) if s.searching));
        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        ui.hits = vec![(Rect::new(1, 1, 2, 1), Action::PermissionItem(2))];
        ui.mouse(wheel(MouseEventKind::Down(MouseButton::Left)));
        assert!(
            matches!(&ui.modal, Some(Modal::Permissions(s)) if s.form.as_ref().is_some_and(|f| f.before.as_deref() == Some("Read(2)")))
        );
        if let Some(Modal::Permissions(state)) = &mut ui.modal {
            state.form.as_mut().context("form")?.value.clear();
        }
        ui.hits = vec![(Rect::new(1, 1, 2, 1), Action::PermissionSave)];
        ui.mouse(wheel(MouseEventKind::Down(MouseButton::Left)));
        assert!(
            matches!(&ui.modal, Some(Modal::Permissions(s)) if s.error.as_deref().is_some_and(|e| e.contains("before saving")))
        );
        Ok(())
    }

    #[test]
    fn permission_save_refresh_and_delete_reach_the_exact_ipc_boundary() -> Result<()> {
        for (action, code, modifiers, mode) in [
            (
                PERMISSIONS_REFRESH,
                KeyCode::Char('r'),
                KeyModifiers::NONE,
                "list",
            ),
            (
                PERMISSIONS_REFRESH_FORM,
                KeyCode::Char('r'),
                KeyModifiers::CONTROL,
                "form",
            ),
            (
                PERMISSIONS_REFRESH_FORM,
                KeyCode::Char('r'),
                KeyModifiers::CONTROL,
                "delete",
            ),
            (
                PERMISSIONS_SUBMIT,
                KeyCode::Enter,
                KeyModifiers::CONTROL,
                "form",
            ),
            (
                PERMISSIONS_ACTIVATE,
                KeyCode::Enter,
                KeyModifiers::NONE,
                "button",
            ),
            (
                NAVIGATION_ACTIVATE,
                KeyCode::Enter,
                KeyModifiers::NONE,
                "delete",
            ),
            (
                NAVIGATION_ACTIVATE,
                KeyCode::Enter,
                KeyModifiers::NONE,
                "mode",
            ),
            (
                PERMISSIONS_REFRESH_FORM,
                KeyCode::Char('r'),
                KeyModifiers::CONTROL,
                "choice-kind",
            ),
            (
                PERMISSIONS_REFRESH_FORM,
                KeyCode::Char('r'),
                KeyModifiers::CONTROL,
                "choice-scope",
            ),
            (
                PERMISSIONS_REFRESH_FORM,
                KeyCode::Char('r'),
                KeyModifiers::CONTROL,
                "button",
            ),
            (
                PERMISSIONS_REFRESH_FORM,
                KeyCode::Char('r'),
                KeyModifiers::CONTROL,
                "multiline",
            ),
            (
                PERMISSIONS_SUBMIT,
                KeyCode::Enter,
                KeyModifiers::CONTROL,
                "choice-kind",
            ),
            (
                PERMISSIONS_SUBMIT,
                KeyCode::Enter,
                KeyModifiers::CONTROL,
                "choice-scope",
            ),
            (
                PERMISSIONS_SUBMIT,
                KeyCode::Enter,
                KeyModifiers::CONTROL,
                "button",
            ),
            (
                PERMISSIONS_SUBMIT,
                KeyCode::Enter,
                KeyModifiers::CONTROL,
                "multiline",
            ),
        ] {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                let mut state = State::new("one".into(), provider::Provider::Claude);
                state.tab = Tab::Allow;
                state
                    .data
                    .revisions
                    .insert(Scope::Session, "revision-42".into());
                if mode == "mode" {
                    state.tab = Tab::Mode;
                    state.data.modes.push(permissions::Choice {
                        id: "plan".into(),
                        label: "Plan".into(),
                        description: "fixture".into(),
                        available: true,
                        reason: None,
                    });
                } else if mode != "list" {
                    state.begin_edit(true);
                    let form = state.form.as_mut().context("form")?;
                    form.value = Editor::from("Read(fixture)");
                    form.delete = mode == "delete";
                    if form.delete {
                        form.before = Some("Read(fixture)".into());
                    }
                    if mode == "button" {
                        form.field = 3;
                    }
                    if mode == "choice-kind" {
                        form.field = 0;
                    }
                    if mode == "choice-scope" {
                        form.field = 1;
                    }
                    if mode == "multiline" {
                        form.kind = SettingKind::AutoEnvironment;
                        form.scope = Scope::User;
                        state
                            .data
                            .revisions
                            .insert(Scope::User, "revision-42".into());
                    }
                }
                ui.modal = Some(Modal::Permissions(Box::new(state)));
                if custom {
                    ui.key(KeyEvent::new(code, modifiers));
                    let Some(Modal::Permissions(state)) = &mut ui.modal else {
                        anyhow::bail!("panel")
                    };
                    assert!(!state.loading, "removed {action:?}");
                    if let Some(form) = &mut state.form {
                        form.value = Editor::from("Read(fixture)");
                    }
                }
                let request = shortcut_tests::request_at_boundary(
                    &mut ui,
                    KeyEvent::new(
                        if custom { KeyCode::F(12) } else { code },
                        if custom {
                            KeyModifiers::NONE
                        } else {
                            modifiers
                        },
                    ),
                )
                .with_context(|| format!("{action:?} {mode} {phase}"))?;
                let Request::Permissions {
                    id,
                    action: requested,
                } = request
                else {
                    anyhow::bail!("wrong request")
                };
                assert_eq!(id, "one");
                if [PERMISSIONS_REFRESH, PERMISSIONS_REFRESH_FORM].contains(&action) {
                    assert!(matches!(requested, permissions::Action::Refresh));
                } else if mode == "mode" {
                    assert!(
                        matches!(requested, permissions::Action::SetMode { mode } if mode == "plan")
                    );
                } else {
                    let permissions::Action::Edit { edit } = requested else {
                        anyhow::bail!("edit")
                    };
                    assert_eq!(
                        edit.kind,
                        if mode == "multiline" {
                            SettingKind::AutoEnvironment
                        } else {
                            SettingKind::Allow
                        }
                    );
                    assert_eq!(
                        edit.scope,
                        if mode == "multiline" {
                            Scope::User
                        } else {
                            Scope::Session
                        }
                    );
                    assert_eq!(edit.revision, "revision-42");
                    assert_eq!(
                        edit.before.as_deref(),
                        (mode == "delete").then_some("Read(fixture)")
                    );
                    assert_eq!(
                        edit.after.as_deref(),
                        (mode != "delete").then_some("Read(fixture)")
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn permission_native_hints_follow_custom_bindings_in_each_field() -> Result<()> {
        for (action, field, searching) in [
            (PERMISSIONS_SEARCH, None, false),
            (PERMISSIONS_REFRESH, None, false),
            (PERMISSIONS_PREVIOUS_TAB, None, false),
            (PERMISSIONS_NEXT_TAB, None, false),
            (PERMISSIONS_ADD, None, false),
            (PERMISSIONS_EDIT, None, false),
            (PERMISSIONS_DELETE, None, false),
            (NAVIGATION_UP, None, false),
            (NAVIGATION_DOWN, None, false),
            (PERMISSIONS_PREVIOUS_VALUE, Some(1), false),
            (PERMISSIONS_NEXT_VALUE, Some(1), false),
            (PERMISSIONS_SUBMIT, Some(2), false),
            (NAVIGATION_NEXT_FOCUS, Some(2), false),
            (NAVIGATION_PREVIOUS_FOCUS, Some(2), false),
            (PERMISSIONS_REFRESH_FORM, Some(2), false),
            (PERMISSIONS_ACTIVATE, Some(2), false),
            (PERMISSIONS_FINISH_SEARCH, None, true),
        ] {
            let (mut ui, _dir, _) = shortcut_tests::fixture(Some(action))?;
            let mut state = State::new("one".into(), provider::Provider::Claude);
            state.tab = Tab::Allow;
            if let Some(field) = field {
                state.begin_edit(true);
                state.form.as_mut().context("form")?.field = field;
            }
            state.searching = searching;
            ui.modal = Some(Modal::Permissions(Box::new(state)));
            for width in [80, 160] {
                let text = render(&mut ui, width, 40)?;
                assert!(text.contains("F12"), "{action:?} width={width}: {text}");
            }
        }
        Ok(())
    }

    // Removing semantic dispatch makes F12 inert; retaining a raw alias mutates
    // the state during the explicit old-key assertion below.
    #[test]
    fn permission_list_actions_replace_all_legacy_keys_at_real_dispatch() -> Result<()> {
        use crate::shortcuts::ids::*;
        for (action, aliases, want) in [
            (PERMISSIONS_SEARCH, vec![KeyCode::Char('/')], "search"),
            (
                PERMISSIONS_PREVIOUS_TAB,
                vec![KeyCode::BackTab, KeyCode::Left],
                "Mode",
            ),
            (
                PERMISSIONS_NEXT_TAB,
                vec![KeyCode::Tab, KeyCode::Right],
                "Allow",
            ),
            (PERMISSIONS_ADD, vec![KeyCode::Char('a')], "new"),
            (PERMISSIONS_EDIT, vec![KeyCode::Char('e')], "edit"),
            (
                PERMISSIONS_DELETE,
                vec![KeyCode::Char('d'), KeyCode::Delete],
                "delete",
            ),
        ] {
            for alias in aliases {
                for (phase, custom) in shortcut_tests::BINDING_PHASES {
                    let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                    let mut state = State::new("one".into(), provider::Provider::Claude);
                    state.tab = if want == "Mode" || want == "Allow" {
                        Tab::Denied
                    } else {
                        Tab::Allow
                    };
                    state.data.rows.push(permissions::SettingRow {
                        kind: SettingKind::Allow,
                        scope: Some(Scope::Project),
                        value: "Read(*)".into(),
                        editable: true,
                        source: "fixture".into(),
                        revision: "r1".into(),
                    });
                    ui.modal = Some(Modal::Permissions(Box::new(state)));
                    let unchanged = |ui: &Ui| -> Result<()> {
                        let Some(Modal::Permissions(state)) = &ui.modal else {
                            anyhow::bail!("permissions closed")
                        };
                        assert!(!state.searching && state.form.is_none());
                        assert_eq!(
                            state.tab,
                            if want == "Mode" || want == "Allow" {
                                Tab::Denied
                            } else {
                                Tab::Allow
                            }
                        );
                        Ok(())
                    };
                    if custom {
                        ui.key(KeyEvent::new(alias, KeyModifiers::NONE));
                        unchanged(&ui)?;
                    }
                    ui.key(KeyEvent::new(
                        if custom { KeyCode::F(12) } else { alias },
                        KeyModifiers::NONE,
                    ));
                    let Some(Modal::Permissions(state)) = &ui.modal else {
                        anyhow::bail!("permissions")
                    };
                    match want {
                        "search" => assert!(state.searching),
                        "Mode" => assert_eq!(state.tab, Tab::Mode),
                        "Allow" => assert_eq!(state.tab, Tab::Allow),
                        "new" => assert!(
                            state
                                .form
                                .as_ref()
                                .is_some_and(|f| f.before.is_none() && !f.delete)
                        ),
                        "edit" => {
                            assert!(state.form.as_ref().is_some_and(|f| f.before.as_deref()
                                == Some("Read(*)")
                                && !f.delete))
                        }
                        "delete" => {
                            assert!(
                                state.form.as_ref().is_some_and(
                                    |f| f.delete && f.before.as_deref() == Some("Read(*)")
                                )
                            )
                        }
                        _ => anyhow::bail!("expectation"),
                    }
                    ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                    assert_eq!(
                        ui.positions.get("one").context("draft")?.draft.text(),
                        "unsent draft"
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn permission_form_navigation_uses_actual_field_context_and_preserves_editing() -> Result<()> {
        use crate::shortcuts::{contexts::*, ids::*};
        for (action, code, modifier, start, want) in [
            (
                NAVIGATION_NEXT_FOCUS,
                KeyCode::Tab,
                KeyModifiers::NONE,
                1,
                2,
            ),
            (
                NAVIGATION_PREVIOUS_FOCUS,
                KeyCode::BackTab,
                KeyModifiers::NONE,
                1,
                0,
            ),
            (
                PERMISSIONS_PREVIOUS_VALUE,
                KeyCode::Left,
                KeyModifiers::NONE,
                1,
                1,
            ),
            (
                PERMISSIONS_NEXT_VALUE,
                KeyCode::Right,
                KeyModifiers::NONE,
                1,
                1,
            ),
            (
                PERMISSIONS_ACTIVATE,
                KeyCode::Enter,
                KeyModifiers::NONE,
                2,
                3,
            ),
            (
                PERMISSIONS_ACTIVATE,
                KeyCode::Enter,
                KeyModifiers::SHIFT,
                2,
                3,
            ),
            (
                NAVIGATION_NEXT_FOCUS,
                KeyCode::Tab,
                KeyModifiers::NONE,
                0,
                1,
            ),
            (
                NAVIGATION_NEXT_FOCUS,
                KeyCode::Tab,
                KeyModifiers::NONE,
                2,
                3,
            ),
            (
                NAVIGATION_NEXT_FOCUS,
                KeyCode::Tab,
                KeyModifiers::NONE,
                3,
                0,
            ),
            (
                NAVIGATION_NEXT_FOCUS,
                KeyCode::Tab,
                KeyModifiers::NONE,
                4,
                3,
            ),
            (
                NAVIGATION_PREVIOUS_FOCUS,
                KeyCode::BackTab,
                KeyModifiers::NONE,
                0,
                3,
            ),
            (
                NAVIGATION_PREVIOUS_FOCUS,
                KeyCode::BackTab,
                KeyModifiers::NONE,
                2,
                1,
            ),
            (
                NAVIGATION_PREVIOUS_FOCUS,
                KeyCode::BackTab,
                KeyModifiers::NONE,
                3,
                2,
            ),
            (
                NAVIGATION_PREVIOUS_FOCUS,
                KeyCode::BackTab,
                KeyModifiers::NONE,
                4,
                1,
            ),
            (
                PERMISSIONS_ACTIVATE,
                KeyCode::Enter,
                KeyModifiers::NONE,
                4,
                3,
            ),
        ] {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                let mut state = State::new("one".into(), provider::Provider::Claude);
                state.tab = Tab::Allow;
                state.begin_edit(true);
                let form = state.form.as_mut().context("form")?;
                let start_field = if start == 4 { 2 } else { start };
                form.field = start_field;
                if start == 4 {
                    form.kind = SettingKind::AutoEnvironment;
                }
                form.value = Editor::from("value");
                ui.modal = Some(Modal::Permissions(Box::new(state)));
                if custom {
                    ui.key(KeyEvent::new(code, modifier));
                    let Some(Modal::Permissions(state)) = &ui.modal else {
                        anyhow::bail!("permissions")
                    };
                    assert_eq!(state.form.as_ref().context("form")?.field, start_field);
                    assert_eq!(state.form.as_ref().context("form")?.scope, Scope::Session);
                }
                ui.key(KeyEvent::new(
                    if custom { KeyCode::F(12) } else { code },
                    if custom { KeyModifiers::NONE } else { modifier },
                ));
                let Some(Modal::Permissions(state)) = &mut ui.modal else {
                    anyhow::bail!("permissions")
                };
                let form = state.form.as_mut().context("form")?;
                assert_eq!(form.field, want);
                if action == PERMISSIONS_PREVIOUS_VALUE {
                    assert_eq!(form.scope, Scope::User);
                }
                if action == PERMISSIONS_NEXT_VALUE {
                    assert_eq!(form.scope, Scope::Project);
                }
                form.field = 2;
                form.value = Editor::from("value");
                assert_eq!(
                    ui.shortcut_context(),
                    if start == 4 {
                        AGENT_PERMISSION_MULTILINE
                    } else {
                        AGENT_PERMISSION_FORM
                    }
                );
                ui.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
                let Some(Modal::Permissions(state)) = &ui.modal else {
                    anyhow::bail!("permissions")
                };
                assert_eq!(
                    state.editor().context("editor")?.selected_text().as_deref(),
                    Some("e")
                );
            }
        }
        Ok(())
    }

    #[test]
    fn permission_search_finish_replaces_enter_and_keeps_raw_typing() -> Result<()> {
        use crate::shortcuts::ids::PERMISSIONS_FINISH_SEARCH;
        for (phase, custom) in shortcut_tests::BINDING_PHASES {
            let (mut ui, _dir, _) =
                shortcut_tests::binding_fixture(PERMISSIONS_FINISH_SEARCH, phase)?;
            let mut state = State::new("one".into(), provider::Provider::Claude);
            state.searching = true;
            ui.modal = Some(Modal::Permissions(Box::new(state)));
            ui.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
            if custom {
                ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                assert!(matches!(&ui.modal, Some(Modal::Permissions(state)) if state.searching));
            }
            ui.key(KeyEvent::new(
                if custom {
                    KeyCode::F(12)
                } else {
                    KeyCode::Enter
                },
                KeyModifiers::NONE,
            ));
            assert!(
                matches!(&ui.modal, Some(Modal::Permissions(state)) if !state.searching && state.query.text().starts_with('x'))
            );
        }
        Ok(())
    }

    #[test]
    fn provider_panel_renders_current_pending_errors_tabs_and_multiline_forms() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = Ui::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().join("cache"),
            },
            &Config::default(),
        );
        let mut state = State::new("one".into(), provider::Provider::Codex);
        state.data.active_mode = Some("default".into());
        state.data.pending_mode = Some("plan".into());
        state.data.modes = vec![
            permissions::Choice {
                id: "default".into(),
                label: "Default".into(),
                description: "Native coding mode".into(),
                available: true,
                reason: None,
            },
            permissions::Choice {
                id: "plan".into(),
                label: "Plan".into(),
                description: "Native planning mode".into(),
                available: true,
                reason: None,
            },
        ];
        state.data.presets = [
            CodexPreset::Ask,
            CodexPreset::Auto,
            CodexPreset::Full,
            CodexPreset::ReadOnly,
        ]
        .into_iter()
        .map(|p| permissions::Choice {
            id: format!("{p:?}"),
            label: p.label().into(),
            description: "Native policy".into(),
            available: p != CodexPreset::Full,
            reason: (p == CodexPreset::Full).then(|| "Restricted by managed policy".into()),
        })
        .collect();
        ui.modal = Some(Modal::Permissions(Box::new(state)));
        let codex = render(&mut ui, 104, 34)?;
        assert!(codex.contains("Codex permissions") && codex.contains("Custom"));
        assert!(codex.contains("Regular · current") && codex.contains("Plan · next turn"));
        for label in [
            "Ask for approval",
            "Approve for me",
            "Full Access · unavailable",
            "Read Only",
        ] {
            assert!(codex.contains(label), "{label}: {codex}");
        }
        let Some(Modal::Permissions(state)) = &mut ui.modal else {
            anyhow::bail!("panel");
        };
        state.selected = 4;
        assert!(render(&mut ui, 104, 34)?.contains("Restricted by managed policy"));
        let Some(Modal::Permissions(state)) = &mut ui.modal else {
            anyhow::bail!("panel");
        };
        state.data.provider = provider::Provider::Claude;
        state.tab = Tab::Denied;
        let denied = render(&mut ui, 104, 24)?;
        assert!(
            denied.contains("Recently denied")
                && denied.contains("Auto mode")
                && denied.contains("Workspace")
        );
        assert!(denied.contains("No denied tools observed in this Difu session"));
        let Some(Modal::Permissions(state)) = &mut ui.modal else {
            anyhow::bail!("panel");
        };
        state.tab = Tab::Auto;
        state.begin_edit(true);
        let form = state.form.as_mut().context("auto form")?;
        form.kind = SettingKind::AutoEnvironment;
        form.value
            .insert("Private staging network\nNever use production");
        let form = render(&mut ui, 104, 28)?;
        assert!(
            form.contains("Scope: User")
                && form.contains("Private staging network")
                && form.contains("Never use production")
                && form.contains("Shift+Enter newline")
        );
        for (w, h) in [(40, 12), (20, 6), (1, 1)] {
            render(&mut ui, w, h)?;
        }
        eprintln!("Codex permissions:\n{codex}\nClaude permissions:\n{denied}\nAuto form:\n{form}");
        Ok(())
    }
    #[test]
    fn turn_errors_and_interruptions_do_not_imply_a_connection_failure() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = Ui::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().join("cache"),
            },
            &Config::default(),
        );
        let mut session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: dir.path().into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: "Test permissions".into(),
                model: None,
                effort: None,
            }),
        );
        session.provider = provider::Provider::Claude;
        session.permission_state.mode = Some("default".into());
        session.permission_state.pending_mode = Some("bypassPermissions".into());
        session.permission_state.reload_pending = true;
        session.status = Status::Failed;
        session.error = Some("Provider turn failed".into());
        let failed = composer_status(&session, "Shift+Tab").to_string();
        assert!(failed.starts_with("Manual · Turn failed"));
        assert!(!failed.contains("reconnect") && !failed.contains("switching"));
        session.status = Status::Interrupted;
        let interrupted = composer_status(&session, "Shift+Tab");
        assert!(interrupted.to_string().starts_with("Manual · Interrupted"));
        assert!(
            !interrupted.to_string().contains("reconnect")
                && !interrupted.to_string().contains("switching")
        );
        assert!(
            interrupted
                .spans
                .iter()
                .any(|span| span.content.contains("Interrupted") && span.style.fg == Some(DIM))
        );
        session.status = Status::Waiting;
        session.error = None;
        assert!(
            composer_status(&session, "Shift+Tab")
                .to_string()
                .contains("switching to Bypass…")
        );
        session.status = Status::Failed;
        session.error = Some("channel is empty and sending half is closed".into());
        assert!(
            composer_status(&session, "Shift+Tab")
                .to_string()
                .contains("reconnect needed")
        );
        let diagnostic =
            r#"["[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use"]"#;
        session.error = Some(diagnostic.into());
        ui.sessions.insert("one".into(), session);
        let mut state = State::new("one".into(), provider::Provider::Claude);
        state.data.active_mode = Some("default".into());
        state.data.pending_mode = Some("bypassPermissions".into());
        state.data.reload_pending = true;
        ui.modal = Some(Modal::Permissions(Box::new(state)));
        let panel = render(&mut ui, 104, 24)?;
        assert!(panel.contains("Claude ended this turn without a final response."));
        assert!(!panel.contains("ede_diagnostic"));
        assert!(!panel.contains("reconnect") && !panel.contains("Switching"));
        assert_eq!(
            ui.sessions.get("one").context("session")?.error.as_deref(),
            Some(diagnostic)
        );
        ui.sessions.get_mut("one").context("session")?.status = Status::Interrupted;
        let interrupted_panel = render(&mut ui, 104, 24)?;
        assert!(interrupted_panel.contains("Interrupted"));
        assert!(
            !interrupted_panel.contains("reconnect") && !interrupted_panel.contains("Switching")
        );
        Ok(())
    }
    #[test]
    fn mode_panel_is_compact_human_readable_and_failed_switches_offer_recovery() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = Ui::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().join("cache"),
            },
            &Config::default(),
        );
        let mut session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: dir.path().into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: "Test permissions".into(),
                model: None,
                effort: None,
            }),
        );
        session.provider = provider::Provider::Claude;
        session.status = Status::Failed;
        session.error = Some("Agent did not return permissions; refresh its connection: channel is empty and sending half is closed".into());
        session.permission_state.mode = Some("auto".into());
        session.permission_state.pending_mode = Some("bypassPermissions".into());
        session.permission_state.reload_pending = true;
        let composer = composer_status(&session, "Shift+Tab").to_string();
        assert!(composer.contains("Auto") && composer.contains("reconnect needed"));
        assert!(!composer.contains("switching") && !composer.contains("pending"));
        ui.sessions.insert("one".into(), session);
        let mut state = State::new("one".into(), provider::Provider::Claude);
        state.data.active_mode = Some("auto".into());
        state.data.pending_mode = Some("bypassPermissions".into());
        state.data.reload_pending = true;
        state.data.modes = [
            "default",
            "acceptEdits",
            "plan",
            "bypassPermissions",
            "auto",
            "dontAsk",
        ]
        .into_iter()
        .map(|id| permissions::Choice {
            id: id.into(),
            label: id.into(),
            description:
                "Verbose native provider explanation that should only appear for the selected row"
                    .into(),
            available: true,
            reason: None,
        })
        .collect();
        state.selected = 3;
        ui.modal = Some(Modal::Permissions(Box::new(state)));
        let failed = render(&mut ui, 160, 50)?;
        assert!(failed.contains("Agent connection closed. Press r to reconnect."));
        assert!(failed.contains("> Bypass") && failed.contains("Auto · current"));
        assert!(!failed.contains("Mode:") && !failed.contains("bypassPermissions"));
        assert!(!failed.contains("Switching") && !failed.contains("waiting for idle"));
        let border_rows = failed
            .lines()
            .filter(|line| line.contains('│') || line.contains('┌') || line.contains('└'))
            .collect::<Vec<_>>();
        assert!(border_rows.len() <= 24);
        for line in &border_rows {
            assert!(line.chars().filter(|c| !c.is_whitespace()).count() <= 90);
        }
        ui.sessions.get_mut("one").context("session")?.status = Status::Idle;
        let switching = render(&mut ui, 104, 28)?;
        assert!(switching.contains("Switching to Bypass…"));
        assert!(switching.contains("> Bypass · switching"));
        assert!(!switching.contains("acceptEdits") && !switching.contains("dontAsk"));
        let Some(Modal::Permissions(state)) = &mut ui.modal else {
            anyhow::bail!("panel");
        };
        state.data.reload_pending = false;
        let queued = render(&mut ui, 104, 28)?;
        assert!(!queued.contains("Switching"));
        assert!(queued.contains("Bypass · next turn"));
        eprintln!("Compact failed permissions:\n{failed}\nSwitching permissions:\n{switching}");
        Ok(())
    }
    #[test]
    fn wrapped_rule_sources_keep_selected_row_visible_and_read_only_edit_explains_source()
    -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = Ui::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().join("cache"),
            },
            &Config::default(),
        );
        let mut state = State::new("one".into(), provider::Provider::Claude);
        state.tab = Tab::Allow;
        state.data.rows = (0..10).map(|i| permissions::SettingRow { kind: SettingKind::Allow, scope: None, value: format!("Tool{i}"), editable: false, source: "/a/very/long/managed/settings/source/that/wraps/over/several/lines/in/a/narrow/terminal/settings.json".into(), revision: "r1".into() }).collect();
        state.selected = 9;
        ui.modal = Some(Modal::Permissions(Box::new(state)));
        assert!(render(&mut ui, 50, 18)?.contains("Tool9"));
        ui.permissions_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE));
        let Some(Modal::Permissions(state)) = &ui.modal else {
            anyhow::bail!("panel");
        };
        assert!(state.form.is_none());
        assert!(
            state
                .error
                .as_deref()
                .is_some_and(|e| e.contains("read-only") && e.contains("settings.json"))
        );
        Ok(())
    }

    #[test]
    fn tabs_search_and_read_only_rows_keep_source_identity() -> Result<()> {
        let mut state = State::new("one".into(), provider::Provider::Claude);
        state.data.rows = vec![
            permissions::SettingRow {
                kind: SettingKind::Allow,
                scope: Some(Scope::Project),
                value: "Bash(cargo test:*)".into(),
                editable: true,
                source: "/repo/.claude/settings.json".into(),
                revision: "project-r1".into(),
            },
            permissions::SettingRow {
                kind: SettingKind::Allow,
                scope: None,
                value: "Read".into(),
                editable: false,
                source: "Managed policy".into(),
                revision: "managed".into(),
            },
        ];
        state.tab = Tab::Allow;
        state.query = Editor::from("PROJECT");
        assert_eq!(state.items().len(), 1);
        state.begin_edit(false);
        let form = state.form.as_ref().context("edit form")?;
        let edit = form.edit(&state.data)?;
        assert_eq!(edit.scope, Scope::Project);
        assert_eq!(edit.before.as_deref(), Some("Bash(cargo test:*)"));
        assert_eq!(edit.revision, "project-r1");
        state.form.as_mut().context("edit form")?.value = Editor::from("Bash(cargo check:*)");
        let mut refreshed = state.data.clone();
        refreshed.rows.first_mut().context("row")?.revision = "project-r2".into();
        state.preserve_form = true;
        state.finish(
            "one",
            0,
            Ok(Reply::Permissions {
                snapshot: refreshed,
            }),
        );
        let edit = state
            .form
            .as_ref()
            .context("refreshed form")?
            .edit(&state.data)?;
        assert_eq!(edit.after.as_deref(), Some("Bash(cargo check:*)"));
        assert_eq!(edit.before.as_deref(), Some("Bash(cargo test:*)"));
        assert_eq!(edit.revision, "project-r2");
        state.form = None;
        state.query = Editor::from("Managed");
        state.begin_edit(false);
        assert!(state.form.is_none());
        assert!(
            state
                .error
                .as_deref()
                .is_some_and(|e| e.contains("read-only"))
        );
        Ok(())
    }
    #[test]
    fn new_auto_rules_are_user_scoped_and_keep_multiline_environment() -> Result<()> {
        let mut state = State::new("one".into(), provider::Provider::Claude);
        state.tab = Tab::Auto;
        state.data.revisions.insert(Scope::User, "user-r1".into());
        state.begin_edit(true);
        let form = state.form.as_mut().context("form")?;
        form.kind = SettingKind::AutoEnvironment;
        form.value.insert("Internal network\nUse staging only");
        form.cycle_scope(false);
        let edit = form.edit(&state.data)?;
        assert_eq!(edit.scope, Scope::User);
        assert_eq!(
            edit.after.as_deref(),
            Some("Internal network\nUse staging only")
        );
        assert_eq!(edit.revision, "user-r1");
        assert!(edit.before.is_none());
        Ok(())
    }
    #[test]
    fn refresh_observes_preset_without_creating_or_changing_user_selection() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = Ui::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().join("cache"),
            },
            &Config::default(),
        );
        let mut session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: dir.path().into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: "Test permissions".into(),
                model: None,
                effort: None,
            }),
        );
        session.provider = provider::Provider::Codex;
        session.permissions = serde_json::json!({"sandbox":{"type":"dangerFullAccess"},"approvalPolicy":"never","approvalsReviewer":"user"});
        ui.sessions.insert("one".into(), session);
        for explicit in [None, Some(CodexPreset::Ask)] {
            ui.sessions
                .get_mut("one")
                .context("session")?
                .permission_state
                .preset = explicit;
            ui.modal = Some(Modal::Permissions(Box::new(State::new(
                "one".into(),
                provider::Provider::Codex,
            ))));
            ui.finish_permissions(
                "one",
                0,
                Ok(Reply::Permissions {
                    snapshot: Snapshot {
                        provider: provider::Provider::Codex,
                        active_preset: Some(CodexPreset::Full),
                        ..Snapshot::default()
                    },
                }),
            );
            let session = ui.sessions.get("one").context("session")?;
            assert_eq!(session.permission_state.preset, explicit);
            assert!(permissions::summary(session).contains("Full Access"));
            let Some(Modal::Permissions(state)) = &ui.modal else {
                anyhow::bail!("panel");
            };
            assert_eq!(state.data.active_preset, Some(CodexPreset::Full));
            assert!(render(&mut ui, 104, 24)?.contains("Full Access"));
        }
        Ok(())
    }
    #[test]
    fn completed_reload_clears_status_and_preserves_diagnostics() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = Ui::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().join("cache"),
            },
            &Config::default(),
        );
        let diagnostic =
            "Could not inspect native Auto defaults; existing settings remain available";
        let snapshot = Snapshot {
            provider: provider::Provider::Claude,
            reload_pending: true,
            message: Some(diagnostic.into()),
            ..Snapshot::default()
        };
        let mut state = State::new("one".into(), provider::Provider::Claude);
        state.finish("one", 0, Ok(Reply::Permissions { snapshot }));
        let mut session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: dir.path().into(),
                isolated: true,
                base: "HEAD".into(),
                prompt: "Test permissions".into(),
                model: None,
                effort: None,
            }),
        );
        session.provider = provider::Provider::Claude;
        session.permission_state.reload_pending = true;
        ui.sessions.insert("one".into(), session);
        ui.modal = Some(Modal::Permissions(Box::new(state)));
        let pending = render(&mut ui, 104, 24)?;
        assert!(pending.contains("Settings update pending"));
        assert!(pending.contains("Could not inspect native Auto defaults"));
        ui.sessions
            .get_mut("one")
            .context("session")?
            .permission_state
            .reload_pending = false;
        ui.tick_permissions();
        let completed = render(&mut ui, 104, 24)?;
        assert!(!completed.contains("Settings update pending"));
        assert!(!completed.contains("waiting for idle"));
        assert!(completed.contains("Could not inspect native Auto defaults"));
        let Some(Modal::Permissions(state)) = &mut ui.modal else {
            anyhow::bail!("panel");
        };
        state.finish(
            "one",
            0,
            Ok(Reply::Permissions {
                snapshot: Snapshot {
                    provider: provider::Provider::Claude,
                    reload_pending: false,
                    message: Some(diagnostic.into()),
                    ..Snapshot::default()
                },
            }),
        );
        assert_eq!(state.data.message.as_deref(), Some(diagnostic));
        let refreshed = render(&mut ui, 104, 24)?;
        assert!(!refreshed.contains("Settings update pending"));
        assert!(refreshed.contains("Could not inspect native Auto defaults"));
        Ok(())
    }
    #[test]
    fn stale_results_and_failed_saves_preserve_form() -> Result<()> {
        let mut state = State::new("one".into(), provider::Provider::Claude);
        state.request = 42;
        state.loading = true;
        state.finish("other", 42, Err("wrong session".into()));
        state.finish("one", 41, Err("old".into()));
        assert!(state.loading && state.error.is_none());
        state.tab = Tab::Ask;
        state.loading = false;
        state.begin_edit(true);
        state.loading = true;
        state.finish(
            "one",
            42,
            Err("Settings changed; refresh and try again".into()),
        );
        assert!(!state.loading && state.form.is_some());
        assert!(
            state
                .error
                .as_deref()
                .is_some_and(|e| e.contains("refresh"))
        );
        state.finish(
            "one",
            42,
            Ok(Reply::Permissions {
                snapshot: Snapshot::default(),
            }),
        );
        assert!(state.form.is_none() && state.error.is_none());
        Ok(())
    }
}
