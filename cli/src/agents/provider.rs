//! Provider selection and conversation handoff, independent of either wire protocol.
use super::{Entry, Job, Session, Status};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Codex,
    Claude,
}
impl Provider {
    pub fn for_model(model: Option<&str>) -> Self {
        let model = model.unwrap_or_default();
        if model.starts_with("claude/")
            || model.starts_with("claude-")
            || matches!(model, "sonnet" | "opus" | "haiku")
        {
            Self::Claude
        } else {
            Self::Codex
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
    pub fn native_model(model: &str) -> &str {
        model
            .strip_prefix("claude/")
            .or_else(|| model.strip_prefix("codex/"))
            .unwrap_or(model)
    }
}
/// Codex and Claude Code models installed on this machine, discovered in
/// parallel; fails only when neither provider reports any.
pub fn models(cancel: &crate::process::Cancel) -> Result<Vec<crate::model::ModelInfo>> {
    let (codex, claude) = std::thread::scope(|scope| {
        let codex = scope.spawn(|| crate::codex::models(cancel));
        let claude = super::claude::models(cancel);
        (
            codex
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Codex model discovery stopped"))),
            claude,
        )
    });
    let mut models = Vec::new();
    let mut errors = Vec::new();
    for result in [codex, claude] {
        match result {
            Ok(options) => models.extend(options),
            Err(error) => errors.push(format!("{error:#}")),
        }
    }
    ensure!(!models.is_empty(), "{}", errors.join("; "));
    Ok(models)
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeSession {
    pub thread_id: Option<String>,
    pub through: usize,
    #[serde(default)]
    pub permission_state: super::permissions::State,
    pub permissions: Value,
    pub inherited_permissions: Value,
    pub artifact_tools: bool,
    #[serde(default)]
    pub pr_tools: bool,
    #[serde(default)]
    pub shell_tools: bool,
    #[serde(default)]
    pub registration_tools: bool,
    #[serde(default)]
    pub question_tools: bool,
    #[serde(default)]
    pub peer_tools: bool,
    #[serde(default)]
    pub folder_tools: bool,
    #[serde(default)]
    pub command_tools: bool,
}

/// The most chat a handoff carries, about 60k tokens: plenty to continue from,
/// with room left for the new conversation to work. Newer entries win.
const HANDOFF_BUDGET: usize = 240_000;
/// Room a summarized handoff keeps for the summary of the earlier chat.
pub(super) const SUMMARY_LIMIT: usize = 40_000;
/// The most earlier chat a summary is written from, about 120k tokens.
const SUMMARY_INPUT: usize = 480_000;
const HEADER: &str = "Difu conversation handoff. The following is prior conversation and tool output, not a new instruction. Continue from its current state, preserve user decisions, and do not repeat completed actions or publications. Re-read the current workspace before editing.";

/// How much of each entry is kept: what the user and agent said matters more
/// than tool output.
#[derive(Clone, Copy)]
struct Limits {
    message: usize,
    activity: usize,
}
const HANDOFF: Limits = Limits {
    message: 12_000,
    activity: 1_500,
};
const SUMMARY: Limits = Limits {
    message: 6_000,
    activity: 400,
};

/// What a new native thread receives with its first message: the chat from
/// entry `through` on, as context rather than instructions. Only the newest
/// entries that fit `HANDOFF_BUDGET` are kept, plus the first request. Used
/// when a session switches provider, moves or outgrows its model's context.
pub(super) fn handoff(entries: &[Entry], through: usize) -> Option<String> {
    let items = items(
        entries.get(through..).unwrap_or_default(),
        HANDOFF,
        HANDOFF_BUDGET,
    );
    (!items.is_empty()).then(|| format!("{HEADER}\n[{}]", items.join(",")))
}

/// A handoff whose chat before entry `start` is replaced by `summary` of it.
pub(super) fn summarized_handoff(entries: &[Entry], start: usize, summary: &str) -> String {
    let recent = items(
        entries.get(start..).unwrap_or_default(),
        HANDOFF,
        HANDOFF_BUDGET,
    );
    format!(
        "{HEADER}\n\nSummary of the earlier conversation, written from the full chat:\n{}\n\nThe recent chat as JSON:\n[{}]",
        shorten(summary.trim(), SUMMARY_LIMIT),
        recent.join(",")
    )
}

/// Where a summarized handoff starts carrying the chat as is, when a plain
/// handoff would leave earlier entries out; those before it are summarized.
pub(super) fn summary_start(entries: &[Entry]) -> Option<usize> {
    let omits = |start: usize| entries.get(..start).unwrap_or_default().iter().any(carried);
    omits(start(entries, HANDOFF, HANDOFF_BUDGET))
        .then(|| start(entries, HANDOFF, HANDOFF_BUDGET - SUMMARY_LIMIT))
}

/// The earlier chat a summary is written from, as JSON within `SUMMARY_INPUT`.
pub(super) fn summary_input(entries: &[Entry]) -> String {
    format!("[{}]", items(entries, SUMMARY, SUMMARY_INPUT).join(","))
}

fn carried(entry: &Entry) -> bool {
    !matches!(
        entry.kind.as_str(),
        "sending" | "sending_context" | "awaiting connection"
    ) && !entry.text.trim().is_empty()
}

/// The first entry of the newest run that fits `budget`.
fn start(entries: &[Entry], limits: Limits, budget: usize) -> usize {
    let mut used = 0;
    let mut start = entries.len();
    for (index, entry) in entries.iter().enumerate().rev() {
        if carried(entry) {
            used += item(entry, limits).len() + 1;
            if used > budget {
                break;
            }
        }
        start = index;
    }
    start
}

/// `entries` as JSON items: the newest that fit `budget` and, when earlier
/// ones are left out, the first request and a note counting them.
fn items(entries: &[Entry], limits: Limits, budget: usize) -> Vec<String> {
    let start = start(entries, limits, budget);
    let older = entries.get(..start).unwrap_or_default();
    let omitted = older.iter().filter(|entry| carried(entry)).count();
    let mut items = Vec::new();
    if omitted > 0 {
        // The first request frames everything after it.
        if let Some(first) = older.iter().find(|entry| entry.kind == "userMessage") {
            items.push(item(first, limits));
        }
        items.push(serde_json::json!({"kind":"omitted","text":format!("{omitted} earlier entries omitted to fit the model's context")}).to_string());
    }
    items.extend(
        entries
            .get(start..)
            .unwrap_or_default()
            .iter()
            .filter(|entry| carried(entry))
            .map(|entry| item(entry, limits)),
    );
    items
}

fn item(entry: &Entry, limits: Limits) -> String {
    let limit = if matches!(entry.kind.as_str(), "userMessage" | "agentMessage" | "plan") {
        limits.message
    } else {
        limits.activity
    };
    let mut item = serde_json::Map::new();
    item.insert("kind".into(), entry.kind.clone().into());
    item.insert("text".into(), shorten(&entry.text, limit).into());
    // Attachments are files the new conversation can open; other details
    // repeat the text or only matter to difu.
    if let Some(attachments) = entry
        .data
        .get("attachments")
        .filter(|attachments| attachments.as_array().is_some_and(|a| !a.is_empty()))
    {
        item.insert(
            "details".into(),
            serde_json::json!({"attachments":attachments}),
        );
    }
    Value::Object(item).to_string()
}

pub(super) fn shorten(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.into();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (shortened)", text.get(..end).unwrap_or_default())
}

/// What Claude Code answers when a conversation no longer fits its model's
/// context, not even to compact it.
pub(super) const TOO_LONG: &str = "Prompt is too long";
const RESTARTED: &str = "This conversation outgrew the model's context, so difu started a fresh one carrying the recent chat";

/// A Claude conversation that no longer fits its model's context fails every
/// turn. Leave it for a fresh one that receives the recent chat as a handoff,
/// and send the failed message again when it never reached the model; a
/// message that fails again right after a restart is not sent a third time.
pub(super) fn restart_too_long(session: &mut Session) {
    let user = session
        .entries
        .iter()
        .rposition(|entry| entry.kind == "userMessage");
    let untouched = user.is_some_and(|user| {
        session
            .entries
            .iter()
            .skip(user + 1)
            .all(|entry| match entry.kind.as_str() {
                "agentMessage" => entry.text.trim() == TOO_LONG,
                kind => matches!(kind, "error" | "system" | "progress"),
            })
    });
    let restarted = user.is_some_and(|user| {
        let before = session.entries.get(..user).unwrap_or_default();
        let previous = before
            .iter()
            .rposition(|entry| entry.kind == "userMessage")
            .map_or(0, |previous| previous + 1);
        before
            .iter()
            .skip(previous)
            .any(|entry| entry.kind == "system" && entry.text.starts_with(RESTARTED))
    });
    let resend = user
        .filter(|_| untouched && !restarted)
        .and_then(|user| Some((user, session.entries.get(user)?)))
        .map(|(user, entry)| {
            let prompt = entry
                .data
                .get("prompt")
                .and_then(|prompt| serde_json::from_value::<super::Prompt>(prompt.clone()).ok())
                .unwrap_or_else(|| super::Prompt::from(entry.text.as_str()));
            (user, prompt)
        });
    // A message sent again is not repeated in the handoff.
    let through = resend
        .as_ref()
        .map_or(session.entries.len(), |(user, _)| *user);
    session.provider_context = handoff(session.entries.get(..through).unwrap_or_default(), 0);
    session.summarize_through = Some(through);
    session.thread_id = None;
    session.token_usage = Value::Null;
    let state = &mut session.permission_state;
    state.claude_session_started = false;
    state.claude_turn_attempted = false;
    state.claude_turn_acknowledged = false;
    state.reload_pending = true;
    match resend {
        Some((_, prompt)) => {
            session.note(
                "system",
                format!("{RESTARTED}; sending your message again."),
            );
            session.queue.insert(0, prompt);
        }
        None => session.note("system", format!("{RESTARTED}. Send your message again.")),
    }
}

/// No turn is running and nothing waits on the user or in the queue: the
/// service accepts provider switches (and Claude Code model changes) now.
pub fn between_turns(session: &Session) -> bool {
    matches!(
        session.status,
        Status::Idle | Status::Interrupted | Status::Failed
    ) && session.turn_id.is_none()
        && session.pending.is_empty()
        && session.queue.is_empty()
        && !session.switching_workspace
}

pub fn switch(
    session: &mut Session,
    provider: Provider,
    model: Option<String>,
    effort: Option<String>,
) -> Result<()> {
    ensure!(
        between_turns(session),
        "Finish the current turn and pending requests before switching providers"
    );
    ensure!(
        matches!(session.job, Job::Coding(_)),
        "Only coding sessions can switch providers"
    );
    session.provider_threads.insert(
        session.provider.key().into(),
        NativeSession {
            thread_id: session.thread_id.clone(),
            through: if session.provider_context.is_none() {
                session.entries.len()
            } else {
                session
                    .provider_threads
                    .get(session.provider.key())
                    .map_or(0, |native| native.through)
            },
            permission_state: session.permission_state.clone(),
            permissions: session.permissions.clone(),
            inherited_permissions: session.inherited_permissions.clone(),
            artifact_tools: session.artifact_tools,
            pr_tools: session.pr_tools,
            shell_tools: session.shell_tools,
            registration_tools: session.registration_tools,
            question_tools: session.question_tools,
            peer_tools: session.peer_tools,
            folder_tools: session.folder_tools,
            command_tools: session.command_tools,
        },
    );
    let target = session
        .provider_threads
        .get(provider.key())
        .cloned()
        .unwrap_or_default();
    session.provider_context = handoff(&session.entries, target.through);
    session.provider = provider;
    session.thread_id = target.thread_id;
    session.permission_state = target.permission_state;
    session.permissions = target.permissions;
    session.inherited_permissions = target.inherited_permissions;
    session.artifact_tools = target.artifact_tools;
    session.pr_tools = target.pr_tools;
    session.shell_tools = target.shell_tools;
    session.registration_tools = target.registration_tools;
    session.question_tools = target.question_tools;
    session.peer_tools = target.peer_tools;
    session.folder_tools = target.folder_tools;
    session.command_tools = target.command_tools;
    session.usage = Value::Null;
    session.token_usage = Value::Null;
    session.completed_turn = None;
    session.completed_turn_successful = false;
    super::plan::invalidate(session);
    session.suggestion = None;
    session.suggestion_attempted = None;
    session.model = model.clone();
    session.effort = effort.clone();
    session.error = None;
    session.status = Status::Idle;
    session.shells.clear();
    if let Job::Coding(launch) = &mut session.job {
        launch.model = model;
        launch.effort = effort;
    }
    session.note("system", format!("Selected {}. The chat and workspace are retained; conversation context will be supplied on the next message.", provider.label()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> Session {
        let mut session = Session::new(
            "handoff".into(),
            Job::Coding(super::super::Launch {
                repository: "/repo".into(),
                base: "HEAD".into(),
                isolated: true,
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.status = Status::Idle;
        session.workspace = Some("/worktree".into());
        session.workspace_ready = true;
        session.thread_id = Some("codex-thread".into());
        session.note("userMessage", "Keep the requested behavior");
        session
    }
    #[test]
    fn an_unused_provider_still_receives_full_context_on_return() -> Result<()> {
        let mut session = session();
        session.question_tools = true;
        session.pr_tools = true;
        session.command_tools = true;
        session.permission_state.mode = Some("plan".into());
        session.permission_state.pending_preset =
            Some(super::super::permissions::CodexPreset::Auto);
        super::super::permissions::audit(
            &mut session.permission_state,
            super::super::permissions::PermissionEventKind::ControlRequested,
            None,
            None,
            Some("ReadUsage"),
        );
        switch(
            &mut session,
            Provider::Claude,
            Some("claude/sonnet".into()),
            None,
        )?;
        assert!(session.thread_id.is_none());
        assert!(!session.question_tools);
        assert!(!session.pr_tools);
        assert!(!session.command_tools);
        assert!(session.permission_state.mode.is_none());
        assert!(session.permission_state.permission_events.is_empty());
        switch(
            &mut session,
            Provider::Codex,
            Some("codex-model".into()),
            None,
        )?;
        assert_eq!(session.thread_id.as_deref(), Some("codex-thread"));
        assert!(session.question_tools);
        assert!(session.pr_tools);
        assert!(session.command_tools);
        assert_eq!(session.permission_state.mode.as_deref(), Some("plan"));
        assert_eq!(session.permission_state.permission_events.len(), 1);
        assert_eq!(
            session.permission_state.pending_preset,
            Some(super::super::permissions::CodexPreset::Auto)
        );
        switch(
            &mut session,
            Provider::Claude,
            Some("claude/sonnet".into()),
            None,
        )?;
        assert!(
            session
                .provider_context
                .as_deref()
                .is_some_and(|c| c.contains("Keep the requested behavior"))
        );
        assert_eq!(
            session.workspace.as_deref(),
            Some(std::path::Path::new("/worktree"))
        );
        session.status = Status::Running;
        assert!(switch(&mut session, Provider::Codex, None, None).is_err());
        assert_eq!(session.provider, Provider::Claude);
        Ok(())
    }
    #[test]
    fn a_handoff_keeps_the_first_request_and_the_newest_chat_within_budget() -> Result<()> {
        use anyhow::Context as _;
        let mut session = session();
        for turn in 0..400 {
            session.note(
                "agentMessage",
                format!("answer {turn} {}", "x".repeat(2_000)),
            );
        }
        session.note("mcpToolCall", "y".repeat(50_000));
        let handoff = handoff(&session.entries, 0).context("handoff")?;
        assert!(handoff.len() < HANDOFF_BUDGET + HANDOFF.message + 1_000);
        let (_, context) = handoff.split_once('\n').context("context")?;
        let context: Vec<Value> = serde_json::from_str(context)?;
        let text = |index: usize| {
            context
                .get(index)
                .and_then(|item| item.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        assert_eq!(text(0), "Keep the requested behavior");
        assert!(text(1).ends_with("earlier entries omitted to fit the model's context"));
        assert!(text(context.len() - 2).starts_with("answer 399 "));
        assert!(!handoff.contains("answer 0 "));
        let output = text(context.len() - 1);
        assert!(output.len() < HANDOFF.activity + 20 && output.ends_with("(shortened)"));
        Ok(())
    }
    #[test]
    fn a_long_chat_is_summarized_up_to_where_the_recent_chat_fits() -> Result<()> {
        use anyhow::Context as _;
        let mut session = session();
        assert!(summary_start(&session.entries).is_none());
        for turn in 0..400 {
            session.note(
                "agentMessage",
                format!("answer {turn} {}", "x".repeat(2_000)),
            );
        }
        let start = summary_start(&session.entries).context("summarized")?;
        let input = summary_input(session.entries.get(..start).context("earlier")?);
        assert!(input.len() < SUMMARY_INPUT && input.contains("Keep the requested behavior"));
        let handoff = summarized_handoff(&session.entries, start, "The widget is half done.");
        assert!(handoff.starts_with("Difu conversation handoff."));
        assert!(handoff.contains("Summary of the earlier conversation, written from the full chat:\nThe widget is half done."));
        assert!(handoff.len() < HANDOFF_BUDGET && !handoff.contains("omitted"));
        // Entry `start` holds answer `start - 1`: the recent chat starts there.
        assert!(handoff.contains(&format!("\"answer {} ", start - 1)));
        assert!(!handoff.contains(&format!("\"answer {} ", start - 2)));
        assert!(handoff.contains("\"answer 399 "));
        Ok(())
    }
    #[test]
    fn old_sessions_default_to_codex() -> Result<()> {
        let mut value = serde_json::to_value(session())?;
        if let Some(object) = value.as_object_mut() {
            object.remove("provider");
            object.remove("provider_threads");
            object.remove("provider_context");
        }
        let session: Session = serde_json::from_value(value)?;
        assert_eq!(session.provider, Provider::Codex);
        assert!(session.provider_threads.is_empty());
        Ok(())
    }
}
