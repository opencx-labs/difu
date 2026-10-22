//! Cancellable, draft-scoped discovery. Browsing never authenticates or pairs.
use super::*;
use crate::{
    agents::{
        connections::{self, Capabilities, Status},
        provider::Provider,
    },
    process::Cancel,
    storage::AgentDefaults,
};
use std::sync::mpsc;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    provider: Provider,
    model: Option<String>,
    repository: Option<PathBuf>,
}
impl Key {
    fn from(defaults: &AgentDefaults) -> Self {
        Self {
            provider: defaults.resolved_provider(),
            model: defaults.model.clone(),
            repository: defaults.repository.clone(),
        }
    }
}
struct Worker<T> {
    receiver: mpsc::Receiver<T>,
    cancel: Cancel,
}
impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
fn spawn<T: Send + 'static>(run: impl FnOnce(&Cancel) -> T + Send + 'static) -> Worker<T> {
    let cancel = Cancel::default();
    let token = cancel.clone();
    let (sender, receiver) = mpsc::channel();
    #[cfg(test)]
    let fixture = crate::process::fixture::worker();
    std::thread::spawn(move || {
        #[cfg(test)]
        let _fixture = fixture.map(|f| f.enter());
        let _ = sender.send(run(&token));
        crate::repaint::request();
    });
    Worker { receiver, cancel }
}
#[derive(Default)]
pub(super) struct State {
    key: Option<Key>,
    capability_worker: Option<Worker<Result<Capabilities, String>>>,
    capability_error: Option<String>,
    statuses: Vec<(Provider, Option<Status>, Option<Worker<Status>>)>,
}
impl Panel {
    pub(super) fn retry_capabilities(&mut self) {
        self.connections.key = None;
    }
    pub(super) fn refresh_connections(&mut self) {
        self.connections.statuses.clear();
    }
    pub(crate) fn connection_tick(&mut self) {
        let rows = self.rows();
        if rows.iter().any(|s| {
            matches!(
                s.id,
                SettingId::Provider
                    | SettingId::DefaultModel
                    | SettingId::DefaultEffort
                    | SettingId::StartingMode
            )
        }) {
            let defaults = self.draft.config().agent_defaults.clone();
            let key = Key::from(&defaults);
            if self.connections.key.as_ref() != Some(&key) {
                self.connections.capability_worker = None;
                self.connections.key = Some(key.clone());
                self.connections.capability_error = None;
                for id in [
                    SettingId::DefaultModel,
                    SettingId::DefaultEffort,
                    SettingId::StartingMode,
                ] {
                    self.dynamic_choices.remove(&id);
                }
                self.connections.capability_worker = Some(spawn(move |cancel| {
                    connections::capabilities(&defaults, cancel).map_err(|_| "Capabilities unavailable; saved choices retained. Refresh provider status to retry.".into())
                }));
            }
        }
        let received = self
            .connections
            .capability_worker
            .as_ref()
            .and_then(|w| w.receiver.try_recv().ok());
        if let Some(result) = received {
            self.connections.capability_worker = None;
            if let Some(key) = self.connections.key.clone() {
                self.receive_capabilities(key, result);
            }
        }
        if rows.iter().any(|s| {
            matches!(
                s.id,
                SettingId::ProviderConnections | SettingId::CodexSignIn | SettingId::ClaudeSignIn
            )
        }) && self.connections.statuses.is_empty()
        {
            for provider in [Provider::Codex, Provider::Claude] {
                self.connections.statuses.push((
                    provider,
                    None,
                    Some(spawn(move |cancel| {
                        connections::status(provider, cancel).unwrap_or(Status::Unavailable)
                    })),
                ));
            }
        }
        for (_, status, worker) in &mut self.connections.statuses {
            if let Some(result) = worker.as_ref().and_then(|w| w.receiver.try_recv().ok()) {
                *status = Some(result);
                *worker = None;
            }
        }
    }
    fn receive_capabilities(&mut self, key: Key, result: Result<Capabilities, String>) {
        if key != Key::from(&self.draft.config().agent_defaults) {
            return;
        }
        match result {
            Ok(capabilities) => {
                let defaults = &self.draft.config().agent_defaults;
                let efforts = defaults
                    .model
                    .as_ref()
                    .and_then(|id| capabilities.models.iter().find(|m| &m.id == id))
                    .map(|m| m.efforts.clone());
                self.set_dynamic_choices(
                    SettingId::DefaultModel,
                    capabilities.models.into_iter().map(|m| m.id).collect(),
                );
                self.set_dynamic_choices(SettingId::DefaultEffort, efforts.unwrap_or_default());
                self.set_dynamic_choices(
                    SettingId::StartingMode,
                    capabilities
                        .modes
                        .into_iter()
                        .filter(|m| m.available)
                        .map(|m| m.id)
                        .collect(),
                );
                self.connections.capability_error = None;
            }
            Err(error) => self.connections.capability_error = Some(error),
        }
    }
    pub(super) fn available_choices(&self, id: &SettingId) -> Option<&Vec<String>> {
        if self
            .connections
            .key
            .as_ref()
            .is_some_and(|key| key != &Key::from(&self.draft.config().agent_defaults))
        {
            return None;
        }
        self.dynamic_choices.get(id)
    }
    pub(super) fn invalid_defaults(&self) -> Option<String> {
        for id in [
            SettingId::DefaultModel,
            SettingId::DefaultEffort,
            SettingId::StartingMode,
        ] {
            if id == SettingId::DefaultEffort && self.draft.config().agent_defaults.model.is_none()
            {
                continue;
            }
            if let (Some(choices), SettingValue::Text(Some(value))) =
                (self.available_choices(&id), self.draft.value(&id))
                && !choices.contains(&value)
            {
                return Some(format!(
                    "{} is unavailable for the selected provider/model; choose a supported value or Automatic",
                    spec(&id).map_or("Selection", |s| s.label)
                ));
            }
        }
        self.draft
            .config()
            .agent_defaults
            .validate()
            .err()
            .map(|e| e.to_string())
    }
    pub(super) fn connection_details(&self, id: &SettingId) -> Vec<String> {
        match id {
            SettingId::Provider
            | SettingId::DefaultModel
            | SettingId::DefaultEffort
            | SettingId::StartingMode => {
                let mut lines = Vec::new();
                if self.connections.capability_worker.is_some() {
                    lines.push("Loading native capabilities… Saved choices retained.".into());
                }
                if let Some(error) = &self.connections.capability_error {
                    lines.push(error.clone());
                }
                if let Some(error) = self.invalid_defaults() {
                    lines.push(error);
                }
                if *id == SettingId::DefaultEffort
                    && self.draft.config().agent_defaults.model.is_none()
                {
                    lines.push("Choose an explicit model to browse its supported efforts; Automatic uses native defaults.".into());
                }
                if let Some(choices) = self.available_choices(id) {
                    lines.push(format!("Choices: Automatic, {}", choices.join(", ")));
                }
                lines
            }
            SettingId::ProviderConnections | SettingId::CodexSignIn | SettingId::ClaudeSignIn => {
                self.connections
                    .statuses
                    .iter()
                    .map(|(provider, status, _)| {
                        format!(
                            "{}: {}",
                            provider.label(),
                            status.as_ref().map_or("Loading…", Status::label)
                        )
                    })
                    .collect()
            }
            SettingId::Rules => vec![format!(
                "Target repository: {}",
                self.draft
                    .config()
                    .agent_defaults
                    .repository
                    .clone()
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_default()
                    .display()
            )],
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saving_before_next_tick_ignores_previous_provider_choices() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let mut panel = Panel::new(&storage, Some(Category::AgentDefaults))?;
        panel.connections.key = Some(Key::from(&panel.draft.config().agent_defaults));
        panel.set_dynamic_choices(SettingId::DefaultModel, vec!["gpt-fixture".into()]);
        panel
            .draft
            .set(SettingId::Provider, SettingValue::Choice("claude".into()))?;
        panel.draft.set(
            SettingId::DefaultModel,
            SettingValue::Text(Some("claude/sonnet".into())),
        )?;
        assert!(
            panel.save(&storage).is_some(),
            "Old provider choices must not reject the new draft"
        );
        assert_eq!(
            storage.load_config()?.agent_defaults.resolved_provider(),
            Provider::Claude
        );
        Ok(())
    }
    #[test]
    fn late_provider_results_and_offline_errors_never_replace_draft_or_new_choices()
    -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().into(),
        };
        let mut panel = Panel::new(&storage, Some(Category::AgentDefaults))?;
        let old = Key::from(&panel.draft.config().agent_defaults);
        panel
            .draft
            .set(SettingId::Provider, SettingValue::Choice("claude".into()))?;
        panel.draft.set(
            SettingId::DefaultModel,
            SettingValue::Text(Some("claude/sonnet".into())),
        )?;
        let new = Key::from(&panel.draft.config().agent_defaults);
        panel.receive_capabilities(
            new.clone(),
            Ok(Capabilities {
                models: vec![crate::model::ModelInfo {
                    id: "claude/sonnet".into(),
                    name: "Sonnet".into(),
                    efforts: vec!["high".into()],
                }],
                modes: vec![],
            }),
        );
        panel.receive_capabilities(
            old,
            Ok(Capabilities {
                models: vec![],
                modes: vec![],
            }),
        );
        assert_eq!(
            panel.dynamic_choices.get(&SettingId::DefaultModel),
            Some(&vec!["claude/sonnet".to_string()])
        );
        panel.receive_capabilities(new, Err("Offline".into()));
        assert_eq!(
            panel.draft.config().agent_defaults.model.as_deref(),
            Some("claude/sonnet")
        );
        assert_eq!(
            panel.dynamic_choices.get(&SettingId::DefaultEffort),
            Some(&vec!["high".to_string()])
        );
        assert!(panel.save(&storage).is_some());
        Ok(())
    }
}

#[cfg(test)]
mod asynchronous_tests {
    use super::*;
    use anyhow::{Context, Result, ensure};
    #[test]
    fn native_async_switching_and_failure_preserve_the_current_draft() -> Result<()> {
        crate::agents::connections::boundary_tests::isolated(
            "shell::settings_panel::connections::asynchronous_tests::isolated_panel_child",
        )
    }
    #[test]
    fn isolated_panel_child() -> Result<()> {
        let Some(root) = std::env::var_os("DIFU_CONNECTIONS_FIXTURE").map(PathBuf::from) else {
            return Ok(());
        };
        let storage = Storage {
            config: root.join("config.json"),
            cache: root.clone(),
        };
        let mut panel = Panel::new(&storage, Some(Category::AgentDefaults))?;
        panel.draft.set(
            SettingId::DefaultModel,
            SettingValue::Text(Some("gpt-fixture".into())),
        )?;
        std::fs::write(root.join("slow-codex"), "")?;
        panel.connection_tick();
        let deadline = Instant::now() + std::time::Duration::from_secs(4);
        while !root.join("codex-started").exists() {
            ensure!(Instant::now() < deadline, "Codex discovery did not start");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panel
            .draft
            .set(SettingId::Provider, SettingValue::Choice("claude".into()))?;
        panel.draft.set(
            SettingId::DefaultModel,
            SettingValue::Text(Some("claude/sonnet".into())),
        )?;
        panel.draft.set(
            SettingId::DefaultEffort,
            SettingValue::Text(Some("high".into())),
        )?;
        panel.connection_tick();
        while panel.connections.capability_worker.is_some() {
            ensure!(
                Instant::now() < deadline,
                "Claude discovery blocked behind Codex"
            );
            panel.connection_tick();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            panel.dynamic_choices.get(&SettingId::DefaultModel),
            Some(&vec!["claude/sonnet".to_string()])
        );
        assert_eq!(
            panel.dynamic_choices.get(&SettingId::DefaultEffort),
            Some(&vec!["low".to_string(), "high".to_string()])
        );
        assert!(
            panel
                .dynamic_choices
                .get(&SettingId::StartingMode)
                .context("modes")?
                .contains(&"dontAsk".into())
        );
        assert!(panel.save(&storage).is_some());
        let committed = std::fs::read(&storage.config)?;
        std::fs::write(root.join("fail-models"), "")?;
        panel.retry_capabilities();
        panel.connection_tick();
        while panel.connections.capability_worker.is_some() {
            ensure!(Instant::now() < deadline, "Failure blocked UI");
            panel.connection_tick();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(panel.connections.capability_error.is_some());
        assert_eq!(
            panel.draft.config().agent_defaults.model.as_deref(),
            Some("claude/sonnet")
        );
        assert_eq!(std::fs::read(&storage.config)?, committed);
        panel.category = Category::Connections;
        panel.connection_tick();
        while panel
            .connections
            .statuses
            .iter()
            .any(|(_, _, w)| w.is_some())
        {
            ensure!(Instant::now() < deadline, "Status blocked UI");
            panel.connection_tick();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 45))?;
        terminal.draw(|frame| panel.draw(frame))?;
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("Connected") && text.contains("separately"));
        assert!(!text.contains("private@example.invalid") && !text.contains("sk-secret"));
        assert_eq!(std::fs::read(&storage.config)?, committed);
        Ok(())
    }
}
