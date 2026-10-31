//! Literal source and tool output must never pass through Markdown's prose parser.
use super::*;

pub(super) fn literal(text: &str, width: u16) -> Vec<Line<'static>> {
    text.lines()
        .flat_map(|line| {
            let source = crate::model::clean(line).replace('\t', "    ");
            let rows =
                crate::markdown::wrap(crate::ui::syntax_spans(&source), usize::from(width).max(1));
            if rows.is_empty() {
                vec![Line::default()]
            } else {
                rows.into_iter().map(|row| Line::from(row.spans)).collect()
            }
        })
        .collect()
}

pub(super) fn output(text: &str, width: u16) -> Vec<Line<'static>> {
    if patch::is_diff(text) {
        patch::output(text, width)
    } else {
        literal(text, width)
    }
}

fn heading(rows: &mut Vec<Line<'static>>, title: &str, width: u16) {
    if !rows.is_empty() {
        rows.push(Line::default());
    }
    rows.extend(
        wrapped(title, width)
            .into_iter()
            .map(|line| Line::styled(line, Style::default().fg(DIM))),
    );
}

fn replacement(rows: &mut Vec<Line<'static>>, edit: &Value, width: u16) {
    let mut diff = String::from("@@\n");
    for (key, marker) in [("old_string", '-'), ("new_string", '+')] {
        if let Some(text) = edit.get(key).and_then(Value::as_str) {
            for line in text.lines() {
                diff.push(marker);
                diff.push_str(line);
                diff.push('\n');
            }
        }
    }
    rows.extend(patch::output(&diff, width));
}

/// Native Claude code tools store their input and result separately. Their
/// combined `text` is a legacy fallback, not a Markdown document.
pub(super) fn claude(entry: &Entry, width: u16) -> Option<Vec<Line<'static>>> {
    if entry.kind != "mcpToolCall"
        || entry.data.get("server").and_then(Value::as_str) != Some("Claude Code")
    {
        return None;
    }
    let tool = entry.data.get("tool").and_then(Value::as_str)?;
    if !matches!(
        tool,
        "Bash" | "Read" | "Edit" | "MultiEdit" | "Write" | "Grep" | "Glob"
    ) {
        return None;
    }
    let Some(arguments) = entry
        .data
        .get("arguments")
        .filter(|value| value.is_object())
    else {
        return Some(output(&entry.text, width));
    };
    let mut rows = Vec::new();
    let mut remaining = arguments.clone();
    let input = remaining.as_object_mut()?;
    if tool == "Bash" {
        if let Some(command) = input.remove("command") {
            heading(&mut rows, "Command", width);
            rows.extend(literal(command.as_str().unwrap_or_default(), width));
        }
    } else if matches!(tool, "Edit" | "MultiEdit" | "Write") {
        let path = input.remove("file_path").unwrap_or(Value::Null);
        heading(
            &mut rows,
            &format!("Changes requested · {}", path.as_str().unwrap_or_default()),
            width,
        );
        match tool {
            "Edit" => {
                replacement(&mut rows, arguments, width);
                input.remove("old_string");
                input.remove("new_string");
            }
            "MultiEdit" => {
                if let Some(edits) = input
                    .remove("edits")
                    .and_then(|edits| edits.as_array().cloned())
                {
                    for (index, edit) in edits.iter().enumerate() {
                        if index > 0 {
                            rows.push(Line::styled("    ⋮", Style::default().fg(DIM)));
                        }
                        replacement(&mut rows, edit, width);
                    }
                }
            }
            "Write" => {
                if let Some(content) = input.remove("content") {
                    rows.extend(literal(content.as_str().unwrap_or_default(), width));
                }
            }
            _ => {}
        }
    }
    if !input.is_empty() {
        heading(&mut rows, "Input", width);
        rows.extend(literal(
            &serde_json::to_string_pretty(&remaining).unwrap_or_default(),
            width,
        ));
    }
    let result = if let Some(result) = entry.data.get("result") {
        super::super::claude::events::text(result)
    } else {
        // Older child transcripts have only the combined input/output text.
        let input = arguments.to_string();
        entry
            .text
            .strip_prefix(&input)
            .map(|text| text.strip_prefix('\n').unwrap_or(text))
            .unwrap_or(&entry.text)
            .to_owned()
    };
    if !result.is_empty() {
        heading(&mut rows, "Output", width);
        rows.extend(output(&result, width));
    }
    Some(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use serde_json::json;

    fn text(rows: &[Line<'_>]) -> String {
        rows.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn source_output_keeps_line_breaks_indentation_and_angle_brackets() -> anyhow::Result<()> {
        let source = "pub struct Status {\n    pub hosts: Vec<String>,\n\n    pub error: Option<String>,\n}\n// ``` is literal output\n";
        let arguments =
            json!({"command":"sed -n '1,80p' src/status.rs","description":"Read status"});
        let mut entry = Entry {
            kind: "mcpToolCall".into(),
            text: format!("{arguments}\n{source}").into(),
            data:
                json!({"server":"Claude Code","tool":"Bash","arguments":arguments,"result":source})
                    .into(),
            ..Default::default()
        };
        let rows = claude(&entry, 100).context("native code tool")?;
        let shown = text(&rows);
        assert!(shown.contains("Command\nsed -n '1,80p' src/status.rs"));
        assert!(shown.contains(&format!("Output\n{}", source.trim_end())));
        assert_eq!(shown.matches("pub struct Status").count(), 1);
        assert!(!shown.contains("{\"command\""));
        assert!(rows.iter().flat_map(|row| &row.spans).any(|span| {
            span.content == "pub"
                && span.style.fg == Some(crate::theme::slot(crate::theme::Role::Keyword))
        }));
        // Saved child entries may have only the legacy combined text.
        entry.data.as_object_mut().context("data")?.remove("result");
        assert_eq!(text(&claude(&entry, 100).context("legacy tool")?), shown);
        for width in [16, 32, 60] {
            assert!(
                claude(&entry, width)
                    .context("wrapped tool")?
                    .iter()
                    .all(|row| row.width() <= usize::from(width))
            );
        }
        Ok(())
    }

    #[test]
    fn edit_snippets_are_colored_without_inventing_file_line_numbers() -> anyhow::Result<()> {
        let entry = Entry {
            kind: "mcpToolCall".into(),
            data: json!({"server":"Claude Code","tool":"Edit","status":"failed",
                "arguments":{"file_path":"src/status.rs","old_string":"    let old: Vec<String> = vec![];","new_string":"    let next: Vec<String> = vec![];","replace_all":true},
                "result":[{"type":"text","text":"File not found"}]}).into(),
            ..Default::default()
        };
        let rows = claude(&entry, 80).context("edit")?;
        let shown = text(&rows);
        assert!(shown.contains("Changes requested · src/status.rs"));
        assert!(shown.contains(" -     let old: Vec<String>"));
        assert!(shown.contains(" +     let next: Vec<String>"));
        assert!(!shown.contains("1 -") && !shown.contains("old_string"));
        assert!(
            shown.contains("\"replace_all\": true") && shown.contains("Output\nFile not found")
        );
        for background in [crate::ui::ADD_BG, crate::ui::REMOVE_BG] {
            assert!(rows.iter().any(|row| row.style.bg == Some(background)));
        }
        Ok(())
    }

    #[test]
    fn read_write_and_multiple_edits_keep_code_and_each_replacement() -> anyhow::Result<()> {
        let source = "fn main() {\n    let values: Vec<String> = vec![];\n}";
        for (tool, arguments, result) in [
            ("Read", json!({"file_path":"src/main.rs"}), source),
            (
                "Write",
                json!({"file_path":"src/main.rs","content":source}),
                "File written",
            ),
            (
                "MultiEdit",
                json!({"file_path":"src/main.rs","edits":[
                    {"old_string":"old one","new_string":"new one"},
                    {"old_string":"old two","new_string":"new two"}
                ]}),
                "Edits applied",
            ),
        ] {
            let entry = Entry {
                kind: "mcpToolCall".into(),
                data: json!({"server":"Claude Code","tool":tool,"arguments":arguments,"result":result}).into(),
                ..Default::default()
            };
            let shown = text(&claude(&entry, 80).context("code tool")?);
            if tool == "MultiEdit" {
                for snippet in ["- old one", "+ new one", "- old two", "+ new two"] {
                    assert!(shown.contains(snippet));
                }
            } else {
                assert!(shown.contains(source));
            }
        }
        Ok(())
    }

    #[test]
    fn command_diffs_keep_each_file_and_use_their_real_hunk_coordinates() {
        let diff = "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -10 +20 @@\n-old\n+let one = 1;\ndiff --git a/two.rs b/two.rs\n--- a/two.rs\n+++ b/two.rs\n@@ -30 +40 @@\n-before\n+let two = 2;\n";
        let rows = output(diff, 60);
        let shown = text(&rows);
        for expected in [
            "--- a/one.rs",
            "+++ b/two.rs",
            "10 - old",
            "20 + let one",
            "30 - before",
            "40 + let two",
        ] {
            assert!(shown.contains(expected), "Missing {expected}: {shown}");
        }
        assert_eq!(
            rows.iter()
                .filter(|row| row.style.bg == Some(crate::ui::ADD_BG))
                .count(),
            2
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.style.bg == Some(crate::ui::REMOVE_BG))
                .count(),
            2
        );
    }
}
