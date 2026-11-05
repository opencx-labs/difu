//! Noticing a newer difu installed over the running one, as `brew upgrade` does.
//! The background service then replaces itself, and sessions carry on across
//! the restart; the terminal app offers `/restart`, which reloads it in place.
use crate::{process, storage::Storage};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime},
};

/// How often the installed file is looked at. Its version is read only when
/// the file has changed.
const INTERVAL: Duration = Duration::from_secs(30);

pub struct Watch {
    path: PathBuf,
    /// The file last read, by modification time and size.
    file: Option<(SystemTime, u64)>,
    checked: Option<Instant>,
}

impl Watch {
    /// Watches the path this program runs from. Homebrew's is a symbolic link
    /// that moves to each new version.
    pub fn new() -> Option<Self> {
        let path = std::env::current_exe().ok()?;
        // Linux names a replaced executable `<path> (deleted)`.
        let path = path
            .to_str()
            .and_then(|path| path.strip_suffix(" (deleted)"))
            .map_or(path.clone(), PathBuf::from);
        Some(Self::at(path))
    }

    fn at(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            checked: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The version installed at the path, once, when it is newer than this
    /// program.
    pub fn poll(&mut self, now: Instant) -> Option<String> {
        self.poll_with(now, env!("CARGO_PKG_VERSION"), installed_version)
    }

    fn poll_with(
        &mut self,
        now: Instant,
        running: &str,
        read: impl FnOnce(&Path) -> Option<String>,
    ) -> Option<String> {
        if self
            .checked
            .is_some_and(|at| now.saturating_duration_since(at) < INTERVAL)
        {
            return None;
        }
        self.checked = Some(now);
        let metadata = fs::metadata(&self.path).ok()?;
        let file = (metadata.modified().ok()?, metadata.len());
        if self.file == Some(file) {
            return None;
        }
        // Not recorded until it answers: an install under way is read again.
        let version = read(&self.path)?;
        self.file = Some(file);
        newer(&version, running).then_some(version)
    }
}

fn newer(version: &str, running: &str) -> bool {
    use crate::agents::client::version_number;
    matches!(
        (version_number(version), version_number(running)),
        (Ok(version), Ok(running)) if version > running
    )
}

fn installed_version(path: &Path) -> Option<String> {
    let output = process::run(
        Command::new(path).arg("--version"),
        None,
        &process::Cancel::default(),
    )
    .ok()?;
    (output.code == 0)
        .then(|| parse_version(&String::from_utf8_lossy(&output.stdout)))
        .flatten()
}

/// `difu 1.2.3` as `--version` prints it.
fn parse_version(output: &str) -> Option<String> {
    output
        .trim()
        .strip_prefix("difu ")
        .map(|version| version.trim().to_owned())
}

/// Becomes the service installed at `path`, keeping this process and its log.
/// Only returns if that fails.
pub fn replace_service(path: &Path, storage: &Storage) -> anyhow::Error {
    use std::os::unix::process::CommandExt;
    Command::new(path)
        .arg("--agent-service")
        .arg("--agent-config")
        .arg(&storage.config)
        .arg("--agent-cache")
        .arg(&storage.cache)
        .exec()
        .into()
}

/// Becomes the terminal app installed at `path`, in the same terminal and with
/// the same arguments. Only returns if that fails.
pub fn restart_app(path: &Path) -> anyhow::Error {
    use std::os::unix::process::CommandExt;
    Command::new(path)
        .args(std::env::args_os().skip(1))
        .exec()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    fn watch() -> Result<(tempfile::TempDir, Watch)> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("difu");
        fs::write(&path, "installed")?;
        Ok((dir, Watch::at(path)))
    }

    #[test]
    fn a_newer_install_is_reported_once() -> Result<()> {
        let (_dir, mut watch) = watch()?;
        let start = Instant::now();
        let found = watch.poll_with(start, "1.2.3", |_| Some("1.3.0".into()));
        assert_eq!(found.as_deref(), Some("1.3.0"));
        // The same file is not read again.
        let later = start + INTERVAL;
        let read = std::cell::Cell::new(false);
        let again = watch.poll_with(later, "1.2.3", |_| {
            read.set(true);
            Some("1.3.0".into())
        });
        assert!(again.is_none() && !read.get());
        Ok(())
    }

    #[test]
    fn the_same_or_an_older_install_is_not_an_upgrade() -> Result<()> {
        for installed in ["1.2.3", "1.2.2", "0.9.0", "not a version"] {
            let (_dir, mut watch) = watch()?;
            let found = watch.poll_with(Instant::now(), "1.2.3", |_| Some(installed.into()));
            assert!(found.is_none(), "{installed}");
        }
        Ok(())
    }

    #[test]
    fn the_file_is_only_looked_at_every_interval() -> Result<()> {
        let (_dir, mut watch) = watch()?;
        let start = Instant::now();
        assert!(watch.poll_with(start, "1.2.3", |_| None).is_none());
        let soon = start + INTERVAL - Duration::from_secs(1);
        assert!(
            watch
                .poll_with(soon, "1.2.3", |_| Some("9.0.0".into()))
                .is_none()
        );
        let found = watch.poll_with(start + INTERVAL, "1.2.3", |_| Some("9.0.0".into()));
        assert_eq!(found.as_deref(), Some("9.0.0"));
        Ok(())
    }

    #[test]
    fn an_install_that_does_not_answer_yet_is_read_again() -> Result<()> {
        let (_dir, mut watch) = watch()?;
        let start = Instant::now();
        assert!(watch.poll_with(start, "1.2.3", |_| None).is_none());
        let found = watch.poll_with(start + INTERVAL, "1.2.3", |_| Some("1.2.4".into()));
        assert_eq!(found.as_deref(), Some("1.2.4"));
        Ok(())
    }

    #[test]
    fn a_missing_file_is_no_upgrade() {
        let mut watch = Watch::at("/nonexistent/difu".into());
        assert!(
            watch
                .poll_with(Instant::now(), "1.2.3", |_| Some("9.0.0".into()))
                .is_none()
        );
    }

    #[test]
    fn versions_are_read_from_what_difu_prints() {
        assert_eq!(parse_version("difu 0.42.0\n").as_deref(), Some("0.42.0"));
        assert_eq!(parse_version("something else"), None);
    }
}
