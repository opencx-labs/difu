//! Explicit workspace changes retain history and validate repository ownership.
use super::{
    Job, Session,
    workspace::{self, Workspace},
};
use crate::{
    process::{self, Cancel},
    repo,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(super) const TOOL: &str = "difu_register_worktree";
pub(super) const INSTRUCTIONS: &str = "Before switching to another worktree, call difu_register_worktree with its absolute path and, optionally, its comparison base branch. The worktree must already exist and belong to this repository. Call this tool alone, after other tool calls finish, then stop: difu will resume the same conversation in the registered workspace. Do not merely change shell directories; difu needs this registration to follow changes and PRs. Previous worktrees and PR history are retained.";
pub(super) const CONTINUE: &str = "The registered worktree is now your working directory. Continue the user's task from the existing conversation. Re-read relevant files in this workspace. Do not repeat completed actions or publish unless requested. Do not run local validation suites; rely on PR CI.";

pub(super) fn tool() -> Value {
    json!({"type":"function","name":TOOL,"description":"Register an existing worktree as this session's active workspace. Call alone, then stop; difu resumes this conversation there. Retains previous worktrees and PRs.","inputSchema":{"type":"object","properties":{"path":{"type":"string","description":"Absolute path to an existing worktree in the same repository"},"base":{"type":"string","description":"Optional local comparison branch/ref; defaults to the repository default branch"}},"required":["path"],"additionalProperties":false}})
}

fn read(path: &Path, args: &[&str], cancel: &Cancel) -> Result<String> {
    process::checked(repo::git(path).args(args), cancel).map(|s| s.trim().to_owned())
}
fn common(path: &Path, cancel: &Cancel) -> Result<PathBuf> {
    path.join(read(path, &["rev-parse", "--git-common-dir"], cancel)?)
        .canonicalize()
        .context("Cannot resolve Git common directory")
}

#[derive(Clone)]
pub(super) struct Prepared {
    pub workspace: Workspace,
    baseline: String,
}
pub(super) fn prepare(session: &Session, args: &Value, cancel: &Cancel) -> Result<Prepared> {
    ensure!(
        matches!(session.job, Job::Coding(_)),
        "Only coding sessions have worktrees"
    );
    let path = Path::new(
        args.get("path")
            .and_then(Value::as_str)
            .context("Missing worktree path")?,
    );
    ensure!(path.is_absolute(), "Provide an absolute worktree path");
    let path = path.canonicalize().context("Worktree does not exist")?;
    let root = workspace::repository(&path, cancel)?;
    ensure!(
        path == root,
        "Provide the worktree root, not a subdirectory"
    );
    let repository = workspace::repository(session.job.root(), cancel)?;
    ensure!(
        root != repository,
        "Register an isolated worktree, not the original checkout"
    );
    ensure!(
        common(&root, cancel)? == common(&repository, cancel)?,
        "Worktree belongs to another repository"
    );
    // A linked checkout has its own Git directory, distinct from the common directory.
    let git_dir =
        PathBuf::from(read(&root, &["rev-parse", "--absolute-git-dir"], cancel)?).canonicalize()?;
    ensure!(
        git_dir != common(&root, cancel)?,
        "The original checkout cannot be registered as an editing worktree"
    );
    let branch = read(&root, &["branch", "--show-current"], cancel)?;
    ensure!(
        !branch.is_empty(),
        "Register a worktree with a named branch"
    );
    let base = args
        .get("base")
        .filter(|v| !v.is_null())
        .map(|v| {
            let base = v
                .as_str()
                .context("Comparison base must be a branch or revision")?
                .trim();
            ensure!(
                !base.is_empty() && !base.starts_with('-'),
                "Invalid comparison base"
            );
            read(
                &root,
                &[
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    &format!("{base}^{{commit}}"),
                ],
                cancel,
            )?;
            Ok::<_, anyhow::Error>(base.to_owned())
        })
        .transpose()?;
    let baseline = read(&root, &["rev-parse", "--verify", "HEAD^{commit}"], cancel)?;
    Ok(Prepared {
        workspace: Workspace {
            path: root,
            branch: Some(branch),
            base,
        },
        baseline,
    })
}
pub(super) fn apply(session: &mut Session, prepared: &Prepared) {
    if let Some(path) = &session.workspace {
        let previous = Workspace {
            path: path.clone(),
            branch: session.branch.clone(),
            base: session.comparison_base.clone(),
        };
        if !session
            .workspaces
            .iter()
            .any(|w| w.path == previous.path && w.branch == previous.branch)
        {
            session.workspaces.push(previous);
        }
        for artifact in &mut session.artifacts {
            if artifact.workspace.is_none() {
                artifact.workspace = Some(path.clone());
            }
        }
    }
    session
        .workspaces
        .retain(|w| w.path != prepared.workspace.path || w.branch != prepared.workspace.branch);
    session.workspaces.push(prepared.workspace.clone());
    session.workspace_revision = session.workspace_revision.saturating_add(1);
    session.workspace = Some(prepared.workspace.path.clone());
    session.branch = prepared.workspace.branch.clone();
    session.comparison_base = prepared.workspace.base.clone();
    session.baseline = Some(prepared.baseline.clone());
    session.workspace_ready = true;
    session.workspace_removed = false;
    session.guidance_checked = false;
}

/// Double-quoted paths follow JSON escaping; an existing unquoted path may contain spaces.
pub(super) fn parse_command(args: &str) -> Result<(PathBuf, Option<String>)> {
    let args = args.trim();
    ensure!(!args.is_empty(), "Usage: /worktree <absolute path> [base]");
    let (path, rest) = if args.starts_with('"') {
        let mut values = serde_json::Deserializer::from_str(args).into_iter::<String>();
        let path = values.next().context("Missing worktree path")??;
        (
            PathBuf::from(path),
            args.get(values.byte_offset()..).unwrap_or_default().trim(),
        )
    } else if Path::new(args).is_dir() {
        (PathBuf::from(args), "")
    } else if let Some((path, base)) = args.rsplit_once(char::is_whitespace) {
        (PathBuf::from(path.trim()), base.trim())
    } else {
        (PathBuf::from(args), "")
    };
    ensure!(
        !rest.chars().any(char::is_whitespace),
        "Provide one base ref; quote paths containing spaces"
    );
    Ok((path, (!rest.is_empty()).then(|| rest.to_owned())))
}
