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
}
impl SettingsDraft {
    pub fn new(config: Config) -> Self {
        Self {
            baseline: config.clone(),
            config,
            edited: BTreeSet::new(),
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
        self.recompute_edits();
        Ok(())
    }
    /// Reset only this draft, using the registry's default. Shortcut None removes its override.
    pub fn reset(&mut self, id: &SettingId) -> Result<()> {
        let spec = super::spec(id).ok_or_else(|| anyhow::anyhow!("Unknown setting"))?;
        self.set(id.clone(), spec.default)
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
        for action in shortcuts {
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
        let result = self.try_patch_config(|fresh| {
            for id in &draft.edited {
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
            for id in &draft.edited {
                set(fresh, id, get(&draft.config, id))?;
            }
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
