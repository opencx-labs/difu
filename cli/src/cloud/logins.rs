//! Codex, Claude Code and GitHub logins on a cloud server: status, device-code
//! logins started from any client, and the Claude token kept in `server.env`.
use super::repos::{bounded, failure};
use crate::{
    process::{self, Cancel, ChildGroup},
    storage::Storage,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const CLAUDE_TOKEN: &str = "CLAUDE_CODE_OAUTH_TOKEN";
const STATUS_LIMIT: Duration = Duration::from_secs(20);
const CODE_LIMIT: Duration = Duration::from_secs(30);
/// Device codes expire after 15 minutes; give up on the login shortly after.
const LOGIN_LIMIT: Duration = Duration::from_secs(16 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Tool {
    Github,
    Codex,
    Claude,
}

impl Tool {
    fn parse(name: &str) -> Result<Self> {
        match name {
            "github" => Ok(Self::Github),
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            _ => anyhow::bail!("Unknown login `{name}`; expected github, codex or claude"),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

/// One tool's login on this server (`server.logins`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Login {
    pub tool: &'static str,
    pub logged_in: bool,
    pub account: Option<String>,
    pub detail: String,
}

/// A device-code login waiting for the user (`server.login.start`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeviceCode {
    pub url: String,
    pub code: String,
    pub message: String,
}

/// `<config dir>/server.env`, next to `config.json`.
pub fn env_path(storage: &Storage) -> Result<PathBuf> {
    Ok(storage
        .config
        .parent()
        .context("Missing config directory")?
        .join("server.env"))
}

/// `KEY=value` lines; blank lines and `#` comments are skipped and one pair of
/// surrounding quotes is removed.
fn parse_env(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| {
            let value = value.trim();
            let unquoted = ['"', '\'']
                .into_iter()
                .find_map(|q| value.strip_prefix(q).and_then(|v| v.strip_suffix(q)));
            (key.trim().to_owned(), unquoted.unwrap_or(value).to_owned())
        })
        .filter(|(key, _)| valid_key(key))
        .collect()
}

fn valid_key(key: &str) -> bool {
    key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn read_env(path: &Path) -> Result<Vec<(String, String)>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(parse_env(&text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("Cannot read {}", path.display())),
    }
}

/// Sets (`Some`) or removes (`None`) one variable, keeping the file private.
fn set_env(path: &Path, key: &str, value: Option<&str>) -> Result<()> {
    ensure!(valid_key(key), "Invalid environment variable name");
    let mut pairs = read_env(path)?;
    pairs.retain(|(existing, _)| existing != key);
    if let Some(value) = value {
        ensure!(
            !value.contains(['\n', '\r', '\0']),
            "Environment values must be one line"
        );
        pairs.push((key.to_owned(), value.to_owned()));
    }
    let directory = path.parent().context("Missing config directory")?;
    fs::create_dir_all(directory)?;
    let mut temp = tempfile::NamedTempFile::new_in(directory)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    for (key, value) in &pairs {
        writeln!(temp, "{key}={value}")?;
    }
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Extra environment for agent processes (e.g. `CLAUDE_CODE_OAUTH_TOKEN` from
/// the server's 0600 `server.env`). Empty on ordinary installations.
pub fn agent_env() -> Vec<(String, String)> {
    let Ok(storage) = Storage::discover() else {
        return Vec::new();
    };
    let pairs = env_path(&storage)
        .and_then(|path| read_env(&path))
        .unwrap_or_default();
    let server = storage
        .load_config()
        .is_ok_and(|config| config.server.enabled);
    server_env(pairs, server, dirs::home_dir().as_deref())
}

/// On a server, Claude Code always shares `~/.claude/.claude.json` (see
/// `claude_config`), whatever `server.env` says.
fn server_env(
    mut pairs: Vec<(String, String)>,
    server: bool,
    home: Option<&Path>,
) -> Vec<(String, String)> {
    if let (true, Some(home)) = (server, home) {
        pairs.retain(|(key, _)| key != super::claude_config::VARIABLE);
        pairs.push((
            super::claude_config::VARIABLE.to_owned(),
            super::claude_config::dir(home).display().to_string(),
        ));
    }
    pairs
}

/// Removes terminal colour and cursor sequences.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
        } else if chars.next_if_eq(&'[').is_some() {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

fn output_text(output: &process::Output) -> String {
    plain(&format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(|line| line.trim().trim_start_matches(['✓', 'X', '!', '-']).trim())
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

fn logged_out(tool: Tool, detail: String) -> Login {
    Login {
        tool: tool.name(),
        logged_in: false,
        account: None,
        detail,
    }
}

/// `gh auth status --hostname github.com`, current ("account NAME") and older
/// ("as NAME") formats, preferring the active account.
fn github_status(result: Result<process::Output>) -> Login {
    let output = match result {
        Ok(output) => output,
        Err(error) => return logged_out(Tool::Github, format!("{error:#}")),
    };
    let text = output_text(&output);
    // (account, logged in, active, detail)
    let mut accounts: Vec<(String, bool, bool, String)> = Vec::new();
    for line in text.lines().map(str::trim) {
        let detail = line.trim_start_matches(['✓', 'X', '!']).trim();
        let account = |rest: &str| {
            let rest = rest.trim_start();
            let rest = rest
                .strip_prefix("account ")
                .or_else(|| rest.strip_prefix("as "))
                .unwrap_or(rest);
            rest.split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned()
        };
        if let Some((_, rest)) = line.split_once("Logged in to github.com") {
            accounts.push((account(rest), true, false, detail.to_owned()));
        } else if let Some((_, rest)) = line.split_once("Failed to log in to github.com") {
            accounts.push((account(rest), false, false, detail.to_owned()));
        } else if line.contains("Active account: true")
            && let Some(last) = accounts.last_mut()
        {
            last.2 = true;
        }
    }
    let chosen = accounts
        .iter()
        .find(|a| a.2)
        .or_else(|| accounts.iter().find(|a| a.1))
        .or_else(|| accounts.first());
    match chosen {
        Some((account, logged_in, _, detail)) => Login {
            tool: Tool::Github.name(),
            logged_in: *logged_in,
            account: (!account.is_empty()).then(|| account.clone()),
            detail: detail.clone(),
        },
        None => logged_out(
            Tool::Github,
            first_line(&text).unwrap_or_else(|| "Not logged in to GitHub".into()),
        ),
    }
}

/// `codex login status`: "Logged in using ChatGPT", "Not logged in", ...
fn codex_status(result: Result<process::Output>) -> Login {
    let output = match result {
        Ok(output) => output,
        Err(error) => return logged_out(Tool::Codex, format!("{error:#}")),
    };
    let detail = first_line(&output_text(&output)).unwrap_or_else(|| "Not logged in".into());
    Login {
        tool: Tool::Codex.name(),
        logged_in: output.code == 0 && detail.starts_with("Logged in"),
        account: None,
        detail,
    }
}

fn claude_status(saved: bool, service: bool, credentials: bool) -> Login {
    let detail = if saved {
        "Using the Claude Code token saved on this server"
    } else if service {
        "Using CLAUDE_CODE_OAUTH_TOKEN from the service environment"
    } else if credentials {
        "Using ~/.claude/.credentials.json"
    } else {
        "Not logged in; paste a token from `claude setup-token`"
    };
    Login {
        tool: Tool::Claude.name(),
        logged_in: saved || service || credentials,
        account: None,
        detail: detail.into(),
    }
}

fn run(command: &mut Command) -> Result<process::Output> {
    bounded(STATUS_LIMIT, &Cancel::default(), |cancel| {
        process::run(command, None, cancel)
    })
}

pub fn status(storage: &Storage) -> Vec<Login> {
    let saved = env_path(storage)
        .and_then(|path| read_env(&path))
        .unwrap_or_default()
        .iter()
        .any(|(key, value)| key == CLAUDE_TOKEN && !value.is_empty());
    let service = std::env::var_os(CLAUDE_TOKEN).is_some_and(|value| !value.is_empty());
    let credentials = dirs::home_dir()
        .is_some_and(|home| home.join(".claude").join(".credentials.json").is_file());
    thread::scope(|scope| {
        let github = scope.spawn(|| {
            github_status(run(Command::new("gh").args([
                "auth",
                "status",
                "--hostname",
                "github.com",
            ])))
        });
        let codex = codex_status(run(Command::new("codex").args(["login", "status"])));
        vec![
            github
                .join()
                .unwrap_or_else(|_| logged_out(Tool::Github, "GitHub status check stopped".into())),
            codex,
            claude_status(saved, service, credentials),
        ]
    })
}

fn is_code(text: &str) -> bool {
    (6..=24).contains(&text.len())
        && text.contains('-')
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The verification URL and one-time code from `gh auth login --web` or
/// `codex login --device-auth` output, once both have been printed.
fn parse_device(text: &str) -> Option<(String, String)> {
    let text = plain(text);
    let url = text
        .split_whitespace()
        .find(|word| word.starts_with("https://"))?
        .trim_end_matches(['.', ',', ')']);
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let index = lines
        .iter()
        .position(|line| line.to_lowercase().contains("one-time code"))?;
    let inline = lines
        .get(index)
        .and_then(|line| line.rsplit_once(':'))
        .map(|(_, code)| code.trim())
        .filter(|code| is_code(code));
    let code = match inline {
        Some(code) => code,
        None => lines
            .get(index + 1..)?
            .iter()
            .find(|line| !line.is_empty())
            .copied()
            .filter(|code| is_code(code))?,
    };
    Some((url.to_owned(), code.to_owned()))
}

/// Device logins still waiting for the user, so a new start replaces the old one.
static PENDING: Mutex<BTreeMap<Tool, (u64, Cancel)>> = Mutex::new(BTreeMap::new());
static SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn cancel_pending(tool: Tool) {
    if let Ok(mut pending) = PENDING.lock()
        && let Some((_, cancel)) = pending.remove(&tool)
    {
        cancel.cancel();
    }
}

/// Reaps an exited login first: signalling a group whose leader is a zombie
/// fails on macOS.
fn end(group: &mut ChildGroup) {
    let _ = group.child.try_wait();
    let _ = group.stop();
}

/// Starts a device-code login and returns its code as soon as the tool prints
/// it. The process keeps running in the background until the user finishes,
/// it fails, or the code expires; `finished` runs after a successful login.
fn device_login(
    tool: Tool,
    mut command: Command,
    wait: Duration,
    finished: fn(),
) -> Result<(String, String)> {
    cancel_pending(tool);
    let name = command.get_program().to_string_lossy().into_owned();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut group = ChildGroup::spawn(&mut command).with_context(|| {
        format!("Could not start {name}. Check that it is installed and on PATH.")
    })?;
    let (sender, lines) = mpsc::channel::<String>();
    let pipes: [Box<dyn Read + Send>; 2] = [
        Box::new(group.child.stdout.take().context("Missing login output")?),
        Box::new(group.child.stderr.take().context("Missing login errors")?),
    ];
    for pipe in pipes {
        let sender = sender.clone();
        thread::Builder::new()
            .name("difu-login-output".into())
            .spawn(move || {
                // Keep draining after the code is found so the tool never blocks.
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    let _ = sender.send(line);
                }
            })?;
    }
    drop(sender);
    let started = Instant::now();
    let mut text = String::new();
    let parsed = loop {
        match lines.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                text.push_str(&line);
                text.push('\n');
                if let Some(parsed) = parse_device(&text) {
                    break parsed;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                end(&mut group);
                let detail = first_line(&plain(&text)).unwrap_or_else(|| "no output".into());
                anyhow::bail!("{name} exited before showing a login code: {detail}");
            }
        }
        if started.elapsed() >= wait {
            end(&mut group);
            anyhow::bail!(
                "{name} did not show a login code within {} seconds",
                wait.as_secs()
            );
        }
    };
    let id = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let cancel = Cancel::default();
    if let Some((_, older)) = PENDING
        .lock()
        .map_err(|_| anyhow::anyhow!("Login lock failed"))?
        .insert(tool, (id, cancel.clone()))
    {
        older.cancel();
    }
    thread::Builder::new()
        .name("difu-login".into())
        .spawn(move || {
            let started = Instant::now();
            let succeeded = loop {
                if cancel.cancelled() || started.elapsed() >= LOGIN_LIMIT {
                    break false;
                }
                match group.child.try_wait() {
                    Ok(Some(status)) => break status.success(),
                    Ok(None) => thread::sleep(Duration::from_millis(200)),
                    Err(_) => break false,
                }
            };
            end(&mut group);
            drop(lines);
            if succeeded {
                finished();
            }
            if let Ok(mut pending) = PENDING.lock()
                && pending
                    .get(&tool)
                    .is_some_and(|(current, _)| *current == id)
            {
                pending.remove(&tool);
            }
        })?;
    Ok(parsed)
}

/// Lets Git (and so agents' pushes and session fetches) use gh's login.
fn setup_git() {
    let result = run(Command::new("gh").args(["auth", "setup-git", "--hostname", "github.com"]));
    match result {
        Ok(output) if output.code == 0 => {}
        Ok(output) => eprintln!("gh auth setup-git: {}", failure(&output)),
        Err(error) => eprintln!("gh auth setup-git: {error:#}"),
    }
}

/// `server.login.start`: a device-code login for `github` or `codex`.
pub fn start(tool: &str) -> Result<DeviceCode> {
    let tool = Tool::parse(tool)?;
    let (url, code) = match tool {
        Tool::Github => {
            let mut command = Command::new("gh");
            command
                .args([
                    "auth",
                    "login",
                    "--hostname",
                    "github.com",
                    "--git-protocol",
                    "https",
                    "--web",
                ])
                .env("BROWSER", "true")
                .env("GH_BROWSER", "true");
            device_login(tool, command, CODE_LIMIT, setup_git)?
        }
        Tool::Codex => {
            let mut command = Command::new("codex");
            command.args(["login", "--device-auth"]);
            device_login(tool, command, CODE_LIMIT, || {})?
        }
        Tool::Claude => anyhow::bail!(
            "Claude Code has no device login; run `claude setup-token` on any computer and paste the token"
        ),
    };
    let account = match tool {
        Tool::Codex => "your ChatGPT account",
        _ => "GitHub",
    };
    Ok(DeviceCode {
        message: format!(
            "Open {url}, sign in to {account} and enter the code {code}. The server finishes logging in by itself."
        ),
        url,
        code,
    })
}

fn valid_token(token: &str) -> bool {
    (16..=4096).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic())
}

/// `server.login.token`: saves a `claude setup-token` token for agent processes.
pub fn token(storage: &Storage, tool: &str, token: &str) -> Result<()> {
    ensure!(
        Tool::parse(tool)? == Tool::Claude,
        "Only Claude Code logs in with a token; use a device login for {tool}"
    );
    let token = token.trim();
    ensure!(
        valid_token(token),
        "That does not look like a Claude Code token; copy the whole line `claude setup-token` prints"
    );
    set_env(&env_path(storage)?, CLAUDE_TOKEN, Some(token))
}

pub fn logout(storage: &Storage, tool: &str) -> Result<()> {
    let tool = Tool::parse(tool)?;
    cancel_pending(tool);
    let mut command = match tool {
        Tool::Claude => return set_env(&env_path(storage)?, CLAUDE_TOKEN, None),
        Tool::Github => {
            let login = github_status(run(Command::new("gh").args([
                "auth",
                "status",
                "--hostname",
                "github.com",
            ])));
            if !login.logged_in {
                return Ok(());
            }
            let mut command = Command::new("gh");
            command.args(["auth", "logout", "--hostname", "github.com"]);
            if let Some(account) = &login.account {
                command.args(["--user", account]);
            }
            command
        }
        Tool::Codex => {
            let mut command = Command::new("codex");
            command.arg("logout");
            command
        }
    };
    let output = run(&mut command)?;
    ensure!(
        output.code == 0,
        "Cannot log out of {}: {}",
        tool.name(),
        failure(&output)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(code: i32, stdout: &str, stderr: &str) -> Result<process::Output> {
        Ok(process::Output {
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            code,
        })
    }

    #[test]
    fn device_codes_parse_from_gh_and_codex_output() -> Result<()> {
        let gh = "\n! First copy your one-time code: 7EA8-D92E\nOpen this URL to continue in your web browser: https://github.com/login/device\n";
        assert_eq!(
            parse_device(gh),
            Some(("https://github.com/login/device".into(), "7EA8-D92E".into()))
        );
        assert_eq!(
            parse_device("! First copy your one-time code: 7EA8-D92E\n"),
            None
        );
        let codex = "\nWelcome to Codex [v\x1b[90m0.163.0\x1b[0m]\n\x1b[90mOpenAI's command-line coding agent\x1b[0m\n\nFollow these steps to sign in with ChatGPT using device code authorization:\n\n1. Open this link in your browser and sign in to your account\n   \x1b[94mhttps://auth.openai.com/codex/device\x1b[0m\n\n2. Enter this one-time code \x1b[90m(expires in 15 minutes)\x1b[0m\n   \x1b[94mU2Y1-8BJ28\x1b[0m\n\n\x1b[90mContinue only if you started this login in Codex.\x1b[0m\n";
        assert_eq!(
            parse_device(codex),
            Some((
                "https://auth.openai.com/codex/device".into(),
                "U2Y1-8BJ28".into()
            ))
        );
        let partial = codex.split("   \x1b[94mU2Y1").next().context("prefix")?;
        assert_eq!(parse_device(partial), None);
        Ok(())
    }

    #[test]
    fn github_status_reads_the_active_account() {
        let current = "github.com\n  ✓ Logged in to github.com account octo-cat (keyring)\n  - Active account: false\n  - Git operations protocol: https\n\n  ✓ Logged in to github.com account hubot (GITHUB_TOKEN)\n  - Active account: true\n  - Token: gho_****\n";
        let login = github_status(output(0, current, ""));
        assert!(login.logged_in);
        assert_eq!(login.account.as_deref(), Some("hubot"));
        assert_eq!(
            login.detail,
            "Logged in to github.com account hubot (GITHUB_TOKEN)"
        );
        let older = "github.com\n  ✓ Logged in to github.com as monalisa (oauth_token)\n  ✓ Git operations for github.com configured to use https protocol.\n";
        let login = github_status(output(0, "", older));
        assert_eq!(
            (login.logged_in, login.account.as_deref()),
            (true, Some("monalisa"))
        );
        let failed = "github.com\n  X Failed to log in to github.com account octo-cat (keyring)\n  - Active account: true\n  - The token in keyring is invalid.\n";
        let login = github_status(output(1, "", failed));
        assert_eq!(
            (login.logged_in, login.account.as_deref()),
            (false, Some("octo-cat"))
        );
        let none = "You are not logged into any GitHub hosts. To log in, run: gh auth login\n";
        let login = github_status(output(1, "", none));
        assert!(!login.logged_in && login.account.is_none());
        assert!(
            login
                .detail
                .starts_with("You are not logged into any GitHub hosts")
        );
        let missing = github_status(Err(anyhow::anyhow!("Could not start gh")));
        assert_eq!(missing.detail, "Could not start gh");
    }

    #[test]
    fn server_agents_share_claude_codes_config_directory() {
        let token = ("CLAUDE_CODE_OAUTH_TOKEN".to_owned(), "sk-ant".to_owned());
        let custom = ("CLAUDE_CONFIG_DIR".to_owned(), "/elsewhere".to_owned());
        let home = Path::new("/home/difu");
        assert_eq!(
            server_env(vec![token.clone(), custom.clone()], true, Some(home)),
            [
                token.clone(),
                (
                    "CLAUDE_CONFIG_DIR".to_owned(),
                    "/home/difu/.claude".to_owned()
                ),
            ]
        );
        // Ordinary installations keep the user's own Claude Code setup.
        let ordinary = vec![token];
        assert_eq!(server_env(ordinary.clone(), false, Some(home)), ordinary);
        assert_eq!(server_env(ordinary.clone(), true, None), ordinary);
    }

    #[test]
    fn codex_and_claude_status() -> Result<()> {
        let login = codex_status(output(0, "", "Logged in using ChatGPT\n"));
        assert!(login.logged_in);
        assert_eq!(login.detail, "Logged in using ChatGPT");
        let login = codex_status(output(1, "Not logged in\n", ""));
        assert!(!login.logged_in);
        assert_eq!(login.detail, "Not logged in");
        assert!(claude_status(true, false, false).logged_in);
        assert!(claude_status(false, true, false).logged_in);
        assert!(claude_status(false, false, true).logged_in);
        let login = claude_status(false, false, false);
        assert!(!login.logged_in);
        assert_eq!(
            serde_json::to_value(&login)?,
            serde_json::json!({
                "tool": "claude",
                "loggedIn": false,
                "account": null,
                "detail": "Not logged in; paste a token from `claude setup-token`",
            })
        );
        Ok(())
    }

    #[test]
    fn server_env_round_trips_privately() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage {
            config: dir.path().join("config.json"),
            cache: dir.path().join("cache"),
        };
        let path = env_path(&storage)?;
        assert_eq!(path, dir.path().join("server.env"));
        assert!(read_env(&path)?.is_empty());
        fs::write(
            &path,
            "# comment\nOTHER=\"kept value\"\n\nnot a pair\n1BAD=x\n",
        )?;
        token(&storage, "claude", " sk-ant-oat01-abcdefghijklmnop \n")?;
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            read_env(&path)?,
            [
                ("OTHER".to_owned(), "kept value".to_owned()),
                (
                    CLAUDE_TOKEN.to_owned(),
                    "sk-ant-oat01-abcdefghijklmnop".to_owned()
                ),
            ]
        );
        token(&storage, "claude", "sk-ant-oat01-replacement-token")?;
        assert_eq!(
            fs::read_to_string(&path)?,
            "OTHER=kept value\nCLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-replacement-token\n"
        );
        assert!(token(&storage, "claude", "short").is_err());
        assert!(token(&storage, "claude", "has a space in the token text").is_err());
        assert!(token(&storage, "github", "sk-ant-oat01-abcdefghijklmnop").is_err());
        logout(&storage, "claude")?;
        assert_eq!(fs::read_to_string(&path)?, "OTHER=kept value\n");
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        assert!(set_env(&path, "BAD KEY", Some("x")).is_err());
        assert!(set_env(&path, "KEY", Some("two\nlines")).is_err());
        assert!(logout(&storage, "nobody").is_err());
        Ok(())
    }

    #[test]
    fn device_logins_return_codes_and_report_early_exits() -> Result<()> {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "printf '! First copy your one-time code: ABCD-1234\\n' >&2; printf 'Open this URL to continue in your web browser: https://github.com/login/device\\n' >&2; sleep 0.3",
        ]);
        let (url, code) = device_login(Tool::Github, command, Duration::from_secs(10), || {})?;
        assert_eq!(url, "https://github.com/login/device");
        assert_eq!(code, "ABCD-1234");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "echo 'error: device code login is disabled' >&2; exit 1",
        ]);
        let error = device_login(Tool::Codex, command, Duration::from_secs(10), || {})
            .err()
            .context("Expected an early exit")?;
        assert!(
            error.to_string().contains("device code login is disabled"),
            "{error:#}"
        );
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5"]);
        let started = Instant::now();
        let error = device_login(Tool::Claude, command, Duration::from_millis(200), || {})
            .err()
            .context("Expected a timeout")?;
        assert!(error.to_string().contains("did not show a login code"));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(start("claude").is_err());
        assert!(start("nobody").is_err());
        Ok(())
    }
}
