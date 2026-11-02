//! One Claude Code global config (`.claude.json`) that every Claude Code
//! process on a server shares safely: the service, each session's sandbox and
//! logins over SSH.
//!
//! Claude Code keeps the file at `$CLAUDE_CONFIG_DIR/.claude.json`, or
//! `~/.claude.json` when the variable is unset. It saves the file under a lock
//! directory next to it (`.claude.json.lock`, taken with `mkdir` and released
//! with `rmdir`), writing a temporary `.claude.json.tmp.<pid>.<hex>` beside it
//! and renaming that over the file. Both only work across processes that see
//! the same directory. A sandbox that mounted the single file `~/.claude.json`
//! took the lock in its own throwaway home, so concurrent sessions never
//! excluded each other, and its rename onto the mounted file failed
//! (EXDEV/EBUSY), so Claude Code fell back to truncating and rewriting the file
//! in place, which another process could read half written.
//!
//! On a server every Claude Code process therefore runs with
//! `CLAUDE_CONFIG_DIR=~/.claude`, the directory sandboxes mount: the file, its
//! lock and its temporary files all live in one host directory. [`share`]
//! moves an existing `~/.claude.json` there once, keeping a backup, and leaves
//! a symlink in its place, so a Claude Code started without the variable (an
//! SSH login, an installer) still reads the same file and replaces it
//! atomically through the link.
use anyhow::{Context, Result, bail};
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    thread,
    time::{Duration, SystemTime},
};

/// Moves every Claude Code home-directory file, `.claude.json` included.
pub const VARIABLE: &str = "CLAUDE_CONFIG_DIR";
const FILE: &str = ".claude.json";
/// The legacy file becomes a link to the shared one, relative to the home.
const LINK_TARGET: &str = ".claude/.claude.json";
/// Claude Code's lock library treats a lock older than this as abandoned; a
/// live holder refreshes its timestamp twice as often.
const STALE: Duration = Duration::from_secs(10);
/// How long a migration waits for Claude Code to release the lock.
const WAIT: Duration = Duration::from_secs(15);

/// `<home>/.claude`: the value of `CLAUDE_CONFIG_DIR` on a server.
pub fn dir(home: &Path) -> PathBuf {
    home.join(".claude")
}

/// Creates `<home>/.claude` (private) when missing, so a sandbox can mount it.
pub fn ensure_dir(home: &Path) -> Result<PathBuf> {
    let dir = dir(home);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("Cannot create {} for Claude Code", dir.display()))?;
    Ok(dir)
}

/// Puts the shared layout in place (see the module docs) and returns a note
/// when it moved a file. Cheap once done: one `lstat` of `~/.claude.json`.
pub fn share(home: &Path) -> Result<Option<String>> {
    share_with(home, WAIT)
}

fn share_with(home: &Path, wait: Duration) -> Result<Option<String>> {
    let legacy = home.join(FILE);
    match fs::symlink_metadata(&legacy) {
        // Already a link (difu's, or one the user made on purpose).
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(None),
        Ok(metadata) if metadata.is_file() => {
            ensure_dir(home)?;
            migrate(home, &legacy, wait)
        }
        // A directory or device by that name is not Claude Code's; leave it.
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure_dir(home)?;
            match std::os::unix::fs::symlink(LINK_TARGET, &legacy) {
                // Someone created the file meanwhile; the next call moves it.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(None),
                result => result
                    .map(|()| None)
                    .with_context(|| format!("Cannot link {}", legacy.display())),
            }
        }
        Err(error) => Err(error).with_context(|| format!("Cannot inspect {}", legacy.display())),
    }
}

/// Moves a regular `~/.claude.json` into the shared directory under Claude
/// Code's own lock. When both files exist, the more recently written one
/// wins and the other is kept as a backup; nothing is ever deleted.
fn migrate(home: &Path, legacy: &Path, wait: Duration) -> Result<Option<String>> {
    let shared = dir(home).join(FILE);
    let _lock = Lock::acquire(&lock_path(&shared), wait)?;
    // Another process may have finished the move while this one waited.
    let metadata = match fs::symlink_metadata(legacy) {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return Ok(None),
    };
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let backup = unused(&home.join(format!("{FILE}.difu-backup-{stamp}")));
    copy_private(legacy, &backup)?;
    let note = match fs::metadata(&shared) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            replace(&shared, &fs::read(legacy)?)?;
            format!(
                "Moved {} to {} so every Claude Code process shares it; the original is kept at {}",
                legacy.display(),
                shared.display(),
                backup.display()
            )
        }
        Err(error) => {
            return Err(error).with_context(|| format!("Cannot inspect {}", shared.display()));
        }
        Ok(existing) if modified(&metadata) > modified(&existing) => {
            let older = unused(&dir(home).join(format!("{FILE}.difu-backup-{stamp}")));
            copy_private(&shared, &older)?;
            replace(&shared, &fs::read(legacy)?)?;
            format!(
                "Both {} and {} existed; kept the newer {} as the shared file. Backups: {} and {}",
                legacy.display(),
                shared.display(),
                legacy.display(),
                backup.display(),
                older.display()
            )
        }
        Ok(_) => format!(
            "Both {} and {} existed; kept the newer {}. The older {} is kept at {}",
            legacy.display(),
            shared.display(),
            shared.display(),
            legacy.display(),
            backup.display()
        ),
    };
    link(home, legacy)?;
    Ok(Some(note))
}

fn modified(metadata: &fs::Metadata) -> SystemTime {
    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH)
}

/// `path`, or `path-2`, `path-3`, ... when taken.
fn unused(path: &Path) -> PathBuf {
    let mut candidate = path.to_owned();
    let mut index = 2;
    while fs::symlink_metadata(&candidate).is_ok() {
        let mut name = path.as_os_str().to_owned();
        name.push(format!("-{index}"));
        candidate = PathBuf::from(name);
        index += 1;
    }
    candidate
}

fn copy_private(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to)
        .with_context(|| format!("Cannot back up {} to {}", from.display(), to.display()))?;
    fs::set_permissions(to, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Writes the whole file beside the target, then renames it over the target,
/// as Claude Code does, so no reader sees it half written.
fn replace(target: &Path, contents: &[u8]) -> Result<()> {
    let directory = target.parent().context("Missing config directory")?;
    let mut temp = tempfile::Builder::new()
        .prefix(".claude.json.difu-")
        .tempfile_in(directory)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.write_all(contents)?;
    temp.as_file().sync_all()?;
    temp.persist(target)
        .map_err(|error| error.error)
        .with_context(|| format!("Cannot write {}", target.display()))?;
    Ok(())
}

/// Replaces the legacy file with a link to the shared one in one rename.
fn link(home: &Path, legacy: &Path) -> Result<()> {
    let temporary = unused(&home.join(format!("{FILE}.difu-link-{}", std::process::id())));
    std::os::unix::fs::symlink(LINK_TARGET, &temporary)
        .with_context(|| format!("Cannot create {}", temporary.display()))?;
    fs::rename(&temporary, legacy).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })?;
    Ok(())
}

fn lock_path(file: &Path) -> PathBuf {
    let mut path = file.as_os_str().to_owned();
    path.push(".lock");
    PathBuf::from(path)
}

/// Claude Code's lock on its config: a directory created next to the file.
struct Lock(PathBuf);

impl Lock {
    fn acquire(path: &Path, wait: Duration) -> Result<Self> {
        let started = std::time::Instant::now();
        loop {
            match fs::create_dir(path) {
                Ok(()) => return Ok(Self(path.to_owned())),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("Cannot lock {}", path.display()));
                }
            }
            let abandoned = fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|at| at.elapsed().is_ok_and(|age| age > STALE));
            if abandoned {
                // A crashed holder; Claude Code's own lock library does the same.
                let _ = fs::remove_dir(path);
                continue;
            }
            if started.elapsed() >= wait {
                bail!(
                    "Claude Code kept {} locked for {} seconds; try again when its sessions are idle",
                    path.display(),
                    wait.as_secs()
                );
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> Result<(tempfile::TempDir, PathBuf)> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().canonicalize()?;
        Ok((temp, home))
    }

    fn backups(directory: &Path) -> Result<Vec<PathBuf>> {
        let mut found = Vec::new();
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".claude.json.difu-backup-"))
            {
                found.push(path);
            }
        }
        Ok(found)
    }

    fn set_modified(path: &Path, seconds_ago: u64) -> Result<()> {
        let at = SystemTime::now() - Duration::from_secs(seconds_ago);
        fs::File::options()
            .write(true)
            .open(path)?
            .set_modified(at)?;
        Ok(())
    }

    #[test]
    fn an_existing_config_moves_once_with_a_backup_and_a_link() -> Result<()> {
        let (_temp, home) = temp_home()?;
        let legacy = home.join(".claude.json");
        fs::write(&legacy, r#"{"hasCompletedOnboarding":true}"#)?;
        let note = share(&home)?.context("Expected a note about the move")?;
        assert!(note.contains("Moved"), "{note}");
        let shared = home.join(".claude/.claude.json");
        assert_eq!(
            fs::read_to_string(&shared)?,
            r#"{"hasCompletedOnboarding":true}"#
        );
        assert_eq!(fs::metadata(&shared)?.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            fs::metadata(home.join(".claude"))?.permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(fs::read_link(&legacy)?, Path::new(".claude/.claude.json"));
        // Reads through the link reach the shared file.
        assert_eq!(fs::read_to_string(&legacy)?, fs::read_to_string(&shared)?);
        let [backup] = backups(&home)?.try_into().ok().context("One backup")?;
        assert_eq!(
            fs::read_to_string(&backup)?,
            r#"{"hasCompletedOnboarding":true}"#
        );
        assert_eq!(fs::metadata(&backup)?.permissions().mode() & 0o777, 0o600);
        // The lock is released and the move happens once.
        assert!(!home.join(".claude/.claude.json.lock").exists());
        assert_eq!(share(&home)?, None);
        assert_eq!(backups(&home)?.len(), 1);
        Ok(())
    }

    #[test]
    fn without_a_config_the_link_points_at_the_future_shared_file() -> Result<()> {
        let (_temp, home) = temp_home()?;
        assert_eq!(share(&home)?, None);
        assert!(home.join(".claude").is_dir());
        assert_eq!(
            fs::read_link(home.join(".claude.json"))?,
            Path::new(".claude/.claude.json")
        );
        // A Claude Code without CLAUDE_CONFIG_DIR writes through the link.
        fs::write(home.join(".claude/.claude.json"), "{}")?;
        assert_eq!(fs::read_to_string(home.join(".claude.json"))?, "{}");
        assert_eq!(share(&home)?, None);
        Ok(())
    }

    #[test]
    fn when_both_exist_the_newer_wins_and_the_other_is_kept() -> Result<()> {
        for legacy_newer in [true, false] {
            let (_temp, home) = temp_home()?;
            let legacy = home.join(".claude.json");
            let shared = home.join(".claude/.claude.json");
            ensure_dir(&home)?;
            fs::write(&legacy, r#"{"from":"home"}"#)?;
            fs::write(&shared, r#"{"from":"shared"}"#)?;
            set_modified(&legacy, if legacy_newer { 10 } else { 600 })?;
            set_modified(&shared, if legacy_newer { 600 } else { 10 })?;
            let note = share(&home)?.context("Expected a note")?;
            assert!(note.contains("Both"), "{note}");
            let winner = if legacy_newer { "home" } else { "shared" };
            assert_eq!(
                fs::read_to_string(&shared)?,
                format!(r#"{{"from":"{winner}"}}"#)
            );
            assert!(fs::symlink_metadata(&legacy)?.file_type().is_symlink());
            // Both originals survive somewhere.
            let mut kept: Vec<String> = backups(&home)?
                .into_iter()
                .chain(backups(&home.join(".claude"))?)
                .map(fs::read_to_string)
                .collect::<io::Result<_>>()?;
            kept.push(fs::read_to_string(&shared)?);
            assert!(kept.contains(&r#"{"from":"home"}"#.to_owned()));
            assert!(kept.contains(&r#"{"from":"shared"}"#.to_owned()));
        }
        Ok(())
    }

    #[test]
    fn links_and_foreign_entries_are_left_alone() -> Result<()> {
        let (_temp, home) = temp_home()?;
        std::os::unix::fs::symlink("dotfiles/claude.json", home.join(".claude.json"))?;
        assert_eq!(share(&home)?, None);
        assert_eq!(
            fs::read_link(home.join(".claude.json"))?,
            Path::new("dotfiles/claude.json")
        );
        let (_temp, home) = temp_home()?;
        fs::create_dir(home.join(".claude.json"))?;
        assert_eq!(share(&home)?, None);
        assert!(home.join(".claude.json").is_dir());
        Ok(())
    }

    #[test]
    fn the_move_waits_for_claude_codes_lock_and_reclaims_an_abandoned_one() -> Result<()> {
        let (_temp, home) = temp_home()?;
        let legacy = home.join(".claude.json");
        fs::write(&legacy, "{}")?;
        let lock = home.join(".claude/.claude.json.lock");
        ensure_dir(&home)?;
        // A live holder: the move gives up rather than racing it.
        fs::create_dir(&lock)?;
        let error = share_with(&home, Duration::from_millis(200))
            .err()
            .context("A held lock must stop the move")?;
        assert!(format!("{error:#}").contains("locked"), "{error:#}");
        assert!(legacy.is_file() && !fs::symlink_metadata(&legacy)?.is_symlink());
        assert!(lock.is_dir(), "The holder's lock stays");
        // The holder releases it while the move waits.
        let release = lock.clone();
        let holder = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            fs::remove_dir(release)
        });
        assert!(share_with(&home, Duration::from_secs(5))?.is_some());
        holder
            .join()
            .map_err(|_| anyhow::anyhow!("holder panicked"))??;
        assert!(fs::symlink_metadata(&legacy)?.is_symlink());
        // An abandoned lock (older than Claude Code's stale limit) is taken over.
        let (_temp, home) = temp_home()?;
        fs::write(home.join(".claude.json"), "{}")?;
        let lock = home.join(".claude/.claude.json.lock");
        ensure_dir(&home)?;
        fs::create_dir(&lock)?;
        let old = SystemTime::now() - STALE - Duration::from_secs(5);
        fs::File::open(&lock)?.set_modified(old)?;
        assert!(share_with(&home, Duration::from_millis(200))?.is_some());
        assert!(!lock.exists());
        Ok(())
    }
}
