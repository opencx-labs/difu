//! Copy source text, never rendered gutters, wrapping, or terminal styling.
use crate::{
    app::{App, Focus, Message, Notice, View},
    context::FileContext,
    diff::DiffLine,
    repo,
    review::{Anchor, Side},
    workflow::Target,
};
use anyhow::{Context, Result, ensure};
use crossterm::{clipboard::CopyToClipboard, execute};
use std::{collections::BTreeMap, io::Write};

/// None means hidden context is needed. Never silently copy a partial range.
fn extract<'a>(
    anchor: &Anchor,
    lines: impl Iterator<Item = &'a DiffLine>,
) -> Result<Option<String>> {
    ensure!(
        anchor.start > 0 && anchor.start <= anchor.end,
        "Invalid copy range"
    );
    let mut source = BTreeMap::new();
    for line in lines {
        let number = match anchor.side {
            Side::Left => line.old,
            Side::Right => line.new,
        };
        if let Some(number) = number.filter(|n| *n >= anchor.start && *n <= anchor.end)
            && let Some(previous) = source.insert(number, line.text.as_str())
        {
            ensure!(
                previous == line.text,
                "Conflicting source lines in copy range"
            );
        }
    }
    if u64::try_from(source.len())? != anchor.end - anchor.start + 1 {
        return Ok(None);
    }
    Ok(Some(source.into_values().collect::<Vec<_>>().join("\n")))
}

impl App {
    pub(crate) fn copy_diff(&mut self) {
        if self.home || self.view == View::Overview || self.focus != Focus::Content {
            return;
        }
        self.clipboard_id = self.clipboard_id.wrapping_add(1);
        self.clipboard = None;
        let request = self.clipboard_id;
        let target = self
            .document
            .as_ref()
            .and_then(|doc| doc.rows.get(self.workflow.cursor.unwrap_or(self.scroll)))
            .and_then(|row| row.right.target.clone());
        let anchor = match target {
            Some(Target::Header { path, .. }) => {
                self.clipboard = Some(path);
                return;
            }
            Some(target) => target.line(self.workflow.side),
            None => None,
        };
        let Some(mut anchor) = anchor else {
            self.notice = Notice::info("Choose a code line; Left/Right selects the old/new side");
            return;
        };
        if let Some(start) = &self.workflow.selection
            && start.path == anchor.path
            && start.side == anchor.side
        {
            anchor.start = start.start.min(anchor.end);
            anchor.end = start.start.max(anchor.end);
        }
        let Some(id) = self.key() else { return };
        let Some(review) = self.reviews.get_mut(&id) else {
            return;
        };
        let Some(snapshot) = review.snapshot.clone() else {
            return;
        };
        let Some(file) = snapshot.files.iter().find(|file| file.path == anchor.path) else {
            return;
        };
        let available =
            if let Some(context) = review.context.get(&file.path).and_then(|c| c.data.as_ref()) {
                extract(&anchor, context.lines.iter())
            } else {
                extract(&anchor, file.hunks.iter().flat_map(|h| &h.lines))
            };
        match available {
            Ok(Some(text)) => {
                self.clipboard = Some(text);
                return;
            }
            Err(error) => {
                self.notice = Notice::error(format!("Could not copy: {error:#}"));
                return;
            }
            Ok(None) => {}
        }
        let Some(root) = review.root.clone() else {
            self.notice =
                Notice::error("Could not copy: local repository is unavailable for hidden context");
            return;
        };
        review.context.entry(anchor.path.clone()).or_default();
        self.notice = Notice::info("Reading selected lines from the local PR revision…");
        self.spawn(move |tx, cancel| {
            let output = (|| -> Result<String> {
                let file = snapshot
                    .files
                    .iter()
                    .find(|f| f.path == anchor.path)
                    .context("File missing from snapshot")?;
                let context =
                    FileContext::new(file, repo::file_context(&root, &snapshot, file, &cancel)?)?;
                let text = extract(&anchor, context.lines.iter())?
                    .context("Selected source lines are unavailable")?;
                let _ = tx.send(Message::Context(id, snapshot, anchor.path, Ok(context)));
                Ok(text)
            })();
            let _ = tx.send(Message::Clipboard(
                request,
                output.map_err(|e| format!("{e:#}")),
            ));
        });
    }

    /// Run on the UI thread so OSC output never interleaves with a frame.
    pub fn flush_clipboard(&mut self, output: &mut impl Write) {
        let Some(text) = self.clipboard.take() else {
            return;
        };
        self.notice = match execute!(output, CopyToClipboard::to_clipboard_from(text)) {
            Ok(()) => Notice::info("Sent to terminal clipboard"),
            Err(error) => Notice::error(format!("Could not write to terminal clipboard: {error}")),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::LineKind;
    #[test]
    fn copy_preserves_source_and_requires_every_line_on_the_selected_side() -> Result<()> {
        let anchor = Anchor {
            path: "file".into(),
            side: Side::Right,
            start: 1,
            end: 2,
        };
        let lines = [
            DiffLine {
                kind: LineKind::Remove,
                old: Some(1),
                new: None,
                text: "old".into(),
            },
            DiffLine {
                kind: LineKind::Add,
                old: None,
                new: Some(1),
                text: "\t界 = new();  ".into(),
            },
            DiffLine {
                kind: LineKind::Context,
                old: Some(2),
                new: Some(2),
                text: String::new(),
            },
        ];
        assert_eq!(
            extract(&anchor, lines.iter())?,
            Some("\t界 = new();  \n".into())
        );
        assert_eq!(
            extract(
                &Anchor {
                    side: Side::Left,
                    ..anchor.clone()
                },
                lines.iter()
            )?,
            Some("old\n".into())
        );
        assert_eq!(extract(&Anchor { end: 3, ..anchor }, lines.iter())?, None);
        Ok(())
    }
    #[test]
    fn osc_output_encodes_controls_and_reports_write_errors() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut app = App::new(
            crate::storage::Storage {
                config: directory.path().join("config.json"),
                cache: directory.path().into(),
            },
            Default::default(),
        );
        app.clipboard = Some("\x1b]x".into());
        let mut output = Vec::new();
        app.flush_clipboard(&mut output);
        assert_eq!(output, b"\x1b]52;c;G114\x1b\\");
        assert!(app.clipboard.is_none());
        assert_eq!(app.notice.message, "Sent to terminal clipboard");
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("fixture"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        app.clipboard = Some("code".into());
        app.flush_clipboard(&mut Broken);
        assert!(app.notice.message.contains("Could not write"));
        Ok(())
    }
}
