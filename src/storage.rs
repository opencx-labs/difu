use crate::{codex::Guide, model::ModelChoice};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub repositories: BTreeMap<String, PathBuf>,
    /// Explicit whitelist for the repository inbox; values are active filters.
    #[serde(default)]
    pub review_repositories: BTreeMap<String, bool>,
    #[serde(default)]
    pub model: ModelChoice,
    #[serde(default)]
    pub unified: bool,
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
        serde_json::from_slice(&fs::read(&self.config)?)
            .context("Cannot read difu config.json; fix its JSON or move it aside")
    }
    pub fn save_config(&self, config: &Config) -> Result<()> {
        atomic_json(&self.config, config)
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

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
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
    fn atomic_settings_and_private_cache_round_trip() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = Storage {
            config: directory.path().join("config.json"),
            cache: directory.path().into(),
        };
        let config = Config {
            unified: true,
            review_repositories: BTreeMap::from([("owner/repo".into(), false)]),
            ..Config::default()
        };
        storage.save_config(&config)?;
        assert!(storage.load_config()?.unified);
        assert_eq!(
            storage.load_config()?.review_repositories.get("owner/repo"),
            Some(&false)
        );
        let old: Config = serde_json::from_str(r#"{"repositories":{},"unified":true}"#)?;
        assert!(old.review_repositories.is_empty());
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
