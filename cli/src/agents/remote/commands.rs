//! Slash commands for the phone: difu's session commands that make sense on a
//! phone, plus the session provider's skills and commands. Provider entries are
//! sent as messages the way the terminal app sends them: Claude Code commands
//! as `/name` text, Codex skills as `$name` text with the skill attached.
use super::super::{
    Job, Session, Skill,
    permissions::{self, CodexPreset},
    provider::Provider,
};
use anyhow::{Result, bail, ensure};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// Runs on the computer through `session.command`.
    Action,
    /// Sent to the agent as a message.
    Prompt,
    /// Opens a phone screen; with arguments `session.command` runs it directly.
    Screen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Arguments {
    None,
    Optional,
    Required,
}

/// One entry of the phone's `/` list.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    /// Without the trigger: `compact`, `review`.
    pub name: String,
    /// What the composer shows and inserts: `/compact`, `/review`, `$review`.
    pub trigger: String,
    pub description: String,
    /// Argument hint such as `<path>` or `[model] [effort]`.
    pub hint: Option<String>,
    pub kind: Kind,
    /// The phone screen a `screen` command opens.
    pub screen: Option<&'static str>,
    /// Destructive: `session.command` refuses it without `confirm: true`.
    pub confirm: bool,
    pub arguments: Arguments,
    /// `difu`, or the provider (`codex`, `claude`) for its skills and commands.
    pub source: &'static str,
}

pub struct Builtin {
    pub name: &'static str,
    description: &'static str,
    hint: Option<&'static str>,
    kind: Kind,
    pub screen: Option<&'static str>,
    confirm: bool,
    pub arguments: Arguments,
    /// Needs a coding session (skills, usage, permissions and workspaces).
    coding: bool,
    /// Needs the session's own repository, which the orchestrator has not.
    repository: bool,
}

/// The terminal's session commands that work on a phone, in its order.
const BUILTINS: &[Builtin] = &[
    Builtin {
        name: "compact",
        description: "Compact the current conversation",
        hint: None,
        kind: Kind::Action,
        screen: None,
        confirm: false,
        arguments: Arguments::None,
        coding: false,
        repository: false,
    },
    Builtin {
        name: "model",
        description: "Choose a model",
        hint: Some("[model] [effort]"),
        kind: Kind::Screen,
        screen: Some("model"),
        confirm: false,
        arguments: Arguments::Optional,
        coding: false,
        repository: false,
    },
    Builtin {
        name: "effort",
        description: "Choose the reasoning level",
        hint: Some("[level]"),
        kind: Kind::Screen,
        screen: Some("model"),
        confirm: false,
        arguments: Arguments::Optional,
        coding: false,
        repository: false,
    },
    Builtin {
        name: "skills",
        description: "Browse skills for this workspace",
        hint: None,
        kind: Kind::Screen,
        screen: Some("skills"),
        confirm: false,
        arguments: Arguments::None,
        coding: true,
        repository: false,
    },
    Builtin {
        name: "status",
        description: "Show session, workspace and permissions",
        hint: None,
        kind: Kind::Screen,
        screen: Some("status"),
        confirm: false,
        arguments: Arguments::None,
        coding: false,
        repository: false,
    },
    Builtin {
        name: "rules",
        description: "Edit repository rules for new sessions",
        hint: None,
        kind: Kind::Screen,
        screen: Some("rules"),
        confirm: false,
        arguments: Arguments::None,
        coding: true,
        repository: true,
    },
    Builtin {
        name: "permissions",
        description: "Manage provider modes and approval policies",
        hint: Some("[mode]"),
        kind: Kind::Screen,
        screen: Some("permissions"),
        confirm: false,
        arguments: Arguments::Optional,
        coding: true,
        repository: false,
    },
    Builtin {
        name: "usage",
        description: "Show provider usage and rate limits",
        hint: None,
        kind: Kind::Screen,
        screen: Some("usage"),
        confirm: false,
        arguments: Arguments::None,
        coding: true,
        repository: false,
    },
    Builtin {
        name: "diff",
        description: "Show current session changes",
        hint: None,
        kind: Kind::Screen,
        screen: Some("changes"),
        confirm: false,
        arguments: Arguments::None,
        coding: true,
        repository: false,
    },
    Builtin {
        name: "new",
        description: "Launch a new coding session",
        hint: None,
        kind: Kind::Screen,
        screen: Some("new"),
        confirm: false,
        arguments: Arguments::None,
        coding: false,
        repository: false,
    },
    Builtin {
        name: "rename",
        description: "Rename this session",
        hint: Some("[title]"),
        kind: Kind::Screen,
        screen: Some("rename"),
        confirm: false,
        arguments: Arguments::Optional,
        coding: false,
        repository: false,
    },
    Builtin {
        name: "clear",
        description: "Start over: clear context, PRs and artifacts; fresh worktree from main",
        hint: None,
        kind: Kind::Action,
        screen: None,
        confirm: true,
        arguments: Arguments::None,
        coding: false,
        repository: false,
    },
    Builtin {
        name: "repo",
        description: "Change repository and discard previous workspace work",
        hint: Some("<path>"),
        kind: Kind::Action,
        screen: None,
        confirm: true,
        arguments: Arguments::Required,
        coding: true,
        repository: true,
    },
];

pub fn builtin(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.name == name)
}

/// Whether `builtin` works in `session`: skills, usage, permissions and
/// workspace commands need a coding session, repository commands one with its
/// own repository.
pub fn available(builtin: &Builtin, session: &Session) -> bool {
    (!builtin.coding || matches!(session.job, Job::Coding(_)))
        && (!builtin.repository || !session.orchestrator)
        && (builtin.name != "diff" || session.summary().can_read_changes)
}

/// difu's commands available in `session`. On a server `/repo` also takes a
/// server repository's `owner/name`.
pub fn difu(session: &Session, server: bool) -> Vec<Command> {
    BUILTINS
        .iter()
        .filter(|b| available(b, session))
        .map(|b| Command {
            name: b.name.into(),
            trigger: format!("/{}", b.name),
            description: b.description.into(),
            hint: if b.name == "repo" && server {
                Some("<owner/name or path>".into())
            } else {
                b.hint.map(str::to_owned)
            },
            kind: b.kind,
            screen: b.screen,
            confirm: b.confirm,
            arguments: b.arguments,
            source: "difu",
        })
        .collect()
}

/// The provider's enabled skills and commands. As in the terminal, difu's own
/// command runs when a Claude Code command shares its name.
pub fn provider(provider: Provider, skills: &[Skill]) -> Vec<Command> {
    let mut commands: Vec<Command> = Vec::new();
    for skill in skills.iter().filter(|s| s.enabled) {
        if skill.slash && builtin(&skill.name).is_some() {
            continue;
        }
        let trigger = format!("{}{}", if skill.slash { '/' } else { '$' }, skill.name);
        if commands.iter().any(|c| c.trigger == trigger) {
            continue;
        }
        let (hint, description) = if skill.slash {
            split_hint(&skill.description)
        } else {
            (None, skill.description.clone())
        };
        commands.push(Command {
            name: skill.name.clone(),
            trigger,
            description,
            hint,
            kind: Kind::Prompt,
            screen: None,
            confirm: false,
            arguments: Arguments::Optional,
            source: provider.key(),
        });
    }
    commands
}

/// Claude Code commands arrive as `hint · description`.
fn split_hint(description: &str) -> (Option<String>, String) {
    match description.split_once(" · ") {
        Some((hint, rest)) if !hint.trim().is_empty() && hint.chars().count() <= 60 => {
            (Some(hint.trim().to_owned()), rest.trim().to_owned())
        }
        _ => (None, description.to_owned()),
    }
}

/// The skill or command named `name`; `attached` limits the search to skills
/// sent with the message (Codex), as opposed to `/name` commands.
pub fn find_skill<'a>(skills: &'a [Skill], name: &str, attached: bool) -> Result<&'a Skill> {
    let name = name.trim().trim_start_matches(['/', '$']);
    skills
        .iter()
        .filter(|s| s.enabled && (!attached || !s.slash))
        .find(|s| s.name == name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown command `{}{name}`; reload the commands",
                if attached { '$' } else { '/' }
            )
        })
}

/// The message text that runs `skill` with `args`, as the terminal composes it.
pub fn skill_text(skill: &Skill, args: &str) -> String {
    let trigger = if skill.slash { '/' } else { '$' };
    let args = args.trim();
    if args.is_empty() {
        format!("{trigger}{}", skill.name)
    } else {
        format!("{trigger}{} {args}", skill.name)
    }
}

/// `/model <model> [effort]` and `/effort <level>`, parsed as the terminal
/// does: a model of the session's provider keeps the current effort.
pub fn model_arguments(
    name: &str,
    args: &str,
    session: &Session,
) -> Result<(Option<String>, Option<String>)> {
    let fields: Vec<&str> = args.split_whitespace().collect();
    if name == "model" {
        let [model, rest @ ..] = fields.as_slice() else {
            bail!("Usage: /model <model> [effort]");
        };
        ensure!(rest.len() <= 1, "Usage: /model <model> [effort]");
        let effort = match rest.first() {
            Some(effort) => Some((*effort).to_owned()),
            None if Provider::for_model(Some(model)) == session.provider => session.effort.clone(),
            None => None,
        };
        return Ok((Some((*model).to_owned()), effort));
    }
    let [effort] = fields.as_slice() else {
        bail!("Usage: /effort <level>");
    };
    Ok((session.model.clone(), Some((*effort).to_owned())))
}

/// The mode (or Codex approval preset) `/permissions <choice>` names, by id or
/// label.
pub fn permission_action(provider: Provider, choice: &str) -> Result<permissions::Action> {
    let choice = choice.trim();
    let modes = permissions::mode_choices(provider);
    if let Some(mode) = modes.iter().find(|m| m.id == choice).or_else(|| {
        modes
            .iter()
            .find(|m| m.id.eq_ignore_ascii_case(choice) || m.label.eq_ignore_ascii_case(choice))
    }) {
        return Ok(permissions::Action::SetMode {
            mode: mode.id.clone(),
        });
    }
    if provider == Provider::Codex
        && let Some(preset) = CodexPreset::parse(choice)
    {
        return Ok(permissions::Action::SetPreset { preset });
    }
    let mut names: Vec<String> = modes.iter().map(|m| m.id.clone()).collect();
    if provider == Provider::Codex {
        names.extend(CodexPreset::ALL.iter().map(|p| p.id().to_owned()));
    }
    bail!(
        "Unknown mode `{choice}` for {}; choose {}",
        provider.label(),
        names.join(", ")
    )
}

/// Modes and approval presets for the phone's permissions screen.
pub fn permissions_view(snapshot: &permissions::Snapshot) -> Value {
    json!({
        "provider": snapshot.provider.key(),
        "activeMode": snapshot.active_mode,
        "pendingMode": snapshot.pending_mode,
        "activePreset": snapshot.active_preset,
        "pendingPreset": snapshot.pending_preset,
        "reloadPending": snapshot.reload_pending,
        "modes": snapshot.modes,
        "presets": snapshot.presets,
        "message": snapshot.message,
    })
}

/// What the terminal's `/status` shows, as fields.
pub fn status_view(session: &Session) -> Value {
    let mode = session.permission_state.mode.clone().or_else(|| {
        (session.provider == Provider::Claude)
            .then(|| session.permissions.get("permissionMode"))
            .flatten()
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let mode = mode.map(|id| {
        permissions::mode_choices(session.provider)
            .into_iter()
            .find(|choice| choice.id == id)
            .map_or(id, |choice| choice.label)
    });
    json!({
        "title": session.title,
        "provider": session.provider.key(),
        "providerLabel": session.provider.label(),
        "model": session.model,
        "effort": session.effort,
        "state": session.status.label(),
        "repository": session.job.root().display().to_string(),
        "workspace": session.workspace.as_ref().map(|p| p.display().to_string()),
        "branch": session.branch,
        "providerSession": session.thread_id,
        "mode": mode,
        "preset": (session.provider == Provider::Codex)
            .then(|| permissions::detected_preset(&session.permissions))
            .flatten()
            .map(|preset| preset.label()),
    })
}

#[derive(Debug, Default, Serialize)]
struct Usage {
    limits: Vec<Limit>,
    details: Vec<Detail>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Limit {
    section: Option<String>,
    title: String,
    /// Percent left for Codex, used for Claude Code (as the terminal shows it).
    percent: f64,
    remaining: bool,
    /// Unix milliseconds.
    resets_at: Option<i64>,
}

#[derive(Debug, Serialize)]
struct Detail {
    section: Option<String>,
    label: String,
    value: String,
}

/// Provider usage as the terminal's `/usage` lays it out: rate-limit windows
/// and labelled details.
pub fn usage_view(provider: Provider, value: &Value) -> Value {
    let mut usage = Usage::default();
    walk(value, "", None, provider, &mut usage);
    json!({
        "provider": provider.key(),
        "title": match provider {
            Provider::Codex => "OpenAI Codex · Usage",
            Provider::Claude => "Claude Code · Usage",
        },
        "limits": usage.limits,
        "details": usage.details,
    })
}

fn walk(value: &Value, key: &str, section: Option<&str>, provider: Provider, out: &mut Usage) {
    if let Some(used) = used(value) {
        let remaining = provider == Provider::Codex;
        out.limits.push(Limit {
            section: section.map(str::to_owned),
            title: window_title(key, value, provider),
            percent: if remaining { 100.0 - used } else { used }.clamp(0.0, 100.0),
            remaining,
            resets_at: value
                .get("resetsAt")
                .or_else(|| value.get("resets_at"))
                .and_then(reset_millis),
        });
        return;
    }
    match value {
        Value::Object(fields) => {
            // Codex supplies a legacy default bucket alongside the complete map.
            let buckets = fields
                .get("rateLimitsByLimitId")
                .and_then(Value::as_object)
                .is_some_and(|b| !b.is_empty());
            let mut fields: Vec<_> = fields.iter().collect();
            fields.sort_by_key(|(key, _)| match key.as_str() {
                "model" | "subscription_type" | "planType" => 0,
                "reasoningEffort" => 1,
                "five_hour" | "primary" => 2,
                "seven_day" | "secondary" => 3,
                _ => 4,
            });
            for (key, value) in fields {
                if value.is_null() || (buckets && key == "rateLimits") {
                    continue;
                }
                if value.is_object() || value.is_array() {
                    let container = used(value).is_some()
                        || matches!(
                            key.as_str(),
                            "limits" | "rateLimits" | "rate_limits" | "rateLimitsByLimitId"
                        );
                    let nested = if container {
                        section.map(str::to_owned)
                    } else {
                        Some(label(key))
                    };
                    walk(value, key, nested.as_deref(), provider, out);
                } else {
                    out.details.push(Detail {
                        section: section.map(str::to_owned),
                        label: match key.as_str() {
                            "planType" | "subscription_type" => "Account".into(),
                            _ => label(key),
                        },
                        value: scalar(key, value),
                    });
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                walk(item, key, section, provider, out);
            }
        }
        _ => out.details.push(Detail {
            section: section.map(str::to_owned),
            label: label(key),
            value: scalar(key, value),
        }),
    }
}

fn used(value: &Value) -> Option<f64> {
    value
        .get("usedPercent")
        .or_else(|| value.get("utilization"))
        .and_then(Value::as_f64)
}

/// Codex reports seconds, Claude Code RFC 3339 times.
fn reset_millis(value: &Value) -> Option<i64> {
    value.as_i64().map(|s| s.saturating_mul(1000)).or_else(|| {
        value
            .as_str()
            .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
            .map(|at| at.timestamp_millis())
    })
}

fn label(key: &str) -> String {
    let mut text = String::new();
    for (index, ch) in key.chars().enumerate() {
        if ch == '_' {
            text.push(' ');
        } else {
            if ch.is_uppercase() && index > 0 {
                text.push(' ');
            }
            text.push(if index == 0 {
                ch.to_ascii_uppercase()
            } else {
                ch.to_ascii_lowercase()
            });
        }
    }
    text.replace(" usd", " (USD)")
        .replace(" mins", " (minutes)")
        .replace(" ms", " (ms)")
}

fn window_title(key: &str, value: &Value, provider: Provider) -> String {
    if provider == Provider::Claude {
        if key == "five_hour" {
            return "Current session (5 hours)".into();
        }
        if key == "seven_day" {
            return "Current week (all models)".into();
        }
        if let Some(model) = key.strip_prefix("seven_day_") {
            return format!("Current week ({})", label(model));
        }
    }
    match value.get("windowDurationMins").and_then(Value::as_u64) {
        Some(10080) => "Weekly limit".into(),
        Some(1440) => "Daily limit".into(),
        Some(minutes) if minutes % 60 == 0 => format!("{}-hour limit", minutes / 60),
        Some(minutes) => format!("{minutes}-minute limit"),
        None => label(key),
    }
}

fn scalar(key: &str, value: &Value) -> String {
    if matches!(key, "resetsAt" | "resets_at")
        && let Some(at) = reset_millis(value).and_then(chrono::DateTime::from_timestamp_millis)
    {
        return at.format("%Y-%m-%d %H:%M UTC").to_string();
    }
    if matches!(key, "usedPercent" | "utilization" | "pct")
        && let Some(number) = value.as_f64()
    {
        return format!("{number:.1}%");
    }
    match value {
        Value::String(text) => crate::model::clean(text),
        Value::Bool(value) => if *value { "Yes" } else { "No" }.into(),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{Launch, Status};
    use super::*;

    fn at<'a>(value: &'a Value, pointer: &str) -> &'a Value {
        value.pointer(pointer).unwrap_or(&Value::Null)
    }

    fn session(provider: Provider) -> Session {
        let mut session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: "/repo".into(),
                isolated: true,
                base: "origin/main".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        );
        session.provider = provider;
        session.status = Status::Idle;
        session.workspace = Some("/worktree".into());
        session
    }

    fn skill(name: &str, description: &str, slash: bool, enabled: bool) -> Skill {
        Skill {
            name: name.into(),
            path: if slash {
                Default::default()
            } else {
                format!("{name}/SKILL.md").into()
            },
            description: description.into(),
            enabled,
            slash,
        }
    }

    #[test]
    fn coding_sessions_offer_every_phone_command_in_the_terminal_order() {
        let names: Vec<_> = difu(&session(Provider::Codex), false)
            .into_iter()
            .map(|c| c.trigger)
            .collect();
        assert_eq!(
            names,
            [
                "/compact",
                "/model",
                "/effort",
                "/skills",
                "/status",
                "/rules",
                "/permissions",
                "/usage",
                "/new",
                "/rename",
                "/clear",
                "/repo"
            ],
            "no /diff without readable changes"
        );
    }

    #[test]
    fn other_sessions_drop_coding_commands_and_servers_name_repositories() -> Result<()> {
        let mut conflicts = session(Provider::Codex);
        conflicts.job = Job::Conflict {
            root: "/repo".into(),
            key: crate::model::PrKey {
                owner: "owner".into(),
                repo: "repo".into(),
                number: 1,
            },
            head: String::new(),
            model: Default::default(),
        };
        let names: Vec<_> = difu(&conflicts, false)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(
            names,
            [
                "compact", "model", "effort", "status", "new", "rename", "clear"
            ]
        );
        let mut orchestrator = session(Provider::Codex);
        orchestrator.orchestrator = true;
        let names: Vec<_> = difu(&orchestrator, false)
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert!(names.contains(&"skills".to_owned()));
        assert!(!names.contains(&"rules".to_owned()) && !names.contains(&"repo".to_owned()));
        let repo = difu(&session(Provider::Codex), true)
            .into_iter()
            .find(|c| c.name == "repo")
            .ok_or_else(|| anyhow::anyhow!("repo"))?;
        assert_eq!(repo.hint.as_deref(), Some("<owner/name or path>"));
        assert!(repo.confirm);
        assert_eq!(repo.arguments, Arguments::Required);
        assert_eq!(repo.kind, Kind::Action);
        Ok(())
    }

    #[test]
    fn provider_skills_become_prompts_and_difu_commands_win_name_clashes() {
        let commands = provider(
            Provider::Claude,
            &[
                skill("review", "[pr] · Review a pull request", true, true),
                skill("compact", "Claude's own compaction", true, true),
                skill("init", "Initialize CLAUDE.md", true, true),
                skill("off", "Disabled", true, false),
                skill("lint", "Lint the repository", false, true),
                skill("lint", "Second copy elsewhere", false, true),
            ],
        );
        let rows: Vec<_> = commands
            .iter()
            .map(|c| {
                (
                    c.trigger.as_str(),
                    c.hint.as_deref(),
                    c.description.as_str(),
                    c.kind,
                    c.source,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                (
                    "/review",
                    Some("[pr]"),
                    "Review a pull request",
                    Kind::Prompt,
                    "claude"
                ),
                (
                    "/init",
                    None,
                    "Initialize CLAUDE.md",
                    Kind::Prompt,
                    "claude"
                ),
                ("$lint", None, "Lint the repository", Kind::Prompt, "claude"),
            ]
        );
    }

    #[test]
    fn skills_resolve_by_name_and_compose_the_terminals_text() -> Result<()> {
        let skills = [
            skill("review", "", true, true),
            skill("lint", "", false, true),
        ];
        let review = find_skill(&skills, "/review", false)?;
        assert_eq!(skill_text(review, "  45 "), "/review 45");
        let lint = find_skill(&skills, "$lint", true)?;
        assert_eq!(skill_text(lint, ""), "$lint");
        let error = find_skill(&skills, "review", true)
            .err()
            .ok_or_else(|| anyhow::anyhow!("slash commands are never attached"))?;
        assert!(error.to_string().contains("Unknown command `$review`"));
        Ok(())
    }

    #[test]
    fn model_arguments_keep_the_effort_only_within_a_provider() -> Result<()> {
        let mut session = session(Provider::Codex);
        session.model = Some("gpt-5.5".into());
        session.effort = Some("high".into());
        assert_eq!(
            model_arguments("model", "gpt-5.6", &session)?,
            (Some("gpt-5.6".into()), Some("high".into()))
        );
        assert_eq!(
            model_arguments("model", "claude/opus", &session)?,
            (Some("claude/opus".into()), None)
        );
        assert_eq!(
            model_arguments("model", "claude/opus max", &session)?,
            (Some("claude/opus".into()), Some("max".into()))
        );
        assert_eq!(
            model_arguments("effort", "low", &session)?,
            (Some("gpt-5.5".into()), Some("low".into()))
        );
        assert!(model_arguments("model", "a b c", &session).is_err());
        assert!(model_arguments("effort", "", &session).is_err());
        Ok(())
    }

    #[test]
    fn permission_choices_match_ids_labels_and_codex_presets() -> Result<()> {
        assert!(matches!(
            permission_action(Provider::Claude, "Accept edits")?,
            permissions::Action::SetMode { mode } if mode == "acceptEdits"
        ));
        assert!(matches!(
            permission_action(Provider::Codex, "plan")?,
            permissions::Action::SetMode { mode } if mode == "plan"
        ));
        assert!(matches!(
            permission_action(Provider::Codex, "readonly")?,
            permissions::Action::SetPreset {
                preset: CodexPreset::ReadOnly
            }
        ));
        let error = permission_action(Provider::Claude, "ReadOnly")
            .err()
            .ok_or_else(|| anyhow::anyhow!("presets are Codex only"))?;
        assert!(error.to_string().contains("bypassPermissions"), "{error}");
        Ok(())
    }

    #[test]
    fn status_names_the_provider_mode_and_workspace() {
        let mut session = session(Provider::Claude);
        session.thread_id = Some("thread".into());
        session.permissions = json!({"permissionMode": "plan"});
        let status = status_view(&session);
        assert_eq!(*at(&status, "/providerLabel"), "Claude Code");
        assert_eq!(*at(&status, "/mode"), "Plan");
        assert_eq!(*at(&status, "/state"), "Ready");
        assert_eq!(*at(&status, "/workspace"), "/worktree");
        assert_eq!(*at(&status, "/repository"), "/repo");
        assert_eq!(*at(&status, "/providerSession"), "thread");
        assert_eq!(*at(&status, "/preset"), Value::Null);
    }

    #[test]
    fn usage_lists_windows_with_reset_times_and_labelled_details() {
        let claude = usage_view(
            Provider::Claude,
            &json!({"subscription_type": "max", "rate_limits": {
                "seven_day": {"utilization": 2, "resets_at": "2027-01-15T08:00:00Z"},
                "seven_day_fable": {"utilization": 0}
            }}),
        );
        assert_eq!(*at(&claude, "/title"), "Claude Code · Usage");
        assert_eq!(
            *at(&claude, "/details"),
            json!([{"section": null, "label": "Account", "value": "max"}])
        );
        assert_eq!(
            *at(&claude, "/limits"),
            json!([
                {"section": null, "title": "Current week (all models)", "percent": 2.0,
                 "remaining": false, "resetsAt": 1_800_000_000_000_i64},
                {"section": null, "title": "Current week (Fable)", "percent": 0.0,
                 "remaining": false, "resetsAt": null},
            ])
        );
        let codex = usage_view(
            Provider::Codex,
            &json!({"limits": {"rateLimitsByLimitId": {"codex": {"secondary": {
                "usedPercent": 4, "windowDurationMins": 10080, "resetsAt": 1_800_000_000
            }}}, "rateLimits": {"primary": {"usedPercent": 99}}}}),
        );
        assert_eq!(
            *at(&codex, "/limits"),
            json!([{"section": "Codex", "title": "Weekly limit", "percent": 96.0,
                    "remaining": true, "resetsAt": 1_800_000_000_000_i64}]),
            "the legacy bucket is skipped"
        );
    }
}
