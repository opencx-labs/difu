use super::*;

impl Ui {
    pub(super) fn open_usage(&mut self) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        self.modal = Some(Modal::Usage {
            id: id.clone(),
            provider: session.provider,
            data: None,
            error: None,
            scroll: 0,
            maximum: 0,
        });
        self.task(Task::Usage(id.clone()), Request::Usage { id }, false);
    }
    pub(super) fn usage_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.modal = None;
            return;
        }
        if key.code == KeyCode::Char('r') {
            self.open_usage();
            return;
        }
        let Some(Modal::Usage {
            scroll, maximum, ..
        }) = &mut self.modal
        else {
            return;
        };
        *scroll = match key.code {
            KeyCode::Down => scroll.saturating_add(1),
            KeyCode::Up => scroll.saturating_sub(1),
            KeyCode::PageDown => scroll.saturating_add(10),
            KeyCode::PageUp => scroll.saturating_sub(10),
            KeyCode::Home => 0,
            KeyCode::End => *maximum,
            _ => *scroll,
        }
        .min(*maximum);
    }
    pub(super) fn draw_usage(&mut self, frame: &mut Frame, area: Rect) {
        let Some(Modal::Usage {
            provider,
            data,
            error,
            scroll,
            maximum,
            ..
        }) = &mut self.modal
        else {
            return;
        };
        let mut lines = vec![
            match provider {
                provider::Provider::Codex => "Codex usage and limits".into(),
                provider::Provider::Claude => "Claude Code usage".into(),
            },
            String::new(),
        ];
        if let Some(error) = error {
            lines.push(error.clone());
        } else if let Some(value) = data {
            rows(value, 0, &mut lines);
        } else {
            lines.push("Loading provider usage…".into());
        }
        let lines = lines
            .into_iter()
            .flat_map(|line| wrapped(&line, area.width))
            .map(Line::from)
            .collect::<Vec<_>>();
        *maximum = lines.len().saturating_sub(usize::from(area.height));
        *scroll = (*scroll).min(*maximum);
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(*scroll)
                    .take(usize::from(area.height))
                    .collect::<Vec<_>>(),
            )
            .style(Style::default().fg(TEXT)),
            area,
        );
    }
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
fn scalar(key: &str, value: &Value) -> String {
    if matches!(key, "resetsAt" | "resets_at") {
        let reset = value
            .as_i64()
            .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
            .or_else(|| {
                value
                    .as_str()
                    .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
                    .map(|at| at.to_utc())
            });
        if let Some(reset) = reset {
            return reset
                .with_timezone(&chrono::Local)
                .format("%b %d, %H:%M %Z")
                .to_string();
        }
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
fn rows(value: &Value, depth: usize, output: &mut Vec<String>) {
    let indent = "  ".repeat(depth);
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if value.is_null() {
                    continue;
                }
                if value.is_object() || value.is_array() {
                    if value.as_object().is_some_and(|v| v.is_empty())
                        || value.as_array().is_some_and(|v| v.is_empty())
                    {
                        continue;
                    }
                    output.push(format!("{indent}{}", label(key)));
                    rows(value, depth + 1, output);
                } else {
                    output.push(format!("{indent}{}: {}", label(key), scalar(key, value)));
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                rows(item, depth, output);
                output.push(String::new());
            }
        }
        _ => output.push(format!("{indent}{}", scalar("", value))),
    }
}
