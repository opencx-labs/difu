//! This computer's copy of each saved server's sessions, so that they stay in
//! the sidebar and previewable while the server is unreachable, and the
//! messages waiting to be sent to it (docs/cloud-agents.md, *Offline servers*).
//!
//! Everything lives under `<config>/agents/servers/<server>/` (0700, files
//! written atomically):
//!
//! * `server.json`: the server id the cache belongs to; another server saved
//!   under the same name starts with an empty cache.
//! * `sessions.json`: the server's last session list and when it was read.
//! * `snapshots/<session>@<version>.json`: the last full copy of each session
//!   this computer opened or prefetched, with the server's own session id.
//!   Bounded: sessions the server no longer lists are dropped, then the least
//!   recently used copies beyond `MAX_SNAPSHOTS` or `MAX_BYTES`.
//! * `outbox.json`: messages sent while the server was unreachable, in order.
use crate::{
    agents::{Control, Prompt, Session, Skill, Summary, media},
    storage::Storage,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// Session copies kept per server.
pub const MAX_SNAPSHOTS: usize = 100;
/// Bytes of session copies kept per server; the newest copy is always kept.
pub const MAX_BYTES: u64 = 256 << 20;

/// One saved server's cache directory.
#[derive(Clone, Debug)]
pub struct Cache {
    dir: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Owner {
    server: String,
}

/// The server's session list as last read, with this service's ids.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Listed {
    /// Milliseconds since the epoch.
    pub at: i64,
    pub sessions: Vec<Summary>,
}

/// A message waiting for its server. Sent with `queue: true` once the server
/// is back, so it waits for any running turn like a queued message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Queued {
    /// Unique within the server's outbox.
    pub id: u64,
    /// The server's own session id (without the `<server>:` prefix).
    pub session: String,
    pub text: String,
    #[serde(default)]
    pub skills: Vec<Skill>,
    /// Private copies on this computer, uploaded when the message is sent.
    #[serde(default)]
    pub attachments: Vec<media::Attachment>,
    /// Milliseconds since the epoch.
    pub queued_at: i64,
    /// The server refused it; it waits for an edit or removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The connection dropped after it was sent and before the server
    /// confirmed it; before sending it again, difu checks whether it arrived.
    #[serde(default)]
    pub uncertain: bool,
}

impl Queued {
    /// How it appears in the session's queue.
    pub fn prompt(&self) -> Prompt {
        Prompt::WithSkills {
            text: self.text.clone(),
            skills: self.skills.clone(),
            attachments: self.attachments.clone(),
        }
    }

    /// The message as the server receives it (attachments still local).
    pub fn control(&self) -> Control {
        if self.attachments.is_empty() {
            Control::Message {
                text: self.text.clone(),
                queue: true,
                skills: self.skills.clone(),
                attachments: Vec::new(),
            }
        } else {
            Control::MessageWithAttachments {
                text: self.text.clone(),
                queue: true,
                skills: self.skills.clone(),
                attachments: self.attachments.clone(),
            }
        }
    }

    /// Takes an edited queue entry's text, skills and attachments.
    pub fn replace(&mut self, prompt: &Prompt) {
        self.text = prompt.text().to_owned();
        self.skills = prompt.skills().to_vec();
        self.attachments = prompt.attachments().to_vec();
        self.error = None;
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Outbox {
    #[serde(default)]
    messages: Vec<Queued>,
}

/// `<config>/agents/servers`.
fn root(storage: &Storage) -> Result<PathBuf> {
    Ok(crate::agents::server::home(storage)?.join("servers"))
}

/// Server names are lowercase letters, digits and dashes; session ids are
/// similar. Anything else is never used as a file name.
fn safe(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    anyhow::ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "{} must not be a symlink",
        path.display()
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut temp =
        tempfile::NamedTempFile::new_in(path.parent().context("Missing parent directory")?)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Forgets the cache of a server that is no longer saved.
pub fn remove(storage: &Storage, name: &str) -> Result<()> {
    if !safe(name) {
        return Ok(());
    }
    match fs::remove_dir_all(root(storage)?.join(name)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

/// Forgets the caches of every server not in `saved`.
pub fn retain(storage: &Storage, saved: &BTreeSet<String>) -> Result<()> {
    let Ok(entries) = fs::read_dir(root(storage)?) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !saved.contains(&name) {
            remove(storage, &name)?;
        }
    }
    Ok(())
}

impl Cache {
    /// The cache of server `name` (id `server`), emptied when it belonged to
    /// another server saved under the same name.
    pub fn open(storage: &Storage, name: &str, server: &str) -> Result<Self> {
        anyhow::ensure!(safe(name), "Invalid server name for the cache: {name}");
        let root = root(storage)?;
        private_dir(&root)?;
        let dir = root.join(name);
        let owner = dir.join("server.json");
        if read::<Owner>(&owner).is_some_and(|owner| owner.server != server) {
            fs::remove_dir_all(&dir)?;
        }
        private_dir(&dir)?;
        private_dir(&dir.join("snapshots"))?;
        if !owner.exists() {
            atomic(
                &owner,
                &serde_json::to_vec(&Owner {
                    server: server.to_owned(),
                })?,
            )?;
        }
        Ok(Self { dir })
    }

    pub fn list(&self) -> Option<Listed> {
        read(&self.dir.join("sessions.json"))
    }

    pub fn save_list(&self, listed: &Listed) -> Result<()> {
        atomic(
            &self.dir.join("sessions.json"),
            &serde_json::to_vec(listed)?,
        )
    }

    pub fn outbox(&self) -> Vec<Queued> {
        read::<Outbox>(&self.dir.join("outbox.json"))
            .map(|outbox| outbox.messages)
            .unwrap_or_default()
    }

    pub fn save_outbox(&self, messages: &[Queued]) -> Result<()> {
        let path = self.dir.join("outbox.json");
        if messages.is_empty() {
            return match fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
                _ => Ok(()),
            };
        }
        atomic(
            &path,
            &serde_json::to_vec_pretty(&Outbox {
                messages: messages.to_vec(),
            })?,
        )
    }

    /// Cached copies: session id, version, path, bytes and last use.
    fn snapshots(&self) -> Vec<(String, u64, PathBuf, u64, SystemTime)> {
        let Ok(entries) = fs::read_dir(self.dir.join("snapshots")) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let (id, version) = name.strip_suffix(".json")?.rsplit_once('@')?;
                let metadata = entry.metadata().ok()?;
                Some((
                    id.to_owned(),
                    version.parse().ok()?,
                    entry.path(),
                    metadata.len(),
                    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                ))
            })
            .collect()
    }

    /// The version of each cached copy, by session id.
    pub fn versions(&self) -> BTreeMap<String, u64> {
        let mut versions = BTreeMap::new();
        for (id, version, ..) in self.snapshots() {
            let newest = versions.entry(id).or_insert(version);
            *newest = (*newest).max(version);
        }
        versions
    }

    /// The version of the cached copy of session `id`, if any.
    pub fn version(&self, id: &str) -> Option<u64> {
        self.versions().get(id).copied()
    }

    /// The cached copy of session `id` (the server's id), marked as used.
    pub fn snapshot(&self, id: &str) -> Option<Session> {
        let (_, _, path, ..) = self
            .snapshots()
            .into_iter()
            .filter(|(session, ..)| session == id)
            .max_by_key(|(_, version, ..)| *version)?;
        let session: Session = read(&path)?;
        if let Ok(file) = fs::File::options().append(true).open(&path) {
            let _ = file.set_modified(SystemTime::now());
        }
        (session.id == id).then_some(session)
    }

    /// Keeps `session` (with the server's own id) as the copy of its session,
    /// replacing older copies, then enforces the bounds. A newer copy, kept
    /// meanwhile by another watch of the same session, stays instead.
    pub fn save_snapshot(&self, session: &Session) -> Result<()> {
        if !safe(&session.id) {
            return Ok(());
        }
        let copies: Vec<_> = self
            .snapshots()
            .into_iter()
            .filter(|(id, ..)| *id == session.id)
            .collect();
        if copies
            .iter()
            .any(|(_, version, ..)| *version > session.version)
        {
            return Ok(());
        }
        let name = format!("{}@{}.json", session.id, session.version);
        let path = self.dir.join("snapshots").join(&name);
        atomic(&path, &serde_json::to_vec(session)?)?;
        for (_, _, older, ..) in copies {
            if older != path {
                let _ = fs::remove_file(older);
            }
        }
        self.bound()
    }

    pub fn forget_snapshot(&self, id: &str) {
        for (session, _, path, ..) in self.snapshots() {
            if session == id {
                let _ = fs::remove_file(path);
            }
        }
    }

    /// Drops copies of sessions the server no longer lists, then bounds the rest.
    pub fn prune(&self, listed: &BTreeSet<String>) -> Result<()> {
        for (id, _, path, ..) in self.snapshots() {
            if !listed.contains(&id) {
                fs::remove_file(path)?;
            }
        }
        self.bound()
    }

    /// At most `MAX_SNAPSHOTS` copies and `MAX_BYTES`, most recently used first;
    /// the newest copy always stays.
    fn bound(&self) -> Result<()> {
        let mut copies = self.snapshots();
        copies.sort_by_key(|copy| std::cmp::Reverse(copy.4));
        let mut kept = 0;
        let mut bytes = 0u64;
        for (_, _, path, size, _) in copies {
            if kept > 0 && (kept >= MAX_SNAPSHOTS || bytes.saturating_add(size) > MAX_BYTES) {
                fs::remove_file(path)?;
                continue;
            }
            kept += 1;
            bytes = bytes.saturating_add(size);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::{federation::tests::session, servers::tests::storage};

    #[test]
    fn session_lists_and_copies_survive_a_restart_and_follow_the_server() -> Result<()> {
        let (_dir, storage) = storage()?;
        let cache = Cache::open(&storage, "aws", "id-aws")?;
        assert!(cache.list().is_none());
        assert!(cache.snapshot("1").is_none());
        let mut summary = session("1").summary();
        summary.id = "aws:1".into();
        summary.server = Some("aws".into());
        cache.save_list(&Listed {
            at: 42,
            sessions: vec![summary],
        })?;
        let mut copy = session("1");
        copy.note("agentMessage", "kept for offline reading");
        copy.version = 7;
        cache.save_snapshot(&copy)?;
        let mut newer = copy.clone();
        newer.version = 8;
        cache.save_snapshot(&newer)?;
        // A late writer with an older copy does not replace the newer one.
        cache.save_snapshot(&copy)?;

        let reopened = Cache::open(&storage, "aws", "id-aws")?;
        let listed = reopened.list().context("list")?;
        assert_eq!(listed.at, 42);
        assert_eq!(
            listed.sessions.first().map(|s| s.id.as_str()),
            Some("aws:1")
        );
        assert_eq!(reopened.version("1"), Some(8));
        let snapshot = reopened.snapshot("1").context("snapshot")?;
        assert_eq!(snapshot.version, 8);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(reopened.snapshots().len(), 1, "older copies are replaced");
        let dir = root(&storage)?.join("aws");
        assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o700);

        // Another server saved as `aws` starts empty.
        let other = Cache::open(&storage, "aws", "id-other")?;
        assert!(other.list().is_none());
        assert!(other.snapshot("1").is_none());
        // Removed servers lose their cache; unsafe names never reach the disk.
        Cache::open(&storage, "home", "id-home")?.save_list(&Listed::default())?;
        retain(&storage, &BTreeSet::from(["aws".to_owned()]))?;
        assert!(!root(&storage)?.join("home").exists());
        assert!(root(&storage)?.join("aws").exists());
        remove(&storage, "aws")?;
        assert!(!root(&storage)?.join("aws").exists());
        assert!(Cache::open(&storage, "../x", "id").is_err());
        let mut odd = session("../escape");
        odd.version = 1;
        Cache::open(&storage, "aws", "id-aws")?.save_snapshot(&odd)?;
        assert!(!root(&storage)?.join("aws/escape@1.json").exists());
        Ok(())
    }

    #[test]
    fn copies_are_bounded_and_unlisted_sessions_are_dropped() -> Result<()> {
        let (_dir, storage) = storage()?;
        let cache = Cache::open(&storage, "aws", "id-aws")?;
        for index in 0..MAX_SNAPSHOTS + 5 {
            let mut copy = session(&format!("s{index}"));
            copy.version = 1;
            cache.save_snapshot(&copy)?;
            // Distinct use times, oldest first.
            let path = cache.dir.join("snapshots").join(format!("s{index}@1.json"));
            fs::File::options().append(true).open(&path)?.set_modified(
                SystemTime::UNIX_EPOCH
                    + std::time::Duration::from_secs(1_000 + u64::try_from(index)?),
            )?;
        }
        cache.bound()?;
        assert_eq!(cache.snapshots().len(), MAX_SNAPSHOTS);
        // The least recently used went first.
        assert!(cache.snapshot("s0").is_none());
        assert!(cache.snapshot("s4").is_none());
        assert!(cache.snapshot("s5").is_some());
        let listed = BTreeSet::from(["s5".to_owned(), "s6".to_owned()]);
        cache.prune(&listed)?;
        let mut left: Vec<_> = cache.snapshots().into_iter().map(|c| c.0).collect();
        left.sort();
        assert_eq!(left, vec!["s5", "s6"]);
        cache.forget_snapshot("s6");
        assert!(cache.version("s6").is_none());
        assert_eq!(cache.version("s5"), Some(1));
        Ok(())
    }

    #[test]
    fn the_outbox_survives_a_restart_in_order() -> Result<()> {
        let (_dir, storage) = storage()?;
        let cache = Cache::open(&storage, "aws", "id-aws")?;
        assert!(cache.outbox().is_empty());
        let queued = |id: u64, text: &str| Queued {
            id,
            session: "1".into(),
            text: text.into(),
            skills: Vec::new(),
            attachments: Vec::new(),
            queued_at: 1,
            error: None,
            uncertain: false,
        };
        cache.save_outbox(&[queued(1, "first"), queued(2, "second")])?;
        let texts: Vec<_> = Cache::open(&storage, "aws", "id-aws")?
            .outbox()
            .into_iter()
            .map(|q| q.text)
            .collect();
        assert_eq!(texts, vec!["first", "second"]);
        let mut edited = queued(1, "first");
        edited.error = Some("refused".into());
        edited.replace(&Prompt::WithSkills {
            text: "edited".into(),
            skills: Vec::new(),
            attachments: Vec::new(),
        });
        assert_eq!(
            (edited.text.as_str(), edited.error.as_deref()),
            ("edited", None)
        );
        assert!(matches!(
            edited.control(),
            Control::Message { queue: true, ref text, .. } if text == "edited"
        ));
        cache.save_outbox(&[])?;
        assert!(cache.outbox().is_empty());
        assert!(!cache.dir.join("outbox.json").exists());
        Ok(())
    }
}
