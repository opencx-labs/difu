//! Copy only explicitly approved missing local guidance into an isolated worktree.
use super::{
    Control, Job, Pending, Session, Status,
    server::{Command, Store},
};
use crate::{
    process::{self, Cancel},
    repo,
};
use anyhow::{Context, Result, ensure};
use serde_json::json;
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::{Arc, mpsc},
    time::Duration,
};

struct File {
    relative: PathBuf,
    bytes: Vec<u8>,
    mode: u32,
}
fn regular_path(root: &Path, relative: &Path, missing: bool) -> Result<PathBuf> {
    let mut result = root.to_path_buf();
    for part in relative.components() {
        ensure!(
            matches!(part, Component::Normal(_)),
            "Invalid guidance path"
        );
        result.push(part);
        match fs::symlink_metadata(&result) {
            Ok(meta) => ensure!(
                !meta.file_type().is_symlink(),
                "Guidance path is a symlink: {}",
                result.display()
            ),
            Err(error) if missing && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(result)
}
fn discover(source: &Path, destination: &Path, cancel: &Cancel) -> Result<Vec<File>> {
    // Omitting --exclude-standard includes ignored guidance, without scanning dependency trees.
    let result = process::run(
        repo::git(source).args([
            "ls-files",
            "--others",
            "-z",
            "--",
            "AGENTS.md",
            "AGENTS.override.md",
            ":(glob)**/AGENTS.md",
            ":(glob)**/AGENTS.override.md",
            ".agents/skills/",
            ".codex/skills/",
        ]),
        None,
        cancel,
    )?;
    ensure!(result.code == 0, "Cannot enumerate local guidance");
    let paths = String::from_utf8(result.stdout).context("Guidance paths must be UTF-8")?;
    let mut files = Vec::new();
    for relative in paths
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
    {
        let target = regular_path(destination, &relative, true)?;
        if target.exists() {
            continue;
        }
        let original = regular_path(source, &relative, false)?;
        ensure!(
            original.is_file(),
            "Guidance is not a regular file: {}",
            original.display()
        );
        files.push(File {
            relative,
            mode: fs::metadata(&original)?.permissions().mode() & 0o777,
            bytes: fs::read(original)?,
        });
    }
    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(files)
}
fn copy(source: &Path, destination: &Path, files: &[File]) -> Result<()> {
    // Validate the complete approved set before writing; never overwrite existing guidance.
    for file in files {
        ensure!(
            fs::read(regular_path(source, &file.relative, false)?)? == file.bytes,
            "Guidance changed while awaiting approval: {}. Continue again to review the new files.",
            file.relative.display()
        );
        ensure!(
            !regular_path(destination, &file.relative, true)?.exists(),
            "Guidance destination changed: {}",
            file.relative.display()
        );
    }
    for file in files {
        let target = regular_path(destination, &file.relative, true)?;
        fs::create_dir_all(target.parent().context("Missing guidance parent")?)?;
        regular_path(destination, &file.relative, true)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(file.mode)
            .open(target)?;
        output.write_all(&file.bytes)?;
    }
    Ok(())
}
pub fn confirm(
    store: &Arc<Store>,
    id: &str,
    session: &Session,
    controls: &mpsc::Receiver<Command>,
    cancel: &Cancel,
) -> Result<()> {
    let Job::Coding(launch) = &session.job else {
        return Ok(());
    };
    if !launch.isolated || !session.workspace_ready || session.guidance_checked {
        return Ok(());
    }
    let destination = session.workspace.as_deref().context("Missing worktree")?;
    let files = discover(&launch.repository, destination, cancel)?;
    if !files.is_empty() {
        let request = json!("difu-missing-guidance");
        let paths = files
            .iter()
            .map(|f| f.relative.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        store.update(id, |s| {
            s.status = Status::Waiting;
            s.pending.push(Pending { id: request.clone(), responded:false, method:"item/tool/requestUserInput".into(), params:json!({"questions":[{"id":"copy_guidance","header":"Repository guidance","question":format!("These local guidance files are missing from this worktree:\n{paths}\n\nCopy them before starting Codex? Existing worktree files will remain unchanged."),"options":[{"label":"Copy missing guidance","description":"Copy only the listed files into this worktree."},{"label":"Continue without copying","description":"Use guidance already present in the worktree and normal global Codex settings."}]}]}) });
        })?;
        store.save(id)?;
        loop {
            cancel.check()?;
            match controls.recv_timeout(Duration::from_millis(50)) {
                Ok(command) => {
                    let control = match command.control {
                        Control::AnswerQuestion {
                            request: supplied,
                            question,
                            answer,
                        } if supplied == request && question == "copy_guidance" => {
                            if answer.is_none() {
                                cancel.cancel();
                                let _ = command.reply.send(Ok(()));
                                cancel.check()?;
                            }
                            Control::Respond {
                                request: supplied,
                                response: json!({"answers":{"copy_guidance":{"answers":answer.into_iter().collect::<Vec<_>>()}}}),
                            }
                        }
                        control => control,
                    };
                    match control {
                        Control::Respond {
                            request: supplied,
                            response,
                        } if supplied == request => {
                            let choice = response
                                .pointer("/answers/copy_guidance/answers/0")
                                .and_then(serde_json::Value::as_str);
                            let result = match choice {
                                Some("Copy missing guidance") => {
                                    copy(&launch.repository, destination, &files)
                                }
                                Some("Continue without copying") => Ok(()),
                                _ => Err(anyhow::anyhow!(
                                    "Choose whether to copy the listed guidance files"
                                )),
                            };
                            let accepted = result.is_ok();
                            let _ = command.reply.send(result);
                            if accepted {
                                break;
                            }
                        }
                        Control::Interrupt => {
                            cancel.cancel();
                            let _ = command.reply.send(Ok(()));
                            cancel.check()?;
                        }
                        _ => {
                            let _ = command.reply.send(Err(anyhow::anyhow!("Answer the repository guidance question before starting this session")));
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("Session service disconnected")
                }
            }
        }
    }
    store.update(id, |s| {
        s.guidance_checked = true;
        s.pending.retain(|p| p.id != json!("difu-missing-guidance"));
        s.status = Status::Starting;
    })?;
    store.save(id)
}
