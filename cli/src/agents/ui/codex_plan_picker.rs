// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Adapted from openai/codex at 430a16799d90e296fb27b1e94f206023902b7e56,
// codex-rs/tui/src/chatwidget/plan_implementation.rs.
// Difu changes: data-only rows, local typed actions, and native thread preservation.
// License and upstream NOTICE: cli/third_party/codex/.
use crate::agents::plan::Choice;

pub(super) const TITLE: &str = "Implement this plan?";
pub(super) const LABELS: [&str; 3] = [
    "Yes, implement this plan",
    "Yes, clear context and implement",
    "No, stay in Plan mode",
];
pub(super) struct Item {
    pub name: &'static str,
    pub description: String,
    pub choice: Choice,
    pub disabled_reason: Option<&'static str>,
}

/// The upstream selection builder, adapted to Difu's session controls rather
/// than Codex TUI's AppEvent closures. No choice is submitted by constructing it.
pub(super) fn items(
    default_available: bool,
    plan_markdown: Option<&str>,
    usage: Option<&str>,
) -> [Item; 3] {
    let unavailable = (!default_available).then_some("Default mode unavailable");
    let clear_disabled = unavailable.or_else(|| {
        (!plan_markdown.is_some_and(|plan| !plan.trim().is_empty()))
            .then_some("No approved plan available")
    });
    let description = usage.map_or_else(
        || "Fresh thread with this plan".to_owned(),
        |label| format!("Start a fresh thread (current context: {label})"),
    );
    [
        Item {
            name: LABELS.first().copied().unwrap_or_default(),
            description: "Switch to Default and start coding".into(),
            choice: Choice::Implement,
            disabled_reason: unavailable,
        },
        Item {
            name: LABELS.get(1).copied().unwrap_or_default(),
            description,
            choice: Choice::FreshContext,
            disabled_reason: clear_disabled,
        },
        Item {
            name: LABELS.get(2).copied().unwrap_or_default(),
            description: "Continue planning with the model".into(),
            choice: Choice::ContinuePlanning,
            disabled_reason: None,
        },
    ]
}
