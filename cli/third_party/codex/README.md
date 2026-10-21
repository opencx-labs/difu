# Codex plan selector

`src/agents/ui/codex_plan_picker.rs` adapts the plan selection descriptor builder
from OpenAI Codex, pinned to commit
`430a16799d90e296fb27b1e94f206023902b7e56`:

https://github.com/openai/codex/blob/430a16799d90e296fb27b1e94f206023902b7e56/codex-rs/tui/src/chatwidget/plan_implementation.rs

The upstream three choices, descriptions, availability checks, and optional
context-usage description are retained. Codex application event closures are
adapted to Difu's typed session actions. Difu renders these rows in its own
Ratatui conversation pane rather than importing the entire Codex application.
The upstream plan-popup snapshot is the layout reference: full-width bottom
pane, numbered rows, aligned descriptions, and Enter/Escape controls.

Copyright 2025 OpenAI. The Apache-2.0 license and upstream NOTICE are included
alongside this file. Difu's original code retains its existing license.
