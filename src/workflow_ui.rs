use crate::{
    app::{Action, App, Modal},
    ui::{self, ACCENT, BG, DIM, GREEN, PANEL, TEXT},
    workflow::{Kind, WAction, Wizard},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Clear, Paragraph},
};
fn action(label: impl Into<String>, action: WAction) -> ui::TextRow {
    ui::link(label, Action::Workflow(action))
}
pub fn draw(frame: &mut Frame, app: &mut App) {
    let Some(Modal::Workflow(modal)) = app.modal.take() else {
        return;
    };
    let wizard = *modal;
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(100);
    let height = area.height.saturating_sub(4).min(32);
    let rect = Rect::new(
        (area.width - width) / 2,
        (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(ACCENT))
            .style(Style::default().bg(PANEL).fg(TEXT)),
        rect,
    );
    app.hits.clear();
    let inner = Rect::new(
        rect.x + 2,
        rect.y + 1,
        rect.width.saturating_sub(4),
        rect.height.saturating_sub(2),
    );
    let mut rows = Vec::new();
    match &wizard {
        Wizard::Home(selected) => {
            rows.push(ui::bold("Actions", ACCENT));
            rows.push(ui::text("↑↓ select · Enter open · Esc close", DIM));
            rows.push(ui::text("", DIM));
            for (i, label) in ["PR controls", "Memory management · worktrees"]
                .iter()
                .enumerate()
            {
                rows.push(action(
                    format!("{} {label}", if i == *selected { ">" } else { " " }),
                    WAction::Choose(i),
                ));
            }
        }
        Wizard::Controls { key, selected, .. } => {
            rows.push(ui::bold(format!("PR controls · {}", key.id()), ACCENT));
            rows.push(ui::text("↑↓ select · Enter open · Esc close", DIM));
            rows.push(ui::text("", DIM));
            for (i, label) in [
                "Review PR",
                "Merge",
                "Squash merge",
                "Merge with admin override",
                "Squash merge with admin override",
                "Close PR (optional comment)",
            ]
            .iter()
            .enumerate()
            {
                rows.push(action(
                    format!("{} {label}", if i == *selected { ">" } else { " " }),
                    WAction::Choose(i),
                ));
            }
            if let Some(r) = app.review() {
                rows.push(ui::text(
                    if r.interaction.github.pending.is_some() {
                        "Your existing pending review will be reused."
                    } else {
                        "Line comments can start a pending review."
                    },
                    DIM,
                ));
            }
        }
        Wizard::Compose(draft) => {
            rows.push(ui::bold(
                format!(
                    "{} · {}",
                    match draft.kind {
                        Kind::Review => "Review PR",
                        Kind::Comment(_) => "Line comment",
                        Kind::Close => "Close PR",
                    },
                    draft.key.id()
                ),
                ACCENT,
            ));
            if let Kind::Comment(anchor) = &draft.kind {
                rows.extend(ui::prose(
                    &format!(
                        "{} · {} {}–{}",
                        anchor.path,
                        anchor.side.api(),
                        anchor.start,
                        anchor.end
                    ),
                    inner.width as usize,
                ));
            }
            rows.push(ui::text(
                "Tab focus/complete @mention · Ctrl+Enter next · F5 mentions · Esc back",
                DIM,
            ));
            let top = rows.len() as u16;
            let editor_height = inner.height.saturating_sub(top + 11).max(1);
            let editor_rect = Rect::new(
                inner.x,
                inner.y + top,
                inner.width,
                editor_height.min(inner.height.saturating_sub(top)),
            );
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" Message ")
                .border_style(Style::default().fg(if draft.focus == 0 { ACCENT } else { DIM }));
            let input = block.inner(editor_rect);
            frame.render_widget(block, editor_rect);
            app.hits
                .push((editor_rect, Action::Workflow(WAction::FocusEditor)));
            let (lines, (x, y)) = draft.editor.layout(input.width as usize);
            let start = y.saturating_sub(input.height.saturating_sub(1) as usize);
            for (i, line) in lines
                .iter()
                .skip(start)
                .take(input.height as usize)
                .enumerate()
            {
                frame.render_widget(
                    Paragraph::new(line.as_str()).style(Style::default().fg(TEXT)),
                    Rect::new(input.x, input.y + i as u16, input.width, 1),
                );
            }
            if draft.focus == 0 && input.width > 0 && input.height > 0 {
                frame.set_cursor_position((
                    input.x + (x as u16).min(input.width - 1),
                    input.y + (y - start) as u16,
                ));
            }
            for _ in 0..editor_rect.height {
                rows.push(ui::text("", DIM));
            }
            for (i, label) in draft.choices().iter().enumerate() {
                rows.push(action(
                    format!(
                        "{} {} {label}",
                        if draft.focus == 1 && draft.choice == i {
                            ">"
                        } else {
                            " "
                        },
                        if draft.choice == i { "[x]" } else { "[ ]" }
                    ),
                    WAction::Choose(i),
                ));
            }
            rows.push(ui::text("", DIM));
            rows.push(action(
                format!(
                    "{} [ Next: confirm ]",
                    if draft.focus == 2 { ">" } else { " " }
                ),
                WAction::Next,
            ));
            rows.push(ui::text("", DIM));
            let options = app.mention_options(draft);
            if draft.focus == 0 && !options.is_empty() {
                let room = inner.height.saturating_sub(rows.len() as u16) as usize;
                for (i, login) in options
                    .iter()
                    .enumerate()
                    .skip(draft.mention.saturating_sub(room.saturating_sub(1)))
                    .take(room)
                {
                    rows.push(action(
                        format!("{} @{login}", if i == draft.mention { ">" } else { " " }),
                        WAction::Complete(login.clone()),
                    ));
                }
            } else {
                rows.push(action(
                    if app.workflow.mentions_loading.contains(&draft.key.id()) {
                        "Refreshing mention suggestions…"
                    } else {
                        "[ Refresh @mention suggestions · F5 ]"
                    },
                    WAction::RefreshMentions,
                ));
            }
        }
        Wizard::Confirm {
            key,
            head,
            operation,
            ..
        } => {
            rows.push(ui::bold("Confirm GitHub action", ACCENT));
            rows.extend(ui::prose(
                &format!("{}\n{}\nRevision: {head}", key.id(), operation.label()),
                inner.width as usize,
            ));
            match operation {
                crate::review::Operation::Review { body, .. }
                | crate::review::Operation::Comment { body, .. }
                | crate::review::Operation::Close { body } => {
                    rows.extend(ui::prose(body, inner.width as usize));
                }
                _ => {}
            }
            // Keep the explicit submit controls visible even for a long review message.
            rows.truncate(inner.height.saturating_sub(4) as usize);
            rows.push(ui::text(
                "GitHub's head must still match this revision.",
                DIM,
            ));
            rows.push(action("[ Confirm · Enter ]", WAction::Submit));
            rows.push(action("[ Back · Esc ]", WAction::Back));
        }
        Wizard::Result { notice } => {
            rows.push(ui::bold("Result", ACCENT));
            for mut row in ui::prose(&notice.message, inner.width as usize) {
                for span in &mut row.spans {
                    span.style = span.style.fg(ui::notice_color(notice.kind));
                }
                rows.push(row);
            }
            rows.push(action("[ Close · Enter / Esc ]", WAction::Back));
        }
        Wizard::Trees {
            entries,
            selected,
            loading,
        } => {
            rows.push(ui::bold(
                format!("Memory management · {} worktrees", entries.len()),
                ACCENT,
            ));
            rows.push(ui::text(
                format!(
                    "{} stale and eligible · {} protected",
                    entries.iter().filter(|e| e.reason.is_none()).count(),
                    entries.iter().filter(|e| e.reason.is_some()).count()
                ),
                DIM,
            ));
            rows.push(action("[ Delete selected · Enter ]", WAction::DeleteOne));
            rows.push(action("[ Delete all stale · A ]", WAction::DeleteStale));
            rows.push(action("[ Refresh · F5 ]", WAction::Trees));
            if *loading {
                rows.push(ui::text("Inspecting worktrees…", DIM));
            }
            let count = inner.height.saturating_sub(7) as usize / 3;
            for (i, entry) in entries
                .iter()
                .enumerate()
                .skip(selected.saturating_sub(count.saturating_sub(1)))
                .take(count)
            {
                rows.push(action(
                    format!(
                        "{} {}",
                        if *selected == i { ">" } else { " " },
                        entry.path.display()
                    ),
                    WAction::Choose(i),
                ));
                rows.push(ui::text(
                    entry
                        .reason
                        .as_deref()
                        .unwrap_or("Inactive, clean and unlocked · safe to delete"),
                    if entry.reason.is_some() { DIM } else { GREEN },
                ));
                rows.push(ui::text("", DIM));
            }
        }
        Wizard::Delete { directories } => {
            rows.push(ui::bold(
                format!("Delete {} inactive worktrees?", directories.len()),
                ACCENT,
            ));
            rows.push(ui::text(
                "Ownership, activity, locks and changes are checked again.",
                DIM,
            ));
            for path in directories
                .iter()
                .take(inner.height.saturating_sub(6) as usize)
            {
                rows.push(ui::text(path.display().to_string(), TEXT));
            }
            rows.push(action("[ Delete · Enter ]", WAction::ConfirmDelete));
            rows.push(action("[ Cancel · Esc ]", WAction::Back));
        }
    }
    // Text box cells are painted separately; skip their blank placeholder rows.
    for (i, row) in rows.iter().take(inner.height as usize).enumerate() {
        if row.spans.iter().all(|s| s.content.is_empty()) {
            continue;
        }
        ui::paint(
            frame,
            Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
            row,
            app,
        );
    }
    if app.workflow.busy {
        app.hits.clear();
        frame.render_widget(
            Paragraph::new("Working… waiting for confirmation")
                .style(Style::default().fg(ACCENT).bg(BG)),
            Rect::new(inner.x, rect.bottom().saturating_sub(2), inner.width, 1),
        );
    }
    app.modal = Some(Modal::Workflow(Box::new(wizard)));
}
