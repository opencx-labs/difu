//! Explicit native Codex choices after a completed plan.
use super::codex_plan_picker::{self, LABELS};
use super::*;
use crate::agents::plan::{Choice, ClaudeChoice};

#[derive(Default)]
pub(super) struct State {
    dismissed: HashMap<String, String>,
    sending: HashMap<String, Submission>,
    errors: HashMap<String, (String, String)>,
    selections: HashMap<String, (String, usize)>,
    feedbacks: HashMap<String, (String, String)>,
}
struct Submission {
    plan_id: String,
    acknowledged: bool,
}
pub struct Prompt {
    id: String,
    plan_id: String,
    selected: usize,
    error: Option<String>,
    feedback: Editor,
    feedback_active: bool,
    pub(super) scroll: usize,
    pub(super) maximum: usize,
    text: String,
    request: Option<Value>,
    auto_available: bool,
}
impl Prompt {
    pub(super) fn editor(&self) -> Option<&Editor> {
        self.feedback_active.then_some(&self.feedback)
    }
    pub(super) fn paste(&mut self, text: &str) {
        if self.feedback_active {
            self.feedback.paste(text);
        }
    }
}
struct Offer {
    id: String,
    text: String,
    request: Option<Value>,
    auto_available: bool,
}
fn choice(index: usize) -> Option<Choice> {
    codex_plan_picker::items(true, Some("plan"), None)
        .get(index)
        .map(|item| item.choice)
}
impl Ui {
    fn plan_offer(&self, id: &str) -> Option<Offer> {
        let session = self.sessions.get(id)?;
        if session.archived || session.workspace_removed || self.deleting.contains(id) {
            return None;
        }
        if session.provider == provider::Provider::Codex {
            session.plan_prompt.as_ref().map(|p| Offer {
                id: p.id.clone(),
                text: p.text.clone(),
                request: None,
                auto_available: false,
            })
        } else {
            session.pending.iter().find(|p|!p.responded && p.params.get("difuClaudePlan").is_some()).map(|p|Offer {
                id:format!("claude-plan:{}",p.id),
                text:p.params.pointer("/difuClaudePlan/text").and_then(Value::as_str).unwrap_or("Claude did not provide the plan text. Ask it to provide the plan again.").into(),
                request:Some(p.id.clone()),
                auto_available:p.params.pointer("/difuClaudePlan/auto_available").and_then(Value::as_bool)==Some(true),
            })
        }
    }
    pub(super) fn plan_available(&self) -> bool {
        self.selected
            .as_ref()
            .and_then(|id| self.plan_offer(id))
            .is_some()
    }
    pub(super) fn open_plan_prompt(&mut self) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(proposal) = self.plan_offer(&id) else {
            return;
        };
        let error = self
            .plans
            .errors
            .get(&id)
            .filter(|(plan_id, _)| plan_id == &proposal.id)
            .map(|(_, error)| error.clone());
        let selected = self
            .plans
            .selections
            .get(&id)
            .filter(|(plan_id, _)| plan_id == &proposal.id)
            .map_or(0, |(_, selected)| *selected);
        let mut feedback = Editor::default();
        if let Some((_, text)) = self
            .plans
            .feedbacks
            .get(&id)
            .filter(|(plan, _)| plan == &proposal.id)
        {
            feedback.insert(text);
        }
        self.modal = Some(Modal::Plan(Box::new(Prompt {
            id,
            plan_id: proposal.id.clone(),
            selected,
            error,
            feedback,
            feedback_active: false,
            scroll: 0,
            maximum: 0,
            text: proposal.text,
            request: proposal.request,
            auto_available: proposal.auto_available,
        })));
    }
    pub(super) fn tick_plan_prompt(&mut self, visible: bool) {
        // A claim clears the offer before the native switch finishes. Keep the
        // submitted choice visible until its control response is acknowledged.
        let offers: HashMap<_, _> = self
            .sessions
            .keys()
            .filter_map(|id| self.plan_offer(id).map(|p| (id.clone(), p.id)))
            .collect();
        self.plans.sending.retain(|id, submitted| {
            !submitted.acknowledged || offers.get(id) == Some(&submitted.plan_id)
        });
        if let Some(Modal::Plan(prompt)) = &self.modal {
            let current = self.plan_offer(&prompt.id);
            let submitting = self
                .plans
                .sending
                .get(&prompt.id)
                .is_some_and(|s| s.plan_id == prompt.plan_id);
            if self.selected.as_deref() != Some(prompt.id.as_str())
                || current.as_ref().is_some_and(|p| p.id != prompt.plan_id)
                || (current.is_none() && !submitting && prompt.error.is_none())
            {
                self.modal = None;
            }
        }
        if !visible || !self.drilled || self.modal.is_some() || self.busy || self.voice_active() {
            return;
        }
        let Some(id) = self.selected.as_ref() else {
            return;
        };
        let Some(session) = self.sessions.get(id).filter(|s| {
            !s.archived
                && !s.workspace_removed
                && !self.deleting.contains(id)
                && s.queue.is_empty()
                && ((s.provider == provider::Provider::Codex
                    && s.status == Status::Idle
                    && s.pending.is_empty())
                    || (s.provider == provider::Provider::Claude
                        && s.status == Status::Waiting
                        && s.pending
                            .iter()
                            .all(|p| p.params.get("difuClaudePlan").is_some())))
        }) else {
            return;
        };
        let Some(proposal) = self.plan_offer(&session.id) else {
            return;
        };
        if self.plans.dismissed.get(id) == Some(&proposal.id) || self.plans.sending.contains_key(id)
        {
            return;
        }
        if self.positions.get(id).is_some_and(|p| {
            !p.draft.text().trim().is_empty()
                || !p.active_attachments().is_empty()
                || !p.outgoing.is_empty()
        }) {
            return;
        }
        if self
            .send_queues
            .get(id)
            .is_some_and(|queue| !queue.is_empty())
        {
            return;
        }
        self.open_plan_prompt();
    }
    pub(super) fn choose_plan(&mut self, index: usize) {
        let Some(choice) = choice(index) else {
            return;
        };
        let current = self.selected.as_ref().and_then(|id| self.plan_offer(id));
        let Some(Modal::Plan(prompt)) = &mut self.modal else {
            return;
        };
        if self.plans.sending.contains_key(&prompt.id) {
            return;
        }
        prompt.selected = index;
        self.plans
            .selections
            .insert(prompt.id.clone(), (prompt.plan_id.clone(), index));
        if self.selected.as_deref() != Some(prompt.id.as_str())
            || current.as_ref().map(|p| p.id.as_str()) != Some(prompt.plan_id.as_str())
        {
            prompt.error =
                Some("This plan changed. Close this prompt and review the current plan.".into());
            return;
        }
        let Some(offer) = current else {
            return;
        };
        let control = if let Some(request) = offer.request {
            let choice = match index {
                0 if offer.auto_available => ClaudeChoice::Auto,
                0 => {
                    prompt.error =
                        Some("Auto mode is not available for this Claude session.".into());
                    return;
                }
                1 => ClaudeChoice::Manual,
                2 => {
                    if !prompt.feedback_active {
                        prompt.feedback_active = true;
                        return;
                    }
                    let text = prompt.feedback.text();
                    if text.trim().is_empty() {
                        prompt.error = Some("Tell Claude what to change first.".into());
                        return;
                    }
                    ClaudeChoice::Feedback { text }
                }
                _ => return,
            };
            Control::DecideClaudePlan { request, choice }
        } else {
            Control::DecidePlan {
                plan_id: prompt.plan_id.clone(),
                choice,
            }
        };
        prompt.error = None;
        let (id, plan_id) = (prompt.id.clone(), prompt.plan_id.clone());
        self.plans.errors.remove(&id);
        self.plans.sending.insert(
            id.clone(),
            Submission {
                plan_id: plan_id.clone(),
                acknowledged: false,
            },
        );
        self.task(
            Task::DecidePlan(id.clone(), plan_id.clone()),
            Request::Control { id, control },
            false,
        );
    }
    pub(super) fn finish_plan_choice(
        &mut self,
        id: &str,
        plan_id: &str,
        result: std::result::Result<Reply, String>,
    ) {
        if !self
            .plans
            .sending
            .get(id)
            .is_some_and(|s| s.plan_id == plan_id)
        {
            return;
        }
        let error = match result {
            Ok(Reply::Ok) => None,
            Ok(Reply::Error(error)) | Err(error) => Some(error),
            Ok(_) => Some("Unexpected plan response. Review the plan and try again.".into()),
        };
        if let Some(error) = error {
            self.plans.sending.remove(id);
            self.plans
                .errors
                .insert(id.into(), (plan_id.into(), error.clone()));
            if let Some(Modal::Plan(prompt)) = &mut self.modal
                && prompt.id == id
                && prompt.plan_id == plan_id
            {
                prompt.error = Some(error);
            }
        } else if let Some(submitted) = self.plans.sending.get_mut(id) {
            submitted.acknowledged = true;
        }
        self.task(
            Task::Read(id.into()),
            Request::Read {
                id: id.into(),
                version: None,
            },
            false,
        );
    }
    pub(super) fn plan_key(&mut self, key: KeyEvent) {
        let Some(Modal::Plan(prompt)) = &mut self.modal else {
            return;
        };
        if key.code != KeyCode::Esc && self.plans.sending.contains_key(&prompt.id) {
            return;
        }
        if prompt.feedback_active {
            match key.code {
                KeyCode::Esc => prompt.feedback_active = false,
                KeyCode::Enter
                    if key.kind == crossterm::event::KeyEventKind::Press
                        && !key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    self.choose_plan(2)
                }
                _ => prompt.feedback.key(key),
            }
            return;
        }
        match key.code {
            KeyCode::PageUp => prompt.scroll = prompt.scroll.saturating_sub(10),
            KeyCode::PageDown => {
                prompt.scroll = prompt.scroll.saturating_add(10).min(prompt.maximum)
            }
            KeyCode::Home => prompt.scroll = 0,
            KeyCode::End => prompt.scroll = prompt.maximum,
            KeyCode::Esc => {
                self.plans.feedbacks.insert(
                    prompt.id.clone(),
                    (prompt.plan_id.clone(), prompt.feedback.text()),
                );
                self.plans
                    .selections
                    .insert(prompt.id.clone(), (prompt.plan_id.clone(), prompt.selected));
                self.plans
                    .dismissed
                    .insert(prompt.id.clone(), prompt.plan_id.clone());
                self.modal = None;
            }
            KeyCode::Up | KeyCode::BackTab => prompt.selected = prompt.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => {
                prompt.selected = (prompt.selected + 1).min(LABELS.len() - 1)
            }
            KeyCode::Enter if key.kind == crossterm::event::KeyEventKind::Press => {
                let selected = prompt.selected;
                self.choose_plan(selected);
            }
            _ => {}
        }
    }
    pub(super) fn draw_plan_affordance(&mut self, frame: &mut Frame, area: Rect) {
        self.button(
            frame,
            area,
            "Plan ready · Review implementation choices",
            Action::OpenPlan,
            false,
        );
    }
    pub(super) fn draw_plan_prompt(&mut self, frame: &mut Frame, area: Rect) {
        self.hits
            .retain(|(_, action)| !matches!(action, Action::PlanChoice(_)));
        let Some(Modal::Plan(prompt)) = &self.modal else {
            return;
        };
        let offer = Offer {
            id: prompt.plan_id.clone(),
            text: prompt.text.clone(),
            request: prompt.request.clone(),
            auto_available: prompt.auto_available,
        };
        let claude = offer.request.is_some();
        let selected = prompt.selected;
        let feedback = prompt.feedback_active;
        let error = prompt.error.clone();
        let submitting = self.plans.sending.contains_key(&prompt.id);
        let items = codex_plan_picker::items(true, Some(&offer.text), None);
        let labels = if claude {
            [
                "Yes, and use auto mode",
                "Yes, manually approve edits",
                "Tell Claude what to change",
            ]
        } else {
            std::array::from_fn(|i| items.get(i).map_or("", |item| item.name))
        };
        let title = if claude {
            "Claude has written up a plan and is ready to execute. Would you like to proceed?"
        } else {
            codex_plan_picker::TITLE
        };
        let title_rows = wrapped(title, area.width.saturating_sub(2));
        let dock_height = (title_rows.len() as u16
            + 7
            + if feedback { 3 } else { 0 }
            + u16::from(error.is_some()))
        .min(area.height.saturating_sub(3));
        let dock = Rect::new(
            area.x,
            area.bottom().saturating_sub(dock_height),
            area.width,
            dock_height,
        );
        let body = Rect::new(
            area.x,
            area.y,
            area.width,
            dock.y.saturating_sub(area.y + 1),
        );
        let heading = if claude {
            "Ready to code?"
        } else {
            "Proposed Plan"
        };
        frame.render_widget(
            Paragraph::new(heading).style(
                Style::default()
                    .fg(if claude { ACCENT } else { TEXT })
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Rect::new(
                body.x + body.width.min(1),
                body.y,
                body.width.saturating_sub(2),
                1.min(body.height),
            ),
        );
        let top = if claude { 4 } else { 2 };
        if claude && body.height > 2 {
            frame.render_widget(
                Paragraph::new("Here's Claude's plan:").style(Style::default().fg(TEXT)),
                Rect::new(body.x + 1, body.y + 2, body.width.saturating_sub(2), 1),
            );
        }
        let content = Rect::new(
            body.x,
            body.y + top.min(body.height),
            body.width,
            body.height.saturating_sub(top),
        );
        let (rows, links) = transcript::prose_links(&offer.text, content.width.saturating_sub(2));
        let maximum = rows.len().saturating_sub(usize::from(content.height));
        let scroll = if let Some(Modal::Plan(prompt)) = &mut self.modal {
            prompt.maximum = maximum;
            prompt.scroll = prompt.scroll.min(maximum);
            prompt.scroll
        } else {
            0
        };
        let block = Rect::new(
            content.x,
            content.y,
            content.width,
            content.height.min(rows.len().saturating_sub(scroll) as u16),
        );
        let plan_background = if claude {
            BG
        } else {
            crate::ui::user_message_style().bg.unwrap_or(PANEL)
        };
        frame.render_widget(
            Block::default().style(Style::default().bg(plan_background)),
            block,
        );
        let text_area = Rect::new(
            block.x + block.width.min(1),
            block.y,
            block.width.saturating_sub(2),
            block.height,
        );
        let shown = rows
            .into_iter()
            .skip(scroll)
            .take(usize::from(block.height))
            .collect::<Vec<_>>();
        self.text_selection
            .register_window(1, text_area, scroll, scroll, &shown);
        frame.render_widget(
            Paragraph::new(shown).style(Style::default().fg(TEXT).bg(plan_background)),
            text_area,
        );
        self.hits
            .extend(transcript::link_hits(&links, text_area, scroll));
        frame.render_widget(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(BORDER)),
            dock,
        );
        let mut y = dock.y.saturating_add(1);
        for line in title_rows {
            if y >= dock.bottom() {
                break;
            }
            frame.render_widget(
                Paragraph::new(line).style(
                    Style::default()
                        .fg(TEXT)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Rect::new(dock.x + 1, y, dock.width.saturating_sub(2), 1),
            );
            y += 1;
        }
        y = y.saturating_add(1);
        let desc_col = labels
            .iter()
            .map(|label| unicode_width::UnicodeWidthStr::width(*label))
            .max()
            .unwrap_or(0)
            + 7;
        for (index, label) in labels.into_iter().enumerate() {
            if y >= dock.bottom().saturating_sub(1) {
                break;
            }
            let disabled = claude && index == 0 && !offer.auto_available;
            let prefix = format!(
                "{} {}. {label}",
                if selected == index { "›" } else { " " },
                index + 1
            );
            let detail = if claude {
                if disabled {
                    "Auto mode unavailable"
                } else {
                    ""
                }
            } else {
                items
                    .get(index)
                    .map_or("", |item| item.disabled_reason.unwrap_or(&item.description))
            };
            let prefix_width = unicode_width::UnicodeWidthStr::width(prefix.as_str());
            let padding = desc_col.saturating_sub(prefix_width);
            let text = if usize::from(dock.width) > desc_col + 12 {
                format!("{prefix}{}{detail}", " ".repeat(padding))
            } else {
                prefix
            };
            let row = Rect::new(dock.x, y, dock.width, 1);
            let style = if disabled {
                Style::default().fg(DIM)
            } else if selected == index && !claude {
                Style::default()
                    .fg(BG)
                    .bg(ACCENT)
                    .add_modifier(ratatui::style::Modifier::BOLD)
            } else if selected == index {
                Style::default()
                    .fg(ACCENT)
                    .add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                Style::default().fg(TEXT)
            };
            frame.render_widget(Paragraph::new(text).style(style), row);
            if !submitting && !disabled {
                self.hits.push((row, Action::PlanChoice(index)));
            }
            y += 1;
        }
        if feedback && let Some(Modal::Plan(prompt)) = &self.modal {
            let input = Rect::new(
                dock.x + 2,
                y,
                dock.width.saturating_sub(4),
                2.min(dock.bottom().saturating_sub(y + 1)),
            );
            let (lines, (col, row)) = prompt.feedback.layout(usize::from(input.width.max(1)));
            frame.render_widget(
                Paragraph::new(if prompt.feedback.text().is_empty() {
                    "Tell Claude what to change…".into()
                } else {
                    lines.join("\n")
                })
                .style(Style::default().fg(TEXT)),
                input,
            );
            if row < usize::from(input.height) {
                frame.set_cursor_position((
                    input.x + (col as u16).min(input.width.saturating_sub(1)),
                    input.y + row as u16,
                ));
            }
        }
        let footer = error.as_deref().unwrap_or(if submitting {
            "Applying choice…"
        } else if feedback {
            "enter send feedback · esc choices"
        } else {
            "enter select · esc back · PgUp/PgDn read plan"
        });
        frame.render_widget(
            Paragraph::new(footer).style(Style::default().fg(if error.is_some() {
                RED
            } else {
                DIM
            })),
            Rect::new(
                dock.x + 1,
                dock.bottom().saturating_sub(1),
                dock.width.saturating_sub(2),
                1.min(dock.height),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::plan::Proposal;
    use anyhow::{Context, Result};
    #[test]
    fn claude_picker_dispatches_only_explicit_native_choices() -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let (_dir, mut ui) = fixture()?;
        let listener = UnixListener::bind(crate::agents::server::socket(&ui.storage)?)?;
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || -> Result<()> {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept()?;
                let mut line = String::new();
                BufReader::new(stream.try_clone()?).read_line(&mut line)?;
                tx.send(serde_json::from_str::<Request>(&line)?)?;
                serde_json::to_writer(&mut stream, &Reply::Ok)?;
                stream.write_all(b"\n")?;
            }
            Ok(())
        });
        let s = ui.sessions.get_mut("one").context("session")?;
        s.provider = provider::Provider::Claude;
        s.status = Status::Waiting;
        s.plan_prompt = None;
        s.pending.push(Pending{id:serde_json::json!("native-exit"),method:"item/tool/requestUserInput".into(),params:serde_json::json!({"tool":"ExitPlanMode","difuClaudePlan":{"text":"# Approved plan","auto_available":true}}),responded:false});
        for (index, expected) in [
            (0, ClaudeChoice::Auto),
            (1, ClaudeChoice::Manual),
            (
                2,
                ClaudeChoice::Feedback {
                    text: "Keep the API".into(),
                },
            ),
        ] {
            ui.plans.sending.clear();
            ui.open_plan_prompt();
            ui.choose_plan(index);
            if index == 2 {
                assert!(ui.plans.sending.is_empty());
                ui.paste("Keep the API");
                ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            }
            assert!(
                matches!(rx.recv_timeout(Duration::from_secs(5))?,Request::Control{id,control:Control::DecideClaudePlan{request,choice}} if id=="one" && request==serde_json::json!("native-exit") && choice==expected)
            );
            ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            assert!(rx.try_recv().is_err());
        }
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("fixture stopped"))??;
        Ok(())
    }
    #[test]
    fn native_plan_review_is_docked_and_claude_feedback_is_explicit() -> Result<()> {
        crate::theme::select(crate::theme::index(Some("dracula")));
        let (_dir, mut ui) = fixture()?;
        for claude in [false, true] {
            let session = ui.sessions.get_mut("one").context("session")?;
            let text = "# A visible proposed plan\n\n## Context\nKeep the existing workspace.\n\n## Steps\n- Read the relevant files.\n- Make the approved change.\n- Verify the result.\n\n## Verification\nThe requested behavior works and unrelated files remain intact.";
            if claude {
                session.provider = provider::Provider::Claude;
                session.status = Status::Waiting;
                session.plan_prompt = None;
                session.pending.push(Pending{id:serde_json::json!("native-exit"),method:"item/tool/requestUserInput".into(),params:serde_json::json!({"tool":"ExitPlanMode","difuClaudePlan":{"text":text,"auto_available":true}}),responded:false});
            } else if let Some(plan) = session.plan_prompt.as_mut() {
                plan.text = text.into();
            }
            ui.modal = None;
            ui.open_plan_prompt();
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40))?;
            terminal.draw(|frame| ui.draw_plan_prompt(frame, frame.area()))?;
            let cells = terminal.backend().buffer();
            let rows = (0..40)
                .map(|y| {
                    (0..120)
                        .filter_map(|x| cells.cell((x, y)))
                        .map(|c| c.symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>();
            assert!(
                rows.iter()
                    .take(12)
                    .any(|r| r.contains("A visible proposed plan"))
            );
            let choices = ui
                .hits
                .iter()
                .filter(|(_, a)| matches!(a, Action::PlanChoice(_)))
                .collect::<Vec<_>>();
            assert_eq!(choices.len(), 3);
            assert!(
                choices
                    .iter()
                    .all(|(r, _)| r.x == 0 && r.width == 120 && r.y >= 32)
            );
            if claude {
                assert!(rows.iter().any(|r| r.contains("Yes, and use auto mode")));
                assert!(!rows.iter().any(|r| r.contains("clear context")));
                ui.choose_plan(2);
                assert!(ui.plans.sending.is_empty());
                ui.paste("Preserve the existing API");
                assert!(
                    matches!(&ui.modal,Some(Modal::Plan(p)) if p.feedback.text()=="Preserve the existing API")
                );
                ui.plan_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                assert!(matches!(&ui.modal,Some(Modal::Plan(p)) if !p.feedback_active));
            } else {
                assert!(
                    rows.iter()
                        .any(|r| r.contains("1. Yes, implement this plan")
                            && r.contains("Switch to Default"))
                );
                let selected = choices.first().context("selected row")?.0;
                assert_eq!(
                    cells.cell((119, selected.y)).context("full highlight")?.bg,
                    ACCENT
                );
            }
            if let Ok(path) = std::env::var("DIFU_PLAN_RENDER_DIR") {
                std::fs::create_dir_all(&path)?;
                let mut rendered = cells.clone();
                crate::theme::present(&mut rendered);
                let cells=rendered.content.iter().map(|c|serde_json::json!({"s":c.symbol(),"fg":format!("{:?}",c.fg),"bg":format!("{:?}",c.bg),"bold":c.modifier.contains(ratatui::style::Modifier::BOLD)})).collect::<Vec<_>>();
                std::fs::write(
                    std::path::Path::new(&path).join(if claude {
                        "claude.json"
                    } else {
                        "codex.json"
                    }),
                    serde_json::to_vec(
                        &serde_json::json!({"width":120,"height":40,"cells":cells}),
                    )?,
                )?;
            }
            ui.summaries = ui.sessions.values().map(Session::summary).collect();
            terminal.draw(|frame| ui.draw(frame))?;
            let dock_hits = ui
                .hits
                .iter()
                .filter(|(_, action)| matches!(action, Action::PlanChoice(_)))
                .collect::<Vec<_>>();
            assert_eq!(dock_hits.len(), 3);
            assert!(
                dock_hits
                    .iter()
                    .all(|(rect, _)| rect.y >= 28 && rect.width > 50),
                "The full app must keep clickable choices in the conversation bottom pane"
            );
        }
        Ok(())
    }
    fn fixture() -> Result<(tempfile::TempDir, Ui)> {
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
                prompt: "Plan the change".into(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        session.provider = provider::Provider::Codex;
        session.permission_state.mode = Some("plan".into());
        session.plan_prompt = Some(Proposal {
            id: "plan-one".into(),
            thread_id: "thread-one".into(),
            turn_id: "turn-one".into(),
            text: "Build the approved implementation.".into(),
        });
        ui.sessions.insert("one".into(), session);
        ui.selected = Some("one".into());
        ui.positions.insert("one".into(), Position::default());
        ui.drilled = true;
        ui.focus = Focus::Composer;
        Ok((dir, ui))
    }
    #[test]
    fn saved_plan_opens_without_execution_and_reopens_after_escape() -> Result<()> {
        let (_dir, mut ui) = fixture()?;
        let session = ui.sessions.get_mut("one").context("session")?;
        session.plan_handled = Some("plan-one".into());
        session.completed_turn = Some("turn-one".into());
        session.note("userMessage", "Make a plan");
        let prompt_id = session.entries.last().context("prompt")?.id.clone();
        session.suggestion = Some(crate::agents::suggestions::Suggestion {
            turn: "turn-one".into(),
            prompt: prompt_id,
            text: "Generic generated follow-up".into(),
        });
        assert!(ui.composer_suggestion().is_none());
        ui.tick_plan_prompt(true);
        assert!(matches!(&ui.modal, Some(Modal::Plan(state)) if state.plan_id == "plan-one"));
        assert!(!ui.busy && ui.plans.sending.is_empty());
        assert!(ui.receiver.try_recv().is_err());
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 26))?;
        terminal.draw(|frame| ui.draw_plan_prompt(frame, frame.area()))?;
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for label in [
            "Implement this plan?",
            "Yes, implement this plan",
            "Yes, clear context and implement",
            "No, stay in Plan mode",
            "Fresh thread with this plan",
        ] {
            assert!(screen.contains(label), "{label}");
        }
        for (width, height) in [(60, 22), (40, 18), (40, 12)] {
            let mut small =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))?;
            small.draw(|frame| ui.draw_plan_prompt(frame, frame.area()))?;
            assert_eq!(
                ui.hits
                    .iter()
                    .filter(|(_, action)| matches!(action, Action::PlanChoice(_)))
                    .count(),
                3
            );
            assert!(
                ui.hits
                    .iter()
                    .all(|(rect, _)| rect.height > 0 && rect.bottom() <= height)
            );
        }
        ui.plan_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        ui.tick_plan_prompt(true);
        assert!(ui.modal.is_none());
        assert!(ui.plan_available());
        let reopen = ui
            .menu_entries()
            .iter()
            .position(|label| *label == "Review completed plan")
            .context("plan menu affordance")?;
        ui.menu_action(reopen);
        assert!(matches!(ui.modal, Some(Modal::Plan(_))));
        assert_eq!(ui.focus, Focus::Composer);
        assert!(ui.plans.sending.is_empty());
        Ok(())
    }
    #[test]
    fn auto_open_requires_an_idle_open_chat_without_voice_or_deleted_workspace() -> Result<()> {
        let (_dir, mut ui) = fixture()?;
        ui.drilled = false;
        ui.tick_plan_prompt(true);
        assert!(ui.modal.is_none());
        ui.drilled = true;
        for status in [Status::Failed, Status::Interrupted, Status::Running] {
            ui.sessions.get_mut("one").context("session")?.status = status;
            ui.tick_plan_prompt(true);
            assert!(ui.modal.is_none());
        }
        let session = ui.sessions.get_mut("one").context("session")?;
        session.status = Status::Idle;
        session.archived = true;
        ui.tick_plan_prompt(true);
        assert!(ui.modal.is_none() && !ui.plan_available());
        let session = ui.sessions.get_mut("one").context("session")?;
        session.archived = false;
        session.workspace_removed = true;
        ui.tick_plan_prompt(true);
        assert!(ui.modal.is_none() && !ui.plan_available());
        ui.sessions
            .get_mut("one")
            .context("session")?
            .workspace_removed = false;
        ui.deleting.insert("one".into());
        ui.tick_plan_prompt(true);
        assert!(ui.modal.is_none() && !ui.plan_available());
        ui.deleting.clear();
        ui.voice = voice::State::new(true);
        ui.voice_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(ui.voice_active());
        ui.tick_plan_prompt(true);
        assert!(ui.modal.is_none());
        ui.cancel_voice();
        ui.positions
            .get_mut("one")
            .context("position")?
            .draft
            .clear();
        ui.tick_plan_prompt(true);
        assert!(matches!(ui.modal, Some(Modal::Plan(_))));
        Ok(())
    }
    #[test]
    fn drafts_attachments_and_competing_modals_prevent_auto_open() -> Result<()> {
        let (_dir, mut ui) = fixture()?;
        ui.positions
            .get_mut("one")
            .context("position")?
            .draft
            .insert("Keep this draft");
        ui.tick_plan_prompt(true);
        assert!(ui.modal.is_none() && ui.plan_available());
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 28))?;
        terminal.draw(|frame| ui.draw(frame))?;
        let rect = ui
            .hits
            .iter()
            .find_map(|(rect, action)| matches!(action, Action::OpenPlan).then_some(*rect))
            .context("visible plan affordance")?;
        ui.mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + 2,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(ui.modal, Some(Modal::Plan(_))));
        ui.plan_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            ui.positions.get("one").context("position")?.draft.text(),
            "Keep this draft"
        );
        ui.positions
            .get_mut("one")
            .context("position")?
            .draft
            .clear();
        ui.plans.dismissed.clear();
        ui.positions
            .get_mut("one")
            .context("position")?
            .draft
            .insert_attachment("[image 1]");
        ui.positions
            .get_mut("one")
            .context("position")?
            .attachments
            .push(super::super::super::media::Attachment {
                label: "image 1".into(),
                path: "image.png".into(),
                kind: super::super::super::media::Kind::Image,
                hash: "image".into(),
            });
        ui.tick_plan_prompt(true);
        assert!(ui.modal.is_none());
        ui.positions
            .get_mut("one")
            .context("position")?
            .attachments
            .clear();
        ui.positions
            .get_mut("one")
            .context("position")?
            .draft
            .clear();
        ui.sessions
            .get_mut("one")
            .context("session")?
            .queue
            .push("Queued message".into());
        ui.tick_plan_prompt(true);
        assert!(ui.modal.is_none());
        ui.sessions.get_mut("one").context("session")?.queue.clear();
        ui.positions
            .get_mut("one")
            .context("position")?
            .outgoing
            .push(PendingSend {
                token: 1,
                text: "Sending message".into(),
                queued: false,
                observed_before: 0,
                failed: None,
            });
        ui.tick_plan_prompt(true);
        assert!(ui.modal.is_none());
        ui.positions
            .get_mut("one")
            .context("position")?
            .outgoing
            .clear();
        ui.modal = Some(Modal::Status { scroll: 0 });
        ui.tick_plan_prompt(true);
        assert!(matches!(ui.modal, Some(Modal::Status { .. })));
        Ok(())
    }
    #[test]
    fn explicit_keyboard_click_choices_dispatch_exact_controls_and_wait_for_confirmation()
    -> Result<()> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let (_dir, mut ui) = fixture()?;
        let listener = UnixListener::bind(super::super::super::server::socket(&ui.storage)?)?;
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || -> Result<()> {
            let mut controls = 0;
            while controls < 4 {
                let (mut stream, _) = listener.accept()?;
                let mut line = String::new();
                BufReader::new(stream.try_clone()?).read_line(&mut line)?;
                let request: Request = serde_json::from_str(&line)?;
                serde_json::to_writer(&mut stream, &Reply::Ok)?;
                stream.write_all(b"\n")?;
                if matches!(request, Request::Control { .. }) {
                    tx.send(request)?;
                    controls += 1;
                }
            }
            Ok(())
        });
        let attachment = super::super::super::media::Attachment {
            label: "image 1".into(),
            path: "image.png".into(),
            kind: super::super::super::media::Kind::Image,
            hash: "image".into(),
        };
        let position = ui.positions.get_mut("one").context("position")?;
        position.draft.insert("Keep this draft ");
        position.draft.insert_attachment("[image 1]");
        position.attachments.push(attachment.clone());
        position.draft.anchor = Some(0);
        position.draft.cursor = 4;
        let draft = position.draft.text();
        for (index, expected) in [
            (0, Choice::Implement),
            (1, Choice::FreshContext),
            (2, Choice::ContinuePlanning),
        ] {
            let plan_id = format!("plan-{index}");
            let session = ui.sessions.get_mut("one").context("session")?;
            session.plan_prompt = Some(Proposal {
                id: plan_id.clone(),
                thread_id: "thread-one".into(),
                turn_id: format!("turn-{index}"),
                text: "Approved plan".into(),
            });
            session.permission_state.mode = Some("plan".into());
            ui.open_plan_prompt();
            if index == 1 {
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 26))?;
                terminal.draw(|frame| ui.draw_plan_prompt(frame, frame.area()))?;
                let rect = ui
                    .hits
                    .iter()
                    .find_map(|(rect, action)| {
                        matches!(action, Action::PlanChoice(1)).then_some(*rect)
                    })
                    .context("fresh-context choice")?;
                ui.mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: rect.x + 2,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                });
            } else {
                for _ in 0..index {
                    ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
                }
                ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            }
            ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            ui.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            assert!(matches!(&ui.modal, Some(Modal::Plan(prompt)) if prompt.selected == index));
            let request = rx.recv_timeout(Duration::from_secs(5))?;
            assert!(
                matches!(request, Request::Control { id, control: Control::DecidePlan { plan_id: actual, choice } } if id == "one" && actual == plan_id && choice == expected)
            );
            assert!(rx.try_recv().is_err());
            assert_eq!(
                ui.sessions
                    .get("one")
                    .context("session")?
                    .permission_state
                    .mode
                    .as_deref(),
                Some("plan")
            );
            if index == 0 {
                ui.finish_plan_choice(
                    "one",
                    &plan_id,
                    Err("Native mode switch failed; try again".into()),
                );
                assert!(matches!(&ui.modal, Some(Modal::Plan(prompt)) if prompt.error.is_some()));
                ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                assert!(
                    matches!(rx.recv_timeout(Duration::from_secs(5))?, Request::Control { id, control: Control::DecidePlan { plan_id: actual, choice: Choice::Implement } } if id == "one" && actual == plan_id)
                );
            }
            // Provider claims the offer before its native switch is acknowledged.
            ui.sessions.get_mut("one").context("session")?.plan_prompt = None;
            ui.tick_plan_prompt(true);
            assert!(matches!(ui.modal, Some(Modal::Plan(_))));
            ui.finish_plan_choice("one", &plan_id, Ok(Reply::Ok));
            ui.tick_plan_prompt(true);
            assert!(ui.modal.is_none());
            let position = ui.positions.get("one").context("position")?;
            assert_eq!(position.draft.text(), draft);
            assert_eq!(position.draft.anchor, Some(0));
            assert_eq!(position.draft.cursor, 4);
            assert_eq!(
                position.attachments.as_slice(),
                std::slice::from_ref(&attachment)
            );
            assert!(position.outgoing.is_empty());
            assert_eq!(ui.selected.as_deref(), Some("one"));
            assert_eq!(ui.focus, Focus::Composer);
        }
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("fixture panicked"))??;
        Ok(())
    }
    #[test]
    fn other_sessions_do_not_steal_focus_and_stale_plan_is_not_sent() -> Result<()> {
        let (_dir, mut ui) = fixture()?;
        let mut other = ui.sessions.get("one").context("session")?.clone();
        other.id = "two".into();
        other.plan_prompt = None;
        ui.sessions.insert("two".into(), other);
        ui.selected = Some("two".into());
        ui.focus = Focus::List;
        ui.tick_plan_prompt(true);
        assert!(ui.modal.is_none());
        assert_eq!(ui.selected.as_deref(), Some("two"));
        ui.selected = Some("one".into());
        ui.sessions.get_mut("one").context("session")?.orchestrator = true;
        ui.tick_plan_prompt(true);
        assert!(matches!(ui.modal, Some(Modal::Plan(_))));
        ui.sessions
            .get_mut("one")
            .context("session")?
            .plan_prompt
            .as_mut()
            .context("proposal")?
            .id = "new-plan".into();
        ui.choose_plan(0);
        assert!(ui.plans.sending.is_empty());
        assert!(ui.receiver.try_recv().is_err());
        assert_eq!(ui.focus, Focus::List);
        Ok(())
    }
}
