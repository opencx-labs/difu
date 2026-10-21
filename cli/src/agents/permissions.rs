//! Native provider permissions and per-session mode state.
pub mod settings;
use super::{Session, provider::Provider};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodexPreset {
    Ask,
    Auto,
    Full,
    ReadOnly,
}
impl CodexPreset {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ask => "Ask for approval",
            Self::Auto => "Approve for me",
            Self::Full => "Full Access",
            Self::ReadOnly => "Read Only",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Scope {
    Session,
    Project,
    Local,
    User,
}
impl Scope {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Session => "Session",
            Self::Project => "Project",
            Self::Local => "Local",
            Self::User => "User",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingKind {
    Allow,
    Ask,
    Deny,
    AutoAllow,
    AutoSoftDeny,
    AutoHardDeny,
    AutoEnvironment,
    Directory,
}
impl SettingKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Allow => "Allow",
            Self::Ask => "Ask",
            Self::Deny => "Deny",
            Self::AutoAllow => "Auto allow",
            Self::AutoSoftDeny => "Auto soft deny",
            Self::AutoHardDeny => "Auto hard deny",
            Self::AutoEnvironment => "Auto environment",
            Self::Directory => "Directory",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingsEdit {
    pub kind: SettingKind,
    pub scope: Scope,
    pub before: Option<String>,
    pub after: Option<String>,
    pub revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Action {
    Refresh,
    CycleMode,
    SetMode { mode: String },
    SetPreset { preset: CodexPreset },
    Edit { edit: SettingsEdit },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Choice {
    pub id: String,
    pub label: String,
    pub description: String,
    pub available: bool,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SettingRow {
    pub kind: SettingKind,
    pub scope: Option<Scope>,
    pub value: String,
    pub editable: bool,
    pub source: String,
    pub revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Denial {
    pub tool: String,
    pub detail: String,
    pub reason: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionEventKind {
    RequestSurfaced,
    ControlRequested,
    InterruptRequested,
    NativeCancel,
    AnswerAction,
    ModeChange,
    ResultCancelled,
    ResultFailed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PermissionEvent {
    pub timestamp: i64,
    pub event: PermissionEventKind,
    pub request_id: Option<String>,
    pub tool: Option<String>,
    pub control: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub provider: Provider,
    pub active_mode: Option<String>,
    pub pending_mode: Option<String>,
    pub active_preset: Option<CodexPreset>,
    pub pending_preset: Option<CodexPreset>,
    pub reload_pending: bool,
    pub modes: Vec<Choice>,
    pub presets: Vec<Choice>,
    pub rows: Vec<SettingRow>,
    pub revisions: BTreeMap<Scope, String>,
    pub denials: Vec<Denial>,
    pub message: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub mode: Option<String>,
    pub pending_mode: Option<String>,
    pub preset: Option<CodexPreset>,
    pub pending_preset: Option<CodexPreset>,
    pub claude_settings: Value,
    pub reload_pending: bool,
    pub bypass_enabled: bool,
    pub denials: Vec<Denial>,
    pub capabilities: Value,
    pub requirements: Value,
    pub codex_rollout: bool,
    pub codex_turn_attempted: bool,
    pub codex_thread_started: bool,
    pub claude_turn_attempted: bool,
    pub claude_turn_acknowledged: bool,
    pub claude_session_started: bool,
    pub permission_events: Vec<PermissionEvent>,
    pub cancellation_diagnostics: Vec<String>,
}

pub(super) fn audit(
    state: &mut State,
    event: PermissionEventKind,
    request_id: Option<&Value>,
    tool: Option<&str>,
    control: Option<&str>,
) {
    let bounded = |value: &str, limit| value.chars().take(limit).collect::<String>();
    let request_id = request_id.and_then(|id| {
        id.as_str()
            .map(|s| bounded(s, 160))
            .or_else(|| id.is_number().then(|| id.to_string()))
    });
    state.permission_events.push(PermissionEvent {
        timestamp: chrono::Utc::now().timestamp_millis(),
        event,
        request_id,
        tool: tool.map(|s| bounded(s, 160)),
        control: control.map(|s| bounded(s, 80)),
    });
    let excess = state.permission_events.len().saturating_sub(128);
    state.permission_events.drain(..excess);
}

/// Legacy IDs may be replaced only after the native provider says they are
/// missing, and only when Difu has evidence of administrative/unsent content.
pub(super) fn legacy_empty(session: &Session) -> bool {
    let state = &session.permission_state;
    !state.codex_rollout
        && !state.codex_turn_attempted
        && !state.claude_turn_attempted
        && !state.claude_turn_acknowledged
        && session.turn_id.is_none()
        && session.pending.is_empty()
        && session.queue.is_empty()
        && matches!(&session.job,super::Job::Coding(launch) if launch.prompt.is_empty())
        && session.entries.iter().all(|entry| {
            matches!(
                entry.kind.as_str(),
                "system" | "error" | "progress" | "awaiting connection"
            )
        })
}

fn mode_label(mode: &str, provider: Provider) -> &str {
    match mode {
        "default" => {
            if provider == Provider::Codex {
                "Regular"
            } else {
                "Manual"
            }
        }
        "plan" => "Plan",
        "acceptEdits" => "Accept edits",
        "bypassPermissions" => "Bypass permissions",
        "dontAsk" => "Don't ask",
        "auto" => "Auto",
        _ => mode,
    }
}
pub fn summary(session: &Session) -> String {
    let state = &session.permission_state;
    let mode = state
        .mode
        .as_deref()
        .map(|m| mode_label(m, session.provider))
        .unwrap_or("Inherited");
    let preset = if session.provider == Provider::Codex {
        format!(
            " · {}",
            detected_preset(&session.permissions).map_or(
                if session.permissions.is_null() {
                    "Inherited"
                } else {
                    "Custom"
                },
                |p| p.label()
            )
        )
    } else {
        String::new()
    };
    let pending = if let Some(mode) = &state.pending_mode {
        format!(
            " · {} pending {}",
            mode_label(mode, session.provider),
            if state.reload_pending {
                "reload"
            } else {
                "next turn"
            }
        )
    } else if let Some(preset) = state.pending_preset {
        format!(" · {} pending next turn", preset.label())
    } else if state.reload_pending {
        " · settings reload pending".into()
    } else {
        String::new()
    };
    format!("{mode}{preset}{pending}")
}
fn choice(id: &str, label: &str, description: &str) -> Choice {
    Choice {
        id: id.into(),
        label: label.into(),
        description: description.into(),
        available: true,
        reason: None,
    }
}
pub fn detected_preset(value: &Value) -> Option<CodexPreset> {
    let sandbox = value.get("sandbox")?;
    let kind = sandbox.get("type").and_then(Value::as_str)?;
    let allowed = match kind {
        "workspaceWrite" => &[
            "type",
            "writableRoots",
            "networkAccess",
            "excludeTmpdirEnvVar",
            "excludeSlashTmp",
        ][..],
        "readOnly" => &["type", "networkAccess"][..],
        "dangerFullAccess" => &["type"][..],
        _ => return None,
    };
    if sandbox
        .as_object()?
        .keys()
        .any(|key| !allowed.contains(&key.as_str()))
        || sandbox.get("networkAccess").and_then(Value::as_bool) == Some(true)
        || sandbox
            .get("writableRoots")
            .and_then(Value::as_array)
            .is_some_and(|roots| !roots.is_empty())
        || ["excludeTmpdirEnvVar", "excludeSlashTmp"]
            .iter()
            .any(|key| sandbox.get(key).and_then(Value::as_bool) == Some(true))
    {
        return None;
    }
    match (
        value.pointer("/sandbox/type").and_then(Value::as_str),
        value.get("approvalPolicy").and_then(Value::as_str),
        value.get("approvalsReviewer").and_then(Value::as_str),
    ) {
        (Some("readOnly"), Some("on-request"), Some("user")) => Some(CodexPreset::ReadOnly),
        (Some("workspaceWrite"), Some("on-request"), Some("auto_review" | "guardian_subagent")) => {
            Some(CodexPreset::Auto)
        }
        (Some("workspaceWrite"), Some("on-request"), Some("user")) => Some(CodexPreset::Ask),
        (Some("dangerFullAccess"), Some("never"), Some("user")) => Some(CodexPreset::Full),
        _ => None,
    }
}
pub fn snapshot(session: &Session) -> Snapshot {
    let state = &session.permission_state;
    let mut result = Snapshot {
        provider: session.provider,
        active_mode: state.mode.clone(),
        pending_mode: state.pending_mode.clone(),
        active_preset: detected_preset(&session.permissions),
        pending_preset: state.pending_preset,
        reload_pending: state.reload_pending,
        denials: state.denials.clone(),
        ..Snapshot::default()
    };
    match session.provider {
        Provider::Codex => {
            result.modes = vec![
                choice("default", "Regular", "Native coding mode"),
                choice("plan", "Plan", "Native planning mode"),
            ];
            for mode in &mut result.modes {
                if !state
                    .capabilities
                    .get("collaborationModes")
                    .and_then(Value::as_array)
                    .is_some_and(|modes| {
                        modes.iter().any(|v| {
                            v.get("mode").and_then(Value::as_str) == Some(mode.id.as_str())
                        })
                    })
                {
                    mode.available = false;
                    mode.reason = Some("Provider has not reported support for this mode".into());
                }
            }
            result.presets = [
                CodexPreset::Ask,
                CodexPreset::Auto,
                CodexPreset::Full,
                CodexPreset::ReadOnly,
            ]
            .into_iter()
            .map(|p| {
                let mut c = choice(
                    &format!("{p:?}"),
                    p.label(),
                    "Native Codex approval and sandbox policy",
                );
                if let Err(e) = validate_preset(session, p) {
                    c.available = false;
                    c.reason = Some(e.to_string());
                }
                c
            })
            .collect();
        }
        Provider::Claude => {
            result.active_mode = state.mode.clone().or_else(|| {
                session
                    .permissions
                    .get("permissionMode")
                    .and_then(Value::as_str)
                    .map(String::from)
            });
            result.modes = vec![
                choice("default", "Manual", "Ask when Claude needs permission"),
                choice(
                    "acceptEdits",
                    "Accept edits",
                    "Accept file edits with native checks",
                ),
                choice("plan", "Plan", "Explore and plan with native restrictions"),
                choice(
                    "bypassPermissions",
                    "Bypass permissions",
                    "Explicitly enable Claude's native bypass mode",
                ),
                choice(
                    "auto",
                    "Auto",
                    "Native Claude automatic permission decisions",
                ),
                choice(
                    "dontAsk",
                    "Don't ask",
                    "Deny operations requiring an approval",
                ),
            ];
            let cwd = session.workspace.as_deref().unwrap_or(session.job.root());
            let auto_disabled = settings::has_auto_disabled(cwd);
            let bypass_disabled = settings::has_bypass_disabled(cwd);
            for mode in &mut result.modes {
                if mode.id == "auto"
                    && (auto_disabled
                        || state.capabilities.get("auto").and_then(Value::as_bool) == Some(false))
                {
                    mode.available = false;
                    mode.reason = Some(
                        state
                            .capabilities
                            .get("autoReason")
                            .and_then(Value::as_str)
                            .unwrap_or(
                                "Auto mode is disabled or rejected by Claude for the current model",
                            )
                            .into(),
                    );
                }
                if mode.id == "bypassPermissions" && bypass_disabled {
                    mode.available = false;
                    mode.reason = Some("Bypass mode is disabled by settings".into());
                }
            }
            if state.capabilities.get("auto").is_none()
                && let Some(auto) = result.modes.iter_mut().find(|c| c.id == "auto")
            {
                auto.description = "Claude checks native Auto availability when selected".into();
            }
        }
    }
    result
}
pub(super) fn preset_params(preset: CodexPreset) -> Value {
    match preset {
        CodexPreset::Ask => {
            json!({"sandbox":"workspace-write","approvalPolicy":"on-request","approvalsReviewer":"user","config":{"sandbox_workspace_write":{"network_access":false,"writable_roots":[],"exclude_tmpdir_env_var":false,"exclude_slash_tmp":false}}})
        }
        CodexPreset::Auto => {
            json!({"sandbox":"workspace-write","approvalPolicy":"on-request","approvalsReviewer":"auto_review","config":{"sandbox_workspace_write":{"network_access":false,"writable_roots":[],"exclude_tmpdir_env_var":false,"exclude_slash_tmp":false}}})
        }
        CodexPreset::Full => {
            json!({"sandbox":"danger-full-access","approvalPolicy":"never","approvalsReviewer":"user"})
        }
        CodexPreset::ReadOnly => {
            json!({"sandbox":"read-only","approvalPolicy":"on-request","approvalsReviewer":"user"})
        }
    }
}
pub(super) fn validate_preset(session: &Session, preset: CodexPreset) -> Result<()> {
    ensure!(
        !session.waiting_for_workspace() || preset == CodexPreset::ReadOnly,
        "An isolated editing workspace is required before changing this policy"
    );
    let requirements = &session.permission_state.requirements;
    ensure!(
        !requirements.is_null(),
        "Provider policy requirements have not been loaded"
    );
    let params = preset_params(preset);
    for (field, key) in [
        ("allowedApprovalPolicies", "approvalPolicy"),
        ("allowedSandboxModes", "sandbox"),
        ("allowedApprovalsReviewers", "approvalsReviewer"),
    ] {
        if let Some(allowed) = requirements.get(field).and_then(Value::as_array) {
            ensure!(
                allowed
                    .iter()
                    .any(|v| v == params.get(key).unwrap_or(&Value::Null)),
                "Managed policy does not allow this preset ({key})"
            );
        }
    }
    if let Some(models) = requirements
        .pointer("/autoReview/requiredOnModels")
        .and_then(Value::as_array)
    {
        let model = session.model.as_deref().map(Provider::native_model);
        ensure!(
            preset == CodexPreset::Auto || !models.iter().any(|v| v.as_str() == model),
            "Managed policy requires automatic review for the current model"
        );
    }
    Ok(())
}
pub(super) fn next_mode(session: &Session) -> String {
    let snapshot = snapshot(session);
    let cycle = if session.provider == Provider::Codex {
        vec!["default", "plan"]
    } else {
        vec![
            "default",
            "acceptEdits",
            "plan",
            "bypassPermissions",
            "auto",
        ]
    };
    let choices = cycle
        .into_iter()
        .filter(|id| {
            snapshot.modes.iter().any(|v| {
                v.id == *id
                    && v.available
                    && (*id != "bypassPermissions" || session.permission_state.bypass_enabled)
            })
        })
        .collect::<Vec<_>>();
    let current = if session.provider == Provider::Codex {
        session.permission_state.mode.as_deref()
    } else {
        session
            .permission_state
            .pending_mode
            .as_deref()
            .or(session.permission_state.mode.as_deref())
    }
    .unwrap_or("default");
    choices
        .iter()
        .position(|id| *id == current)
        .and_then(|i| choices.get((i + 1) % choices.len()))
        .or_else(|| choices.first())
        .copied()
        .unwrap_or("default")
        .into()
}

pub(super) fn set_capability(state: &mut State, key: &str, value: Value) {
    if !state.capabilities.is_object() {
        state.capabilities = json!({});
    }
    if let Some(capabilities) = state.capabilities.as_object_mut() {
        capabilities.insert(key.into(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> Session {
        Session::new(
            "permissions-test".into(),
            super::super::Job::Coding(super::super::Launch {
                repository: "/repo".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: String::new(),
                model: None,
                effort: None,
            }),
        )
    }
    #[test]
    fn native_presets_and_managed_requirements() -> Result<()> {
        let mut s = session();
        assert!(validate_preset(&s, CodexPreset::Auto).is_err());
        s.permission_state.requirements = json!({"allowedSandboxModes":["workspace-write","read-only"],"allowedApprovalPolicies":["on-request"],"allowedApprovalsReviewers":["user"]});
        assert!(validate_preset(&s, CodexPreset::Ask).is_ok());
        assert!(validate_preset(&s, CodexPreset::Auto).is_err());
        assert!(validate_preset(&s, CodexPreset::Full).is_err());
        assert_eq!(
            preset_params(CodexPreset::Auto).get("approvalsReviewer"),
            Some(&json!("auto_review"))
        );
        Ok(())
    }
    #[test]
    fn mode_cycles_preserve_pending_latest_selection() -> Result<()> {
        let mut s = session();
        s.permission_state.capabilities =
            json!({"collaborationModes":[{"mode":"default"},{"mode":"plan"}]});
        assert_eq!(next_mode(&s), "plan");
        s.permission_state.pending_mode = Some("plan".into());
        assert_eq!(next_mode(&s), "plan");
        s.permission_state.mode = Some("plan".into());
        assert_eq!(next_mode(&s), "default");
        Ok(())
    }
    #[test]
    fn unknown_policies_are_custom() -> Result<()> {
        assert_eq!(
            detected_preset(&json!({"sandbox":{"type":"workspaceWrite"},"approvalPolicy":"never"})),
            None
        );
        assert_eq!(
            detected_preset(
                &json!({"sandbox":{"type":"workspaceWrite","networkAccess":true},"approvalPolicy":"on-request"})
            ),
            None
        );
        assert_eq!(
            detected_preset(
                &json!({"sandbox":{"type":"workspaceWrite","writableRoots":["/private"]},"approvalPolicy":"on-request"})
            ),
            None
        );
        assert_eq!(
            detected_preset(
                &json!({"sandbox":{"type":"workspaceWrite"},"approvalPolicy":"on-request","approvalsReviewer":"auto_review"})
            ),
            Some(CodexPreset::Auto)
        );
        Ok(())
    }
    #[test]
    fn permission_audit_is_bounded_metadata_only() -> Result<()> {
        let mut state = State::default();
        for _ in 0..200 {
            audit(
                &mut state,
                PermissionEventKind::ControlRequested,
                Some(&json!({"secret":"must not serialize"})),
                Some("Bash"),
                Some("ReadUsage"),
            );
        }
        assert_eq!(state.permission_events.len(), 128);
        let encoded = serde_json::to_string(&state.permission_events)?;
        assert!(!encoded.contains("must not serialize") && !encoded.contains("secret"));
        assert!(
            state
                .permission_events
                .iter()
                .all(|event| event.request_id.is_none() && event.timestamp > 0)
        );
        Ok(())
    }
}
