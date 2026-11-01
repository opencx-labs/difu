//! Server repositories: one base clone per `owner/name`, refreshed in the
//! background so new sessions start without a network clone.
use super::cli::ReposCommand;
use crate::{
    process::{self, Cancel},
    repo,
    storage::{self, Storage},
};
use anyhow::{Context, Result, ensure};
use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, IsTerminal, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const STATUS: &str = ".status.json";
const FETCH_LIMIT: Duration = Duration::from_secs(10 * 60);
const CLONE_LIMIT: Duration = Duration::from_secs(60 * 60);

/// One server repository as clients see it (`server.repos`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoStatus {
    pub name: String,
    pub path: PathBuf,
    pub cloned: bool,
    /// Last successful clone or fetch, in milliseconds since the epoch.
    pub fetched_at: Option<i64>,
    pub error: Option<String>,
}

/// Fetch results, kept in `.status.json` next to the clones.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Fetch {
    fetched_at: Option<i64>,
    error: Option<String>,
}

/// Checks `owner/name` against GitHub's characters so it is safe as a path and
/// as a command argument. Returns `(owner, name)`.
pub fn validate(name: &str) -> Result<(&str, &str)> {
    let valid = name.split_once('/').filter(|(owner, repo)| {
        (1..=39).contains(&owner.len())
            && owner.starts_with(|c: char| c.is_ascii_alphanumeric())
            && owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && (1..=100).contains(&repo.len())
            && !repo.starts_with('-')
            && *repo != "."
            && *repo != ".."
            && repo
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    });
    valid.with_context(|| format!("Invalid repository `{name}`; expected owner/name"))
}

fn root(storage: &Storage) -> Result<PathBuf> {
    let root = crate::agents::server::home(storage)?.join("repos");
    fs::create_dir_all(&root)?;
    Ok(root)
}

/// Where the base clone of `owner/name` lives (whether or not it exists yet).
pub fn path(storage: &Storage, name: &str) -> Result<PathBuf> {
    let (owner, repo) = validate(name)?;
    Ok(root(storage)?.join(owner).join(repo))
}

fn cloned(path: &Path) -> bool {
    path.join(".git").is_dir()
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Waits for an exclusive lock on `path` until `cancel` fires.
fn lock(path: &Path, cancel: &Cancel) -> Result<Flock<File>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("Cannot open {}", path.display()))?;
    loop {
        cancel.check()?;
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lease) => return Ok(lease),
            Err((returned, nix::errno::Errno::EWOULDBLOCK)) => file = returned,
            Err((_, error)) => return Err(error.into()),
        }
        thread::sleep(Duration::from_millis(40));
    }
}

/// Serialises clones and fetches of one repository.
fn repository_lock(root: &Path, owner: &str, repo: &str, cancel: &Cancel) -> Result<Flock<File>> {
    lock(
        &root.join(".locks").join(owner).join(format!("{repo}.lock")),
        cancel,
    )
}

fn statuses(root: &Path) -> BTreeMap<String, Fetch> {
    fs::read(root.join(STATUS))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn record(root: &Path, name: &str, change: impl FnOnce(&mut Fetch)) -> Result<()> {
    let _lock = lock(&root.join(".status.lock"), &Cancel::default())?;
    let mut all = statuses(root);
    change(all.entry(name.to_owned()).or_default());
    storage::atomic_json(&root.join(STATUS), &all)
}

/// Runs `work` with a cancel that fires after `limit` or with `outer`, so a
/// stalled network operation cannot hold its locks forever.
pub(super) fn bounded<T>(
    limit: Duration,
    outer: &Cancel,
    work: impl FnOnce(&Cancel) -> Result<T>,
) -> Result<T> {
    let cancel = Cancel::default();
    let done = AtomicBool::new(false);
    let expired = AtomicBool::new(false);
    let result = thread::scope(|scope| {
        scope.spawn(|| {
            let start = Instant::now();
            while !done.load(Ordering::Relaxed) {
                if outer.cancelled() || start.elapsed() >= limit {
                    expired.store(!outer.cancelled(), Ordering::Relaxed);
                    cancel.cancel();
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
        });
        let result = work(&cancel);
        done.store(true, Ordering::Relaxed);
        result
    });
    match result {
        Err(error) if expired.load(Ordering::Relaxed) => {
            Err(error.context(format!("Timed out after {} seconds", limit.as_secs())))
        }
        result => result,
    }
}

/// The last few lines of a failed command, without progress noise.
pub(super) fn failure(output: &process::Output) -> String {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = text
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.contains('%'))
        .collect();
    let tail = lines.get(lines.len().saturating_sub(3)..).unwrap_or(&[]);
    if tail.is_empty() {
        format!("exit status {}", output.code)
    } else {
        tail.join(" ")
    }
}

pub fn list(storage: &Storage) -> Result<Vec<RepoStatus>> {
    let root = root(storage)?;
    let statuses = statuses(&root);
    Ok(storage
        .load_config()?
        .server
        .repositories
        .into_iter()
        .map(|name| match validate(&name) {
            Ok((owner, repo)) => {
                let path = root.join(owner).join(repo);
                let fetch = statuses.get(&name).cloned().unwrap_or_default();
                RepoStatus {
                    cloned: cloned(&path),
                    path,
                    fetched_at: fetch.fetched_at,
                    error: fetch.error,
                    name,
                }
            }
            Err(error) => RepoStatus {
                error: Some(format!("{error:#}")),
                name,
                path: PathBuf::new(),
                cloned: false,
                fetched_at: None,
            },
        })
        .collect())
}

/// Lists `name` without cloning it. Names compare case-insensitively, like
/// GitHub's; returns the spelling that is listed.
pub fn add(storage: &Storage, name: &str) -> Result<String> {
    let name = name.trim();
    validate(name)?;
    let mut listed = name.to_owned();
    storage.update_config(|config| {
        let repositories = &mut config.server.repositories;
        match repositories.iter().find(|r| r.eq_ignore_ascii_case(name)) {
            Some(existing) => listed = existing.clone(),
            None => repositories.push(name.to_owned()),
        }
    })?;
    Ok(listed)
}

/// Stops listing `name`. The clone stays, since sessions may still use it.
pub fn remove(storage: &Storage, name: &str) -> Result<()> {
    let name = name.trim();
    validate(name)?;
    storage.try_update_config(|config| {
        let repositories = &mut config.server.repositories;
        let before = repositories.len();
        repositories.retain(|r| !r.eq_ignore_ascii_case(name));
        ensure!(
            repositories.len() != before,
            "{name} is not a server repository"
        );
        Ok(())
    })?;
    let root = root(storage)?;
    let _lock = lock(&root.join(".status.lock"), &Cancel::default())?;
    let mut all = statuses(&root);
    all.retain(|listed, _| !listed.eq_ignore_ascii_case(name));
    storage::atomic_json(&root.join(STATUS), &all)
}

/// Lists `name` and clones it when missing. Used when a session starts on a
/// repository that is not listed yet.
pub fn ensure(storage: &Storage, name: &str) -> Result<PathBuf> {
    let name = add(storage, name)?;
    clone(storage, &name, None, &Cancel::default())
}

/// A session's `repository`: a path, or `owner/name` (listed and cloned first).
/// A server repository's path that was never cloned is cloned too.
pub fn resolve(storage: &Storage, repository: &str) -> Result<PathBuf> {
    let repository = repository.trim();
    if !repository.starts_with(['/', '~', '.']) && validate(repository).is_ok() {
        return ensure(storage, repository);
    }
    let path = PathBuf::from(repository);
    let root = root(storage)?;
    if let Ok(relative) = path.strip_prefix(&root)
        && let Some(name) = relative.to_str().filter(|name| validate(name).is_ok())
        && !cloned(&path)
    {
        return ensure(storage, name);
    }
    Ok(path)
}

fn clone(
    storage: &Storage,
    name: &str,
    progress: Option<fn(&str)>,
    cancel: &Cancel,
) -> Result<PathBuf> {
    bounded(CLONE_LIMIT, cancel, |cancel| {
        clone_with(storage, name, progress, cancel)
    })
}

fn clone_with(
    storage: &Storage,
    name: &str,
    progress: Option<fn(&str)>,
    cancel: &Cancel,
) -> Result<PathBuf> {
    let (owner, repo) = validate(name)?;
    let root = root(storage)?;
    let destination = root.join(owner).join(repo);
    let _lock = repository_lock(&root, owner, repo, cancel)?;
    if cloned(&destination) {
        return Ok(destination);
    }
    record(&root, name, |fetch| fetch.error = None)?;
    let result = (|| {
        ensure!(
            !destination.exists(),
            "{} exists but is not a Git clone; move it aside",
            destination.display()
        );
        // Clone beside the final path so an interrupted clone never looks done.
        let partial = root.join(".partial").join(owner).join(repo);
        if fs::symlink_metadata(&partial).is_ok() {
            fs::remove_dir_all(&partial)?;
        }
        fs::create_dir_all(partial.parent().context("Missing clone directory")?)?;
        let mut command = Command::new("gh");
        command
            .env("GH_PROMPT_DISABLED", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["repo", "clone", name])
            .arg(&partial);
        if progress.is_some() {
            command.args(["--", "--progress"]);
        }
        let output = process::streaming_with_stderr(
            &mut command,
            None,
            cancel,
            |_| {},
            move |line| {
                if let Some(progress) = progress {
                    progress(line)
                }
            },
        )?;
        if output.code != 0 {
            let _ = fs::remove_dir_all(&partial);
            anyhow::bail!("Cannot clone {name}: {}", failure(&output));
        }
        fs::create_dir_all(destination.parent().context("Missing owner directory")?)?;
        fs::rename(&partial, &destination)?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            record(&root, name, |fetch| {
                fetch.fetched_at = Some(now());
                fetch.error = None;
            })?;
            Ok(destination)
        }
        Err(error) => {
            let message = format!("{error:#}");
            record(&root, name, |fetch| fetch.error = Some(message))?;
            Err(error)
        }
    }
}

/// `git fetch --prune origin` under the repository lock and the lock session
/// starts take for their fetch-and-pin, so the two never race.
fn fetch(storage: &Storage, name: &str, cancel: &Cancel) -> Result<()> {
    let (owner, repo) = validate(name)?;
    let root = root(storage)?;
    let path = root.join(owner).join(repo);
    let _lock = repository_lock(&root, owner, repo, cancel)?;
    let result = (|| {
        let _main = lock(&path.join(".git").join("difu-main-fetch.lock"), cancel)?;
        let output = process::run(
            repo::git(&path)
                .env("GIT_ALLOW_PROTOCOL", "https:ssh:file")
                .args([
                    // gh's own authenticated Git commands use the same helper.
                    "-c",
                    "credential.https://github.com.helper=",
                    "-c",
                    "credential.https://github.com.helper=!gh auth git-credential",
                    "fetch",
                    "--prune",
                    "--no-auto-maintenance",
                    "origin",
                ]),
            None,
            cancel,
        )?;
        ensure!(
            output.code == 0,
            "Cannot fetch {name}: {}",
            failure(&output)
        );
        Ok(())
    })();
    match result {
        Ok(()) => record(&root, name, |fetch| {
            fetch.fetched_at = Some(now());
            fetch.error = None;
        }),
        Err(error) => {
            let message = format!("{error:#}");
            record(&root, name, |fetch| fetch.error = Some(message))?;
            Err(error)
        }
    }
}

fn refresh_one(storage: &Storage, name: &str, cancel: &Cancel) -> Result<()> {
    if cloned(&path(storage, name)?) {
        bounded(FETCH_LIMIT, cancel, |cancel| fetch(storage, name, cancel))
    } else {
        clone(storage, name, None, cancel).map(|_| ())
    }
}

/// Listed names to refresh: all of them, or the listed spelling of `name`.
fn selected(storage: &Storage, name: Option<&str>) -> Result<Vec<String>> {
    let listed = storage.load_config()?.server.repositories;
    let Some(name) = name.map(str::trim) else {
        return Ok(listed);
    };
    validate(name)?;
    listed
        .into_iter()
        .find(|r| r.eq_ignore_ascii_case(name))
        .map(|r| vec![r])
        .with_context(|| {
            format!("{name} is not a server repository; add it with `difu repos add {name}`")
        })
}

/// Fetches one listed repository, or all of them, cloning any that are missing.
/// Each result is recorded for `list`.
pub fn refresh(storage: &Storage, name: Option<&str>) -> Result<()> {
    let mut failures = Vec::new();
    for name in selected(storage, name)? {
        if let Err(error) = refresh_one(storage, &name, &Cancel::default()) {
            failures.push(format!("{error:#}"));
        }
    }
    ensure!(failures.is_empty(), "{}", failures.join("; "));
    Ok(())
}

/// Validates a `refresh` target up front, for callers that refresh in the background.
pub fn check(storage: &Storage, name: Option<&str>) -> Result<()> {
    selected(storage, name).map(|_| ())
}

/// Refreshes every server repository now and then every `refresh_minutes`
/// until `stopped`. Failures are recorded per repository and logged; they never
/// stop the service.
pub fn start(storage: Storage, stopped: Arc<AtomicBool>) {
    let spawned = thread::Builder::new()
        .name("difu-repos".into())
        .spawn(move || {
            let mut next = Instant::now();
            while !stopped.load(Ordering::Relaxed) {
                if Instant::now() >= next {
                    let settings = storage.load_config().map(|c| c.server).unwrap_or_default();
                    if settings.enabled
                        && let Err(error) = refresh(&storage, None)
                    {
                        eprintln!("Server repositories: {error:#}");
                    }
                    next = Instant::now()
                        + Duration::from_secs(u64::from(settings.refresh_minutes()) * 60);
                }
                thread::sleep(Duration::from_millis(500));
            }
        });
    if let Err(error) = spawned {
        eprintln!("Cannot start the server repository refresh: {error}");
    }
}

pub fn command(storage: &Storage, command: ReposCommand) -> Result<()> {
    match command {
        ReposCommand::List => print(storage),
        ReposCommand::Add { name: Some(name) } => each(&[name], "added", |name, cancel| {
            install(storage, name, cancel)
        }),
        ReposCommand::Add { name: None } => {
            let picked = pick(storage)?;
            if picked.is_empty() {
                println!("Nothing added.");
                return Ok(());
            }
            each(&picked, "added", |name, cancel| {
                install(storage, name, cancel)
            })
        }
        ReposCommand::Remove { name } => {
            remove(storage, &name)?;
            let path = path(storage, name.trim())?;
            print!("Removed {} from the server repositories.", name.trim());
            if cloned(&path) {
                print!(" Its clone stays at {}.", path.display());
            }
            println!();
            Ok(())
        }
        ReposCommand::Refresh { name } => {
            let names = selected(storage, name.as_deref())?;
            ensure!(
                !names.is_empty(),
                "No server repositories yet; add one with `difu repos add`"
            );
            each(&names, "refreshed", |name, cancel| {
                print!("Fetching {name}... ");
                std::io::stdout().flush()?;
                let result = refresh_one(storage, name, cancel);
                println!("{}", if result.is_ok() { "done" } else { "failed" });
                result
            })
        }
    }
}

/// Runs `work` for each name. Ctrl-C stops the running clone or fetch through
/// difu, so Git is never left running orphaned, and skips the rest.
fn each(
    names: &[String],
    outcome: &str,
    mut work: impl FnMut(&str, &Cancel) -> Result<()>,
) -> Result<()> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let id = signal_hook::flag::register(signal_hook::consts::SIGINT, interrupted.clone())?;
    let cancel = Cancel::default();
    let finished = AtomicBool::new(false);
    let errors = thread::scope(|scope| {
        scope.spawn(|| {
            while !finished.load(Ordering::Relaxed) {
                if interrupted.load(Ordering::Relaxed) {
                    cancel.cancel();
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
        });
        let mut errors = Vec::new();
        for name in names {
            if cancel.cancelled() {
                break;
            }
            if let Err(error) = work(name, &cancel) {
                errors.push(error);
            }
        }
        finished.store(true, Ordering::Relaxed);
        errors
    });
    signal_hook::low_level::unregister(id);
    ensure!(!interrupted.load(Ordering::Relaxed), "Interrupted");
    match errors.as_slice() {
        [] => Ok(()),
        [error] if names.len() == 1 => Err(anyhow::anyhow!("{error:#}")),
        errors => {
            for error in errors {
                eprintln!("{error:#}");
            }
            anyhow::bail!(
                "{} of {} repositories could not be {outcome}",
                errors.len(),
                names.len()
            )
        }
    }
}

fn ago(milliseconds: i64) -> String {
    let seconds = milliseconds.max(0) / 1000;
    match seconds {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86400 => format!("{} h ago", seconds / 3600),
        _ => format!("{} d ago", seconds / 86400),
    }
}

fn print(storage: &Storage) -> Result<()> {
    let repositories = list(storage)?;
    if repositories.is_empty() {
        println!("No server repositories yet. Add one with `difu repos add [owner/name]`.");
        return Ok(());
    }
    let width = repositories.iter().map(|r| r.name.len()).max().unwrap_or(0);
    for repository in &repositories {
        let state = match (&repository.error, repository.cloned, repository.fetched_at) {
            (Some(error), _, _) => format!("error: {error}"),
            (None, false, _) => "not cloned yet".into(),
            (None, true, Some(at)) => format!("fetched {}", ago(now() - at)),
            (None, true, None) => "cloned".into(),
        };
        println!("{:<width$}  {state}", repository.name);
        if repository.cloned {
            println!("{:<width$}  {}", "", repository.path.display());
        }
    }
    let minutes = storage.load_config()?.server.refresh_minutes();
    println!("\nThe difu service fetches these every {minutes} minutes.");
    Ok(())
}

/// Git's progress on one terminal line.
fn progress_line(line: &str) {
    if std::io::stderr().is_terminal() {
        let line: String = line.chars().take(100).collect();
        eprint!("\r\x1b[2K  {line}");
    }
}

fn install(storage: &Storage, name: &str, cancel: &Cancel) -> Result<()> {
    let name = add(storage, name)?;
    let path = path(storage, &name)?;
    if cloned(&path) {
        println!("{name} is already cloned at {}", path.display());
        return Ok(());
    }
    println!("Cloning {name}...");
    let result = clone(storage, &name, Some(progress_line), cancel);
    if std::io::stderr().is_terminal() {
        eprint!("\r\x1b[2K");
    }
    println!("Cloned {name} into {}", result?.display());
    Ok(())
}

/// Numbers (`3`), ranges (`5-7`) or names, separated by commas or spaces.
fn selection(input: &str, choices: &[String]) -> Result<Vec<String>> {
    let mut picked = Vec::new();
    for token in input
        .split([',', ' ', '\t', '\n', '\r'])
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        if token.contains('/') {
            validate(token)?;
            picked.push(token.to_owned());
            continue;
        }
        let numbers = match token.split_once('-') {
            Some((first, last)) => first.parse::<usize>().ok().zip(last.parse::<usize>().ok()),
            None => token.parse::<usize>().ok().map(|n| (n, n)),
        };
        let (first, last) = numbers.with_context(|| format!("Not a number: `{token}`"))?;
        ensure!(first <= last, "Invalid range `{token}`");
        for number in first..=last {
            let choice = number
                .checked_sub(1)
                .and_then(|index| choices.get(index))
                .with_context(|| format!("No repository numbered {number}"))?;
            picked.push(choice.clone());
        }
    }
    let mut seen = BTreeSet::new();
    picked.retain(|name| seen.insert(name.to_lowercase()));
    Ok(picked)
}

/// A numbered list of the user's GitHub repositories that are not listed yet;
/// returns the chosen names.
fn pick(storage: &Storage) -> Result<Vec<String>> {
    println!("Loading your GitHub repositories...");
    let listed: BTreeSet<String> = storage
        .load_config()?
        .server
        .repositories
        .iter()
        .map(|r| r.to_lowercase())
        .collect();
    let choices: Vec<String> = crate::github::repositories(&Cancel::default())
        .context("Cannot list your GitHub repositories; is `gh` logged in?")?
        .into_iter()
        .filter(|r| !listed.contains(&r.to_lowercase()))
        .collect();
    if choices.is_empty() {
        println!("Every repository you can access on GitHub is already a server repository.");
        return Ok(Vec::new());
    }
    let digits = choices.len().to_string().len();
    for (index, name) in choices.iter().enumerate() {
        println!("{:>digits$}. {name}", index + 1);
    }
    print!("\nRepositories to add (numbers separated by commas, e.g. 1,3,5-7): ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().lock().read_line(&mut input)?;
    selection(&input, &choices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn git(root: &Path, args: &[&str]) -> Result<String> {
        process::checked(
            Command::new("git")
                .arg("-C")
                .arg(root)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(args),
            &Cancel::default(),
        )
        .map(|output| output.trim().to_owned())
    }

    fn fixture() -> Result<(tempfile::TempDir, Storage)> {
        let dir = tempfile::tempdir()?;
        fs::create_dir_all(dir.path().join("config"))?;
        let storage = Storage {
            config: dir.path().join("config").join("config.json"),
            cache: dir.path().join("cache"),
        };
        Ok((dir, storage))
    }

    fn text(path: &Path) -> Result<&str> {
        path.to_str().context("Fixture paths are UTF-8")
    }

    /// A bare "origin" with one commit on main, and a base clone of it listed
    /// as `owner/repo`. Returns the work tree that pushes to origin.
    fn listed_clone(dir: &Path, storage: &Storage) -> Result<(PathBuf, PathBuf)> {
        let origin = dir.join("origin.git");
        let work = dir.join("work");
        fs::create_dir_all(&work)?;
        git(dir, &["init", "--bare", text(&origin)?])?;
        git(&work, &["init"])?;
        git(&work, &["commit", "--allow-empty", "-m", "first"])?;
        git(&work, &["remote", "add", "origin", text(&origin)?])?;
        git(&work, &["push", "origin", "main"])?;
        add(storage, "owner/repo")?;
        let clone = path(storage, "owner/repo")?;
        fs::create_dir_all(clone.parent().context("Missing owner directory")?)?;
        git(dir, &["clone", text(&origin)?, text(&clone)?])?;
        Ok((work, clone))
    }

    #[test]
    fn names_must_be_github_owner_and_repository() -> Result<()> {
        for name in [
            "opencx-labs/difu",
            "a/b",
            "owner_1/.github",
            "Owner/repo.name_x-y",
        ] {
            assert_eq!(Some(validate(name)?), name.split_once('/'));
        }
        let long_owner = format!("{}/repo", "a".repeat(40));
        let long_repo = format!("owner/{}", "r".repeat(101));
        for name in [
            "",
            "difu",
            "owner/",
            "/repo",
            "owner/repo/extra",
            "owner/..",
            "owner/.",
            "../repo",
            "-owner/repo",
            ".owner/repo",
            "owner/-repo",
            "own er/repo",
            "owner/re$po",
            "owner/repo\n",
            long_owner.as_str(),
            long_repo.as_str(),
        ] {
            assert!(validate(name).is_err(), "{name:?} should be invalid");
        }
        Ok(())
    }

    #[test]
    fn add_and_remove_persist_listed_names_case_insensitively() -> Result<()> {
        let (_dir, storage) = fixture()?;
        assert_eq!(add(&storage, " Owner/Repo ")?, "Owner/Repo");
        assert_eq!(add(&storage, "owner/repo")?, "Owner/Repo");
        assert_eq!(add(&storage, "other/thing")?, "other/thing");
        assert!(add(&storage, "not a name").is_err());
        assert_eq!(
            storage.load_config()?.server.repositories,
            ["Owner/Repo", "other/thing"]
        );
        let listed = list(&storage)?;
        let first = listed.first().context("Missing first repository")?;
        assert_eq!(first.name, "Owner/Repo");
        assert!(first.path.ends_with("agents/repos/Owner/Repo"));
        assert!(!first.cloned && first.fetched_at.is_none() && first.error.is_none());
        record(&root(&storage)?, "Owner/Repo", |fetch| {
            fetch.fetched_at = Some(1)
        })?;
        remove(&storage, "OWNER/repo")?;
        assert!(statuses(&root(&storage)?).is_empty());
        assert!(remove(&storage, "owner/repo").is_err());
        assert_eq!(storage.load_config()?.server.repositories, ["other/thing"]);
        assert!(check(&storage, Some("owner/repo")).is_err());
        assert!(check(&storage, Some("Other/Thing")).is_ok());
        Ok(())
    }

    #[test]
    fn status_json_round_trips_with_client_field_names() -> Result<()> {
        let (_dir, storage) = fixture()?;
        add(&storage, "owner/repo")?;
        let root = root(&storage)?;
        record(&root, "owner/repo", |fetch| {
            fetch.fetched_at = Some(42);
            fetch.error = Some("offline".into());
        })?;
        record(&root, "owner/repo", |fetch| fetch.error = None)?;
        let saved: Value = serde_json::from_slice(&fs::read(root.join(STATUS))?)?;
        assert_eq!(
            saved,
            json!({"owner/repo": {"fetched_at": 42, "error": null}})
        );
        let value = serde_json::to_value(list(&storage)?)?;
        assert_eq!(
            value,
            json!([{
                "name": "owner/repo",
                "path": root.join("owner").join("repo"),
                "cloned": false,
                "fetchedAt": 42,
                "error": null,
            }])
        );
        Ok(())
    }

    #[test]
    fn refresh_fetches_and_prunes_listed_clones_and_records_failures() -> Result<()> {
        let (dir, storage) = fixture()?;
        let (work, clone) = listed_clone(dir.path(), &storage)?;
        git(&work, &["commit", "--allow-empty", "-m", "second"])?;
        git(&work, &["push", "origin", "main", "main:gone"])?;
        refresh(&storage, Some("OWNER/REPO"))?;
        let head = git(&work, &["rev-parse", "HEAD"])?;
        assert_eq!(git(&clone, &["rev-parse", "origin/main"])?, head);
        assert!(git(&clone, &["rev-parse", "--verify", "origin/gone"]).is_ok());
        let status = list(&storage)?;
        let fetched = status.first().context("Missing status")?;
        assert!(fetched.cloned && fetched.error.is_none());
        let first = fetched.fetched_at.context("Missing fetch time")?;
        git(&work, &["push", "origin", ":gone"])?;
        refresh(&storage, None)?;
        assert!(git(&clone, &["rev-parse", "--verify", "origin/gone"]).is_err());
        git(
            &clone,
            &["remote", "set-url", "origin", "/nonexistent/origin.git"],
        )?;
        let error = refresh(&storage, None)
            .err()
            .context("Expected a failure")?;
        assert!(error.to_string().contains("Cannot fetch owner/repo"));
        let status = list(&storage)?;
        let failed = status.first().context("Missing status")?;
        assert!(
            failed
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Cannot fetch owner/repo"))
        );
        assert!(failed.fetched_at >= Some(first));
        assert!(refresh(&storage, Some("owner/unlisted")).is_err());
        Ok(())
    }

    #[test]
    fn refresh_waits_for_a_session_fetch_and_pin() -> Result<()> {
        let (dir, storage) = fixture()?;
        let (_work, clone) = listed_clone(dir.path(), &storage)?;
        let held = lock(
            &clone.join(".git").join("difu-main-fetch.lock"),
            &Cancel::default(),
        )?;
        let worker = {
            let storage = storage.clone();
            thread::spawn(move || refresh(&storage, None))
        };
        thread::sleep(Duration::from_millis(300));
        assert!(!worker.is_finished());
        drop(held);
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("Refresh thread failed"))??;
        Ok(())
    }

    #[test]
    fn resolve_keeps_paths_and_maps_names_to_base_clones() -> Result<()> {
        let (_dir, storage) = fixture()?;
        assert_eq!(
            resolve(&storage, "/some/checkout")?,
            PathBuf::from("/some/checkout")
        );
        assert_eq!(resolve(&storage, "~/code/x")?, PathBuf::from("~/code/x"));
        let clone = path(&storage, "owner/repo")?;
        fs::create_dir_all(clone.join(".git"))?;
        assert_eq!(resolve(&storage, " owner/repo ")?, clone);
        assert_eq!(resolve(&storage, text(&clone)?)?, clone);
        assert_eq!(storage.load_config()?.server.repositories, ["owner/repo"]);
        Ok(())
    }

    #[test]
    fn bounded_work_times_out_and_follows_its_caller() -> Result<()> {
        let started = Instant::now();
        let error = bounded(Duration::from_millis(100), &Cancel::default(), |cancel| {
            process::run(Command::new("sleep").arg("5"), None, cancel)
        })
        .err()
        .context("Expected a timeout")?;
        assert!(format!("{error:#}").contains("Timed out"));
        assert!(started.elapsed() < Duration::from_secs(3));
        let outer = Cancel::default();
        outer.cancel();
        let error = bounded(Duration::from_secs(60), &outer, |cancel| {
            process::run(Command::new("sleep").arg("5"), None, cancel)
        })
        .err()
        .context("Expected a cancellation")?;
        assert!(!format!("{error:#}").contains("Timed out"));
        Ok(())
    }

    #[test]
    fn picker_selection_accepts_numbers_ranges_and_names() -> Result<()> {
        let choices: Vec<String> = ["a/one", "a/two", "b/three", "b/four"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(
            selection("1, 3-4,2 c/five 1\n", &choices)?,
            ["a/one", "b/three", "b/four", "a/two", "c/five"]
        );
        assert!(selection(" \n", &choices)?.is_empty());
        for bad in ["0", "5", "x", "3-1", "1-", "bad/../name"] {
            assert!(selection(bad, &choices).is_err(), "{bad}");
        }
        Ok(())
    }

    #[test]
    fn failures_keep_the_last_meaningful_lines() {
        let output = process::Output {
            stdout: Vec::new(),
            stderr: b"Receiving objects: 50%\rReceiving objects: 100%\nfatal: one\nfatal: two\n"
                .to_vec(),
            code: 128,
        };
        assert_eq!(failure(&output), "fatal: one fatal: two");
        let empty = process::Output {
            stdout: Vec::new(),
            stderr: Vec::new(),
            code: 1,
        };
        assert_eq!(failure(&empty), "exit status 1");
    }
}
