use super::usage::text_lines;
use super::*;
use crate::agents::mcp::{self, Snapshot};
use crate::shortcuts::{contexts::*, ids::*};
use crate::ui::YELLOW;
use std::sync::atomic::{AtomicU64, Ordering};

const MCP_CHOICES: [crate::shortcuts::ActionId; 4] =
    [MCP_CHOOSE_1, MCP_CHOOSE_2, MCP_CHOOSE_3, MCP_CHOOSE_4];

static REQUEST: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Page {
    Servers,
    Server(String),
    Tools(String),
    Tool(String, String),
}
pub struct State {
    pub(super) id: String,
    provider: provider::Provider,
    pub(super) data: Snapshot,
    pub(super) error: Option<String>,
    pub(super) loading: bool,
    pub(super) request: u64,
    activity: &'static str,
    pub(super) page: Page,
    pub(super) selected: usize,
    scroll: usize,
    maximum: usize,
    checked: Instant,
}
impl State {
    fn new(id: String, provider: provider::Provider) -> Self {
        Self {
            id,
            provider,
            data: Snapshot::default(),
            error: None,
            loading: false,
            request: 0,
            activity: "Refreshing status…",
            page: Page::Servers,
            selected: 0,
            scroll: 0,
            maximum: 0,
            checked: Instant::now(),
        }
    }
    pub(super) fn shortcut_context(&self) -> crate::shortcuts::Context {
        match self.page {
            Page::Servers => AGENT_MCP,
            Page::Server(_) => AGENT_MCP_SERVER,
            Page::Tools(_) => AGENT_MCP_TOOLS,
            Page::Tool(..) => AGENT_MCP_TOOL,
        }
    }
    fn server(&self) -> Option<&mcp::Server> {
        let name = match &self.page {
            Page::Servers => {
                return self
                    .order()
                    .get(self.selected)
                    .and_then(|index| self.data.servers.get(*index));
            }
            Page::Server(name) | Page::Tools(name) | Page::Tool(name, _) => name,
        };
        self.data.servers.iter().find(|s| &s.name == name)
    }
    fn order(&self) -> Vec<usize> {
        let mut order = (0..self.data.servers.len()).collect::<Vec<_>>();
        order.sort_by_key(|index| {
            self.data
                .servers
                .get(*index)
                .map(|s| (server_group(s, self.provider).0, s.name.to_lowercase()))
        });
        order
    }
    fn actions(&self) -> Vec<(String, Option<mcp::Action>)> {
        let Some(server) = self.server() else {
            return Vec::new();
        };
        let name = &server.name;
        let mut actions = vec![("View tools".into(), None)];
        if server.can_authenticate
            && server.status != "disabled"
            && (server.status == "needs-auth" || server.auth.as_deref() == Some("notLoggedIn"))
        {
            actions.push((
                "Authenticate".into(),
                Some(mcp::Action::Authenticate { name: name.clone() }),
            ));
        } else if server.can_clear_auth {
            actions.push((
                "Clear authentication".into(),
                Some(mcp::Action::ClearAuth { name: name.clone() }),
            ));
        } else if server.can_authenticate && server.status != "disabled" {
            actions.push((
                "Authenticate".into(),
                Some(mcp::Action::Authenticate { name: name.clone() }),
            ));
        }
        if server.can_reconnect && server.status != "disabled" {
            actions.push((
                "Reconnect".into(),
                Some(mcp::Action::Reconnect { name: name.clone() }),
            ));
        }
        if server.can_toggle {
            let enabled = server.status == "disabled";
            actions.push((
                if enabled { "Enable" } else { "Disable" }.into(),
                Some(mcp::Action::Toggle {
                    name: name.clone(),
                    enabled,
                }),
            ));
        }
        actions
    }
    fn count(&self) -> usize {
        match self.page {
            Page::Servers => self.data.servers.len(),
            Page::Server(_) => self.actions().len(),
            Page::Tools(_) => self.server().map_or(0, |s| s.tools.len()),
            Page::Tool(..) => 0,
        }
    }
    fn navigate(&mut self, page: Page) {
        self.page = page;
        self.selected = 0;
        self.scroll = 0;
    }
    /// A result may arrive after closing, reopening, or switching sessions.
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
        self.checked = Instant::now();
        match result {
            Ok(Reply::Mcp(mut data)) => {
                let selected_name = if self.page == Page::Servers {
                    self.server().map(|s| s.name.clone())
                } else {
                    None
                };
                if data.message.is_none() {
                    data.message = self.data.message.take();
                    data.authorization_url = self.data.authorization_url.take();
                    data.authentication_server = self.data.authentication_server.take();
                }
                self.data = data;
                if let Some(name) = selected_name {
                    self.selected = self
                        .order()
                        .iter()
                        .position(|index| {
                            self.data
                                .servers
                                .get(*index)
                                .is_some_and(|s| s.name == name)
                        })
                        .unwrap_or(0);
                }
                self.selected = self.selected.min(self.count().saturating_sub(1));
                self.error = None;
            }
            Ok(_) => {
                self.error = Some(
                    "Unexpected MCP response; restart Difu's agent service after updating".into(),
                )
            }
            Err(error) => self.error = Some(error),
        }
    }
}
impl Ui {
    pub(super) fn open_mcp(&mut self) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        self.modal = Some(Modal::Mcp(Box::new(State::new(id, session.provider))));
        self.request_mcp(mcp::Action::Refresh);
    }
    fn request_mcp(&mut self, action: mcp::Action) {
        let Some(Modal::Mcp(state)) = &mut self.modal else {
            return;
        };
        if state.loading {
            return;
        }
        state.loading = true;
        state.activity = match &action {
            mcp::Action::Refresh => "Refreshing status…",
            mcp::Action::Reconnect { .. } => "Reconnecting…",
            mcp::Action::Toggle { enabled: true, .. } => "Enabling server…",
            mcp::Action::Toggle { enabled: false, .. } => "Disabling server…",
            mcp::Action::Authenticate { .. } => "Starting authentication…",
            mcp::Action::ClearAuth { .. } => "Clearing authentication…",
        };
        state.error = None;
        state.request = REQUEST.fetch_add(1, Ordering::Relaxed);
        let (id, request) = (state.id.clone(), state.request);
        self.task(
            Task::Mcp(id.clone(), request),
            Request::Mcp { id, action },
            false,
        );
    }
    pub(super) fn tick_mcp(&mut self) {
        if let Some(Modal::Mcp(state)) = &self.modal
            && !state.loading
            && state.error.is_none()
            && state.checked.elapsed() >= Duration::from_secs(5)
        {
            self.request_mcp(mcp::Action::Refresh);
        }
    }
    pub(super) fn mcp_key(&mut self, key: KeyEvent) {
        let shortcut = self.shortcut(key);
        let Some(Modal::Mcp(state)) = &mut self.modal else {
            return;
        };
        match shortcut {
            _ if key.code == KeyCode::Esc || shortcut == Some(MCP_BACK) => {
                let (page, selected) = match &state.page {
                    Page::Servers => {
                        self.modal = None;
                        return;
                    }
                    Page::Server(name) => (
                        Page::Servers,
                        state
                            .order()
                            .iter()
                            .position(|index| {
                                state
                                    .data
                                    .servers
                                    .get(*index)
                                    .is_some_and(|s| &s.name == name)
                            })
                            .unwrap_or(0),
                    ),
                    Page::Tools(name) => (Page::Server(name.clone()), 0),
                    Page::Tool(name, tool) => (
                        Page::Tools(name.clone()),
                        state
                            .server()
                            .and_then(|s| s.tools.iter().position(|t| &t.name == tool))
                            .unwrap_or(0),
                    ),
                };
                state.navigate(page);
                state.selected = selected;
            }
            Some(MCP_REFRESH) => self.request_mcp(mcp::Action::Refresh),
            Some(MCP_OPEN) => {
                if let Some(url) = state.data.authorization_url.clone() {
                    self.action(Action::Link(url));
                }
            }
            Some(NAVIGATION_UP) => {
                if matches!(state.page, Page::Tool(..)) {
                    state.scroll = state.scroll.saturating_sub(1);
                } else {
                    state.selected = state.selected.saturating_sub(1);
                }
            }
            Some(NAVIGATION_DOWN) => {
                if matches!(state.page, Page::Tool(..)) {
                    state.scroll = state.scroll.saturating_add(1).min(state.maximum);
                } else {
                    state.selected = state
                        .selected
                        .saturating_add(1)
                        .min(state.count().saturating_sub(1));
                }
            }
            Some(
                NAVIGATION_PAGE_UP | NAVIGATION_PAGE_DOWN | NAVIGATION_FIRST | NAVIGATION_LAST,
            ) => {
                let maximum = if matches!(state.page, Page::Tool(..)) {
                    state.maximum
                } else {
                    state.count().saturating_sub(1)
                };
                let position = if matches!(state.page, Page::Tool(..)) {
                    &mut state.scroll
                } else {
                    &mut state.selected
                };
                *position = match shortcut {
                    Some(NAVIGATION_FIRST) => 0,
                    Some(NAVIGATION_LAST) => maximum,
                    Some(NAVIGATION_PAGE_UP) => position.saturating_sub(10),
                    _ => position.saturating_add(10).min(maximum),
                };
            }
            Some(action) if MCP_CHOICES.contains(&action) => {
                if let Some(index) = MCP_CHOICES.iter().position(|id| *id == action) {
                    self.mcp_select(Some(index));
                }
            }
            Some(NAVIGATION_ACTIVATE | MCP_SELECT) => self.mcp_select(None),
            _ => {}
        }
    }
    pub(super) fn mcp_scroll(&mut self, down: bool) {
        let Some(Modal::Mcp(state)) = &mut self.modal else {
            return;
        };
        let maximum = if matches!(state.page, Page::Tool(..)) {
            state.maximum
        } else {
            state.count().saturating_sub(1)
        };
        let position = if matches!(state.page, Page::Tool(..)) {
            &mut state.scroll
        } else {
            &mut state.selected
        };
        *position = if down {
            position.saturating_add(3).min(maximum)
        } else {
            position.saturating_sub(3)
        };
    }
    pub(super) fn mcp_select(&mut self, index: Option<usize>) {
        let Some(Modal::Mcp(state)) = &mut self.modal else {
            return;
        };
        if let Some(index) = index {
            state.selected = index;
        }
        match state.page.clone() {
            Page::Servers => {
                if let Some(server) = state.server() {
                    state.navigate(Page::Server(server.name.clone()));
                }
            }
            Page::Server(name) => {
                if let Some((_, action)) = state.actions().get(state.selected).cloned() {
                    if let Some(action) = action {
                        self.request_mcp(action);
                    } else {
                        state.navigate(Page::Tools(name));
                    }
                }
            }
            Page::Tools(name) => {
                if let Some(tool) = state.server().and_then(|s| s.tools.get(state.selected)) {
                    state.navigate(Page::Tool(name, tool.name.clone()));
                }
            }
            Page::Tool(..) => {}
        }
    }
    pub(super) fn draw_mcp_overlay(&mut self, frame: &mut Frame) {
        let full = frame.area();
        let rect = Rect::new(
            full.x + full.width.min(2),
            full.y + full.height.min(1),
            full.width.saturating_sub(4),
            full.height.saturating_sub(2),
        );
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Block::default()
                .style(Style::default().bg(PANEL).fg(TEXT))
                .borders(ratatui::widgets::Borders::TOP)
                .border_style(Style::default().fg(BORDER)),
            rect,
        );
        self.hits.clear();
        let area = Rect::new(
            rect.x + rect.width.min(2),
            rect.y + rect.height.min(2),
            rect.width.saturating_sub(4),
            rect.height.saturating_sub(3),
        );
        self.draw_mcp(frame, area);
    }
    pub(super) fn draw_mcp(&mut self, frame: &mut Frame, area: Rect) {
        let bindings = crate::shortcuts::Bindings::new(&self.shortcut_config);
        let Some(Modal::Mcp(state)) = &mut self.modal else {
            return;
        };
        if area.width == 0 || area.height == 0 {
            return;
        }
        let context = state.shortcut_context();
        let hint = |id| bindings.primary_hint_in(id, context);
        let title = match &state.page {
            Page::Servers => "Manage MCP servers".into(),
            Page::Server(name) => format!("{name} MCP Server"),
            Page::Tools(name) => format!("{name} · Tools"),
            Page::Tool(_, name) => name.clone(),
        };
        frame.render_widget(
            Paragraph::new(title).style(
                Style::default()
                    .fg(ACCENT)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Rect::new(area.x, area.y, area.width, 1),
        );
        if area.height > 1 && matches!(state.page, Page::Servers) {
            frame.render_widget(
                Paragraph::new(format!(
                    "{} {}",
                    state.data.servers.len(),
                    if state.data.servers.len() == 1 {
                        "server"
                    } else {
                        "servers"
                    }
                ))
                .style(Style::default().fg(DIM)),
                Rect::new(area.x, area.y + 1, area.width, 1),
            );
        }
        let body = Rect::new(
            area.x,
            area.y + area.height.min(3),
            area.width,
            area.height.saturating_sub(7),
        );
        let mut lines = Vec::new();
        let mut choices = Vec::new();
        if let Some(error) = &state.error {
            text_lines(error, body.width, Style::default().fg(RED), &mut lines);
            lines.push(Line::default());
        }
        match &state.page {
            Page::Servers => {
                if state.data.servers.is_empty() && !state.loading && state.error.is_none() {
                    lines.push(Line::raw("No MCP servers configured for this agent."));
                    lines.push(Line::styled(
                        "Add servers in your provider configuration, then refresh.",
                        Style::default().fg(DIM),
                    ));
                }
                let order = state.order();
                let name_width = order
                    .iter()
                    .filter_map(|i| state.data.servers.get(*i))
                    .map(|s| unicode_width::UnicodeWidthStr::width(s.name.as_str()))
                    .max()
                    .unwrap_or(0)
                    .min(48)
                    .min(usize::from(body.width.saturating_sub(26)))
                    .max(1);
                let mut previous = None;
                for (index, server) in order
                    .iter()
                    .filter_map(|i| state.data.servers.get(*i))
                    .enumerate()
                {
                    let group = server_group(server, state.provider);
                    if previous != Some(group) {
                        if previous.is_some() {
                            lines.push(Line::default());
                        }
                        let mut header = vec![Span::styled(
                            format!("  {}", group.1),
                            Style::default()
                                .fg(TEXT)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        )];
                        if !matches!(group.0, 3 | 4)
                            && let Some(location) = &server.config_location
                        {
                            let available =
                                usize::from(body.width).saturating_sub(group.1.len() + 5);
                            header.push(Span::styled(
                                format!(" ({})", clip(location, available)),
                                Style::default().fg(DIM),
                            ));
                        }
                        lines.push(Line::from(header));
                        previous = Some(group);
                    }
                    let selected = state.selected == index;
                    let (icon, color, _) = status_style(&server.status);
                    let name = clip(&server.name, name_width);
                    let padding = " ".repeat(
                        name_width
                            .saturating_sub(unicode_width::UnicodeWidthStr::width(name.as_str()))
                            + 2,
                    );
                    let summary = match server.status.as_str() {
                        "connected" | "ready" | "cached" => tool_count(server.tools.len()),
                        "needs-auth" => "needs authentication".into(),
                        "disabled" => String::new(),
                        "pending" | "starting" => "connecting…".into(),
                        "failed" => format!("failed · {} for details", hint(NAVIGATION_ACTIVATE)),
                        _ => "status unavailable".into(),
                    };
                    choices.push((lines.len(), index));
                    lines.push(Line::from(vec![
                        Span::styled(
                            if selected { "❯ " } else { "  " },
                            Style::default().fg(ACCENT),
                        ),
                        Span::styled(format!("{icon} "), Style::default().fg(color)),
                        Span::styled(
                            name,
                            Style::default().fg(if selected { ACCENT } else { TEXT }),
                        ),
                        Span::raw(padding),
                        Span::styled(summary, Style::default().fg(DIM)),
                    ]));
                }
            }
            Page::Server(_) => {
                if let Some(server) = state.server() {
                    let (icon, color, label) = status_style(&server.status);
                    field(
                        &mut lines,
                        "Status:",
                        format!("{icon} {label}"),
                        color,
                        body.width,
                    );
                    let (auth, color) = auth_label(server);
                    field(&mut lines, "Auth:", auth, color, body.width);
                    if let Some(endpoint) = &server.endpoint {
                        field(&mut lines, "URL:", endpoint.clone(), DIM, body.width);
                    } else {
                        field(
                            &mut lines,
                            "Transport:",
                            server.transport.clone(),
                            DIM,
                            body.width,
                        );
                    }
                    field(
                        &mut lines,
                        "Config location:",
                        server
                            .config_location
                            .clone()
                            .unwrap_or_else(|| server.scope.clone()),
                        DIM,
                        body.width,
                    );
                    if !server.capabilities.is_empty() {
                        field(
                            &mut lines,
                            "Capabilities:",
                            server.capabilities.join(", "),
                            DIM,
                            body.width,
                        );
                    }
                    field(
                        &mut lines,
                        "Tools:",
                        tool_count(server.tools.len()),
                        DIM,
                        body.width,
                    );
                    if let Some(error) = &server.error {
                        lines.push(Line::default());
                        text_lines(error, body.width, Style::default().fg(RED), &mut lines);
                    }
                    if state.provider == provider::Provider::Codex {
                        lines.push(Line::default());
                        text_lines(
                            "Reconnect reloads all servers in this agent. Enable/disable saves a Codex user setting.",
                            body.width,
                            Style::default().fg(DIM),
                            &mut lines,
                        );
                    }
                    lines.push(Line::default());
                    lines.push(Line::default());
                    for (index, (label, _)) in state.actions().iter().enumerate() {
                        choices.push((lines.len(), index));
                        lines.push(Line::from(vec![
                            Span::styled(
                                if state.selected == index {
                                    "❯ "
                                } else {
                                    "  "
                                },
                                Style::default().fg(ACCENT),
                            ),
                            Span::styled(
                                format!(
                                    "{}. ",
                                    MCP_CHOICES
                                        .get(index)
                                        .map(|id| hint(*id))
                                        .unwrap_or_default()
                                ),
                                Style::default().fg(DIM),
                            ),
                            Span::styled(
                                label.clone(),
                                Style::default().fg(if state.selected == index {
                                    ACCENT
                                } else {
                                    TEXT
                                }),
                            ),
                        ]));
                    }
                } else {
                    lines.push(Line::raw(
                        "Server no longer available. Esc returns to the list.",
                    ));
                }
            }
            Page::Tools(_) => {
                if let Some(server) = state.server() {
                    if server.tools.is_empty() {
                        lines.push(Line::styled(
                            "No tools reported by this server.",
                            Style::default().fg(DIM),
                        ));
                    }
                    for (index, tool) in server.tools.iter().enumerate() {
                        choices.push((lines.len(), index));
                        lines.push(choice(tool.name.clone(), state.selected == index));
                    }
                }
            }
            Page::Tool(_, name) => {
                if let Some(tool) = state
                    .server()
                    .and_then(|s| s.tools.iter().find(|t| &t.name == name))
                {
                    if !tool.description.is_empty() {
                        text_lines(
                            &tool.description,
                            body.width,
                            Style::default().fg(TEXT),
                            &mut lines,
                        );
                        lines.push(Line::default());
                    }
                    if let Some(schema) = &tool.schema {
                        lines.push(Line::styled(
                            "Input schema",
                            Style::default()
                                .fg(TEXT)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        ));
                        text_lines(
                            &serde_json::to_string_pretty(schema).unwrap_or_default(),
                            body.width,
                            Style::default().fg(DIM),
                            &mut lines,
                        );
                    } else {
                        text_lines(
                            "This provider does not expose the tool's input schema.",
                            body.width,
                            Style::default().fg(DIM),
                            &mut lines,
                        );
                    }
                }
            }
        }
        let height = usize::from(body.height);
        let content_end = body
            .y
            .saturating_add(u16::try_from(lines.len()).unwrap_or(u16::MAX));
        state.maximum = lines.len().saturating_sub(height);
        let footer_y = if state.maximum == 0 {
            content_end
                .saturating_add(2)
                .min(area.bottom().saturating_sub(3))
        } else {
            area.bottom().saturating_sub(3)
        };
        if let Some((row, _)) = choices.iter().find(|(_, index)| *index == state.selected) {
            if *row < state.scroll {
                state.scroll = *row;
            } else if *row >= state.scroll + height {
                state.scroll = row.saturating_add(1).saturating_sub(height);
            }
        }
        state.scroll = state.scroll.min(state.maximum);
        let scroll = state.scroll;
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(scroll)
                    .take(height)
                    .collect::<Vec<_>>(),
            ),
            body,
        );
        let note = if state.loading {
            Some(state.activity.to_owned())
        } else if state.data.authorization_url.is_some() {
            Some(format!(
                "{} Open authentication for {}",
                hint(MCP_OPEN),
                state
                    .data
                    .authentication_server
                    .as_deref()
                    .unwrap_or("MCP server")
            ))
        } else {
            state.data.message.clone()
        };
        if let Some(note) = note
            && area.height > 2
        {
            frame.render_widget(
                Paragraph::new(clip(&note, usize::from(area.width)))
                    .style(Style::default().fg(DIM)),
                Rect::new(area.x, footer_y.saturating_sub(1), area.width, 1),
            );
        }
        let select = if matches!(state.page, Page::Tool(..)) {
            String::new()
        } else {
            format!(
                " · {}/{} select",
                hint(NAVIGATION_ACTIVATE),
                hint(MCP_SELECT)
            )
        };
        let hint = format!(
            "{}/{} navigate{} · {}/Esc back · {} refresh",
            hint(NAVIGATION_UP),
            hint(NAVIGATION_DOWN),
            select,
            hint(MCP_BACK),
            hint(MCP_REFRESH)
        );
        if area.height > 1 {
            frame.render_widget(
                Paragraph::new(hint)
                    .wrap(ratatui::widgets::Wrap { trim: true })
                    .style(
                        Style::default()
                            .fg(DIM)
                            .add_modifier(ratatui::style::Modifier::ITALIC),
                    ),
                Rect::new(
                    area.x,
                    footer_y,
                    area.width,
                    area.bottom().saturating_sub(footer_y).min(3),
                ),
            );
        }
        for (row, index) in choices {
            if row >= scroll && row < scroll + height {
                self.hits.push((
                    Rect::new(body.x, body.y + (row - scroll) as u16, body.width, 1),
                    Action::McpSelect(index),
                ));
            }
        }
    }
}

fn server_group(server: &mcp::Server, provider: provider::Provider) -> (u8, &'static str) {
    let source = if server.source.is_empty() {
        server.scope.as_str()
    } else {
        server.source.as_str()
    };
    if provider == provider::Provider::Codex {
        return if source == "plugin" || server.scope.starts_with("Plugin:") {
            (4, "Plugin MCPs")
        } else {
            (0, "Configured MCPs")
        };
    }
    match source {
        "claudeai" => (3, "claude.ai"),
        "sdk" | "plugin" => (4, "Built-in MCPs"),
        "user" => (1, "User MCPs"),
        "project" => (2, "Project MCPs"),
        "managed" | "enterprise" => (5, "Managed MCPs"),
        _ if server.transport == "sdk" => (4, "Built-in MCPs"),
        _ if server.source.is_empty() && server.name.starts_with("claude.ai ") => (3, "claude.ai"),
        _ => (0, "Local MCPs"),
    }
}
fn status_style(status: &str) -> (&'static str, ratatui::style::Color, &'static str) {
    match status {
        "connected" | "ready" => ("✓", GREEN, "connected"),
        "needs-auth" => ("⚠", YELLOW, "needs authentication"),
        "disabled" => ("○", DIM, "disabled"),
        "pending" | "starting" => ("•", YELLOW, "connecting"),
        "failed" => ("✗", RED, "failed"),
        "cached" => ("○", DIM, "cached"),
        _ => ("?", DIM, "not reported"),
    }
}
fn auth_label(server: &mcp::Server) -> (String, ratatui::style::Color) {
    match server.auth.as_deref() {
        Some("authenticated" | "oAuth") => ("✓ authenticated".into(), GREEN),
        Some("bearerToken") => ("Bearer token configured".into(), DIM),
        Some("notLoggedIn") => ("⚠ needs authentication".into(), YELLOW),
        Some("unsupported") => ("Not supported".into(), DIM),
        _ if server.status == "needs-auth" => ("⚠ needs authentication".into(), YELLOW),
        _ if server.transport == "stdio" || server.transport == "sdk" => {
            ("Not required".into(), DIM)
        }
        _ => ("Not reported by provider".into(), DIM),
    }
}
fn tool_count(count: usize) -> String {
    format!("{count} {}", if count == 1 { "tool" } else { "tools" })
}
fn clip(text: &str, width: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if text.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let size = ch.width().unwrap_or(0);
        if used + size >= width {
            break;
        }
        result.push(ch);
        used += size;
    }
    result.push('…');
    result
}
fn field(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    value: String,
    color: ratatui::style::Color,
    width: u16,
) {
    let column = 18;
    if width < 36 {
        lines.push(Line::styled(
            label.to_owned(),
            Style::default()
                .fg(TEXT)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ));
        text_lines(&value, width, Style::default().fg(color), lines);
        return;
    }
    for (index, line) in wrapped(&value, width.saturating_sub(column))
        .into_iter()
        .enumerate()
    {
        lines.push(Line::from(vec![
            Span::styled(
                if index == 0 {
                    format!("{label:<18}")
                } else {
                    " ".repeat(18)
                },
                Style::default()
                    .fg(TEXT)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Span::styled(line, Style::default().fg(color)),
        ]));
    }
}

fn choice(label: String, selected: bool) -> Line<'static> {
    Line::styled(
        format!("{} {label}", if selected { "›" } else { " " }),
        Style::default().fg(if selected { ACCENT } else { TEXT }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};
    use serde_json::json;
    fn snapshot() -> Result<Snapshot> {
        Ok(Snapshot {
            servers: mcp::claude(
                &json!({"mcpServers":[{"name":"fixture","status":"connected","config":{"type":"http"},"tools":[{"name":"hello"}]}]}),
            )?,
            ..Snapshot::default()
        })
    }
    #[test]
    fn mcp_numbered_actions_respect_authentication_and_disabled_capabilities() -> Result<()> {
        for (status, action, code) in [
            ("needs-auth", MCP_CHOOSE_2, '2'),
            ("disabled", MCP_CHOOSE_3, '3'),
        ] {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                let mut state = State::new("one".into(), provider::Provider::Claude);
                state.data = snapshot()?;
                let server = state.data.servers.first_mut().context("server")?;
                server.status = status.into();
                server.can_authenticate = true;
                state.page = Page::Server("fixture".into());
                ui.modal = Some(Modal::Mcp(Box::new(state)));
                if custom {
                    ui.key(KeyEvent::new(KeyCode::Char(code), KeyModifiers::NONE));
                    assert!(matches!(&ui.modal, Some(Modal::Mcp(s)) if !s.loading));
                }
                let request = shortcut_tests::request_at_boundary(
                    &mut ui,
                    KeyEvent::new(
                        if custom {
                            KeyCode::F(12)
                        } else {
                            KeyCode::Char(code)
                        },
                        KeyModifiers::NONE,
                    ),
                )?;
                assert!(match request {
                    Request::Mcp {
                        id,
                        action: mcp::Action::Authenticate { name },
                    } => status == "needs-auth" && id == "one" && name == "fixture",
                    Request::Mcp {
                        id,
                        action:
                            mcp::Action::Toggle {
                                name,
                                enabled: true,
                            },
                    } => status == "disabled" && id == "one" && name == "fixture",
                    _ => false,
                });
            }
        }
        Ok(())
    }

    #[test]
    fn mcp_back_rebinding_keeps_each_parent_page_and_modal_escape() -> Result<()> {
        for page in [
            Page::Servers,
            Page::Server("fixture".into()),
            Page::Tools("fixture".into()),
            Page::Tool("fixture".into(), "hello".into()),
        ] {
            for code in [KeyCode::Left, KeyCode::Backspace] {
                for (phase, custom) in shortcut_tests::BINDING_PHASES {
                    let (mut ui, _dir, _) = shortcut_tests::binding_fixture(MCP_BACK, phase)?;
                    let mut state = State::new("one".into(), provider::Provider::Claude);
                    state.data = snapshot()?;
                    state.page = page.clone();
                    ui.modal = Some(Modal::Mcp(Box::new(state)));
                    if custom {
                        ui.key(KeyEvent::new(code, KeyModifiers::NONE));
                        assert!(matches!(&ui.modal, Some(Modal::Mcp(s)) if s.page == page));
                    }
                    ui.key(KeyEvent::new(
                        if custom { KeyCode::F(12) } else { code },
                        KeyModifiers::NONE,
                    ));
                    match page {
                        Page::Servers => assert!(ui.modal.is_none()),
                        Page::Server(_) => assert!(
                            matches!(&ui.modal, Some(Modal::Mcp(s)) if s.page == Page::Servers)
                        ),
                        Page::Tools(_) => assert!(
                            matches!(&ui.modal, Some(Modal::Mcp(s)) if s.page == Page::Server("fixture".into()))
                        ),
                        Page::Tool(..) => assert!(
                            matches!(&ui.modal, Some(Modal::Mcp(s)) if s.page == Page::Tools("fixture".into()))
                        ),
                    }
                    for _ in 0..4 {
                        ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                    }
                    assert!(ui.modal.is_none());
                }
            }
        }
        Ok(())
    }

    #[test]
    fn mcp_capabilities_offer_at_most_four_actions_and_unknown_slots_stay_opaque() -> Result<()> {
        let (mut ui, _dir, mut config) = shortcut_tests::fixture(None)?;
        let mut state = State::new("one".into(), provider::Provider::Claude);
        state.data = snapshot()?;
        state.page = Page::Server("fixture".into());
        for status in ["connected", "disabled", "needs-auth"] {
            for flags in 0..16 {
                let server = state.data.servers.first_mut().context("server")?;
                server.status = status.into();
                server.can_authenticate = flags & 1 != 0;
                server.can_clear_auth = flags & 2 != 0;
                server.can_reconnect = flags & 4 != 0;
                server.can_toggle = flags & 8 != 0;
                assert!(state.actions().len() <= 4);
            }
        }
        ui.modal = Some(Modal::Mcp(Box::new(state)));
        for digit in '5'..='9' {
            let id = format!("mcp.choose_{digit}");
            assert!(
                crate::settings::spec(&crate::settings::SettingId::Shortcut(id.clone())).is_none()
            );
            config.preferences.keyboard.bindings.insert(
                id,
                crate::shortcuts::key(crate::settings::BindingKey::Function(12), 0),
            );
            ui.key(KeyEvent::new(KeyCode::Char(digit), KeyModifiers::NONE));
            assert!(
                matches!(&ui.modal, Some(Modal::Mcp(s)) if s.page == Page::Server("fixture".into()) && s.selected == 0 && !s.loading)
            );
        }
        ui.storage.update_config(|saved| *saved = config.clone())?;
        let mut draft = ui.storage.settings_draft()?;
        draft.set(
            crate::settings::SettingId::Shortcut(MCP_SELECT.0.into()),
            crate::settings::SettingValue::Shortcut(Some(crate::shortcuts::key(
                crate::settings::BindingKey::Function(11),
                0,
            ))),
        )?;
        let saved = ui.storage.save_settings_draft(&mut draft)?;
        for digit in '5'..='9' {
            assert_eq!(
                saved
                    .preferences
                    .keyboard
                    .bindings
                    .get(&format!("mcp.choose_{digit}")),
                Some(&crate::shortcuts::key(
                    crate::settings::BindingKey::Function(12),
                    0
                ))
            );
        }
        ui.apply_settings(&saved);
        ui.key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE));
        assert!(
            matches!(&ui.modal, Some(Modal::Mcp(s)) if s.page == Page::Server("fixture".into()) && s.selected == 0 && !s.loading)
        );
        Ok(())
    }

    #[test]
    fn mcp_navigation_affects_the_actual_server_list_action_list_or_source_scroll() -> Result<()> {
        for page in ["servers", "actions", "source"] {
            for (action, code, want_list, want_actions, want_source) in [
                (NAVIGATION_UP, KeyCode::Up, 4, 1, 14),
                (NAVIGATION_DOWN, KeyCode::Down, 6, 3, 16),
                (NAVIGATION_PAGE_UP, KeyCode::PageUp, 0, 0, 5),
                (NAVIGATION_PAGE_DOWN, KeyCode::PageDown, 15, 3, 25),
                (NAVIGATION_FIRST, KeyCode::Home, 0, 0, 0),
                (NAVIGATION_LAST, KeyCode::End, 19, 3, 30),
            ] {
                for (phase, custom) in shortcut_tests::BINDING_PHASES {
                    let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                    let mut state = State::new("one".into(), provider::Provider::Claude);
                    state.data = snapshot()?;
                    let template = state.data.servers.first().context("server")?.clone();
                    if page == "servers" {
                        state.data.servers = (0..20)
                            .map(|i| {
                                let mut server = template.clone();
                                server.name = format!("fixture-{i:02}");
                                server
                            })
                            .collect();
                        state.selected = 5;
                    } else if page == "actions" {
                        state.page = Page::Server("fixture".into());
                        state.selected = 2;
                    } else {
                        state.page = Page::Tool("fixture".into(), "hello".into());
                        state.scroll = 15;
                        state.maximum = 30;
                    }
                    ui.modal = Some(Modal::Mcp(Box::new(state)));
                    if custom {
                        ui.key(KeyEvent::new(code, KeyModifiers::NONE));
                        assert!(
                            matches!(&ui.modal, Some(Modal::Mcp(s)) if (page == "servers" && s.selected == 5) || (page == "actions" && s.selected == 2) || (page == "source" && s.scroll == 15))
                        );
                    }
                    ui.key(KeyEvent::new(
                        if custom { KeyCode::F(12) } else { code },
                        KeyModifiers::NONE,
                    ));
                    let Some(Modal::Mcp(state)) = &ui.modal else {
                        anyhow::bail!("MCP")
                    };
                    if page == "servers" {
                        assert_eq!(state.selected, want_list);
                    } else if page == "actions" {
                        assert_eq!(state.selected, want_actions);
                    } else {
                        assert_eq!(state.scroll, want_source);
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn mcp_page_contexts_keep_early_actions_copy_guards_and_safe_escape() -> Result<()> {
        for page in [
            Page::Servers,
            Page::Server("fixture".into()),
            Page::Tools("fixture".into()),
            Page::Tool("fixture".into(), "hello".into()),
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
                    let mut state = State::new("one".into(), provider::Provider::Claude);
                    state.data = snapshot()?;
                    state.page = page.clone();
                    ui.modal = Some(Modal::Mcp(Box::new(state)));
                    let context = ui.shortcut_context();
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
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, _dir, _) = shortcut_tests::binding_fixture(SELECTION_COPY, phase)?;
                let mut state = State::new("one".into(), provider::Provider::Claude);
                state.data = snapshot()?;
                state.page = page.clone();
                ui.positions
                    .get_mut("one")
                    .context("draft")?
                    .draft
                    .key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
                ui.modal = Some(Modal::Mcp(Box::new(state)));
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
                assert!(ui.clipboard.is_none(), "never copy the hidden draft");
                ui.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                match &page {
                    Page::Servers => assert!(ui.modal.is_none()),
                    Page::Server(_) => {
                        assert!(matches!(&ui.modal, Some(Modal::Mcp(s)) if s.page == Page::Servers))
                    }
                    Page::Tools(_) => assert!(
                        matches!(&ui.modal, Some(Modal::Mcp(s)) if s.page == Page::Server("fixture".into()))
                    ),
                    Page::Tool(..) => assert!(
                        matches!(&ui.modal, Some(Modal::Mcp(s)) if s.page == Page::Tools("fixture".into()))
                    ),
                }
            }
        }
        Ok(())
    }

    #[test]
    fn mcp_mouse_wheels_and_clicks_survive_keyboard_rebinding() -> Result<()> {
        let (mut ui, _dir, mut config) = shortcut_tests::fixture(Some(NAVIGATION_DOWN))?;
        config.preferences.keyboard.bindings.insert(
            NAVIGATION_UP.0.into(),
            crate::shortcuts::key(crate::settings::BindingKey::Function(11), 0),
        );
        config.preferences.keyboard.bindings.insert(
            NAVIGATION_ACTIVATE.0.into(),
            crate::shortcuts::key(crate::settings::BindingKey::Function(10), 0),
        );
        ui.apply_settings(&config);
        let mut state = State::new("one".into(), provider::Provider::Claude);
        state.data = snapshot()?;
        state.data.servers.first_mut().context("server")?.tools = (0..20)
            .map(|i| mcp::Tool {
                name: format!("tool-{i}"),
                description: String::new(),
                schema: None,
            })
            .collect();
        state.page = Page::Tools("fixture".into());
        ui.modal = Some(Modal::Mcp(Box::new(state)));
        let wheel = |kind| MouseEvent {
            kind,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        ui.mouse(wheel(MouseEventKind::ScrollDown));
        assert!(matches!(&ui.modal, Some(Modal::Mcp(s)) if s.selected == 3));
        ui.mouse(wheel(MouseEventKind::ScrollUp));
        assert!(matches!(&ui.modal, Some(Modal::Mcp(s)) if s.selected == 0));
        ui.hits = vec![(Rect::new(1, 1, 2, 1), Action::McpSelect(2))];
        ui.mouse(wheel(MouseEventKind::Down(MouseButton::Left)));
        assert!(
            matches!(&ui.modal, Some(Modal::Mcp(s)) if s.page == Page::Tool("fixture".into(), "tool-2".into()))
        );
        if let Some(Modal::Mcp(state)) = &mut ui.modal {
            state.maximum = 20;
        }
        ui.mouse(wheel(MouseEventKind::ScrollDown));
        assert!(matches!(&ui.modal, Some(Modal::Mcp(s)) if s.scroll == 3));
        Ok(())
    }

    #[test]
    fn mcp_refresh_and_select_use_each_real_page_context() -> Result<()> {
        for page in [
            Page::Servers,
            Page::Server("fixture".into()),
            Page::Tools("fixture".into()),
            Page::Tool("fixture".into(), "hello".into()),
        ] {
            for (action, code) in [
                (MCP_REFRESH, KeyCode::Char('r')),
                (MCP_SELECT, KeyCode::Right),
                (NAVIGATION_ACTIVATE, KeyCode::Enter),
            ] {
                for (phase, custom) in shortcut_tests::BINDING_PHASES {
                    let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                    let mut state = State::new("one".into(), provider::Provider::Claude);
                    state.data = snapshot()?;
                    state.page = page.clone();
                    ui.modal = Some(Modal::Mcp(Box::new(state)));
                    if custom {
                        ui.key(KeyEvent::new(code, KeyModifiers::NONE));
                        assert!(
                            matches!(&ui.modal, Some(Modal::Mcp(s)) if s.page == page && !s.loading)
                        );
                    }
                    let key = KeyEvent::new(
                        if custom { KeyCode::F(12) } else { code },
                        KeyModifiers::NONE,
                    );
                    if action == MCP_REFRESH {
                        let request = shortcut_tests::request_at_boundary(&mut ui, key)?;
                        assert!(
                            matches!(request, Request::Mcp { id, action: mcp::Action::Refresh } if id == "one")
                        );
                    } else {
                        ui.key(key);
                        let Some(Modal::Mcp(state)) = &ui.modal else {
                            anyhow::bail!("panel")
                        };
                        match &page {
                            Page::Servers => assert_eq!(state.page, Page::Server("fixture".into())),
                            Page::Server(_) => {
                                assert_eq!(state.page, Page::Tools("fixture".into()))
                            }
                            Page::Tools(_) => {
                                assert_eq!(state.page, Page::Tool("fixture".into(), "hello".into()))
                            }
                            Page::Tool(..) => assert_eq!(state.page, page),
                        }
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn mcp_open_uses_exact_validated_url_and_rebinding_removes_old_launcher_key() -> Result<()> {
        for page in [
            Page::Servers,
            Page::Server("fixture".into()),
            Page::Tools("fixture".into()),
            Page::Tool("fixture".into(), "hello".into()),
        ] {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let launch = crate::process::fixture::UrlScope::new()?;
                let (mut ui, _dir, _) = shortcut_tests::binding_fixture(MCP_OPEN, phase)?;
                let mut state = State::new("one".into(), provider::Provider::Claude);
                state.data = snapshot()?;
                state.page = page.clone();
                state.data.authorization_url = Some(launch.url.clone());
                ui.modal = Some(Modal::Mcp(Box::new(state)));
                if custom {
                    ui.key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
                    assert!(
                        launch
                            .receiver
                            .recv_timeout(Duration::from_millis(30))
                            .is_err()
                    );
                }
                ui.key(KeyEvent::new(
                    if custom {
                        KeyCode::F(12)
                    } else {
                        KeyCode::Char('o')
                    },
                    KeyModifiers::NONE,
                ));
                assert_eq!(
                    launch.receiver.recv_timeout(Duration::from_secs(3))?,
                    launch.url
                );
            }
        }
        let cancelled = crate::process::fixture::UrlScope::new()?;
        let cancel = crate::process::Cancel::default();
        cancel.cancel();
        assert!(crate::github::open_url(&cancelled.url, &cancel).is_err());
        assert!(
            cancelled
                .receiver
                .recv_timeout(Duration::from_millis(30))
                .is_err()
        );
        let launch = crate::process::fixture::UrlScope::new()?;
        let url = launch.url.clone();
        drop(launch);
        assert!(
            crate::github::open_url(&url, &crate::process::Cancel::default()).is_err(),
            "late fixture launch fails closed"
        );
        assert!(
            crate::github::open_url("file:///tmp/fixture", &crate::process::Cancel::default())
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn mcp_action_keys_replace_old_aliases_and_reach_the_existing_boundary() -> Result<()> {
        use crate::shortcuts::ids::*;
        for (action, code, want) in [
            (MCP_REFRESH, 'r', "refresh"),
            (MCP_CHOOSE_1, '1', "tools"),
            (MCP_CHOOSE_2, '2', "clear"),
            (MCP_CHOOSE_3, '3', "reconnect"),
            (MCP_CHOOSE_4, '4', "disable"),
        ] {
            for (phase, custom) in shortcut_tests::BINDING_PHASES {
                let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                let mut state = State::new("one".into(), provider::Provider::Claude);
                state.data = snapshot()?;
                state.navigate(Page::Server("fixture".into()));
                ui.modal = Some(Modal::Mcp(Box::new(state)));
                if custom {
                    ui.key(KeyEvent::new(KeyCode::Char(code), KeyModifiers::NONE));
                    assert!(
                        matches!(&ui.modal, Some(Modal::Mcp(s)) if s.page == Page::Server("fixture".into()) && s.selected == 0 && !s.loading)
                    );
                }
                let key = KeyEvent::new(
                    if custom {
                        KeyCode::F(12)
                    } else {
                        KeyCode::Char(code)
                    },
                    KeyModifiers::NONE,
                );
                if want == "tools" {
                    ui.key(key);
                    assert!(
                        matches!(&ui.modal, Some(Modal::Mcp(s)) if s.page == Page::Tools("fixture".into()) && !s.loading)
                    );
                } else {
                    let request = shortcut_tests::request_at_boundary(&mut ui, key)?;
                    let Request::Mcp { id, action } = request else {
                        anyhow::bail!("MCP request")
                    };
                    assert_eq!(id, "one");
                    match want {
                        "refresh" => assert!(matches!(action, mcp::Action::Refresh)),
                        "clear" => assert!(
                            matches!(action, mcp::Action::ClearAuth { name } if name == "fixture")
                        ),
                        "reconnect" => assert!(
                            matches!(action, mcp::Action::Reconnect { name } if name == "fixture")
                        ),
                        "disable" => assert!(
                            matches!(action, mcp::Action::Toggle { name, enabled: false } if name == "fixture")
                        ),
                        _ => anyhow::bail!("expectation"),
                    }
                }
                assert_eq!(
                    ui.positions.get("one").context("draft")?.draft.text(),
                    "unsent draft"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn mcp_back_select_and_shared_navigation_replace_every_legacy_alias() -> Result<()> {
        use crate::shortcuts::ids::*;
        for (action, aliases, start, want) in [
            (MCP_BACK, vec![KeyCode::Left, KeyCode::Backspace], 5, 0),
            (MCP_SELECT, vec![KeyCode::Right], 5, 0),
            (NAVIGATION_ACTIVATE, vec![KeyCode::Enter], 5, 0),
            (NAVIGATION_UP, vec![KeyCode::Up, KeyCode::Char('k')], 5, 4),
            (
                NAVIGATION_DOWN,
                vec![KeyCode::Down, KeyCode::Char('j')],
                5,
                6,
            ),
            (NAVIGATION_PAGE_UP, vec![KeyCode::PageUp], 15, 5),
            (NAVIGATION_PAGE_DOWN, vec![KeyCode::PageDown], 5, 15),
            (NAVIGATION_FIRST, vec![KeyCode::Home], 5, 0),
            (NAVIGATION_LAST, vec![KeyCode::End], 5, 19),
        ] {
            for alias in aliases {
                for (phase, custom) in shortcut_tests::BINDING_PHASES {
                    let (mut ui, _dir, _) = shortcut_tests::binding_fixture(action, phase)?;
                    let mut state = State::new("one".into(), provider::Provider::Claude);
                    state.data = snapshot()?;
                    let server = state.data.servers.first_mut().context("server")?;
                    server.tools = (0..20)
                        .map(|i| mcp::Tool {
                            name: format!("tool-{i}"),
                            description: String::new(),
                            schema: None,
                        })
                        .collect();
                    state.page = Page::Tools("fixture".into());
                    state.selected = start;
                    ui.modal = Some(Modal::Mcp(Box::new(state)));
                    if custom {
                        ui.key(KeyEvent::new(alias, KeyModifiers::NONE));
                        assert!(
                            matches!(&ui.modal, Some(Modal::Mcp(s)) if s.page == Page::Tools("fixture".into()) && s.selected == start)
                        );
                    }
                    ui.key(KeyEvent::new(
                        if custom { KeyCode::F(12) } else { alias },
                        KeyModifiers::NONE,
                    ));
                    let Some(Modal::Mcp(s)) = &ui.modal else {
                        anyhow::bail!("MCP panel")
                    };
                    if action == MCP_BACK {
                        assert_eq!(s.page, Page::Server("fixture".into()));
                    } else if action == MCP_SELECT || action == NAVIGATION_ACTIVATE {
                        assert_eq!(
                            s.page,
                            Page::Tool("fixture".into(), format!("tool-{start}"))
                        );
                    } else {
                        assert_eq!(s.selected, want);
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn mcp_hints_render_the_effective_page_bindings() -> Result<()> {
        use crate::shortcuts::ids::*;
        for action in [
            MCP_REFRESH,
            MCP_OPEN,
            MCP_BACK,
            MCP_SELECT,
            MCP_CHOOSE_1,
            MCP_CHOOSE_2,
            MCP_CHOOSE_3,
            MCP_CHOOSE_4,
            NAVIGATION_UP,
            NAVIGATION_DOWN,
            NAVIGATION_ACTIVATE,
        ] {
            let (mut ui, _dir, _) = shortcut_tests::fixture(Some(action))?;
            let mut state = State::new("one".into(), provider::Provider::Claude);
            state.data = snapshot()?;
            state.data.authorization_url = Some("https://fixture.invalid/authorize".into());
            state.navigate(Page::Server("fixture".into()));
            ui.modal = Some(Modal::Mcp(Box::new(state)));
            for width in [80, 160] {
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 40))?;
                terminal.draw(|frame| ui.draw_mcp(frame, frame.area()))?;
                let text = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(text.contains("F12"), "{action:?} width={width}: {text}");
            }
        }
        Ok(())
    }

    #[test]
    #[ignore = "exports native terminal buffers for visual review; requires DIFU_MCP_PREVIEW_DIR"]
    fn export_mcp_visual_preview() -> Result<()> {
        let output = std::path::PathBuf::from(std::env::var("DIFU_MCP_PREVIEW_DIR")?);
        std::fs::create_dir_all(&output)?;
        let theme = std::env::var("DIFU_MCP_PREVIEW_THEME").ok();
        crate::theme::select(crate::theme::index(theme.as_deref()));
        let dir = tempfile::tempdir()?;
        let mut ui = Ui::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().join("cache"),
            },
            &Config::default(),
        );
        let mut state = State::new("preview".into(), provider::Provider::Claude);
        if let Ok(input) = std::env::var("DIFU_MCP_PREVIEW_INPUT") {
            state.data = serde_json::from_slice(&std::fs::read(input)?)?;
        } else {
            state.data.servers = mcp::claude(&json!({"mcpServers":[
                {"name":"customer_support","status":"connected","source":"local","scope":"local","config":{"type":"http","url":"https://api.example.com/mcp","headers":{"Authorization":"fixture"}},"tools":[{"name":"search"},{"name":"reply"}]},
                {"name":"knowledge_base","status":"connected","source":"local","config":{"type":"stdio"},"tools":[{"name":"search"}]},
                {"name":"web_search","status":"disabled","source":"local","config":{"type":"stdio"}},
                {"name":"claude.ai Calendar","status":"connected","source":"claudeai","tools":[{"name":"list_events"}]},
                {"name":"claude.ai Linear","status":"needs-auth","source":"claudeai"},
                {"name":"claude.ai Notion","status":"connected","source":"claudeai","tools":[{"name":"search"}]},
                {"name":"difu","status":"connected","source":"sdk","config":{"type":"sdk"},"tools":[{"name":"present_artifact"}]},
                {"name":"plugin:docs:docs","status":"connected","source":"plugin","tools":[{"name":"read_docs"}]}
            ]}))?;
            for server in &mut state.data.servers {
                server.config_location = Some(if server.source == "local" {
                    "~/.claude.json [project: /workspace/support]".into()
                } else {
                    server.source.clone()
                });
            }
        }
        ui.modal = Some(Modal::Mcp(Box::new(state)));
        fn color(color: ratatui::style::Color, fallback: &str) -> String {
            match crate::theme::resolve(color) {
                ratatui::style::Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
                _ => fallback.into(),
            }
        }
        fn escape(value: &str) -> String {
            value
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        }
        for (name, detail) in [("list", false), ("detail", true)] {
            if detail {
                ui.mcp_select(None);
            }
            let (width, height) = (124, 54);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
            terminal.draw(|frame| ui.draw_mcp_overlay(frame))?;
            let mut svg = format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}"><rect width="100%" height="100%" fill="#242731"/><g font-family="Menlo,monospace" font-size="16">"##,
                width * 10,
                height * 22
            );
            for y in 0..height {
                for x in 0..width {
                    if let Some(cell) = terminal.backend().buffer().cell((x, y)) {
                        if cell.symbol().trim().is_empty() {
                            continue;
                        }
                        svg.push_str(&format!(
                            r#"<text x="{}" y="{}" fill="{}" font-weight="{}">{}</text>"#,
                            x * 10,
                            y * 22 + 17,
                            color(cell.fg, "#dddddf"),
                            if cell.modifier.contains(ratatui::style::Modifier::BOLD) {
                                "bold"
                            } else {
                                "normal"
                            },
                            escape(cell.symbol())
                        ));
                    }
                }
            }
            svg.push_str("</g></svg>");
            std::fs::write(output.join(format!("{name}.svg")), svg)?;
        }
        Ok(())
    }
    #[test]
    fn stale_results_cannot_update_reopened_or_different_session_panels() -> Result<()> {
        let mut state = State::new("new".into(), provider::Provider::Claude);
        state.request = 42;
        state.loading = true;
        state.finish("old", 42, Ok(Reply::Mcp(snapshot()?)));
        state.finish("new", 41, Ok(Reply::Mcp(snapshot()?)));
        assert!(state.data.servers.is_empty() && state.loading);
        state.finish("new", 42, Ok(Reply::Mcp(snapshot()?)));
        assert_eq!(state.data.servers.len(), 1);
        assert!(!state.loading);
        Ok(())
    }
    #[test]
    fn panel_renders_and_navigates_servers_actions_and_tools() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = Ui::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().join("cache"),
            },
            &Config::default(),
        );
        let mut state = State::new("one".into(), provider::Provider::Claude);
        state.data = snapshot()?;
        ui.modal = Some(Modal::Mcp(Box::new(state)));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let render = |ui: &mut Ui, width, height| -> Result<String> {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
            terminal.draw(|frame| ui.draw_mcp(frame, frame.area()))?;
            let buffer = terminal.backend().buffer();
            Ok((0..height)
                .map(|y| {
                    (0..width)
                        .filter_map(|x| buffer.cell((x, y)))
                        .map(|c| c.symbol())
                        .collect::<String>()
                        .trim_end()
                        .to_owned()
                })
                .collect::<Vec<_>>()
                .join("\n"))
        };
        let list = render(&mut ui, 80, 24)?;
        assert!(list.contains("Manage MCP servers"));
        assert!(list.contains("Local MCPs"));
        assert!(list.contains("✓ fixture") && list.contains("1 tool"));
        assert!(!list.contains("·  connected"));
        ui.mcp_key(enter);
        let detail = render(&mut ui, 80, 24)?;
        assert!(detail.contains("fixture MCP Server"));
        assert!(detail.contains("Status:") && detail.contains("Config location:"));
        assert!(detail.contains("1. View tools") && detail.contains("2. Clear authentication"));
        assert!(detail.contains("3. Reconnect") && detail.contains("4. Disable"));
        ui.mcp_key(enter);
        assert!(render(&mut ui, 80, 24)?.contains("hello"));
        ui.mcp_key(enter);
        if let Some(Modal::Mcp(state)) = &mut ui.modal {
            // Reordering a refreshed catalog must not switch the tool being inspected.
            state
                .data
                .servers
                .first_mut()
                .context("server")?
                .tools
                .insert(
                    0,
                    mcp::Tool {
                        name: "earlier".into(),
                        description: "wrong tool".into(),
                        schema: None,
                    },
                );
        }
        let tool = render(&mut ui, 80, 24)?;
        assert!(tool.contains("hello") && !tool.contains("wrong tool"));
        for (width, height) in [(40, 12), (20, 6), (1, 1)] {
            render(&mut ui, width, height)?;
        }
        for _ in 0..4 {
            ui.mcp_key(escape);
        }
        assert!(ui.modal.is_none());
        eprintln!("MCP list:\n{list}\nMCP detail:\n{detail}");
        Ok(())
    }
    #[test]
    fn grouped_servers_keep_their_identity_across_refresh_and_back_navigation() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ui = Ui::new(
            Storage {
                config: dir.path().join("config.json"),
                cache: dir.path().join("cache"),
            },
            &Config::default(),
        );
        let data = Snapshot {
            servers: mcp::claude(&json!({"mcpServers":[
                {"name":"claude.ai Calendar","source":"claudeai","status":"connected"},
                {"name":"z-local","source":"local","status":"connected"},
                {"name":"a-local","source":"local","status":"disabled"},
                {"name":"plugin:tools","source":"plugin","status":"connected"}
            ]}))?,
            ..Snapshot::default()
        };
        let mut state = State::new("one".into(), provider::Provider::Claude);
        state.data = data.clone();
        assert_eq!(state.server().context("first")?.name, "a-local");
        state.selected = 2;
        assert_eq!(
            state.server().context("selected")?.name,
            "claude.ai Calendar"
        );
        state.finish("one", 0, Ok(Reply::Mcp(data)));
        assert_eq!(
            state.server().context("refreshed")?.name,
            "claude.ai Calendar"
        );
        ui.modal = Some(Modal::Mcp(Box::new(state)));
        ui.mcp_select(None);
        ui.mcp_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let Some(Modal::Mcp(state)) = &ui.modal else {
            anyhow::bail!("panel")
        };
        assert_eq!(state.server().context("back")?.name, "claude.ai Calendar");
        Ok(())
    }
    #[test]
    fn successful_non_auth_action_discards_the_old_auth_link() -> Result<()> {
        let mut state = State::new("one".into(), provider::Provider::Claude);
        state.data = snapshot()?;
        state.data.authorization_url = Some("https://auth.example.com/old".into());
        state.data.authentication_server = Some("fixture".into());
        state.finish("one", 0, Ok(Reply::Mcp(snapshot()?)));
        assert!(state.data.authorization_url.is_some());
        let mut cleared = snapshot()?;
        cleared.message = Some("Authentication cleared".into());
        state.finish("one", 0, Ok(Reply::Mcp(cleared)));
        assert!(state.data.authorization_url.is_none());
        Ok(())
    }
    #[test]
    fn errors_preserve_inventory_and_disable_does_not_remove_server() -> Result<()> {
        let mut state = State::new("one".into(), provider::Provider::Claude);
        state.data = snapshot()?;
        state.navigate(Page::Server("fixture".into()));
        state.finish("one", 0, Err("Reconnect failed".into()));
        assert_eq!(state.error.as_deref(), Some("Reconnect failed"));
        assert_eq!(state.server().context("server")?.status, "connected");
        let mut updated = snapshot()?;
        updated.servers.first_mut().context("server")?.status = "disabled".into();
        state.finish("one", 0, Ok(Reply::Mcp(updated)));
        assert!(
            state
                .actions()
                .iter()
                .any(|(_, a)| matches!(a, Some(mcp::Action::Toggle { enabled: true, .. })))
        );
        assert!(
            !state
                .actions()
                .iter()
                .any(|(_, a)| matches!(a, Some(mcp::Action::Reconnect { .. })))
        );
        assert_eq!(state.data.servers.len(), 1);
        Ok(())
    }
}
