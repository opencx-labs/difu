use super::*;

impl Ui {
    pub fn take_send_repaint(&mut self) -> bool {
        std::mem::take(&mut self.send_repaint)
    }

    pub(super) fn send(&mut self, queue: bool) {
        if self.busy || self.media_pending.is_some() || self.command_draft() {
            return;
        }
        let Some(id) = self.selected.clone() else {
            return;
        };
        if !self.sessions.contains_key(&id) {
            return;
        }
        let position = self.positions.entry(id.clone()).or_default();
        let text = position.draft.text();
        if text.trim().is_empty() {
            return;
        }
        let attachments = position.active_attachments();
        let skills = std::mem::take(&mut position.skills);
        position.draft = Editor::default();
        position.history = None;
        position.attachments.clear();
        // tick_media persists the cleared attachment draft after the UI repaints.
        let control = if attachments.is_empty() {
            Control::Message {
                text,
                queue,
                skills,
                attachments,
            }
        } else {
            Control::MessageWithAttachments {
                text,
                queue,
                skills,
                attachments,
            }
        };
        self.enqueue_message(id, control);
    }

    fn enqueue_message(&mut self, id: String, control: Control) {
        let (Control::Message { text, queue, .. }
        | Control::MessageWithAttachments { text, queue, .. }) = &control
        else {
            return;
        };
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let token = self.send_sequence;
        self.send_sequence = self.send_sequence.wrapping_add(1);
        let deliveries = self.send_queues.entry(id.clone()).or_default();
        let start = deliveries.is_empty();
        let position = self.positions.entry(id.clone()).or_default();
        let earlier = position
            .outgoing
            .iter()
            .filter(|pending| pending.text == *text && pending.failed.is_none())
            .count();
        position.outgoing.push(PendingSend {
            token,
            text: text.clone(),
            queued: (*queue && (session.turn_id.is_some() || !start))
                || session.preparing()
                || session
                    .pending
                    .iter()
                    .any(|p| p.id == "difu-missing-guidance"),
            observed_before: PendingSend::matching(session, text) + earlier,
            failed: None,
        });
        position.follow = true;
        position.keep_transcript_position = false;
        position.transcript_viewport = None;
        deliveries.push_back((token, control));
        self.send_repaint = true;
        if start {
            self.start_send(&id);
        }
    }

    fn start_send(&mut self, id: &str) {
        let Some((token, control)) = self.send_queues.get(id).and_then(|q| q.front()).cloned()
        else {
            return;
        };
        self.task(
            Task::Send(id.to_owned(), token),
            Request::Control {
                id: id.to_owned(),
                control,
            },
            false,
        );
    }

    pub(super) fn finish_send(&mut self, id: &str, token: u64, result: Result<Reply, String>) {
        let Some(deliveries) = self.send_queues.get_mut(id) else {
            return;
        };
        if !deliveries
            .front()
            .is_some_and(|(pending, _)| *pending == token)
        {
            return;
        }
        let Some((_, control)) = deliveries.pop_front() else {
            return;
        };
        let error = match result {
            Ok(Reply::Ok) => None,
            Ok(_) => Some("Unexpected response while sending message".to_owned()),
            Err(error) => Some(error),
        };
        if let Some(error) = error {
            if let Some(position) = self.positions.get_mut(id)
                && let Some(index) = position.outgoing.iter().position(|p| p.token == token)
            {
                let text = position
                    .outgoing
                    .get(index)
                    .map(|p| p.text.clone())
                    .unwrap_or_default();
                if let Some(pending) = position.outgoing.get_mut(index) {
                    pending.failed = Some(control);
                }
                // A failed duplicate must not prevent later copies from reconciling.
                for pending in position.outgoing.iter_mut().skip(index + 1) {
                    if pending.text == text && pending.failed.is_none() {
                        pending.observed_before = pending.observed_before.saturating_sub(1);
                    }
                }
            }
            self.notice = Some((format!("Could not send message: {error}"), true));
        }
        // Acknowledgements must never clear a newer draft or move its scroll position.
        self.refreshed = None;
        if self.send_queues.get(id).is_some_and(VecDeque::is_empty) {
            self.send_queues.remove(id);
        } else {
            self.start_send(id);
        }
    }

    pub(super) fn retry_send(&mut self, entry: &str) -> bool {
        let Some(id) = self.selected.clone() else {
            return false;
        };
        let Some(position) = self.positions.get_mut(&id) else {
            return false;
        };
        let Some(index) = position
            .outgoing
            .iter()
            .position(|pending| pending.entry_id() == entry && pending.failed.is_some())
        else {
            return false;
        };
        let pending = position.outgoing.remove(index);
        if let Some(control) = pending.failed {
            self.enqueue_message(id, control);
        }
        true
    }
}
