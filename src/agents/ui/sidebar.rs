use super::*;

#[derive(Default)]
pub(super) struct State {
    pub counts: HashMap<String, DiffStatistics>,
    refreshed: HashMap<String, Instant>,
    pending: HashSet<String>,
    turns: HashMap<String, (bool, Option<i64>, u64)>,
    dirty: HashSet<String>,
}

impl State {
    fn path(storage: &Storage) -> std::path::PathBuf {
        storage.cache.join("agent-statistics.json")
    }
    pub fn load(storage: &Storage) -> Self {
        Self {
            counts: std::fs::read(Self::path(storage))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default(),
            ..Self::default()
        }
    }
    pub fn save(&self, storage: &Storage) -> anyhow::Result<()> {
        std::fs::create_dir_all(&storage.cache)?;
        crate::storage::atomic_json(&Self::path(storage), &self.counts)
    }
    pub fn observe(&mut self, sessions: &[Summary]) {
        for session in sessions {
            let current = (
                session.status.active(),
                session.turn_started_at,
                session.workspace_revision,
            );
            if let Some(previous) = self.turns.insert(session.id.clone(), current)
                && previous != current
            {
                self.dirty.insert(session.id.clone());
                if previous.2 != current.2 {
                    self.counts.remove(&session.id);
                }
            }
        }
    }
    pub fn invalidate(&mut self, id: &str) {
        self.dirty.insert(id.to_owned());
    }
    pub fn finished(&mut self, id: &str) {
        self.pending.remove(id);
        self.refreshed.insert(id.to_owned(), Instant::now());
    }
    fn due(&self, session: &Summary) -> bool {
        session.can_read_changes
            && !self.pending.contains(&session.id)
            && (self.dirty.contains(&session.id)
                || self.refreshed.get(&session.id).is_none_or(|at| {
                    at.elapsed()
                        >= Duration::from_secs(if session.status.active() { 10 } else { 30 })
                }))
    }
}

impl Ui {
    pub(super) fn refresh_statistics(&mut self) {
        for session in self.summaries.clone() {
            if self.sidebar.due(&session) {
                self.sidebar.pending.insert(session.id.clone());
                self.sidebar.dirty.remove(&session.id);
                self.task(
                    Task::Statistics(session.id.clone(), session.workspace_revision),
                    Request::Statistics {
                        id: session.id.clone(),
                    },
                    false,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[test]
    fn counts_refresh_on_turn_boundaries_and_ten_second_intervals() -> Result<()> {
        let session = Session::new(
            "one".into(),
            Job::Coding(Launch {
                repository: "/tmp/repo".into(),
                isolated: false,
                base: "HEAD".into(),
                prompt: "task".into(),
                model: None,
                effort: None,
            }),
        );
        let mut summary = session.summary();
        summary.can_read_changes = true;
        summary.status = Status::Running;
        summary.turn_started_at = Some(1);
        let mut state = State::default();
        state.observe(&[summary.clone()]);
        assert!(state.due(&summary));
        state.pending.insert(summary.id.clone());
        assert!(!state.due(&summary));
        state.finished(&summary.id);
        assert!(!state.due(&summary));
        state.refreshed.insert(
            summary.id.clone(),
            Instant::now()
                .checked_sub(Duration::from_secs(11))
                .ok_or_else(|| anyhow::anyhow!("clock"))?,
        );
        assert!(state.due(&summary));
        state.finished(&summary.id);
        summary.status = Status::Idle;
        state.observe(&[summary.clone()]);
        assert!(state.due(&summary));
        state.dirty.clear();
        assert!(!state.due(&summary));
        summary.status = Status::Running;
        summary.turn_started_at = Some(2);
        state.observe(&[summary.clone()]);
        assert!(state.due(&summary));
        summary.can_read_changes = false;
        assert!(!state.due(&summary));
        Ok(())
    }
}
