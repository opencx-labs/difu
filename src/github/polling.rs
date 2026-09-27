//! One request gate and short-lived read cache shared by every difu process.
use crate::{
    process::{self, Cancel},
    storage,
};
use anyhow::{Result, bail};
use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn lock(path: &Path, cancel: &Cancel) -> Result<Flock<File>> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    loop {
        cancel.check()?;
        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(lock) => return Ok(lock),
            Err((returned, nix::errno::Errno::EWOULDBLOCK)) => file = returned,
            Err((_, error)) => return Err(error.into()),
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Gate {
    until: u64,
    failures: u32,
    generation: u64,
}
#[derive(Serialize, Deserialize)]
struct Cached {
    at: u64,
    generation: u64,
    value: serde_json::Value,
}
#[derive(Default, Serialize, Deserialize)]
struct Failure {
    until: u64,
    count: u32,
    message: String,
}
fn load<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn paused() -> bool {
    storage::Storage::discover().ok().is_some_and(|storage| {
        let gate: Gate = load(&storage.cache.join("github-requests/cooldown.json"));
        gate.until > now()
    })
}

fn limited(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("rate limit")
        || text.contains("rate_limit")
        || text.contains("ratelimited")
        || text.contains("http 429")
        || text.contains("abuse detection")
}
fn backoff(count: u32, initial: u64, maximum: u64) -> u64 {
    initial
        .saturating_mul(1u64 << count.saturating_sub(1).min(10))
        .min(maximum)
}

/// gh api --include emits response headers on stdout. Leave the JSON body intact.
fn split_headers(bytes: &[u8]) -> (String, Vec<u8>) {
    if !bytes.starts_with(b"HTTP/") {
        return (String::new(), bytes.to_vec());
    }
    let boundary = bytes
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .map(|at| (at, 4))
        .or_else(|| {
            bytes
                .windows(2)
                .position(|part| part == b"\n\n")
                .map(|at| (at, 2))
        });
    if let Some((at, size)) = boundary {
        (
            String::from_utf8_lossy(bytes.get(..at).unwrap_or_default()).into_owned(),
            bytes.get(at + size..).unwrap_or_default().to_vec(),
        )
    } else {
        (String::new(), bytes.to_vec())
    }
}
fn deadline(headers: &str, at: u64) -> Option<u64> {
    let mut retry = None;
    let mut reset = None;
    let mut exhausted = false;
    for line in headers.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "retry-after" => {
                retry = value
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .map(|seconds| at.saturating_add(seconds))
                    .or_else(|| {
                        chrono::DateTime::parse_from_rfc2822(value.trim())
                            .ok()
                            .and_then(|date| u64::try_from(date.timestamp()).ok())
                    });
            }
            "x-ratelimit-remaining" => exhausted = value.trim() == "0",
            "x-ratelimit-reset" => reset = value.trim().parse::<u64>().ok(),
            _ => {}
        }
    }
    retry.into_iter().chain(reset.filter(|_| exhausted)).max()
}

/// All reads share the gate; only explicitly cacheable reads reuse responses.
/// Mutations are never retried or cached and invalidate cached reads on success.
pub(crate) fn run(
    command: &mut Command,
    input: Option<Vec<u8>>,
    cancel: &Cancel,
    ttl: u64,
    mutation: bool,
    limit: Option<usize>,
) -> Result<process::Output> {
    let root = storage::Storage::discover()?.cache.join("github-requests");
    fs::create_dir_all(&root)?;
    run_in(
        &root,
        command,
        input,
        cancel,
        Policy {
            ttl,
            mutation,
            limit,
        },
    )
}
struct Policy {
    ttl: u64,
    mutation: bool,
    limit: Option<usize>,
}
fn run_in(
    root: &Path,
    command: &mut Command,
    input: Option<Vec<u8>>,
    cancel: &Cancel,
    policy: Policy,
) -> Result<process::Output> {
    let Policy {
        ttl,
        mutation,
        limit,
    } = policy;
    let _lock = lock(&root.join("requests.lock"), cancel)?;
    let gate_path = root.join("cooldown.json");
    let mut gate: Gate = load(&gate_path);
    let at = now();
    // Do not inspect credentials to identify the cache. gh owns authentication.
    let independent = command
        .get_args()
        .next()
        .is_some_and(|arg| arg == "api" || arg == "search")
        || command
            .get_args()
            .any(|arg| arg.to_string_lossy().starts_with("https://github.com/"));
    let cwd = if independent {
        None
    } else {
        command
            .get_current_dir()
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
    };
    let key = storage::hash(format!(
        "{:?}:{:?}:{:?}:{:?}:{:?}",
        command.get_program(),
        command.get_args().collect::<Vec<_>>(),
        cwd,
        std::env::var_os("GH_HOST"),
        std::env::var_os("GH_CONFIG_DIR")
    ));
    let cache_path = root.join(format!("{key}.json"));
    let failure_path = root.join(format!("{key}.failure.json"));
    if gate.until > at {
        bail!(
            "GitHub refresh paused until {} (rate limit); cached status may be stale",
            chrono::DateTime::from_timestamp(gate.until as i64, 0)
                .map(|date| date.to_rfc3339())
                .unwrap_or_default()
        );
    }
    if ttl > 0
        && !mutation
        && let Ok(bytes) = fs::read(&cache_path)
        && let Ok(cached) = serde_json::from_slice::<Cached>(&bytes)
        && cached.generation == gate.generation
        && at.saturating_sub(cached.at) < ttl
    {
        return Ok(process::Output {
            stdout: serde_json::to_vec(&cached.value)?,
            stderr: Vec::new(),
            code: 0,
        });
    }
    let mut failure: Failure = load(&failure_path);
    if !mutation && failure.until > at {
        bail!("GitHub refresh backing off: {}", failure.message);
    }
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    // gh's paginated/slurped output interleaves headers and JSON; keep that format
    // untouched and use the single rate-limit probe on an actual limit error.
    let headers = args.first().is_some_and(|arg| arg == "api")
        && !args
            .iter()
            .any(|arg| matches!(arg.as_str(), "--paginate" | "--slurp" | "--include"));
    if headers {
        command.arg("--include");
    }
    let output = if let Some(limit) = limit {
        process::run_limited(command, cancel, limit)
    } else {
        process::run(command, input, cancel)
    };
    cancel.check()?;
    let mut output = match output {
        Ok(output) => output,
        Err(error) => {
            if !mutation {
                failure.count = failure.count.saturating_add(1);
                failure.until = now() + backoff(failure.count, 30, 300);
                failure.message = format!("{error:#}");
                storage::atomic_json(&failure_path, &failure)?;
            }
            return Err(error);
        }
    };
    let (response_headers, body) = if headers {
        split_headers(&output.stdout)
    } else {
        (String::new(), output.stdout.clone())
    };
    output.stdout = body;
    let value = serde_json::from_slice::<serde_json::Value>(&output.stdout).ok();
    let graphql_errors = value
        .as_ref()
        .and_then(|value| value.get("errors"))
        .filter(|errors| errors.as_array().is_some_and(|errors| !errors.is_empty()));
    let message = format!(
        "{} {} {}",
        String::from_utf8_lossy(&output.stderr),
        graphql_errors.map(ToString::to_string).unwrap_or_default(),
        if output.code != 0 {
            value
                .as_ref()
                .and_then(|v| v.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
        } else {
            ""
        }
    );
    let rate_limited = limited(&message)
        || response_headers
            .lines()
            .next()
            .is_some_and(|line| line.contains(" 429"))
        || (output.code != 0 && deadline(&response_headers, now()).is_some());
    let mut until = deadline(&response_headers, now());
    if rate_limited {
        gate.failures = gate.failures.saturating_add(1);
        // High-level gh commands hide headers. One probe obtains primary reset
        // times; a failed probe is not retried, and secondary limits back off.
        if until.is_none()
            && !headers
            && let Ok(probe) = process::run(
                super::command().args(["api", "rate_limit", "--include"]),
                None,
                cancel,
            )
        {
            let (headers, body) = split_headers(&probe.stdout);
            until = deadline(&headers, now());
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body)
                && let Some(resources) = value.get("resources").and_then(|v| v.as_object())
            {
                until = until
                    .into_iter()
                    .chain(
                        resources
                            .values()
                            .filter(|resource| {
                                resource.get("remaining").and_then(|v| v.as_u64()) == Some(0)
                            })
                            .filter_map(|resource| resource.get("reset").and_then(|v| v.as_u64())),
                    )
                    .max();
            }
        }
        gate.until = until
            .unwrap_or_else(|| now() + backoff(gate.failures, 60, 3600))
            .max(now() + 1);
        storage::atomic_json(&gate_path, &gate)?;
        bail!(
            "GitHub rate limit reached; refresh paused for {} seconds. Cached status may be stale.",
            gate.until.saturating_sub(now())
        );
    }
    if let Some(until) = until.filter(|until| *until > now()) {
        gate.until = until;
        storage::atomic_json(&gate_path, &gate)?;
    }
    if output.code != 0 || graphql_errors.is_some() {
        if !mutation {
            failure.count = failure.count.saturating_add(1);
            failure.until = now() + backoff(failure.count, 30, 300);
            failure.message = message.trim().to_owned();
            storage::atomic_json(&failure_path, &failure)?;
        }
        bail!("GitHub: {}", message.trim());
    }
    let _ = fs::remove_file(&failure_path);
    gate.failures = 0;
    if mutation {
        gate.generation = gate.generation.saturating_add(1);
    }
    storage::atomic_json(&gate_path, &gate)?;
    if ttl > 0
        && !mutation
        && let Some(value) = value
    {
        storage::atomic_json(
            &cache_path,
            &Cached {
                at: now(),
                generation: gate.generation,
                value,
            },
        )?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use std::os::unix::fs::PermissionsExt;

    fn fake(root: &Path, name: &str, response: &str, code: i32) -> Result<std::path::PathBuf> {
        let path = root.join(name);
        // Every fixture is a local executable; no test invokes gh or the network.
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf x >> \"$0.calls\"\nprintf '%s' '{}'\nexit {code}\n",
                response.replace('\'', "'\\''")
            ),
        )?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(path)
    }
    fn calls(path: &Path) -> usize {
        fs::read(path.with_file_name(format!(
            "{}.calls",
            path.file_name().unwrap_or_default().to_string_lossy()
        )))
        .unwrap_or_default()
        .len()
    }
    fn read(root: &Path, executable: &Path, mutation: bool) -> Result<process::Output> {
        run_in(
            root,
            Command::new(executable).arg("api"),
            None,
            &Cancel::default(),
            Policy {
                ttl: 30,
                mutation,
                limit: None,
            },
        )
    }

    #[test]
    fn simultaneous_readers_share_one_result_and_writes_invalidate_it() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let executable = fake(
            dir.path(),
            "read",
            "HTTP/2.0 200 OK\nX-Ratelimit-Remaining: 100\n\n{\"state\":\"OPEN\"}",
            0,
        )?;
        let mut readers = Vec::new();
        for _ in 0..4 {
            let root = dir.path().to_path_buf();
            let executable = executable.clone();
            readers.push(std::thread::spawn(move || read(&root, &executable, false)));
        }
        for reader in readers {
            let output = reader
                .join()
                .map_err(|_| anyhow::anyhow!("reader failed"))??;
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&output.stdout)?
                    .get("state")
                    .and_then(|value| value.as_str()),
                Some("OPEN")
            );
        }
        assert_eq!(calls(&executable), 1);
        read(dir.path(), &executable, true)?;
        read(dir.path(), &executable, true)?;
        assert_eq!(calls(&executable), 3, "writes are never cached");
        read(dir.path(), &executable, false)?;
        assert_eq!(calls(&executable), 4, "a write invalidates previous reads");
        Ok(())
    }

    #[test]
    fn rate_limit_blocks_other_reads_and_writes_and_keeps_cached_results() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let healthy = fake(dir.path(), "healthy", "{\"state\":\"OPEN\"}", 0)?;
        read(dir.path(), &healthy, false)?;
        let exhausted = fake(
            dir.path(),
            "limited",
            "HTTP/2.0 429 Too Many Requests\nRetry-After: 120\n\n{\"message\":\"secondary rate limit\"}",
            1,
        )?;
        assert!(read(dir.path(), &exhausted, false).is_err());
        assert!(read(dir.path(), &healthy, false).is_err());
        assert!(read(dir.path(), &healthy, true).is_err());
        assert_eq!(calls(&healthy), 1);
        assert_eq!(calls(&exhausted), 1);
        let mut gate: Gate = load(&dir.path().join("cooldown.json"));
        assert!(gate.until >= now() + 119);
        gate.until = 0;
        storage::atomic_json(&dir.path().join("cooldown.json"), &gate)?;
        read(dir.path(), &healthy, false)?;
        assert_eq!(
            calls(&healthy),
            1,
            "cooldown preserves the last successful cache"
        );
        Ok(())
    }

    #[test]
    fn primary_reset_graphql_limits_and_retry_dates_are_respected() -> Result<()> {
        assert_eq!(
            deadline("x-ratelimit-remaining: 0\nx-ratelimit-reset: 500", 100),
            Some(500)
        );
        assert_eq!(
            deadline("x-ratelimit-remaining: 1\nx-ratelimit-reset: 500", 100),
            None
        );
        assert_eq!(
            deadline(
                "retry-after: 60\nx-ratelimit-remaining: 0\nx-ratelimit-reset: 500",
                100
            ),
            Some(500)
        );
        let date = chrono::DateTime::from_timestamp(500, 0)
            .context("timestamp")?
            .to_rfc2822();
        assert_eq!(deadline(&format!("Retry-After: {date}"), 100), Some(500));
        let dir = tempfile::tempdir()?;
        let executable = fake(
            dir.path(),
            "graphql",
            "HTTP/2.0 200 OK\n\n{\"errors\":[{\"type\":\"RATE_LIMITED\",\"message\":\"API rate limit exceeded\"}]}",
            0,
        )?;
        assert!(read(dir.path(), &executable, false).is_err());
        let mut gate: Gate = load(&dir.path().join("cooldown.json"));
        assert_eq!(gate.failures, 1);
        assert!(gate.until >= now() + 59);
        gate.until = 0;
        storage::atomic_json(&dir.path().join("cooldown.json"), &gate)?;
        assert!(read(dir.path(), &executable, false).is_err());
        let gate: Gate = load(&dir.path().join("cooldown.json"));
        assert_eq!(gate.failures, 2);
        assert!(gate.until >= now() + 119);
        Ok(())
    }

    #[test]
    fn failed_reads_back_off_and_lock_waits_can_be_cancelled() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let executable = fake(
            dir.path(),
            "offline",
            "HTTP/2.0 503 Unavailable\n\n{\"message\":\"offline\"}",
            1,
        )?;
        assert!(read(dir.path(), &executable, false).is_err());
        assert!(read(dir.path(), &executable, false).is_err());
        assert_eq!(calls(&executable), 1);
        let path = dir.path().join("requests.lock");
        let _lease = lock(&path, &Cancel::default())?;
        let cancel = Cancel::default();
        cancel.cancel();
        assert!(lock(&path, &cancel).is_err());
        Ok(())
    }
}
