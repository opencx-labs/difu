//! Local review comparisons are resolved without fetching or changing the checkout.
use crate::{
    process::{self, Cancel},
    repo,
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub fn discover(roots: &[PathBuf], cancel: &Cancel) -> Result<Vec<PathBuf>> {
    let mut pending = roots.to_vec();
    let mut visited = std::collections::BTreeSet::new();
    let mut repos = std::collections::BTreeSet::new();
    while let Some(path) = pending.pop() {
        cancel.check()?;
        let path = path
            .canonicalize()
            .with_context(|| format!("Cannot scan {}", path.display()))?;
        if !visited.insert(path.clone()) {
            continue;
        }
        if path.join(".git").exists() {
            let root = PathBuf::from(read(&path, &["rev-parse", "--show-toplevel"], cancel)?)
                .canonicalize()?;
            repos.insert(root);
            continue;
        }
        for entry in
            std::fs::read_dir(&path).with_context(|| format!("Cannot scan {}", path.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.')
                || matches!(
                    name.as_ref(),
                    "node_modules"
                        | "target"
                        | "dist"
                        | "build"
                        | "vendor"
                        | "coverage"
                        | "venv"
                        | "__pycache__"
                )
            {
                continue;
            }
            pending.push(entry.path());
        }
    }
    Ok(repos.into_iter().collect())
}

pub fn worktrees(root: &Path, cancel: &Cancel) -> Result<Vec<PathBuf>> {
    let output = read(root, &["worktree", "list", "--porcelain", "-z"], cancel)?;
    let mut paths = std::collections::BTreeSet::new();
    for field in output.split('\0') {
        if let Some(path) = field.strip_prefix("worktree ") {
            let path = PathBuf::from(path);
            // Git can retain entries for worktrees that were removed externally.
            if path.is_dir() {
                paths.insert(path.canonicalize()?);
            }
        }
    }
    Ok(paths.into_iter().collect())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Comparison {
    /// Include staged, unstaged, and non-ignored untracked files against HEAD.
    WorkingTree { head: String },
    /// Review only committed branch changes, excluding later working-tree edits.
    Branch { head: String, merge_base: String },
}

#[derive(Clone, Debug)]
pub struct Checkout {
    pub root: PathBuf,
    pub branch: String,
    pub comparison: Comparison,
}

fn read(root: &Path, args: &[&str], cancel: &Cancel) -> Result<String> {
    process::checked(
        repo::git(root).env("GIT_OPTIONAL_LOCKS", "0").args(args),
        cancel,
    )
    .map(|value| value.trim_end_matches('\n').to_owned())
}

impl Checkout {
    pub fn resolve(path: &Path, cancel: &Cancel) -> Result<Self> {
        let root =
            PathBuf::from(read(path, &["rev-parse", "--show-toplevel"], cancel)?).canonicalize()?;
        let branch = read(&root, &["rev-parse", "--abbrev-ref", "HEAD"], cancel)?;
        let head = read(&root, &["rev-parse", "--verify", "HEAD^{commit}"], cancel)?;
        let comparison = if branch == "main" {
            Comparison::WorkingTree { head }
        } else {
            let merge_base = read(&root, &["merge-base", "refs/heads/main", &head], cancel)
                .context("Cannot compare this branch with local main; a local main branch and shared history are required")?;
            Comparison::Branch { head, merge_base }
        };
        Ok(Self {
            root,
            branch,
            comparison,
        })
    }
}

/// Git rename/copy scores follow the status letter. Untracked files are additions.
pub fn status_letter(status: &str) -> char {
    match status.chars().next() {
        Some('?') => 'A',
        Some(value) => value,
        None => ' ',
    }
}

const LIMIT: usize = 32 * 1024 * 1024;
fn patch_output(
    root: &Path,
    args: &[&str],
    cancel: &Cancel,
    difference_exit: bool,
) -> Result<String> {
    let output = process::run_limited(
        repo::git(root).env("GIT_OPTIONAL_LOCKS", "0").args(args),
        cancel,
        LIMIT,
    )?;
    anyhow::ensure!(
        output.code == 0 || (difference_exit && output.code == 1),
        "Cannot read local diff: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).context("Local diff is not UTF-8")
}

pub fn files(
    checkout: &Checkout,
    context: u32,
    cancel: &Cancel,
) -> Result<Vec<crate::diff::DiffFile>> {
    let root = &checkout.root;
    let revisions: Vec<&str> = match &checkout.comparison {
        Comparison::WorkingTree { head } => vec![head],
        Comparison::Branch { head, merge_base } => vec![merge_base, head],
    };
    let common = [
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--find-renames",
        "--ignore-submodules=none",
    ];
    let mut names_args = common.to_vec();
    names_args.extend(["--name-status", "-z"]);
    names_args.extend(revisions.iter().copied());
    names_args.push("--");
    let mut names = read(root, &names_args, cancel)?;
    let unified = format!("--unified={context}");
    let mut args = common.to_vec();
    args.extend([
        "--no-color",
        unified.as_str(),
        "--src-prefix=a/",
        "--dst-prefix=b/",
    ]);
    args.extend(revisions);
    args.push("--");
    let mut patch = patch_output(root, &args, cancel, false)?;
    if matches!(checkout.comparison, Comparison::WorkingTree { .. }) {
        let untracked = read(
            root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
            cancel,
        )?;
        for path in untracked.split('\0').filter(|path| !path.is_empty()) {
            cancel.check()?;
            let extra = patch_output(
                root,
                &[
                    "diff",
                    "--no-index",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-color",
                    &unified,
                    "--src-prefix=a/",
                    "--dst-prefix=b/",
                    "--",
                    "/dev/null",
                    path,
                ],
                cancel,
                true,
            )?;
            // Empty untracked files still produce Git's metadata-only addition patch.
            if !extra.is_empty() {
                names.push_str(&format!("A\0{path}\0"));
                patch.push_str(&extra);
            }
            anyhow::ensure!(
                patch.len() <= LIMIT,
                "Local diff exceeds the 32 MiB display limit"
            );
        }
    }
    crate::diff::parse(&names, &patch)
}

pub fn snapshot(checkout: &Checkout, cancel: &Cancel) -> Result<crate::diff::Snapshot> {
    let files = files(checkout, 3, cancel)?;
    let (head, base) = match &checkout.comparison {
        Comparison::WorkingTree { head } => (head, head),
        Comparison::Branch { head, merge_base } => (head, merge_base),
    };
    let base_tree = read(
        &checkout.root,
        &["rev-parse", &format!("{base}^{{tree}}")],
        cancel,
    )?;
    let head_tree = if matches!(checkout.comparison, Comparison::WorkingTree { .. }) {
        crate::storage::hash(serde_json::to_vec(&files)?)
    } else {
        read(
            &checkout.root,
            &["rev-parse", &format!("{head}^{{tree}}")],
            cancel,
        )?
    };
    Ok(crate::diff::Snapshot {
        base: base.clone(),
        head: head.clone(),
        merge_base: base.clone(),
        base_tree,
        head_tree,
        files,
    })
}

pub fn file_context(
    checkout: &Checkout,
    snapshot: &crate::diff::Snapshot,
    file: &crate::diff::DiffFile,
    cancel: &Cancel,
) -> Result<Vec<crate::diff::DiffLine>> {
    if matches!(checkout.comparison, Comparison::Branch { .. }) {
        return repo::file_context(&checkout.root, snapshot, file, cancel);
    }
    let current = files(checkout, 3, cancel)?;
    anyhow::ensure!(
        serde_json::to_vec(&current)? == serde_json::to_vec(&snapshot.files)?,
        "Local changes moved since this diff opened. Refresh before expanding context."
    );
    let full = files(checkout, 2_147_483_647, cancel)?;
    let full = full
        .into_iter()
        .find(|f| f.path == file.path && f.old_path == file.old_path)
        .context("The local file is no longer in the diff")?;
    Ok(full
        .hunks
        .into_iter()
        .filter(|h| h.header.starts_with("@@ "))
        .flat_map(|h| h.lines)
        .collect())
}
