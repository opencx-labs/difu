//! One optional follow-up suggestion per completed coding turn. Never sends a message.
use super::*;
use crate::process::Cancel;
use anyhow::Result;
use std::{sync::Arc, thread};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Suggestion {
    pub turn: String,
    pub prompt: String,
    pub text: String,
}
impl Suggestion {
    pub fn current(&self, session: &Session) -> bool {
        session.status == Status::Idle
            && session.turn_id.is_none()
            && session.queue.is_empty()
            && session.completed_turn.as_ref() == Some(&self.turn)
            && session
                .entries
                .iter()
                .rev()
                .find(|e| e.kind == "userMessage")
                .is_some_and(|entry| entry.id == self.prompt)
    }
}

#[derive(Default)]
pub(super) struct Task {
    handle: Option<thread::JoinHandle<()>>,
    cancel: Cancel,
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
impl Task {
    pub(super) fn start(
        &mut self,
        store: &Arc<server::Store>,
        id: &str,
        cancel: &Cancel,
    ) -> Result<()> {
        cancel.check()?;
        if self.handle.as_ref().is_some_and(|h| !h.is_finished()) {
            return Ok(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let session = store.get(id)?;
        let Some(turn) = session.completed_turn.clone() else {
            return Ok(());
        };
        if !matches!(session.job, Job::Coding(_))
            || session.status != Status::Idle
            || session.suggestion_attempted.as_ref() == Some(&turn)
        {
            return Ok(());
        }
        let Some(prompt) = session
            .entries
            .iter()
            .rev()
            .find(|e| e.kind == "userMessage")
        else {
            return Ok(());
        };
        let Some(response) = session
            .entries
            .iter()
            .rev()
            .take_while(|e| e.id != prompt.id)
            .find(|e| e.kind == "agentMessage" && !e.text.is_empty())
        else {
            return Ok(());
        };
        let task = prompt.text.clone();
        let response = response.text.clone();
        let prompt = prompt.id.clone();
        // Persist before starting; reconnecting or a failed generation never repeats the call.
        store.update(id, |s| {
            s.suggestion_attempted = Some(turn.clone());
            s.suggestion = None;
        })?;
        store.save(id)?;
        let store = Arc::clone(store);
        let id = id.to_owned();
        let cancel = self.cancel.clone();
        self.handle = Some(thread::spawn(move || {
            let instructions = "Suggest one short, natural next message the user might send, based on their latest exchange. For example, after a finished implementation plan: 'Okay, implement the plan.' Write in the user's language and voice, as an editable suggestion, not as the assistant. Do not claim the user already approved or performed anything. Use at most one short sentence. Return only a JSON object with a suggestion string.";
            if let Ok(text) = super::title::generate_text(
                "suggestion",
                200,
                instructions,
                &task,
                &response,
                &cancel,
            ) {
                let suggestion = Suggestion { turn, prompt, text };
                let _ = store
                    .update(&id, |s| {
                        if suggestion.current(s) {
                            s.suggestion = Some(suggestion);
                        }
                    })
                    .and_then(|_| store.save(&id));
            }
        }));
        Ok(())
    }
}
