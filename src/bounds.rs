//! Cached file boundaries from immutable, local Git objects.
use crate::{
    app::{App, Message, View},
    context::Direction,
    diff::{DiffFile, Hunk, Snapshot},
    process::{self, Cancel},
    repo, storage,
    workflow::Target,
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs, path::Path};

#[derive(Default)]
pub struct State {
    pub data: Option<Bounds>,
    pub loading: bool,
    pub error: Option<String>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Bounds {
    pub old: u64,
    pub new: u64,
}
impl Bounds {
    pub fn can_expand(self, hunk: &Hunk, direction: Direction) -> bool {
        match direction {
            Direction::Above => {
                hunk.lines
                    .iter()
                    .filter_map(|line| line.old)
                    .min()
                    .is_some_and(|n| n > 1)
                    || hunk
                        .lines
                        .iter()
                        .filter_map(|line| line.new)
                        .min()
                        .is_some_and(|n| n > 1)
            }
            Direction::Below => {
                hunk.lines
                    .iter()
                    .filter_map(|line| line.old)
                    .max()
                    .unwrap_or(0)
                    < self.old
                    || hunk
                        .lines
                        .iter()
                        .filter_map(|line| line.new)
                        .max()
                        .unwrap_or(0)
                        < self.new
            }
        }
    }
}
fn lines(root: &Path, revision: &str, path: &str, cancel: &Cancel) -> Result<u64> {
    ensure!(
        revision.len() == 40 && revision.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid pinned revision"
    );
    let source = process::checked(
        repo::git(root).args(["cat-file", "blob", &format!("{revision}:{path}")]),
        cancel,
    )?;
    Ok(u64::try_from(source.lines().count())?)
}
pub fn load(
    root: &Path,
    snapshot: &Snapshot,
    file: &DiffFile,
    cache: &Path,
    cancel: &Cancel,
) -> Result<Bounds> {
    let key = storage::hash(serde_json::to_vec(&(
        1,
        &snapshot.merge_base,
        &snapshot.head,
        &file.old_path,
        &file.path,
    ))?);
    let path = cache.join(format!("bounds-{key}.json"));
    if path.exists() {
        return Ok(serde_json::from_slice(&fs::read(path)?)?);
    }
    let bounds = Bounds {
        old: if file.status.starts_with('A') {
            0
        } else {
            lines(root, &snapshot.merge_base, &file.old_path, cancel)?
        },
        new: if file.status.starts_with('D') {
            0
        } else {
            lines(root, &snapshot.head, &file.path, cancel)?
        },
    };
    cancel.check()?;
    storage::atomic_json(&path, &bounds)?;
    Ok(bounds)
}
impl App {
    pub(crate) fn load_visible_bounds(&mut self) {
        if self.home || self.view == View::Overview {
            return;
        }
        let Some(id) = self.key() else { return };
        let paths = self
            .document
            .as_ref()
            .map(|doc| {
                doc.rows
                    .iter()
                    .skip(self.scroll)
                    .take(self.viewport.max(1))
                    .filter_map(|row| match &row.right.target {
                        Some(Target::Code { path, .. } | Target::Header { path, .. }) => {
                            Some(path.clone())
                        }
                        _ => None,
                    })
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let Some(review) = self.reviews.get_mut(&id) else {
            return;
        };
        let (Some(root), Some(snapshot)) = (review.root.clone(), review.snapshot.clone()) else {
            return;
        };
        if review.bounds.values().any(|state| state.loading) {
            return;
        }
        let mut needed = Vec::new();
        for path in paths {
            if review
                .context
                .get(&path)
                .is_some_and(|state| state.data.is_some())
            {
                continue;
            }
            let Some(file) = snapshot.files.iter().find(|file| {
                file.path == path && file.hunks.iter().any(|h| h.header.starts_with("@@ "))
            }) else {
                continue;
            };
            let state = review.bounds.entry(path).or_default();
            if state.data.is_some() || state.error.is_some() {
                continue;
            }
            state.loading = true;
            needed.push(file.clone());
        }
        if needed.is_empty() {
            return;
        }
        let cache = self.storage.cache.clone();
        self.spawn(move |tx, cancel| {
            for file in needed {
                let output =
                    load(&root, &snapshot, &file, &cache, &cancel).map_err(|e| format!("{e:#}"));
                let _ = tx.send(Message::Bounds(
                    id.clone(),
                    snapshot.clone(),
                    file.path,
                    output,
                ));
            }
        });
    }
}
