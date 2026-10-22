//! Optimistic field-level transactions over the existing cross-process config lock.
use super::{SettingId, SettingValue, get, registry, set};
use crate::storage::{Config, Storage};
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub struct SettingsDraft {
    baseline: Config,
    config: Config,
    edited: BTreeSet<SettingId>,
    guarded: BTreeSet<SettingId>,
}
impl SettingsDraft {
    pub(crate) fn enables_native(&self) -> bool {
        self.edited.contains(&SettingId::NotificationDelivery)
            && self.baseline.preferences.notifications.delivery
                == super::NotificationDelivery::InApp
            && self.config.preferences.notifications.delivery != super::NotificationDelivery::InApp
    }

    pub fn new(config: Config) -> Self {
        Self {
            baseline: config.clone(),
            config,
            edited: BTreeSet::new(),
            guarded: BTreeSet::new(),
        }
    }
    /// Immutable preview snapshot. Mutations must use validated registry adapters.
    pub fn config(&self) -> &Config {
        &self.config
    }
    pub fn edited(&self) -> &BTreeSet<SettingId> {
        &self.edited
    }
    pub fn value(&self, id: &SettingId) -> SettingValue {
        get(&self.config, id)
    }
    pub fn set(&mut self, id: SettingId, value: SettingValue) -> Result<()> {
        set(&mut self.config, &id, value)?;
        if id == SettingId::SendKey {
            self.guarded.insert(SettingId::SendKey);
            self.guarded.extend(
                crate::shortcuts::send_companions(self.config.preferences.input.send_key)
                    .into_iter()
                    .map(|(action, _)| SettingId::Shortcut(action.0.into())),
            );
        }
        if matches!(
            id,
            SettingId::Provider
                | SettingId::DefaultModel
                | SettingId::DefaultEffort
                | SettingId::StartingMode
        ) {
            self.guarded.extend([
                SettingId::Provider,
                SettingId::DefaultModel,
                SettingId::DefaultEffort,
                SettingId::StartingMode,
            ]);
        }
        self.recompute_edits();
        Ok(())
    }
    /// Legacy model managers deliberately select a provider with the model.
    pub(crate) fn select_default_model(
        &mut self,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<()> {
        if let Some(model) = model.as_deref() {
            let provider = crate::agents::provider::Provider::for_model(Some(model));
            if self.config.agent_defaults.resolved_provider() != provider {
                self.set(SettingId::StartingMode, SettingValue::Text(None))?;
            }
            self.set(
                SettingId::Provider,
                SettingValue::Choice(provider.key().into()),
            )?;
        }
        self.set(SettingId::DefaultModel, SettingValue::Text(model))?;
        self.set(SettingId::DefaultEffort, SettingValue::Text(effort))
    }
    /// Reset only this draft, using the registry's default. Shortcut None removes its override.
    pub fn reset(&mut self, id: &SettingId) -> Result<()> {
        let spec = super::spec(id).ok_or_else(|| anyhow::anyhow!("Unknown setting"))?;
        self.set(id.clone(), spec.default)?;
        if *id == SettingId::SendKey {
            for (action, _) in crate::shortcuts::send_companions(super::SendKey::Enter) {
                self.config.preferences.keyboard.bindings.remove(action.0);
            }
            self.recompute_edits();
        }
        Ok(())
    }
    /// Full keyboard reset includes the send preset; unknown newer action IDs stay opaque.
    pub fn reset_shortcuts(&mut self) -> Result<()> {
        self.reset(&SettingId::SendKey)?;
        self.reset(&SettingId::QuestionOverridesSelection)?;
        for action in crate::shortcuts::all() {
            self.set(
                SettingId::Shortcut(action.id.0.into()),
                SettingValue::Shortcut(None),
            )?;
        }
        Ok(())
    }
    pub fn reset_all(&mut self) -> Result<()> {
        let shortcuts: Vec<_> = self
            .config
            .preferences
            .keyboard
            .bindings
            .keys()
            .cloned()
            .collect();
        for spec in registry().into_iter().filter(|spec| spec.is_stored_field()) {
            self.set(spec.id, spec.default)?;
        }
        for action in shortcuts
            .into_iter()
            .filter(|id| crate::shortcuts::find(id).is_some())
        {
            self.set(SettingId::Shortcut(action), SettingValue::Shortcut(None))?;
        }
        Ok(())
    }
    /// Call with a fresh committed snapshot when Discard is chosen, never on background refresh.
    pub fn discard(&mut self, committed: Config) {
        *self = Self::new(committed);
    }
    fn recompute_edits(&mut self) {
        let mut ids: BTreeSet<_> = registry()
            .into_iter()
            .filter(|spec| spec.is_stored_field())
            .map(|spec| spec.id)
            .collect();
        ids.extend(
            self.baseline
                .preferences
                .keyboard
                .bindings
                .keys()
                .chain(self.config.preferences.keyboard.bindings.keys())
                .cloned()
                .map(SettingId::Shortcut),
        );
        self.edited = ids
            .into_iter()
            .filter(|id| {
                super::registry::snapshot(&self.baseline, id)
                    != super::registry::snapshot(&self.config, id)
            })
            .collect();
    }
}

#[derive(Debug)]
pub struct SaveError {
    pub conflicts: Vec<SettingId>,
    pub source: anyhow::Error,
}
impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.conflicts.is_empty() {
            write!(f, "Cannot save settings: {:#}", self.source)
        } else {
            write!(
                f,
                "Settings changed elsewhere: {}",
                self.conflicts
                    .iter()
                    .map(|id| id.key().into_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }
}
impl std::error::Error for SaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}
impl Storage {
    pub fn settings_draft(&self) -> Result<SettingsDraft> {
        Ok(SettingsDraft::new(self.load_config()?))
    }
    /// Refresh runtime settings independently. Open panels retain their own SettingsDraft.
    pub fn refresh_settings(&self) -> Result<Config> {
        self.load_config()
    }
    pub fn save_settings_draft(
        &self,
        draft: &mut SettingsDraft,
    ) -> std::result::Result<Config, SaveError> {
        let mut conflicts = Vec::new();
        // Repository dependency follows final native-mode intent, not edit history.
        let mut guarded = draft.guarded.clone();
        if draft.config.agent_defaults.starting_mode.is_some()
            && draft.edited.iter().any(|id| {
                matches!(
                    id,
                    SettingId::Provider
                        | SettingId::DefaultModel
                        | SettingId::DefaultEffort
                        | SettingId::StartingMode
                )
            })
        {
            guarded.insert(SettingId::DefaultRepository);
        }
        if draft.edited.contains(&SettingId::DefaultRepository) {
            guarded.insert(SettingId::StartingMode);
        }
        let result = self.try_patch_config(|fresh| {
            for id in draft.edited.union(&guarded) {
                if super::registry::snapshot(fresh, id)
                    != super::registry::snapshot(&draft.baseline, id)
                {
                    conflicts.push(id.clone());
                }
            }
            anyhow::ensure!(
                conflicts.is_empty(),
                "Settings changed while this draft was open"
            );
            // The draft already applied preset companions. Save individual raw leaves;
            // re-applying a preset here would overwrite a newer custom companion.
            for id in &draft.edited {
                if *id == SettingId::SendKey {
                    fresh.preferences.input.send_key = draft.config.preferences.input.send_key;
                } else {
                    set(fresh, id, get(&draft.config, id))?;
                }
            }
            if draft.edited.iter().any(|id| {
                matches!(
                    id,
                    SettingId::Provider
                        | SettingId::DefaultModel
                        | SettingId::DefaultEffort
                        | SettingId::StartingMode
                )
            }) {
                fresh.agent_defaults.validate()?;
            }
            crate::shortcuts::Bindings::new(fresh).validate()?;
            Ok(())
        });
        match result {
            Ok(committed) => {
                draft.discard(committed.clone());
                Ok(committed)
            }
            Err(source) => Err(SaveError { conflicts, source }),
        }
    }
}

/// Allowlisted preferences and binding overrides only. No connections, rules, or session state.
pub fn export_preferences(config: &Config) -> Result<String> {
    let mut preferences: BTreeMap<String, SettingValue> = registry()
        .into_iter()
        .filter(|spec| spec.is_stored_field())
        .map(|spec| (spec.id.key().into_owned(), get(config, &spec.id)))
        .collect();
    for action in config.preferences.keyboard.bindings.keys() {
        let id = SettingId::Shortcut(action.clone());
        preferences.insert(id.key().into_owned(), get(config, &id));
    }
    Ok(serde_json::to_string_pretty(&preferences)?)
}
