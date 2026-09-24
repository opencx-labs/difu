use super::*;
impl Position {
    pub(super) fn active_attachments(&self) -> Vec<super::super::media::Attachment> {
        self.draft
            .attachment_tokens()
            .into_iter()
            .filter_map(|token| {
                self.attachments
                    .iter()
                    .find(|attachment| attachment.token() == token)
                    .cloned()
            })
            .collect()
    }
}
impl Ui {
    pub(super) fn restore_media(&mut self, id: &str) {
        let p = self.positions.entry(id.to_owned()).or_default();
        if p.media_loaded {
            return;
        }
        p.media_loaded = true;
        match super::super::media::load_draft(&self.storage, id) {
            Ok(attachments) => {
                for a in &attachments {
                    let number = a
                        .label
                        .split_whitespace()
                        .last()
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(0);
                    match a.kind {
                        super::super::media::Kind::Image => {
                            p.image_count = p.image_count.max(number)
                        }
                        super::super::media::Kind::Video => {
                            p.video_count = p.video_count.max(number)
                        }
                    }
                }
                for attachment in &attachments {
                    p.draft.insert_attachment(&attachment.token());
                }
                p.saved_attachments = attachments.clone();
                p.attachments = attachments;
            }
            Err(error) => {
                self.notice = Some((format!("Cannot restore attachments: {error:#}"), true))
            }
        }
    }

    pub(super) fn attach(&mut self, paths: Option<Vec<std::path::PathBuf>>) {
        if self.media_pending.is_some() {
            return;
        }
        let Some(id) = self.selected.clone() else {
            return;
        };
        self.restore_media(&id);
        let Some(session) = self.sessions.get(&id) else {
            return;
        };
        let cwd = session
            .workspace
            .clone()
            .unwrap_or_else(|| session.job.root().clone());
        let storage = self.storage.clone();
        let (tx, rx) = mpsc::channel();
        self.media_pending = Some((id.clone(), rx));
        self.notice = Some(("Reading attachment…".into(), false));
        thread::spawn(move || {
            let result = if let Some(paths) = paths {
                super::super::media::files(&storage, &id, paths)
            } else {
                super::super::media::clipboard(&storage, &id, &cwd)
            };
            let _ = tx.send(result.map_err(|e| format!("{e:#}")));
        });
    }
    pub(super) fn tick_media(&mut self) {
        // Keep metadata for undo in memory, but persist/send only tokens still in the draft.
        for (id, position) in &mut self.positions {
            let active = position.active_attachments();
            if position.media_loaded && active != position.saved_attachments {
                match super::super::media::save_draft(&self.storage, id, &active) {
                    Ok(()) => position.saved_attachments = active,
                    Err(error) => {
                        self.notice =
                            Some((format!("Cannot save attachment draft: {error:#}"), true))
                    }
                }
            }
        }
        let Some(result) = self
            .media_pending
            .as_ref()
            .and_then(|(_, rx)| rx.try_recv().ok())
        else {
            return;
        };
        let Some((id, _)) = self.media_pending.take() else {
            return;
        };
        let p = self.positions.entry(id.clone()).or_default();
        match result {
            Ok(super::super::media::Paste::Text(text)) => {
                p.draft.paste(&text);
                self.notice = None;
            }
            Ok(super::super::media::Paste::Attachments(attachments)) => {
                for mut attachment in attachments {
                    let (counter, kind) = match attachment.kind {
                        super::super::media::Kind::Image => (&mut p.image_count, "image"),
                        super::super::media::Kind::Video => (&mut p.video_count, "video"),
                    };
                    *counter = counter.saturating_add(1);
                    attachment.label = format!("{kind} {counter}");
                    p.draft.insert_attachment(&attachment.token());
                    p.attachments.push(attachment);
                }
                let active = p.active_attachments();
                if let Err(error) = super::super::media::save_draft(&self.storage, &id, &active) {
                    self.notice = Some((format!("Cannot save attachment draft: {error:#}"), true));
                    return;
                }
                p.saved_attachments = active;
                self.notice = None;
            }
            Err(error) => self.notice = Some((error, true)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inline_tokens_select_media_in_text_order_and_undo_restores_deletions() {
        let image = super::super::super::media::Attachment {
            label: "image 1".into(),
            path: "image.png".into(),
            kind: super::super::super::media::Kind::Image,
            hash: "image".into(),
        };
        let video = super::super::super::media::Attachment {
            label: "video 1".into(),
            path: "video.mp4".into(),
            kind: super::super::super::media::Kind::Video,
            hash: "video".into(),
        };
        let mut position = Position {
            attachments: vec![video.clone(), image.clone()],
            ..Default::default()
        };
        position.draft.insert("compare ");
        position.draft.insert_attachment(&image.token());
        position.draft.insert(" with ");
        position.draft.insert_attachment(&video.token());
        assert_eq!(position.draft.text(), "compare [image 1] with [video 1]");
        assert_eq!(
            position.active_attachments(),
            [image.clone(), video.clone()]
        );
        position
            .draft
            .key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(position.active_attachments(), std::slice::from_ref(&image));
        position.draft.undo();
        assert_eq!(position.active_attachments(), [image, video]);
        position.draft = Editor::from("typed [image 1] and [video 1]");
        assert!(position.active_attachments().is_empty());
    }
}
