use crate::{
    codex::Guide,
    model::{ModelChoice, PrKey, PrSummary},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentDefaults {
    pub provider: Option<crate::agents::provider::Provider>,
    pub starting_mode: Option<String>,
    pub repository: Option<PathBuf>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub isolated: bool,
}
impl Default for AgentDefaults {
    fn default() -> Self {
        Self {
            provider: None,
            starting_mode: None,
            repository: None,
            model: None,
            effort: None,
            isolated: true,
        }
    }
}

impl AgentDefaults {
    /// Structural checks only: discovery is asynchronous and never runs under the config lock.
    pub fn validate(&self) -> Result<()> {
        if let (Some(provider), Some(model)) = (self.provider, self.model.as_deref()) {
            anyhow::ensure!(
                provider == crate::agents::provider::Provider::for_model(Some(model)),
                "Default model belongs to a different provider; select a compatible model or Automatic"
            );
        }
        if let Some(mode) = self.starting_mode.as_deref() {
            anyhow::ensure!(
                crate::agents::permissions::mode_choices(self.resolved_provider())
                    .iter()
                    .any(|choice| choice.id == mode),
                "Starting mode belongs to a different provider; select a supported mode or Automatic"
            );
        }
        Ok(())
    }
    pub fn resolved_provider(&self) -> crate::agents::provider::Provider {
        self.provider
            .unwrap_or_else(|| crate::agents::provider::Provider::for_model(self.model.as_deref()))
    }
}

/// Chat animation timing, set through `/tweaks`. Zero turns an animation off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tweaks {
    /// Characters per second for agent replies. Replies speed up to stay
    /// within a short lag of the stream.
    pub agent_reply_cps: u32,
    /// Milliseconds for a sent message to type into the chat.
    pub sent_message_ms: u32,
    /// Milliseconds for the composer to clear after sending.
    pub input_clear_ms: u32,
    /// The longest any of these may take, in milliseconds: sent messages and
    /// clearing finish within it, and replies never trail the stream by more.
    /// Zero removes the cap.
    pub max_animation_ms: u32,
}
impl Default for Tweaks {
    fn default() -> Self {
        Self {
            agent_reply_cps: 200,
            sent_message_ms: 1000,
            input_clear_ms: 1000,
            max_animation_ms: 500,
        }
    }
}
impl Tweaks {
    /// `ms`, shortened to the cap.
    pub fn capped(self, ms: u32) -> u32 {
        if self.max_animation_ms == 0 {
            ms
        } else {
            ms.min(self.max_animation_ms)
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub preferences: crate::settings::Preferences,
    #[serde(default)]
    pub slack: Option<crate::agents::slack::Settings>,
    #[serde(default)]
    pub local_diff_roots: Vec<PathBuf>,
    #[serde(default)]
    pub local_diff_repositories: BTreeSet<PathBuf>,
    #[serde(default = "enabled")]
    pub last_tab_agents: bool,
    #[serde(default)]
    pub agent_defaults: AgentDefaults,
    #[serde(default)]
    pub repository_rules: BTreeMap<PathBuf, String>,
    #[serde(default)]
    pub repositories: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub pinned_repositories: BTreeSet<String>,
    #[serde(default)]
    pub pinned_sessions: BTreeSet<String>,
    #[serde(default)]
    pub session_order: Vec<String>,
    #[serde(default)]
    pub model: ModelChoice,
    #[serde(default = "ModelChoice::conflict_default")]
    pub conflict_model: ModelChoice,
    #[serde(default)]
    pub unified: bool,
    #[serde(default)]
    pub wrap_diff: bool,
    #[serde(default = "enabled")]
    pub agent_list_visible: bool,
    #[serde(default)]
    pub agent_changes_visible: bool,
    #[serde(default = "enabled")]
    pub agent_panel_right: bool,
    #[serde(default)]
    pub voice_enabled: bool,
    #[serde(default)]
    pub tweaks: Tweaks,
    /// Colour theme id; None is the Default theme.
    #[serde(default)]
    pub theme: Option<String>,
}
fn enabled() -> bool {
    true
}
impl Default for Config {
    fn default() -> Self {
        Self {
            preferences: crate::settings::Preferences::default(),
            slack: None,
            local_diff_roots: Vec::new(),
            local_diff_repositories: BTreeSet::new(),
            last_tab_agents: true,
            agent_defaults: AgentDefaults::default(),
            repository_rules: BTreeMap::new(),
            repositories: BTreeMap::new(),
            pinned_repositories: BTreeSet::new(),
            pinned_sessions: BTreeSet::new(),
            session_order: Vec::new(),
            model: ModelChoice::default(),
            conflict_model: ModelChoice::conflict_default(),
            unified: false,
            wrap_diff: false,
            agent_list_visible: true,
            agent_changes_visible: false,
            agent_panel_right: true,
            voice_enabled: false,
            tweaks: Tweaks::default(),
            theme: None,
        }
    }
}

#[derive(Clone)]
pub struct Storage {
    pub config: PathBuf,
    pub cache: PathBuf,
}
impl Storage {
    pub fn discover() -> Result<Self> {
        #[cfg(test)]
        if let Some(storage) = crate::process::fixture::storage() {
            return Ok(storage);
        }
        let config = dirs::config_dir()
            .context("Cannot locate your configuration directory")?
            .join("difu");
        let cache = dirs::cache_dir()
            .context("Cannot locate your cache directory")?
            .join("difu");
        fs::create_dir_all(&config)?;
        fs::create_dir_all(&cache)?;
        Ok(Self {
            config: config.join("config.json"),
            cache,
        })
    }
    pub fn load_config(&self) -> Result<Config> {
        self.config_document().map(|(config, _)| config)
    }
    fn config_document(&self) -> Result<(Config, serde_json::Value)> {
        if !self.config.exists() {
            return Ok((Config::default(), serde_json::json!({})));
        }
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&self.config)?)
            .context("Cannot read difu config.json; fix its JSON or move it aside")?;
        let mut config: Config = serde_json::from_value(value.clone())?;
        if value.get("pinned_repositories").is_none() {
            config.pinned_repositories = value
                .get("review_repositories")
                .and_then(serde_json::Value::as_object)
                .map(|repos| repos.keys().cloned().collect())
                .unwrap_or_default();
            for name in &config.pinned_repositories {
                crate::model::validate_repository(name)?;
            }
        }
        Ok((config, value))
    }
    pub fn load_repositories(&self) -> Result<Option<Vec<String>>> {
        let current = crate::github::store::root(self).join("repositories.json");
        let path = if current.exists() {
            current
        } else {
            self.cache.join("repositories.json")
        };
        if !path.exists() {
            return Ok(None);
        }
        let repos: Vec<String> = serde_json::from_slice(&fs::read(path)?)?;
        for repo in &repos {
            crate::model::validate_repository(repo)?;
        }
        Ok(Some(repos))
    }
    pub fn save_repositories(&self, repos: &[String]) -> Result<()> {
        let root = crate::github::store::root(self);
        fs::create_dir_all(&root)?;
        atomic_json(&root.join("repositories.json"), &repos)
    }

    /// Applies `change` to the settings on disk and returns the result.
    ///
    /// Every settings writer goes through here. Agents, Reviews, PR panes and
    /// the background service each keep their own copy of the settings, so
    /// saving a whole in-memory copy would undo whatever another part of difu
    /// saved since that copy was loaded.
    pub fn update_config(&self, change: impl FnOnce(&mut Config)) -> Result<Config> {
        self.try_update_config(|config| {
            change(config);
            Ok(())
        })
    }
    /// Fallible, locked read/modify/write. Neither rejected changes nor parse failures write.
    pub fn try_update_config(
        &self,
        change: impl FnOnce(&mut Config) -> Result<()>,
    ) -> Result<Config> {
        let _lock = self.config_lock()?;
        let mut config = self.load_config()?;
        change(&mut config)?;
        atomic_json(&self.config, &config)?;
        Ok(config)
    }
    /// Preserve unknown fields by patching only differences made through typed settings adapters.
    /// A no-op callback never rewrites a valid legacy document or creates a missing config file.
    pub(crate) fn try_patch_config(
        &self,
        change: impl FnOnce(&mut Config) -> Result<()>,
    ) -> Result<Config> {
        let _lock = self.config_lock()?;
        let (mut config, mut document) = self.config_document()?;
        let before = serde_json::to_value(&config)?;
        change(&mut config)?;
        let after = serde_json::to_value(&config)?;
        if before != after {
            patch_typed_changes(&mut document, &before, &after);
            atomic_json(&self.config, &document)?;
        }
        Ok(config)
    }
    /// Serialises read-modify-write cycles across difu's processes.
    fn config_lock(&self) -> Result<nix::fcntl::Flock<fs::File>> {
        let path = self.config.with_extension("json.lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("Cannot open {}", path.display()))?;
        nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive)
            .map_err(|(_, error)| anyhow::anyhow!("Cannot lock difu settings: {error}"))
    }
    #[cfg(test)]
    pub fn save_config(&self, config: &Config) -> Result<()> {
        atomic_json(&self.config, config)
    }
    pub fn load_inbox(&self, key: &str) -> Result<Option<Vec<PrSummary>>> {
        let path = crate::github::store::root(self)
            .join("lists")
            .join(format!("{}.json", hash(key)));
        if path.exists() {
            let keys: Vec<PrKey> = serde_json::from_slice(&fs::read(path)?)?;
            let mut inbox = Vec::new();
            for key in keys {
                key.validate()?;
                if let Some(summary) =
                    crate::github::store::load(self, &key).and_then(|p| p.summary)
                {
                    inbox.push(summary);
                }
            }
            return Ok(Some(inbox));
        }
        // Import old lists once. Subsequent reads resolve the shared PR records.
        let legacy = self.cache.join(format!("inbox-{key}.json"));
        if !legacy.exists() {
            return Ok(None);
        }
        let inbox: Vec<PrSummary> =
            serde_json::from_slice(&fs::read(&legacy)?).context("Cannot read cached PR list")?;
        self.save_inbox(key, &inbox)?;
        let _ = fs::remove_file(legacy);
        self.load_inbox(key)
    }
    pub fn save_inbox(&self, key: &str, inbox: &[PrSummary]) -> Result<()> {
        for pr in inbox {
            crate::github::store::summary(self, pr)?;
        }
        let root = crate::github::store::root(self).join("lists");
        fs::create_dir_all(&root)?;
        atomic_json(
            &root.join(format!("{}.json", hash(key))),
            &inbox.iter().map(|p| &p.key).collect::<Vec<_>>(),
        )
    }
    pub fn load_guide(&self, key: &str) -> Result<Option<Guide>> {
        let path = self.cache.join(format!("{key}.json"));
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(
            serde_json::from_slice(&fs::read(&path)?)
                .context("Cached guide is damaged; regenerate it")?,
        ))
    }
    pub fn save_guide(&self, key: &str, guide: &Guide) -> Result<()> {
        atomic_json(&self.cache.join(format!("{key}.json")), guide)
    }
}

/// Apply differences between two typed Config serializations, retaining unknown raw leaves.
/// Callers supply typed callbacks, never user-provided JSON paths or values.
fn patch_typed_changes(
    target: &mut serde_json::Value,
    before: &serde_json::Value,
    after: &serde_json::Value,
) {
    use serde_json::Value;
    if before == after {
        return;
    }
    if let (Value::Object(previous), Value::Object(updated)) = (before, after) {
        if !target.is_object() {
            *target = Value::Object(serde_json::Map::new());
        }
        if let Some(object) = target.as_object_mut() {
            for key in previous.keys().filter(|key| !updated.contains_key(*key)) {
                object.remove(key);
            }
            for (key, value) in updated {
                let old = previous.get(key).unwrap_or(&Value::Null);
                if old != value {
                    patch_typed_changes(object.entry(key).or_insert(Value::Null), old, value);
                }
            }
        }
    } else {
        *target = after.clone();
    }
}

pub fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}

pub(crate) fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut temp =
        tempfile::NamedTempFile::new_in(path.parent().context("Missing parent directory")?)?;
    temp.write_all(&serde_json::to_vec_pretty(value)?)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn cached_guides_collapse_duplicates_without_removing_cross_chapter_references() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = Storage {
            config: directory.path().join("config.json"),
            cache: directory.path().into(),
        };
        fs::write(
            storage.cache.join("reused.json"),
            serde_json::to_vec(&serde_json::json!({
                "chapters": [
                    {"category": "regular", "title": "First", "explanation": "First use", "hunks": ["a", "b", "a"]},
                    {"category": "regular", "title": "Second", "explanation": "Second use", "hunks": ["a", "a"]}
                ]
            }))?,
        )?;
        let guide = storage
            .load_guide("reused")?
            .context("Missing cached guide")?;
        assert_eq!(
            guide
                .chapters
                .iter()
                .map(|c| c.hunks.clone())
                .collect::<Vec<_>>(),
            vec![vec!["a", "b"], vec!["a"]]
        );
        Ok(())
    }
    #[test]
    fn whitelist_migrates_once_and_repository_cache_survives_reopening() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = Storage {
            config: directory.path().join("config.json"),
            cache: directory.path().join("cache"),
        };
        fs::create_dir_all(&storage.cache)?;
        fs::write(
            &storage.config,
            r#"{
            "repositories":{"owner/first":"/tmp/local-clone"},
            "review_repositories":{"owner/first":true,"owner/second":false},
            "model":{"model":"custom","effort":"high"},"unified":true
        }"#,
        )?;
        let mut config = storage.load_config()?;
        assert_eq!(
            config.pinned_repositories,
            BTreeSet::from(["owner/first".into(), "owner/second".into()])
        );
        assert_eq!(
            config.repositories.get("owner/first"),
            Some(&PathBuf::from("/tmp/local-clone"))
        );
        assert_eq!(config.model.model, "custom");
        assert!(config.unified);
        config.pinned_repositories.clear();
        storage.save_config(&config)?;
        assert!(storage.load_config()?.pinned_repositories.is_empty());
        assert!(!fs::read_to_string(&storage.config)?.contains("review_repositories"));
        assert!(storage.load_repositories()?.is_none());
        storage.save_repositories(&["owner/first".into(), "owner/second".into()])?;
        assert_eq!(
            storage.load_repositories()?,
            Some(vec!["owner/first".into(), "owner/second".into()])
        );
        Ok(())
    }

    #[test]
    fn loading_legacy_settings_does_not_write_before_a_transaction() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = Storage {
            config: directory.path().join("config.json"),
            cache: directory.path().into(),
        };
        let original = r#"{"review_repositories":{"owner/repo":true},"theme":"nord"}"#;
        fs::write(&storage.config, original)?;
        let config = storage.load_config()?;
        assert!(config.pinned_repositories.contains("owner/repo"));
        assert_eq!(fs::read_to_string(&storage.config)?, original);
        Ok(())
    }

    #[test]
    fn atomic_settings_and_private_cache_round_trip() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = Storage {
            config: directory.path().join("config.json"),
            cache: directory.path().into(),
        };
        let config = Config {
            unified: true,
            pinned_repositories: BTreeSet::from(["owner/repo".into()]),
            ..Config::default()
        };
        storage.save_config(&config)?;
        assert!(storage.load_config()?.unified);
        assert!(
            storage
                .load_config()?
                .pinned_repositories
                .contains("owner/repo")
        );
        let old: Config = serde_json::from_str(r#"{"repositories":{},"unified":true}"#)?;
        assert!(old.pinned_repositories.is_empty());
        assert_eq!(old.conflict_model, ModelChoice::conflict_default());
        let saved: Config =
            serde_json::from_str(r#"{"model":{"model":"custom-guide","effort":"low"}}"#)?;
        assert_eq!(saved.model.model, "custom-guide");
        assert_eq!(saved.conflict_model, ModelChoice::conflict_default());
        assert_eq!(
            fs::metadata(&storage.config)?.permissions().mode() & 0o777,
            0o600
        );
        let key = hash("fixture");
        assert!(storage.load_guide(&key)?.is_none());
        storage.save_guide(&key, &Guide { chapters: vec![] })?;
        assert!(storage.load_guide(&key)?.is_some());
        fs::write(storage.cache.join(format!("{key}.json")), "broken")?;
        assert!(storage.load_guide(&key).is_err());
        Ok(())
    }
}
