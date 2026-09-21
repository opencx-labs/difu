use super::*;
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
                    p.attachments.push(attachment);
                }
                if let Err(error) =
                    super::super::media::save_draft(&self.storage, &id, &p.attachments)
                {
                    self.notice = Some((format!("Cannot save attachment draft: {error:#}"), true));
                    return;
                }
                self.notice = Some((
                    "Attached locally · click × to remove · Enter sends".into(),
                    false,
                ));
            }
            Err(error) => self.notice = Some((error, true)),
        }
    }
    pub(super) fn media_rows(&self, width: u16) -> u16 {
        let Some(p) = self.selected.as_ref().and_then(|id| self.positions.get(id)) else {
            return 0;
        };
        let mut rows = 0;
        let mut used = 0;
        for attachment in &p.attachments {
            let size = attachment
                .token()
                .len()
                .saturating_add(3)
                .min(usize::from(width.max(1)));
            if rows == 0 || used + size > usize::from(width) {
                rows += 1;
                used = 0;
            }
            used += size;
        }
        rows
    }
    pub(super) fn draw_media(&mut self, frame: &mut Frame, area: Rect) {
        let attachments = self
            .selected
            .as_ref()
            .and_then(|id| self.positions.get(id))
            .map(|p| p.attachments.clone())
            .unwrap_or_default();
        let mut x = area.x;
        let mut y = area.y;
        for attachment in attachments {
            let label = format!("{} ×", attachment.token());
            let width = (label.len().saturating_add(1) as u16).min(area.width);
            if x + width > area.right() {
                x = area.x;
                y = y.saturating_add(1);
            }
            if y >= area.bottom() {
                break;
            }
            let rect = Rect::new(x, y, width, 1);
            frame.render_widget(
                Paragraph::new(label).style(Style::default().fg(ACCENT)),
                rect,
            );
            self.hits
                .push((rect, Action::RemoveAttachment(attachment.path)));
            x = x.saturating_add(width);
        }
    }
}
