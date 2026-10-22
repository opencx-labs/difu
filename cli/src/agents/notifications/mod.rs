//! Service-owned, privacy-limited activity notifications.
pub(crate) mod client;
mod dispatch;
mod native;
use super::{Session, Status};
pub use dispatch::{Capabilities, Command, Cursor, Delivery, Dispatcher, Snapshot};
pub use native::Native;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Completion,
    Question,
    Error,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub identity: String,
    pub session: String,
    pub title: String,
    pub kind: Kind,
}
pub(super) struct Transition {
    status: Status,
    completed: Option<String>,
    turn: Option<String>,
    requests: BTreeSet<String>,
}
impl Transition {
    pub fn capture(session: &Session) -> Self {
        Self {
            status: session.status,
            completed: session.completed_turn.clone(),
            turn: session.turn_id.clone(),
            requests: session
                .pending
                .iter()
                .filter(|p| !p.responded)
                .map(|p| serde_json::json!([p.method, p.id]).to_string())
                .collect(),
        }
    }
    pub fn events(self, session: &mut Session) -> Vec<Event> {
        let mut events = Vec::new();
        let completed =
            session.completed_turn.is_some() && self.completed != session.completed_turn;
        if (completed
            && session.completed_turn_successful
            && matches!(session.status, Status::Idle | Status::Completed))
            || (session.status == Status::Completed
                && self.status != Status::Completed
                && !matches!(session.job, super::Job::Coding(_)))
        {
            events.push(event(
                session,
                Kind::Completion,
                session.completed_turn.clone(),
            ));
        } else if session.status == Status::Failed && (self.status != Status::Failed || completed) {
            events.push(event(
                session,
                Kind::Error,
                if completed {
                    session.completed_turn.clone()
                } else {
                    session.turn_id.clone().or(self.turn.clone())
                },
            ));
        }
        for request in &session.pending {
            if !request.responded
                && !self
                    .requests
                    .contains(&serde_json::json!([request.method, request.id]).to_string())
            {
                events.push(event(
                    session,
                    Kind::Question,
                    Some(
                        serde_json::json!([session.turn_id, request.method, request.id])
                            .to_string(),
                    ),
                ));
            }
        }
        events
    }
}
fn event(session: &Session, kind: Kind, identity: Option<String>) -> Event {
    Event {
        identity: serde_json::json!([
            session.id,
            session.thread_id,
            kind,
            identity.unwrap_or_else(|| format!("edge-{}", session.version))
        ])
        .to_string(),
        session: session.id.clone(),
        title: session
            .title
            .chars()
            .filter(|c| !c.is_control())
            .take(100)
            .collect(),
        kind,
    }
}
impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Completion => "Turn completed",
            Self::Question => "Question or approval needed",
            Self::Error => "Session failed",
        }
    }
}
#[cfg(test)]
mod tests;
