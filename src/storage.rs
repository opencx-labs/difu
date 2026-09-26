use crate::{
    codex::Guide,
    model::{ModelChoice, PrSummary},
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
    pub repository: Option<PathBuf>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub isolated: bool,
}
impl Default for AgentDefaults {
    fn default() -> Self {
        Self {
            repository: None,
            model: None,
            effort: None,
            isolated: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub local_diff_roots: Vec<PathBuf>,
    #[serde(default)]
    pub local_diff_repositories: BTreeSet<PathBuf>,
    #[serde(default = "enabled")]
    pub last_tab_agents: bool,
    #[serde(default)]
    pub agent_defaults: AgentDefaults,
    #[serde(default)]
    pub repositories: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub pinned_repositories: BTreeSet<String>,
    #[serde(default)]
    pub pinned_sessions: BTreeSet<String>,
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
}
fn enabled() -> bool {
    true
}
impl Default for Config {
    fn default() -> Self {
        Self {
            local_diff_roots: Vec::new(),
            local_diff_repositories: BTreeSet::new(),
            last_tab_agents: true,
            agent_defaults: AgentDefaults::default(),
            repositories: BTreeMap::new(),
            pinned_repositories: BTreeSet::new(),
            pinned_sessions: BTreeSet::new(),
            model: ModelChoice::default(),
            conflict_model: ModelChoice::conflict_default(),
            unified: false,
            wrap_diff: false,
            agent_list_visible: true,
            agent_changes_visible: false,
            agent_panel_right: true,
            voice_enabled: false,
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
        if !self.config.exists() {
            return Ok(Config::default());
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
            self.save_config(&config)?;
        }
        Ok(config)
    }
    pub fn load_repositories(&self) -> Result<Option<Vec<String>>> {
        let path = self.cache.join("repositories.json");
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
        atomic_json(&self.cache.join("repositories.json"), &repos)
    }

    pub fn save_config(&self, config: &Config) -> Result<()> {
        atomic_json(&self.config, config)
    }
    pub fn load_inbox(&self, key: &str) -> Result<Option<Vec<PrSummary>>> {
        let path = self.cache.join(format!("inbox-{key}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let inbox: Vec<PrSummary> =
            serde_json::from_slice(&fs::read(path)?).context("Cannot read cached PR list")?;
        for pr in &inbox {
            pr.key.validate()?;
        }
        Ok(Some(inbox))
    }
    pub fn save_inbox(&self, key: &str, inbox: &[PrSummary]) -> Result<()> {
        atomic_json(&self.cache.join(format!("inbox-{key}.json")), &inbox)
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
