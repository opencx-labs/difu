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
        let session_info = match &self.modal {
            Some(Modal::Usage { id, .. }) => self.sessions.get(id).map(|session| {
                (
                    session
                        .workspace
                        .as_deref()
                        .unwrap_or(session.job.root())
                        .display()
                        .to_string(),
                    session.thread_id.clone(),
                )
            }),
            _ => None,
        };
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
            Line::styled(
                match provider {
                    provider::Provider::Codex => "OpenAI Codex · Usage",
                    provider::Provider::Claude => "Claude Code · Usage",
                },
                Style::default()
                    .fg(TEXT)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
            Line::default(),
        ];
        if let Some(error) = error {
            text_lines(error, area.width, Style::default().fg(RED), &mut lines);
        } else if let Some(value) = data {
            let mut value = value.clone();
            if *provider == provider::Provider::Codex
                && let Some((directory, thread)) = session_info
                && let Some(fields) = value.as_object_mut()
            {
                fields.insert("directory".into(), Value::String(directory));
                if let Some(thread) = thread {
                    fields.insert("session".into(), Value::String(thread));
                }
            }
            usage_rows(&value, "", *provider, area.width, &mut lines);
        } else {
            lines.push(Line::styled(
                "Loading provider usage…",
                Style::default().fg(DIM),
            ));
        }
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
fn text_lines(text: &str, width: u16, style: Style, output: &mut Vec<Line<'static>>) {
    output.extend(
        wrapped(text, width.max(1))
            .into_iter()
            .map(|line| Line::styled(line, style)),
    );
}

fn window_title(key: &str, value: &Value, provider: provider::Provider) -> String {
    if provider == provider::Provider::Claude {
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

fn usage_rows(
    value: &Value,
    key: &str,
    provider: provider::Provider,
    width: u16,
    output: &mut Vec<Line<'static>>,
) {
    let used = value
        .get("usedPercent")
        .or_else(|| value.get("utilization"))
        .and_then(Value::as_f64);
    if let Some(used) = used {
        let remaining = provider == provider::Provider::Codex;
        let percent = if remaining { 100.0 - used } else { used }.clamp(0.0, 100.0);
        let caption = format!("{percent:.0}% {}", if remaining { "left" } else { "used" });
        output.push(Line::default());
        text_lines(
            &window_title(key, value, provider),
            width,
            Style::default()
                .fg(TEXT)
                .add_modifier(ratatui::style::Modifier::BOLD),
            output,
        );
        let bar_width = usize::from(width).saturating_sub(caption.len() + 2);
        let cells = if bar_width < 8 {
            usize::from(width)
        } else {
            bar_width
        };
        let fill = ((cells as f64 * percent / 100.0).round() as usize).min(cells);
        let mut spans = vec![
            Span::styled(
                "█".repeat(fill),
                Style::default().fg(if remaining { TEXT } else { crate::ui::PURPLE }),
            ),
            Span::styled(
                "░".repeat(cells.saturating_sub(fill)),
                Style::default().fg(BORDER),
            ),
        ];
        if bar_width >= 8 {
            spans.push(Span::raw(format!("  {caption}")));
        }
        output.push(Line::from(spans));
        if bar_width < 8 {
            text_lines(&caption, width, Style::default().fg(TEXT), output);
        }
        if let Some(reset) = value
            .get("resetsAt")
            .or_else(|| value.get("resets_at"))
            .filter(|v| !v.is_null())
        {
            text_lines(
                &format!("Resets {}", scalar("resets_at", reset)),
                width,
                Style::default().fg(DIM),
                output,
            );
        }
        output.push(Line::default());
        return;
    }
    match value {
        Value::Object(fields) => {
            // Codex supplies a legacy default bucket alongside the complete map.
            let has_buckets = fields
                .get("rateLimitsByLimitId")
                .and_then(Value::as_object)
                .is_some_and(|b| !b.is_empty());
            let mut fields = fields.iter().collect::<Vec<_>>();
            fields.sort_by_key(|(key, _)| match key.as_str() {
                "model" | "subscription_type" | "planType" => 0,
                "reasoningEffort" => 1,
                "directory" | "five_hour" | "primary" => 2,
                "session" | "seven_day" | "secondary" => 3,
                _ => 4,
            });
            for (key, value) in fields {
                if value.is_null() || (has_buckets && key == "rateLimits") {
                    continue;
                }
                if value.is_object() || value.is_array() {
                    let window = value
                        .get("usedPercent")
                        .or_else(|| value.get("utilization"))
                        .and_then(Value::as_f64)
                        .is_some();
                    if !window
                        && !matches!(
                            key.as_str(),
                            "limits" | "rateLimits" | "rate_limits" | "rateLimitsByLimitId"
                        )
                    {
                        text_lines(
                            &label(key),
                            width,
                            Style::default()
                                .fg(DIM)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                            output,
                        );
                    }
                    usage_rows(value, key, provider, width, output);
                } else {
                    let name = match key.as_str() {
                        "planType" | "subscription_type" => "Account".into(),
                        _ => label(key),
                    };
                    let content = scalar(key, value);
                    let prefix = format!("{name:<22} ");
                    if prefix.len() + content.len() <= usize::from(width) {
                        output.push(Line::from(vec![
                            Span::styled(prefix, Style::default().fg(DIM)),
                            Span::raw(content),
                        ]));
                    } else {
                        text_lines(
                            &format!("{name}: {content}"),
                            width,
                            Style::default().fg(TEXT),
                            output,
                        );
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                usage_rows(item, key, provider, width, output);
            }
        }
        _ => text_lines(
            &scalar(key, value),
            width,
            Style::default().fg(TEXT),
            output,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(value: Value, provider: provider::Provider, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        usage_rows(&value, "", provider, width, &mut lines);
        lines
    }

    #[test]
    fn provider_windows_show_used_or_remaining_and_handle_narrow_widths() {
        let claude = serde_json::json!({"rate_limits": {
            "seven_day": {"utilization":2,"resets_at":"2027-01-15T08:00:00Z"},
            "seven_day_fable":{"utilization":0}
        }});
        let lines = render(claude, provider::Provider::Claude, 60);
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Current week (all models)"));
        assert!(text.contains("Current week (Fable)"));
        assert!(text.contains("2% used") && text.contains("0% used") && text.contains("Resets"));
        let codex = serde_json::json!({"limits":{"rateLimits":{"secondary":{
            "usedPercent":4,"windowDurationMins":10080,"resetsAt":1800000000
        }}}});
        let lines = render(codex.clone(), provider::Provider::Codex, 60);
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().contains("96% left"))
        );
        assert!(lines.iter().any(|line| line.to_string() == "Weekly limit"));
        for width in [1, 8, 20] {
            assert!(
                render(codex.clone(), provider::Provider::Codex, width)
                    .iter()
                    .all(|line| line.width() <= usize::from(width))
            );
        }
    }
}
