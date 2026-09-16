use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiffFile {
    pub path: String,
    pub old_path: String,
    pub status: String,
    pub hunks: Vec<Hunk>,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hunk {
    pub id: String,
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old: Option<u64>,
    pub new: Option<u64>,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Remove,
    Meta,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub base: String,
    pub head: String,
    pub merge_base: String,
    pub head_tree: String,
    pub base_tree: String,
    pub files: Vec<DiffFile>,
}
impl Snapshot {
    pub fn units(&self) -> impl Iterator<Item = (&DiffFile, &Hunk)> {
        self.files
            .iter()
            .flat_map(|file| file.hunks.iter().map(move |h| (file, h)))
    }
    pub fn find(&self, id: &str) -> Option<(&DiffFile, &Hunk)> {
        self.units().find(|(_, h)| h.id == id)
    }
}

/// Pair Git's NUL-delimited paths with its patch sections, avoiding ambiguity in
/// spaces, quotes, Unicode, renames, or a source line that resembles a header.
pub fn parse(names: &str, patch: &str) -> Result<Vec<DiffFile>> {
    let mut names = names.split('\0').filter(|p| !p.is_empty());
    let mut files = Vec::new();
    while let Some(status) = names.next() {
        let old_path = names.next().context("Missing Git path")?.to_owned();
        let path = if status.starts_with('R') || status.starts_with('C') {
            names.next().context("Missing renamed path")?.to_owned()
        } else {
            old_path.clone()
        };
        files.push(DiffFile {
            path,
            old_path,
            status: status.into(),
            hunks: Vec::new(),
            additions: 0,
            deletions: 0,
        });
    }
    let mut sections: Vec<Vec<&str>> = Vec::new();
    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            sections.push(vec![line]);
        } else if let Some(section) = sections.last_mut() {
            section.push(line);
        } else {
            ensure!(line.is_empty(), "Unexpected text before Git patch");
        }
    }
    ensure!(
        sections.len() == files.len(),
        "Git returned {} paths but {} patch sections; refusing an incomplete diff",
        files.len(),
        sections.len()
    );
    for (fi, (file, section)) in files.iter_mut().zip(sections).enumerate() {
        let mut old = 0;
        let mut new = 0;
        let mut expected_old = 0;
        let mut expected_new = 0;
        let mut actual_old = 0;
        let mut actual_new = 0;
        let mut metadata = Vec::new();
        for line in section.into_iter().skip(1) {
            if line.starts_with("@@ ") {
                if !file.hunks.is_empty() {
                    ensure!(
                        actual_old == expected_old && actual_new == expected_new,
                        "Incomplete diff hunk in {}",
                        file.path
                    );
                }
                let parts: Vec<_> = line.split_whitespace().collect();
                let ["@@", before, after, "@@", ..] = parts.as_slice() else {
                    anyhow::bail!("Invalid Git hunk header");
                };
                (old, expected_old) = range(before, '-')?;
                (new, expected_new) = range(after, '+')?;
                actual_old = 0;
                actual_new = 0;
                file.hunks.push(Hunk {
                    id: format!("f{fi}-h{}", file.hunks.len()),
                    header: line.into(),
                    lines: Vec::new(),
                });
            } else if let Some(hunk) = file.hunks.last_mut() {
                let (kind, old_line, new_line) = match line.as_bytes().first() {
                    Some(b'+') => {
                        let n = new;
                        new = new.checked_add(1).context("Diff line number overflow")?;
                        actual_new += 1;
                        file.additions += 1;
                        (LineKind::Add, None, Some(n))
                    }
                    Some(b'-') => {
                        let o = old;
                        old = old.checked_add(1).context("Diff line number overflow")?;
                        actual_old += 1;
                        file.deletions += 1;
                        (LineKind::Remove, Some(o), None)
                    }
                    Some(b' ') => {
                        let o = old;
                        let n = new;
                        old = old.checked_add(1).context("Diff line number overflow")?;
                        new = new.checked_add(1).context("Diff line number overflow")?;
                        actual_old += 1;
                        actual_new += 1;
                        (LineKind::Context, Some(o), Some(n))
                    }
                    Some(b'\\') => (LineKind::Meta, None, None),
                    _ => anyhow::bail!("Invalid diff line in {}", file.path),
                };
                hunk.lines.push(DiffLine {
                    kind,
                    old: old_line,
                    new: new_line,
                    text: if kind == LineKind::Meta {
                        line.into()
                    } else {
                        line.get(1..).context("Invalid diff line prefix")?.into()
                    },
                });
            } else {
                metadata.push(line.to_owned());
            }
        }
        if !file.hunks.is_empty() {
            ensure!(
                actual_old == expected_old && actual_new == expected_new,
                "Incomplete final diff hunk in {}",
                file.path
            );
            metadata.retain(|line| {
                line.starts_with("old mode ")
                    || line.starts_with("new mode ")
                    || line.starts_with("rename ")
                    || line.starts_with("copy ")
            });
        }
        if file.hunks.is_empty() || !metadata.is_empty() {
            // Binary, empty-file, rename-only, permission and submodule changes
            // remain explicit review units, so they cannot silently disappear.
            file.hunks.push(Hunk {
                id: format!("f{fi}-meta"),
                header: "File metadata".into(),
                lines: metadata
                    .into_iter()
                    .map(|text| DiffLine {
                        kind: LineKind::Meta,
                        old: None,
                        new: None,
                        text,
                    })
                    .collect(),
            });
        } else {
            ensure!(
                actual_old == expected_old && actual_new == expected_new,
                "Incomplete final diff hunk in {}",
                file.path
            );
        }
    }
    Ok(files)
}
fn range(value: &str, prefix: char) -> Result<(u64, u64)> {
    let value = value.strip_prefix(prefix).context("Invalid diff range")?;
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    Ok((start.parse()?, count.parse()?))
}

/// Pair adjacent removal/addition runs for side-by-side rendering, retaining
/// context and no-newline markers. Every row is still backed by an original line.
pub fn split_rows(hunk: &Hunk) -> Vec<(Option<&DiffLine>, Option<&DiffLine>)> {
    let mut rows = Vec::new();
    let mut lines = hunk.lines.iter().peekable();
    while let Some(line) = lines.next() {
        if line.kind == LineKind::Remove {
            let mut removed = vec![line];
            while lines.peek().is_some_and(|l| l.kind == LineKind::Remove) {
                if let Some(line) = lines.next() {
                    removed.push(line);
                }
            }
            let mut added = Vec::new();
            while lines.peek().is_some_and(|l| l.kind == LineKind::Add) {
                if let Some(line) = lines.next() {
                    added.push(line);
                }
            }
            for n in 0..removed.len().max(added.len()) {
                rows.push((removed.get(n).copied(), added.get(n).copied()));
            }
        } else {
            rows.push(match line.kind {
                LineKind::Add => (None, Some(line)),
                _ => (Some(line), Some(line)),
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rename_and_non_text_changes_remain_covered() -> Result<()> {
        let files = parse(
            "R100\0old name\0new name\0M\0image.png\0",
            "diff --git a/old name b/new name\nsimilarity index 100%\nrename from old name\nrename to new name\ndiff --git a/image.png b/image.png\nBinary files a/image.png and b/image.png differ\n",
        )?;
        assert_eq!(
            files.first().context("Missing renamed file")?.path,
            "new name"
        );
        assert_eq!(
            files
                .get(1)
                .and_then(|f| f.hunks.first())
                .context("Missing metadata")?
                .id,
            "f1-meta"
        );
        Ok(())
    }
    #[test]
    fn parses_zero_ranges_header_like_source_and_unicode() -> Result<()> {
        let files = parse(
            "A\0界.txt\0",
            "diff --git a/界.txt b/界.txt\nnew file mode 100644\n--- /dev/null\n+++ b/界.txt\n@@ -0,0 +1,2 @@\n+diff --git fake\n+hello\n\\ No newline at end of file\n",
        )?;
        let file = files.first().context("Missing added file")?;
        assert_eq!(file.additions, 2);
        assert_eq!(
            file.hunks
                .first()
                .and_then(|h| h.lines.get(1))
                .context("Missing line")?
                .new,
            Some(2)
        );
        Ok(())
    }
    #[test]
    fn refuses_truncated_patch() {
        assert!(parse("M\0x\0", "diff --git a/x b/x\n@@ -1,2 +1,2 @@\n one\n").is_err());
    }
    #[test]
    fn unequal_replacements_keep_every_line() -> Result<()> {
        let files = parse(
            "M\0x\0",
            "diff --git a/x b/x\n@@ -1,2 +1,3 @@\n-a\n-b\n+c\n+d\n+e\n",
        )?;
        let rows = split_rows(
            files
                .first()
                .and_then(|f| f.hunks.first())
                .context("Missing hunk")?,
        );
        assert_eq!(rows.len(), 3);
        let row = rows.get(2).context("Missing replacement")?;
        assert!(row.0.is_none());
        assert_eq!(row.1.context("Missing added line")?.text, "e");
        Ok(())
    }
}
