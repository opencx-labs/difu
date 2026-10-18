//! Prepare an empty session without blocking launch or starting a model turn.
use super::{
    Control, Job, Status,
    server::{Command, Store},
};
use crate::process::Cancel;
use anyhow::{Context, Result};
use std::{sync::mpsc, time::Duration};

pub(super) fn prepare(
    store: &Store,
    id: &str,
    controls: &mpsc::Receiver<Command>,
    cancel: &Cancel,
) -> Result<Option<Control>> {
    let mut session = store.get(id)?;
    super::workspace::prepare(&mut session, &store.home, cancel, |prepared| {
        store.update(id, |s| {
            s.job = prepared.job.clone();
            s.workspace = prepared.workspace.clone();
            s.workspace_ready = prepared.workspace_ready;
            s.baseline = prepared.baseline.clone();
            s.branch = prepared.branch.clone();
        })?;
        store.save(id)
    })
    .context("Cannot prepare worktree")?;
    cancel.check()?;
    store.update(id, |s| s.status = Status::Idle)?;
    store.save(id)?;

    // Keep ownership of the control channel until handing it to the provider.
    // No provider process starts for an empty session until the first message.
    loop {
        cancel.check()?;
        if !store.get(id)?.queue.is_empty() {
            store.update(id, |s| s.status = Status::Starting)?;
            store.save(id)?;
            return Ok(None);
        }
        let command = match controls.recv_timeout(Duration::from_millis(40)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("Session service disconnected")
            }
        };
        let result = match command.control {
            Control::Interrupt => {
                cancel.cancel();
                Ok(())
            }
            Control::Resume => {
                store.update(id, |s| s.status = Status::Starting)?;
                store.save(id)?;
                let _ = command.reply.send(Ok(()));
                return Ok(Some(Control::Resume));
            }
            Control::Model { model, effort } => {
                store.update(id, |s| {
                    s.model = model.clone();
                    s.effort = effort.clone();
                    if let Job::Coding(launch) = &mut s.job {
                        launch.model = model;
                        launch.effort = effort;
                    }
                })?;
                store.save(id)
            }
            Control::RefreshShells => Ok(()),
            control @ (Control::Message { .. }
            | Control::MessageWithAttachments { .. }
            | Control::ReplaceQueued { .. }) => {
                super::guidance::queue_while_waiting(store, id, control)
            }
            _ => Err(anyhow::anyhow!(
                "Send the first message to start this session"
            )),
        };
        let _ = command.reply.send(result);
    }
}
