//! `difu server add`: one-time onboarding of a fresh Linux server over SSH, plus
//! the SSH-based helpers (`ssh`, `pair`, `upgrade`) for saved servers.
//!
//! Everything goes through the system `ssh` binary. The first login uses your
//! own SSH keys or the password typed once, which difu answers itself as ssh's
//! `SSH_ASKPASS` helper. Every later connection uses an ed25519 key difu makes
//! for the server and a known_hosts file of its own, both in
//! `<config>/servers/<name>/`. The server side is one idempotent bash script
//! (`onboard/bootstrap.sh`) run as root, a few steps at a time.
use super::{
    DEFAULT_PORT, client, password,
    servers::{self, SavedServer, SshAccess},
};
use crate::storage::Storage;
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::OsString,
    fs,
    io::{self, BufRead, IsTerminal, Write},
    os::unix::{
        fs::{DirBuilderExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Output, Stdio},
};

#[derive(Clone, Debug)]
pub struct Options {
    pub address: String,
    pub name: Option<String>,
    pub user: String,
    pub ssh_port: u16,
    pub tailscale: bool,
    pub sandbox: bool,
}

/// Names the file holding the password while difu acts as ssh's askpass helper.
pub const ASKPASS_SECRET_FILE: &str = "DIFU_ASKPASS_SECRET_FILE";

const BOOTSTRAP: &str = include_str!("onboard/bootstrap.sh");
const UNIT: &str = include_str!("onboard/difu-agent.service");
/// The line in `BOOTSTRAP` replaced by the shell-quoted parameters.
const PARAMETERS: &str = "# @PARAMETERS@";
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Every known_hosts file holds one server, so its key is stored under this
/// alias rather than an address: it stays pinned when the address changes (for
/// example to the server's Tailscale address).
const HOST_KEY_ALIAS: &str = "difu-server";
const KEY_FILE: &str = "id_ed25519";
const KNOWN_HOSTS_FILE: &str = "known_hosts";
/// The address `difu server add` used, so a rerun without `--name` finds the
/// server's key even before the server was saved.
const ADDRESS_FILE: &str = "address";
const SSH_MISSING: &str = "Cannot run `ssh`; install OpenSSH and try again";
/// Host name, architecture and whether the login can act as root.
const PROBE: &str = "hostname; uname -m; \
    if [ \"$(id -u)\" = 0 ] || sudo -n true 2>/dev/null; then echo root; else echo user; fi";
const TAILSCALE_ADDRESS: &str =
    "tailscale ip -4 | head -n 1; tailscale status --json | jq -r '.Self.DNSName // empty'";
const GITHUB_STATUS: &str = "gh auth status";
const GITHUB_LOGIN: &str =
    "gh auth login --hostname github.com --git-protocol https --web && gh auth setup-git";
/// Commits made by agents need an author. Unless one is set, use the GitHub
/// account the server logged in with and its private noreply address.
const GIT_IDENTITY: &str = "git config --global user.email >/dev/null || { \
    login=$(gh api user --jq .login) && id=$(gh api user --jq .id) \
    && name=$(gh api user --jq '.name // .login') \
    && git config --global user.name \"$name\" \
    && git config --global user.email \"$id+$login@users.noreply.github.com\"; }";
const CODEX_STATUS: &str = "\"$HOME/.local/bin/codex\" login status";
const CODEX_LOGIN: &str = "\"$HOME/.local/bin/codex\" login --device-auth";
const CLAUDE_STATUS: &str = "grep -q '^CLAUDE_CODE_OAUTH_TOKEN=' \"$HOME/.config/difu/server.env\"";
/// Reads the token from stdin, so it never appears in a process list.
const CLAUDE_STORE: &str = "umask 077 && mkdir -p \"$HOME/.config/difu\" && IFS= read -r token \
    && file=\"$HOME/.config/difu/server.env\" && touch \"$file\" \
    && { grep -v '^CLAUDE_CODE_OAUTH_TOKEN=' \"$file\" || true; \
    printf 'CLAUDE_CODE_OAUTH_TOKEN=%s\\n' \"$token\"; } > \"$file.tmp\" \
    && mv \"$file.tmp\" \"$file\"";

/// Answers ssh when difu runs as its `SSH_ASKPASS` helper: prints the secret
/// from the file named by `DIFU_ASKPASS_SECRET_FILE`. `None` on every normal
/// run, which then continues untouched.
pub fn answer_askpass() -> Option<Result<()>> {
    askpass(
        std::env::var_os(ASKPASS_SECRET_FILE),
        &mut io::stdout().lock(),
    )
}

fn askpass(file: Option<OsString>, out: &mut impl Write) -> Option<Result<()>> {
    let file = file.filter(|file| !file.is_empty())?;
    Some((|| -> Result<()> {
        let secret = fs::read(&file).context("difu could not read the password for ssh")?;
        out.write_all(&secret)?;
        out.write_all(b"\n")?;
        out.flush()?;
        Ok(())
    })())
}

pub fn add(storage: &Storage, options: Options) -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "`difu server add` asks questions as it goes; run it in an interactive terminal"
    );
    let (user, address) = destination(&options.address, &options.user)?;
    let servers = servers_dir(storage)?;
    private_dir(&servers)?;
    let mut progress = Progress::new(if options.tailscale { 10 } else { 9 });

    progress.next(&format!("Connecting to {user}@{address}"));
    let known = match &options.name {
        Some(name) => Some(chosen_name(name)?),
        None => known_name(storage, &servers, &address),
    };
    // Without a name yet, the first connection trusts the host key in a
    // scratch directory; it moves once the server's host name gives the name.
    let staging = tempfile::Builder::new()
        .prefix(".new-")
        .tempdir_in(&servers)
        .context("Cannot create a directory for the new server")?;
    let probe_dir = match &known {
        Some(name) => servers.join(name),
        None => staging.path().to_owned(),
    };
    private_dir(&probe_dir)?;
    let mut remote = Remote {
        address: address.clone(),
        port: options.ssh_port,
        known_hosts: probe_dir.join(KNOWN_HOSTS_FILE),
    };
    let trusted = has_host_key(&remote.known_hosts);
    let saved_key = known
        .as_ref()
        .map(|name| servers.join(name).join(KEY_FILE))
        .filter(|key| key.exists());
    let (login_user, login, facts) = first_login(&remote, &user, saved_key.as_deref())?;
    if !trusted {
        print_fingerprints(&remote.known_hosts);
    }
    note(match login {
        Login::Key(_) => "Logged in with difu's key for this server",
        Login::Agent => "Logged in with your SSH key",
        Login::Password(_) => "Logged in with the password",
    });
    ensure!(
        facts.root,
        "{login_user} cannot run commands as root without a password. Log in as root \
         (`--user root`) or give {login_user} passwordless sudo, then try again"
    );
    let target = release_target(&facts.arch)?;
    let name = match known {
        Some(name) => name,
        None => unused_name(
            &name_for_host(&facts.hostname, &address),
            &servers,
            &servers::load(storage).unwrap_or_default(),
        ),
    };
    let dir = servers.join(&name);
    private_dir(&dir)?;
    let known_hosts = dir.join(KNOWN_HOSTS_FILE);
    if remote.known_hosts != known_hosts && !has_host_key(&known_hosts) {
        fs::copy(&remote.known_hosts, &known_hosts).context("Cannot save the server's host key")?;
    }
    drop(staging);
    remote.known_hosts = known_hosts;
    fs::write(dir.join(ADDRESS_FILE), format!("{address}\n"))?;
    let mut retry = retry_command(&options, &user, &address, &name);
    note(&format!("Saving it as {name}"));

    progress.next("Installing difu's SSH key for root and a new difu user");
    let key = ensure_key(&dir)?;
    let public_key = read_public_key(&key)?;
    let mut bootstrap = Bootstrap {
        steps: vec![Step::Keys],
        target,
        public_key: &public_key,
        ssh_port: options.ssh_port,
        tailscale: options.tailscale,
        sandbox: options.sandbox,
    };
    let shell = if login_user == "root" {
        "bash -s"
    } else {
        "sudo -n bash -s"
    };
    let status =
        Ssh::new(&remote, &login_user, &login).stream(shell, bootstrap.render()?.as_bytes())?;
    ensure!(
        status.success(),
        "Could not install difu's key (see above); nothing else changed on the server. \
         Fix the problem and run `{retry}` again"
    );
    let with_key = Login::Key(key.clone());
    let check = Ssh::new(&remote, "root", &with_key).output("true", None)?;
    ensure!(
        check.status.success(),
        "The server does not accept difu's key for root ({}). Password logins still work. \
         Allow key logins for root (`PermitRootLogin prohibit-password` in \
         /etc/ssh/sshd_config), then run `{retry}` again",
        stderr(&check)
    );
    note("difu's key works; from now on difu logs in with it");

    progress.next("Securing the server and installing difu (this takes a few minutes)");
    bootstrap.steps = Step::setup(options.tailscale);
    let status =
        Ssh::new(&remote, "root", &with_key).stream("bash -s", bootstrap.render()?.as_bytes())?;
    ensure!(
        status.success(),
        "Server setup stopped (see above). Fix the problem, then run `{retry}` again; \
         finished steps are safe to repeat"
    );

    let mut hosts = vec![address.clone()];
    if options.tailscale {
        progress.next("Switching to Tailscale");
        let (tailnet, tailnet_hosts) =
            switch_to_tailscale(&remote, &with_key, &bootstrap, &name, &retry)?;
        retry = retry_command(&options, "root", &tailnet.address, &name);
        remote = tailnet;
        hosts = tailnet_hosts;
    }
    let root = Ssh::new(&remote, "root", &with_key);
    let difu = Ssh::new(&remote, "difu", &with_key);

    progress.next("Replacing the root password");
    replace_root_password(&root, &name)?;

    progress.next("Logging in to GitHub, Codex and Claude Code on the server");
    guided_login(
        &difu,
        &name,
        "GitHub",
        GITHUB_STATUS,
        GITHUB_LOGIN,
        "gh auth login",
    )?;
    guided_login(
        &difu,
        &name,
        "Codex",
        CODEX_STATUS,
        CODEX_LOGIN,
        "~/.local/bin/codex login --device-auth",
    )?;
    claude_login(&difu, &root, &name)?;
    git_identity(&difu);

    progress.next("Choosing the server password");
    note("Other computers and phones join with the server's address and this password.");
    if confirm("Set the server password now?", true)? {
        let password = choose_server_password()?;
        let status = difu.stream("difu remote password", format!("{password}\n").as_bytes())?;
        if !status.success() {
            warn(&format!(
                "The server password was not set (see above). Later: `difu server ssh {name}`, \
                 then `difu remote password`."
            ));
        }
    }

    progress.next("Choosing repositories");
    if confirm("Pick repositories to keep cloned on the server now?", true)? {
        let status = difu.interactive(Some("difu repos add"))?;
        if !status.success() {
            warn(&format!(
                "No repositories were added. Later: `difu server ssh {name}`, then `difu repos add`."
            ));
        }
    }

    progress.next("Pairing this computer");
    pair_this_computer(storage, &difu, &name, hosts, &key, &retry)?;

    progress.next("Pairing a phone");
    if confirm("Show a QR code for the difu iPhone app now?", true)?
        && let Err(error) = show_pairing(&difu)
    {
        warn(&format!(
            "No QR code was shown ({error:#}). Later: `difu server pair {name}`."
        ));
    }
    println!(
        "\n{name} is ready: pick it when you start a new session in difu.\n\
         Shell: `difu server ssh {name}` · Phone: `difu server pair {name}` · \
         Update: `difu server upgrade {name}`"
    );
    Ok(())
}

/// Checks that this computer reaches the server over Tailscale, then closes
/// its public ports. Returns the server over Tailscale and the hosts to
/// advertise.
fn switch_to_tailscale(
    remote: &Remote,
    with_key: &Login,
    bootstrap: &Bootstrap,
    name: &str,
    retry: &str,
) -> Result<(Remote, Vec<String>)> {
    let output = Ssh::new(remote, "root", with_key).output(TAILSCALE_ADDRESS, None)?;
    ensure!(
        output.status.success(),
        "Cannot read the server's Tailscale address ({}). Run `{retry}` again",
        stderr(&output)
    );
    let (ip, dns) = parse_tailscale(&String::from_utf8_lossy(&output.stdout))?;
    let tailnet = Remote {
        address: ip.clone(),
        ..remote.clone()
    };
    let check = Ssh::new(&tailnet, "root", with_key).output("true", None)?;
    ensure!(
        check.status.success(),
        "This computer cannot reach {name} over Tailscale at {ip} ({}). Install Tailscale here \
         and log in to the same tailnet, then run `{retry}` again; the server's public ports \
         stay open until then",
        stderr(&check)
    );
    let lockdown = Bootstrap {
        steps: vec![Step::Lockdown],
        target: bootstrap.target,
        public_key: bootstrap.public_key,
        ssh_port: bootstrap.ssh_port,
        tailscale: bootstrap.tailscale,
        sandbox: bootstrap.sandbox,
    };
    let status =
        Ssh::new(&tailnet, "root", with_key).stream("bash -s", lockdown.render()?.as_bytes())?;
    ensure!(
        status.success(),
        "Could not close the public ports (see above). Run `{retry}` again with {ip} as the \
         address"
    );
    note(&format!(
        "SSH and difu now answer only over Tailscale at {ip}"
    ));
    Ok((tailnet, std::iter::once(ip).chain(dns).collect()))
}

/// Advertises `hosts`, pairs this computer as a full device and saves the
/// server with its SSH access.
fn pair_this_computer(
    storage: &Storage,
    difu: &Ssh,
    name: &str,
    hosts: Vec<String>,
    key: &Path,
    retry: &str,
) -> Result<()> {
    let output = difu.output(&remote_config(name, &hosts), None)?;
    ensure!(
        output.status.success(),
        "Could not save the server's address on the server ({}). Run `{retry}` again",
        stderr(&output)
    );
    let output = difu.output("difu remote pair --json", None)?;
    ensure!(
        output.status.success(),
        "The server did not give a pairing link ({}). Check difu-agent with \
         `difu server ssh {name}` and `systemctl status difu-agent`, then run `{retry}` again",
        stderr(&output)
    );
    let (link, _) = pairing_answer(&String::from_utf8_lossy(&output.stdout))?;
    let device = computer_name();
    let paired = client::pair_with_link(&link, &device, true).with_context(|| {
        format!(
            "Could not pair this computer with {name}. Check that port {DEFAULT_PORT} is \
             reachable from here, then run `{retry}` again"
        )
    })?;
    servers::save(
        storage,
        SavedServer {
            name: name.to_owned(),
            id: paired.server_id,
            hosts: if paired.hosts.is_empty() {
                hosts
            } else {
                paired.hosts
            },
            port: if paired.port == 0 {
                DEFAULT_PORT
            } else {
                paired.port
            },
            fingerprint: paired.fingerprint,
            device: paired.device,
            token: paired.token,
            ssh: Some(SshAccess {
                address: difu.remote.address.clone(),
                port: difu.remote.port,
                key: key.to_owned(),
            }),
        },
    )
    .with_context(|| format!("Paired with {name}, but could not save it on this computer"))?;
    note(&format!("Paired as \"{device}\" with full access"));
    Ok(())
}

pub fn ssh(storage: &Storage, name: &str) -> Result<()> {
    let access = access(storage, name)?;
    let remote = Remote::saved(&access);
    let login = Login::Key(access.key.clone());
    let error = Command::new("ssh")
        .args(Ssh::new(&remote, "difu", &login).argv(true, None)?)
        .exec();
    Err(error).context(SSH_MISSING)
}

pub fn pair(storage: &Storage, name: &str) -> Result<()> {
    let access = access(storage, name)?;
    let remote = Remote::saved(&access);
    let login = Login::Key(access.key.clone());
    show_pairing(&Ssh::new(&remote, "difu", &login)).with_context(|| {
        format!(
            "Could not show a pairing code for {name}. Check the service with \
             `difu server ssh {name}`, then `systemctl status difu-agent`"
        )
    })
}

/// Brings a saved server up to this computer's difu: the binary, Codex and
/// Claude Code, the sandbox image when sandboxing is on (the bundled
/// Containerfile may have changed), and `difu-agent.service`, which then
/// restarts. Every step is safe to repeat.
pub fn upgrade(storage: &Storage, name: &str) -> Result<()> {
    let access = access(storage, name)?;
    let remote = Remote::saved(&access);
    let login = Login::Key(access.key.clone());
    let root = Ssh::new(&remote, "root", &login);
    println!(
        "Upgrading {name} to difu {VERSION}: difu, Codex and Claude Code, the sandbox image \
         (when sandboxing is on) and difu-agent.service"
    );
    let output = root.output("uname -m", None)?;
    ensure!(
        output.status.success(),
        "Cannot log in to {name} as root with difu's key ({}). Check that the server is \
         running and reachable at {}",
        stderr(&output),
        access.address
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let arch = stdout.lines().map(str::trim).rfind(|line| !line.is_empty());
    let target = release_target(arch.unwrap_or_default())?;
    // Only the keys step reads the public key.
    let public_key = read_public_key(&access.key).unwrap_or_default();
    // The image step reads whether sandboxing is on from the server itself.
    let bootstrap = Bootstrap {
        steps: Step::upgrade(),
        target,
        public_key: &public_key,
        ssh_port: access.port,
        tailscale: false,
        sandbox: false,
    };
    let status = root.stream("bash -s", bootstrap.render()?.as_bytes())?;
    ensure!(
        status.success(),
        "The upgrade stopped (see above). Fix the problem, then run \
         `difu server upgrade {name}` again"
    );
    // Servers set up before display names show their host name; name them as
    // saved here. The service restarts above, so give it a moment to listen.
    let difu = Ssh::new(&remote, "difu", &login);
    let command = format!("difu remote config --name {}", shell_quote(name));
    let named = (0..10).any(|attempt| {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        difu.output(&command, None)
            .is_ok_and(|output| output.status.success())
    });
    if !named {
        println!("Could not name the server {name}; run `difu remote config --name {name}` on it");
    }
    git_identity(&difu);
    println!("{name} now runs difu {VERSION}");
    Ok(())
}

/// Sets the `difu` user's Git author from its GitHub login when none is set;
/// failures only print a hint, since agents can still commit with an override.
fn git_identity(difu: &Ssh) {
    match difu.output(GIT_IDENTITY, None) {
        Ok(output) if output.status.success() => {}
        _ => println!(
            "Could not set a Git author for agents; log in to GitHub on the server, then run \
             `git config --global user.name` and `user.email` as difu"
        ),
    }
}

/// Where one server answers SSH, and its own known_hosts file.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Remote {
    address: String,
    port: u16,
    known_hosts: PathBuf,
}

impl Remote {
    fn saved(access: &SshAccess) -> Self {
        Self {
            address: access.address.clone(),
            port: access.port,
            known_hosts: access.key.with_file_name(KNOWN_HOSTS_FILE),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Login {
    /// Your own keys and agent, never prompting.
    Agent,
    /// The password typed once, answered through `SSH_ASKPASS`.
    Password(String),
    /// difu's key for this server.
    Key(PathBuf),
}

/// One `ssh` invocation: a server, a user and how to log in.
struct Ssh<'a> {
    remote: &'a Remote,
    user: &'a str,
    login: &'a Login,
}

impl<'a> Ssh<'a> {
    fn new(remote: &'a Remote, user: &'a str, login: &'a Login) -> Self {
        Self {
            remote,
            user,
            login,
        }
    }

    fn argv(&self, tty: bool, command: Option<&str>) -> Result<Vec<OsString>> {
        let mut args: Vec<OsString> = vec![if tty { "-t" } else { "-T" }.into()];
        let mut options = vec![
            // The value is split on spaces unless quoted; macOS keeps difu's
            // settings under "Application Support".
            format!(
                "UserKnownHostsFile=\"{}\"",
                ssh_path(&self.remote.known_hosts)?
            ),
            "StrictHostKeyChecking=accept-new".to_owned(),
            format!("HostKeyAlias={HOST_KEY_ALIAS}"),
            "ConnectTimeout=15".to_owned(),
            "ServerAliveInterval=30".to_owned(),
            // A shared connection from your own ssh config would skip the
            // login being tested and could carry another identity.
            "ControlMaster=no".to_owned(),
            "ControlPath=none".to_owned(),
            "ForwardAgent=no".to_owned(),
            "LogLevel=ERROR".to_owned(),
        ];
        match self.login {
            Login::Agent => options.push("BatchMode=yes".to_owned()),
            Login::Password(_) => options.extend(
                [
                    "BatchMode=no",
                    "PubkeyAuthentication=no",
                    "PreferredAuthentications=keyboard-interactive,password",
                    "NumberOfPasswordPrompts=1",
                ]
                .map(str::to_owned),
            ),
            Login::Key(key) => {
                args.extend(["-i".into(), ssh_path(key)?.into()]);
                options.extend(["IdentitiesOnly=yes", "BatchMode=yes"].map(str::to_owned));
            }
        }
        for option in options {
            args.extend(["-o".into(), option.into()]);
        }
        args.extend([
            "-p".into(),
            self.remote.port.to_string().into(),
            "-l".into(),
            self.user.into(),
            // Nothing after the address is read as an ssh option.
            "--".into(),
            self.remote.address.clone().into(),
        ]);
        args.extend(command.map(OsString::from));
        Ok(args)
    }

    /// The command and, for password logins, the askpass file it needs while
    /// it runs.
    fn command(&self, tty: bool, command: Option<&str>) -> Result<(Command, Option<Askpass>)> {
        let mut ssh = Command::new("ssh");
        ssh.args(self.argv(tty, command)?);
        let askpass = match self.login {
            Login::Password(secret) => {
                let askpass = Askpass::new(secret)?;
                askpass.apply(&mut ssh)?;
                Some(askpass)
            }
            _ => None,
        };
        Ok((ssh, askpass))
    }

    /// Runs a remote command and captures its output.
    fn output(&self, command: &str, input: Option<&[u8]>) -> Result<Output> {
        let (mut ssh, _askpass) = self.command(false, Some(command))?;
        ssh.stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let mut child = ssh.spawn().context(SSH_MISSING)?;
        feed(&mut child, input);
        Ok(child.wait_with_output()?)
    }

    /// Runs a remote command with its output on this terminal.
    fn stream(&self, command: &str, input: &[u8]) -> Result<ExitStatus> {
        let (mut ssh, _askpass) = self.command(false, Some(command))?;
        let mut child = ssh.stdin(Stdio::piped()).spawn().context(SSH_MISSING)?;
        feed(&mut child, Some(input));
        Ok(child.wait()?)
    }

    /// Runs a remote command (or a shell) in a terminal on the server.
    fn interactive(&self, command: Option<&str>) -> Result<ExitStatus> {
        let (mut ssh, _askpass) = self.command(true, command)?;
        ssh.status().context(SSH_MISSING)
    }
}

/// Inputs are small (a script, a password), so writing them before reading
/// any output cannot fill a pipe. A write fails only when ssh already exited,
/// and its exit status explains why.
fn feed(child: &mut std::process::Child, input: Option<&[u8]>) {
    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
        let _ = stdin.write_all(input);
    }
}

/// ssh reads `%` as a token and `"` as quoting in option values.
fn ssh_path(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
    ensure!(
        !text.contains(['%', '"']),
        "ssh cannot use {text}: move difu's configuration to a path without % or \""
    );
    Ok(text.to_owned())
}

/// The typed password in a 0600 temporary file for one ssh run; removed when
/// dropped.
struct Askpass(tempfile::NamedTempFile);

impl Askpass {
    fn new(secret: &str) -> Result<Self> {
        let mut file = tempfile::Builder::new()
            .prefix("difu-askpass-")
            .tempfile()
            .context("Cannot create a private file for the password")?;
        fs::set_permissions(file.path(), fs::Permissions::from_mode(0o600))?;
        file.write_all(secret.as_bytes())?;
        file.flush()?;
        Ok(Self(file))
    }

    fn apply(&self, command: &mut Command) -> Result<()> {
        command
            .env(
                "SSH_ASKPASS",
                std::env::current_exe().context("Cannot locate difu for ssh")?,
            )
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(ASKPASS_SECRET_FILE, self.0.path());
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Credentials,
    HostKey,
    Connection,
}

fn classify(stderr: &str) -> Failure {
    if stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
        || stderr.contains("Host key verification failed")
    {
        Failure::HostKey
    } else if [
        "Permission denied",
        "Too many authentication failures",
        "No supported authentication methods",
        "No more authentication methods",
    ]
    .iter()
    .any(|text| stderr.contains(text))
    {
        Failure::Credentials
    } else {
        Failure::Connection
    }
}

/// difu's key for root when this server was set up before, then your own
/// keys, then the password (three tries).
fn first_login(
    remote: &Remote,
    user: &str,
    saved_key: Option<&Path>,
) -> Result<(String, Login, Facts)> {
    if let Some(key) = saved_key {
        let login = Login::Key(key.to_owned());
        if let Ok(facts) = attempt(remote, "root", &login)? {
            return Ok(("root".to_owned(), login, facts));
        }
    }
    let refusal = match attempt(remote, user, &Login::Agent)? {
        Ok(facts) => return Ok((user.to_owned(), Login::Agent, facts)),
        Err(refusal) => refusal,
    };
    ensure!(
        accepts_passwords(&refusal),
        "{user}@{} accepts only SSH keys, and none of yours worked. Add your public key for \
         {user} in your provider's console (or log in as another user with `--user`), then try \
         again",
        remote.address
    );
    for _ in 0..3 {
        let login = Login::Password(secret(&format!("Password for {user}@{}:", remote.address))?);
        if let Ok(facts) = attempt(remote, user, &login)? {
            return Ok((user.to_owned(), login, facts));
        }
        note("That password did not work.");
    }
    bail!(
        "The server refused the password three times. Check or reset it in your provider's \
         console, then try again"
    )
}

/// One login: the server's facts, or ssh's message when the server refused
/// the credentials.
fn attempt(remote: &Remote, user: &str, login: &Login) -> Result<Result<Facts, String>> {
    let output = Ssh::new(remote, user, login).output(PROBE, None)?;
    if output.status.success() {
        return parse_facts(&String::from_utf8_lossy(&output.stdout)).map(Ok);
    }
    let message = stderr(&output);
    if output.status.code() != Some(255) {
        bail!("Logged in, but the server could not run a basic command ({message})");
    }
    match classify(&message) {
        Failure::Credentials => Ok(Err(message)),
        Failure::HostKey => bail!(
            "The server at {} has a different host key than the one difu trusted in {}. If you \
             reinstalled the server, delete that file and try again (or choose another --name if this \
             is a different server); otherwise someone may be intercepting the connection",
            remote.address,
            remote.known_hosts.display()
        ),
        Failure::Connection => bail!(
            "Cannot reach {} on SSH port {} ({message}). Check the address, that the server is \
             running and that its firewall allows SSH, then try again",
            remote.address,
            remote.port
        ),
    }
}

/// `Permission denied (publickey,password).` lists what the server accepts.
fn accepts_passwords(refusal: &str) -> bool {
    let Some((_, methods)) = refusal.split_once("Permission denied (") else {
        return true;
    };
    let methods = methods.split(')').next().unwrap_or_default();
    methods.contains("password") || methods.contains("keyboard-interactive")
}

#[derive(Debug, PartialEq, Eq)]
struct Facts {
    hostname: String,
    arch: String,
    root: bool,
}

/// The last three lines, after anything the server's shell profile printed.
fn parse_facts(stdout: &str) -> Result<Facts> {
    let lines: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let [.., hostname, arch, access] = lines.as_slice() else {
        bail!("The server gave an unexpected answer: {}", stdout.trim());
    };
    Ok(Facts {
        hostname: (*hostname).to_owned(),
        arch: (*arch).to_owned(),
        root: *access == "root",
    })
}

fn release_target(arch: &str) -> Result<&'static str> {
    match arch.trim() {
        "x86_64" | "amd64" => Ok("x86_64-unknown-linux-musl"),
        "aarch64" | "arm64" => Ok("aarch64-unknown-linux-musl"),
        other => bail!(
            "difu has Linux builds for x86_64 and arm64 servers; this one is {other}. Choose an \
             x86_64 or arm64 server"
        ),
    }
}

/// Steps of `onboard/bootstrap.sh`, run in the order given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Keys,
    Ssh,
    Packages,
    Firewall,
    Updates,
    Tailscale,
    Lockdown,
    Difu,
    Tools,
    Sandbox,
    Image,
    Service,
}

impl Step {
    fn name(self) -> &'static str {
        match self {
            Self::Keys => "keys",
            Self::Ssh => "ssh",
            Self::Packages => "packages",
            Self::Firewall => "firewall",
            Self::Updates => "updates",
            Self::Tailscale => "tailscale",
            Self::Lockdown => "lockdown",
            Self::Difu => "difu",
            Self::Tools => "tools",
            Self::Sandbox => "sandbox",
            Self::Image => "image",
            Self::Service => "service",
        }
    }

    /// Everything after the keys, over difu's key. Password logins go first.
    fn setup(tailscale: bool) -> Vec<Self> {
        let mut steps = vec![Self::Ssh, Self::Packages, Self::Firewall, Self::Updates];
        if tailscale {
            steps.push(Self::Tailscale);
        }
        steps.extend([Self::Difu, Self::Tools, Self::Sandbox, Self::Service]);
        steps
    }

    /// `difu server upgrade`: new binary, updated agent CLIs, a fresh sandbox
    /// image, then the current unit and a restart. Never the sandbox step,
    /// which would change whether sandboxing is on.
    fn upgrade() -> Vec<Self> {
        vec![Self::Difu, Self::Tools, Self::Image, Self::Service]
    }
}

/// The bootstrap script with its parameters. Flags only matter to the steps
/// that read them.
struct Bootstrap<'a> {
    steps: Vec<Step>,
    target: &'a str,
    public_key: &'a str,
    ssh_port: u16,
    tailscale: bool,
    sandbox: bool,
}

impl Bootstrap<'_> {
    fn parameters(&self) -> String {
        let flag = |on: bool| if on { "1" } else { "0" }.to_owned();
        let steps: Vec<&str> = self.steps.iter().map(|step| step.name()).collect();
        [
            ("DIFU_STEPS", steps.join(" ")),
            ("DIFU_VERSION", VERSION.to_owned()),
            ("DIFU_TARGET", self.target.to_owned()),
            ("DIFU_PUBLIC_KEY", self.public_key.to_owned()),
            ("DIFU_PORT", DEFAULT_PORT.to_string()),
            ("DIFU_SSH_PORT", self.ssh_port.to_string()),
            ("DIFU_TAILSCALE", flag(self.tailscale)),
            ("DIFU_SANDBOX", flag(self.sandbox)),
            ("DIFU_UNIT", UNIT.to_owned()),
        ]
        .map(|(name, value)| format!("{name}={}", shell_quote(&value)))
        .join("\n")
    }

    fn render(&self) -> Result<String> {
        ensure!(
            BOOTSTRAP.matches(PARAMETERS).count() == 1,
            "The bundled server setup script has no parameter line"
        );
        Ok(BOOTSTRAP.replacen(PARAMETERS, &self.parameters(), 1))
    }
}

/// One single-quoted shell word.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Advertises `hosts` and names the server as this computer saves it, so
/// phones and other clients show that name rather than the host name.
fn remote_config(name: &str, hosts: &[String]) -> String {
    hosts.iter().fold(
        format!("difu remote config --name {}", shell_quote(name)),
        |command, host| format!("{command} --host {}", shell_quote(host)),
    )
}

/// `difu server add` again with what this run settled, for failure messages.
fn retry_command(options: &Options, user: &str, address: &str, name: &str) -> String {
    let mut command = format!("difu server add {address} --name {name}");
    if user != "root" {
        command.push_str(&format!(" --user {user}"));
    }
    if options.ssh_port != 22 {
        command.push_str(&format!(" --ssh-port {}", options.ssh_port));
    }
    if options.tailscale {
        command.push_str(" --tailscale");
    }
    if !options.sandbox {
        command.push_str(" --no-sandbox");
    }
    command
}

/// `user@host` overrides `--user`; IPv6 brackets are optional.
fn destination(address: &str, user: &str) -> Result<(String, String)> {
    let address = address.trim();
    let (user, host) = address.rsplit_once('@').unwrap_or((user, address));
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    ensure!(
        !host.is_empty()
            && !host.starts_with('-')
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_')),
        "`{address}` is not a server address; use its IP address or host name, for example \
         `difu server add 203.0.113.7`"
    );
    ensure!(
        valid_user(user),
        "`{user}` is not a valid user name for --user"
    );
    Ok((user.to_owned(), host.to_owned()))
}

fn valid_user(user: &str) -> bool {
    let mut chars = user.chars();
    user.len() <= 32
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'))
}

/// Lowercase letters, digits and single dashes, at most 40 characters: safe in
/// paths, Keychain item names and shell commands.
fn slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(40);
    slug.trim_end_matches('-').to_owned()
}

/// The server's short host name, else its address.
fn name_for_host(hostname: &str, address: &str) -> String {
    let hostname = hostname.trim();
    let short = if hostname.parse::<std::net::IpAddr>().is_ok() {
        hostname
    } else {
        hostname.split('.').next().unwrap_or_default()
    };
    [slug(short), slug(address)]
        .into_iter()
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "server".to_owned())
}

fn chosen_name(name: &str) -> Result<String> {
    let chosen = slug(name);
    ensure!(
        !chosen.is_empty(),
        "Use letters, digits and dashes for --name, for example `--name dev-box`"
    );
    if chosen != name {
        note(&format!("Using the name {chosen}"));
    }
    Ok(chosen)
}

/// A name for a new server that no other saved or onboarded server uses.
fn unused_name(base: &str, servers_dir: &Path, saved: &[SavedServer]) -> String {
    (1..100)
        .map(|n| {
            if n == 1 {
                base.to_owned()
            } else {
                format!("{base}-{n}")
            }
        })
        .find(|name| {
            !servers_dir.join(name).exists() && !saved.iter().any(|server| &server.name == name)
        })
        .unwrap_or_else(|| format!("{base}-{}", std::process::id()))
}

/// The name this computer used for `address` before: a saved server, or an
/// onboarding that stopped before pairing.
fn known_name(storage: &Storage, servers_dir: &Path, address: &str) -> Option<String> {
    let saved = servers::load(storage).unwrap_or_default();
    if let Some(server) = saved.iter().find(|server| {
        server
            .ssh
            .as_ref()
            .is_some_and(|ssh| ssh.address == address)
    }) {
        return Some(server.name.clone());
    }
    fs::read_dir(servers_dir)
        .ok()?
        .flatten()
        .filter(|entry| {
            fs::read_to_string(entry.path().join(ADDRESS_FILE))
                .is_ok_and(|saved| saved.trim() == address)
        })
        .filter_map(|entry| entry.file_name().into_string().ok())
        .find(|name| slug(name) == *name)
}

fn servers_dir(storage: &Storage) -> Result<PathBuf> {
    Ok(storage
        .config
        .parent()
        .context("Cannot locate difu's configuration directory")?
        .join("servers"))
}

fn private_dir(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("Cannot create {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn has_host_key(known_hosts: &Path) -> bool {
    fs::metadata(known_hosts).is_ok_and(|metadata| metadata.len() > 0)
}

fn public_key_path(key: &Path) -> PathBuf {
    let mut path = key.as_os_str().to_owned();
    path.push(".pub");
    PathBuf::from(path)
}

/// `<dir>/id_ed25519`, generated once without a passphrase.
fn ensure_key(dir: &Path) -> Result<PathBuf> {
    let key = dir.join(KEY_FILE);
    if !key.exists() {
        let comment = match slug(&computer_name()) {
            computer if computer.is_empty() => "difu".to_owned(),
            computer => format!("difu@{computer}"),
        };
        let status = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", &comment, "-f"])
            .arg(&key)
            .stdin(Stdio::null())
            .status()
            .context("Cannot run ssh-keygen; install OpenSSH and try again")?;
        ensure!(
            status.success(),
            "ssh-keygen could not create {}",
            key.display()
        );
    }
    Ok(key)
}

fn read_public_key(key: &Path) -> Result<String> {
    let public = public_key_path(key);
    if !public.exists() {
        let output = Command::new("ssh-keygen")
            .arg("-y")
            .arg("-f")
            .arg(key)
            .stdin(Stdio::null())
            .output()
            .context("Cannot run ssh-keygen; install OpenSSH and try again")?;
        ensure!(
            output.status.success(),
            "Cannot read {}; delete it and try again",
            key.display()
        );
        fs::write(&public, &output.stdout)?;
    }
    let text = fs::read_to_string(&public)?;
    let line = text.trim();
    ensure!(
        line.starts_with("ssh-ed25519 ") && !line.contains(['\n', '\r']),
        "{} is not an ed25519 public key; delete it and the key next to it, then try again",
        public.display()
    );
    Ok(line.to_owned())
}

fn print_fingerprints(known_hosts: &Path) {
    let Ok(output) = Command::new("ssh-keygen")
        .arg("-l")
        .arg("-f")
        .arg(known_hosts)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return;
    };
    for fingerprint in fingerprints(&String::from_utf8_lossy(&output.stdout)) {
        note(&format!(
            "Host key {fingerprint}: first connection, so difu trusts it from now on and checks \
             it every time"
        ));
    }
}

/// `256 SHA256:… difu-server (ED25519)` lines from `ssh-keygen -l`.
fn fingerprints(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(
            |line| match line.split_whitespace().collect::<Vec<_>>().as_slice() {
                [_, fingerprint, .., kind] if fingerprint.starts_with("SHA256:") => {
                    Some(format!("{fingerprint} {kind}"))
                }
                _ => None,
            },
        )
        .collect()
}

fn parse_tailscale(stdout: &str) -> Result<(String, Option<String>)> {
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let ip = lines
        .next()
        .filter(|ip| ip.parse::<std::net::Ipv4Addr>().is_ok())
        .context("Tailscale on the server has no address yet; finish its login and try again")?;
    let dns = lines
        .next()
        .map(|name| name.trim_end_matches('.').to_owned())
        .filter(|name| !name.is_empty());
    Ok((ip.to_owned(), dns))
}

/// `{"link":"difu://pair?...","expires":...,"expiresIn":...}` from
/// `difu remote pair --json`: the link, and when it expires by this
/// computer's clock (from `expiresIn`, which older servers leave out).
fn pairing_answer(stdout: &str) -> Result<(String, Option<i64>)> {
    let line = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .context("`difu remote pair --json` printed no pairing link")?;
    let value: serde_json::Value =
        serde_json::from_str(line).context("`difu remote pair --json` printed invalid JSON")?;
    let link = value
        .get("link")
        .and_then(serde_json::Value::as_str)
        .filter(|link| link.starts_with("difu://"))
        .context("`difu remote pair --json` printed no pairing link")?;
    let expires = match value.get("expiresIn").and_then(serde_json::Value::as_i64) {
        Some(seconds) => Some(
            chrono::Utc::now()
                .timestamp_millis()
                .saturating_add(seconds.saturating_mul(1000)),
        ),
        None => value.get("expires").and_then(serde_json::Value::as_i64),
    };
    Ok((link.to_owned(), expires))
}

/// A phone pairing code for the server, printed here so its expiry is in this
/// computer's time; the server's own `difu remote pair` uses the server's
/// clock and time zone (often UTC).
fn show_pairing(difu: &Ssh) -> Result<()> {
    let output = difu.output("difu remote pair --json", None)?;
    ensure!(output.status.success(), "{}", stderr(&output));
    let (link, expires) = pairing_answer(&String::from_utf8_lossy(&output.stdout))?;
    let now = chrono::Utc::now().timestamp_millis();
    // Codes last 10 minutes when an older server says nothing.
    super::serve::print_pairing(&link, expires.unwrap_or(now + 10 * 60_000), now)
}

fn replace_root_password(root: &Ssh, name: &str) -> Result<()> {
    let output = root.output("passwd -S root", None)?;
    if !(output.status.success() && root_has_password(&String::from_utf8_lossy(&output.stdout))) {
        note("root has no password to replace");
        return Ok(());
    }
    let password = password::generate()?;
    let saved = save_root_password(name, &password);
    let changed = root.output("chpasswd", Some(format!("root:{password}\n").as_bytes()))?;
    if !changed.status.success() {
        warn(&format!(
            "The root password was not changed ({}); the old one still works.",
            stderr(&changed)
        ));
        return Ok(());
    }
    match saved {
        Ok(service) => {
            note(&format!(
                "Saved the new random root password in your Keychain as {service} (account \
                 root), for your provider's web console."
            ));
            note(&format!(
                "Show it with `security find-generic-password -s {service} -w`."
            ));
        }
        Err(_) => {
            note(&format!("New root password: {password}"));
            note("It is shown only this once; keep it for your provider's web console.");
        }
    }
    Ok(())
}

/// `passwd -S`: `root P …` when root has a usable password.
fn root_has_password(status: &str) -> bool {
    status.split_whitespace().nth(1) == Some("P")
}

/// Keychain item names may only hold letters, digits, dots, dashes and
/// underscores, so the item is `app.difu.server.<name>`, account `root`.
fn keychain_service(name: &str) -> String {
    format!("app.difu.server.{name}")
}

#[cfg(target_os = "macos")]
fn save_root_password(name: &str, password: &str) -> Result<String> {
    let service = keychain_service(name);
    crate::keychain::save(&service, "root", password.as_bytes())?;
    Ok(service)
}

#[cfg(not(target_os = "macos"))]
fn save_root_password(name: &str, password: &str) -> Result<String> {
    let _ = (keychain_service(name), password);
    bail!("No keychain on this system")
}

fn guided_login(
    difu: &Ssh,
    name: &str,
    tool: &str,
    status: &str,
    login: &str,
    later: &str,
) -> Result<()> {
    let logged_in = succeeds(difu, status);
    let question = if logged_in {
        format!("{tool} is already logged in on the server. Log in again?")
    } else {
        format!("Log in to {tool} on the server now?")
    };
    if !confirm(&question, !logged_in)? {
        return Ok(());
    }
    if !difu.interactive(Some(login))?.success() {
        warn(&format!(
            "The {tool} login did not finish. Later: `difu server ssh {name}`, then `{later}`."
        ));
    }
    Ok(())
}

fn claude_login(difu: &Ssh, root: &Ssh, name: &str) -> Result<()> {
    let has_token = succeeds(difu, CLAUDE_STATUS);
    let question = if has_token {
        "Claude Code already has a token on the server. Replace it?"
    } else {
        "Set up Claude Code on the server now?"
    };
    if !confirm(question, !has_token)? {
        return Ok(());
    }
    note("Run `claude setup-token` on this computer and paste the token it prints.");
    let token = loop {
        let token = secret("Claude Code token (hidden; Enter to skip):")?;
        if token.is_empty() {
            return Ok(());
        }
        if valid_token(&token) {
            break token;
        }
        note("That does not look like a token; copy the whole line `claude setup-token` printed.");
    };
    let stored = difu.output(CLAUDE_STORE, Some(format!("{token}\n").as_bytes()))?;
    if !stored.status.success() {
        warn(&format!(
            "The token was not saved ({}). Later: `difu server ssh {name}`, then add \
             CLAUDE_CODE_OAUTH_TOKEN=… to ~/.config/difu/server.env.",
            stderr(&stored)
        ));
        return Ok(());
    }
    // The service reads server.env when it starts.
    let restarted = root.output("systemctl restart difu-agent.service", None)?;
    if restarted.status.success() {
        note("Saved; difu-agent restarted to use it");
    } else {
        warn(&format!(
            "Saved, but difu-agent did not restart ({}); it uses the token after its next start.",
            stderr(&restarted)
        ));
    }
    Ok(())
}

/// Tokens go into a systemd environment file unquoted.
fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token
            .chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '\'' | '"' | '\\' | '$' | '`'))
}

fn succeeds(ssh: &Ssh, command: &str) -> bool {
    ssh.output(command, None)
        .is_ok_and(|output| output.status.success())
}

fn choose_server_password() -> Result<String> {
    loop {
        let typed = secret(&format!(
            "Server password (at least {} characters; Enter to generate one):",
            password::MINIMUM_LENGTH
        ))?;
        if typed.is_empty() {
            let generated = password::generate()?;
            note(&format!("Server password: {generated}"));
            note("It is shown only this once; save it in your password manager.");
            return Ok(generated);
        }
        if typed.chars().count() < password::MINIMUM_LENGTH {
            note(&format!(
                "Use at least {} characters.",
                password::MINIMUM_LENGTH
            ));
            continue;
        }
        if secret("Repeat it:")? != typed {
            note("The passwords did not match; try again.");
            continue;
        }
        return Ok(typed);
    }
}

fn access(storage: &Storage, name: &str) -> Result<SshAccess> {
    let server = servers::load(storage)?
        .into_iter()
        .find(|server| server.name == name)
        .with_context(|| {
            format!("No saved server is called {name}; `difu server list` shows them")
        })?;
    server.ssh.with_context(|| {
        format!(
            "This computer has no SSH access to {name}: it joined with the server password. \
             Run this on the computer that set the server up with `difu server add`"
        )
    })
}

/// This computer's name as shown in the server's paired devices.
fn computer_name() -> String {
    #[cfg(target_os = "macos")]
    if let Some(name) = Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
    {
        return name;
    }
    nix::unistd::gethostname()
        .ok()
        .and_then(|name| name.into_string().ok())
        .map(|name| name.trim_end_matches(".local").to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "difu".to_owned())
}

fn stderr(output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if text.is_empty() {
        format!("ssh exited with {}", output.status)
    } else {
        text
    }
}

struct Progress {
    step: usize,
    total: usize,
}

impl Progress {
    fn new(total: usize) -> Self {
        Self { step: 0, total }
    }

    fn next(&mut self, title: &str) {
        self.step += 1;
        println!("\n[{}/{}] {title}", self.step, self.total);
    }
}

fn note(text: &str) {
    println!("      {text}");
}

fn warn(text: &str) {
    println!("      Warning: {text}");
}

fn ask(question: &str) -> Result<String> {
    print!("      {question} ");
    io::stdout().flush()?;
    let mut line = String::new();
    ensure!(io::stdin().lock().read_line(&mut line)? > 0, "Cancelled");
    Ok(line.trim().to_owned())
}

fn confirm(question: &str, default: bool) -> Result<bool> {
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    loop {
        match ask(&format!("{question} {hint}"))?
            .to_ascii_lowercase()
            .as_str()
        {
            "" => return Ok(default),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => note("Please answer y or n."),
        }
    }
}

/// A line typed without echo. Ctrl+C or Ctrl+D cancels.
fn secret(prompt: &str) -> Result<String> {
    use crossterm::{
        event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
        terminal,
    };
    print!("      {prompt} ");
    io::stdout().flush()?;
    terminal::enable_raw_mode().context("Cannot read hidden input from this terminal")?;
    let typed = (|| -> Result<Option<String>> {
        let mut text = String::new();
        loop {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            let control = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Enter => return Ok(Some(text)),
                KeyCode::Char('c' | 'd') if control => return Ok(None),
                KeyCode::Char('u') if control => text.clear(),
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) if !control => text.push(c),
                _ => {}
            }
        }
    })();
    let restored = terminal::disable_raw_mode();
    println!();
    restored?;
    typed?.context("Cancelled")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options {
            address: "203.0.113.7".into(),
            name: None,
            user: "root".into(),
            ssh_port: 22,
            tailscale: false,
            sandbox: true,
        }
    }

    fn remote(known_hosts: &str) -> Remote {
        Remote {
            address: "203.0.113.7".into(),
            port: 2222,
            known_hosts: known_hosts.into(),
        }
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn has_option(args: &[String], option: &str) -> bool {
        args.windows(2)
            .any(|pair| matches!(pair, [flag, value] if flag == "-o" && value == option))
    }

    fn bootstrap(steps: Vec<Step>, public_key: &str) -> Bootstrap<'_> {
        Bootstrap {
            steps,
            target: "x86_64-unknown-linux-musl",
            public_key,
            ssh_port: 2222,
            tailscale: true,
            sandbox: false,
        }
    }

    /// One shell function from the bootstrap script, to run on its own.
    fn bootstrap_function(name: &str) -> Result<&'static str> {
        let start = BOOTSTRAP
            .find(&format!("\n{name}() {{"))
            .with_context(|| format!("no {name} in the bootstrap script"))?;
        BOOTSTRAP
            .get(start + 1..)
            .and_then(|rest| rest.find("\n}\n").and_then(|end| rest.get(..end + 3)))
            .with_context(|| format!("unterminated {name}"))
    }

    fn run_bash(script: &str, args: &[&str]) -> Result<Output> {
        let mut child = Command::new("bash")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .context("bash stdin")?
            .write_all(script.as_bytes())?;
        Ok(child.wait_with_output()?)
    }

    #[test]
    fn askpass_stays_out_of_normal_runs() {
        let mut out = Vec::new();
        assert!(askpass(None, &mut out).is_none());
        assert!(askpass(Some(OsString::new()), &mut out).is_none());
        assert!(out.is_empty());
    }

    #[test]
    fn askpass_prints_the_secret_file() -> Result<()> {
        let file = Askpass::new("pa ss'word$")?;
        let mut out = Vec::new();
        askpass(Some(file.0.path().into()), &mut out).context("askpass inactive")??;
        assert_eq!(out, b"pa ss'word$\n");
        let missing = file.0.path().with_extension("missing");
        assert!(matches!(
            askpass(Some(missing.into()), &mut Vec::new()),
            Some(Err(_))
        ));
        Ok(())
    }

    #[test]
    fn askpass_file_is_private_named_for_ssh_and_removed() -> Result<()> {
        let file = Askpass::new("hunter22")?;
        let path = file.0.path().to_owned();
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read_to_string(&path)?, "hunter22");
        let mut command = Command::new("ssh");
        file.apply(&mut command)?;
        let envs: Vec<(String, String)> = command
            .get_envs()
            .filter_map(|(key, value)| {
                Some((
                    key.to_string_lossy().into_owned(),
                    value?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert!(envs.contains(&("SSH_ASKPASS_REQUIRE".into(), "force".into())));
        assert!(envs.contains(&(
            ASKPASS_SECRET_FILE.into(),
            path.to_string_lossy().into_owned()
        )));
        let exe = std::env::current_exe()?;
        assert!(
            envs.iter()
                .any(|(key, value)| key == "SSH_ASKPASS" && Path::new(value) == exe)
        );
        drop(file);
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn password_logins_use_askpass_and_key_logins_do_not() -> Result<()> {
        let remote = remote("/tmp/known_hosts");
        let password = Login::Password("secret".into());
        let (command, askpass) =
            Ssh::new(&remote, "root", &password).command(false, Some("true"))?;
        let askpass = askpass.context("no askpass file")?;
        assert_eq!(fs::read_to_string(askpass.0.path())?, "secret");
        assert!(
            command
                .get_envs()
                .any(|(key, _)| key == std::ffi::OsStr::new(ASKPASS_SECRET_FILE))
        );
        let key = Login::Key("/tmp/id_ed25519".into());
        let (command, askpass) = Ssh::new(&remote, "root", &key).command(false, Some("true"))?;
        assert!(askpass.is_none());
        assert_eq!(command.get_envs().count(), 0);
        Ok(())
    }

    #[test]
    fn ssh_argv_pins_host_keys_and_ends_with_the_command() -> Result<()> {
        let remote = remote("/Users/a/Library/Application Support/difu/servers/box/known_hosts");
        let args =
            strings(&Ssh::new(&remote, "root", &Login::Agent).argv(false, Some("hostname"))?);
        assert_eq!(args.first().map(String::as_str), Some("-T"));
        assert!(has_option(
            &args,
            "UserKnownHostsFile=\"/Users/a/Library/Application Support/difu/servers/box/known_hosts\""
        ));
        for option in [
            "StrictHostKeyChecking=accept-new",
            "HostKeyAlias=difu-server",
            "BatchMode=yes",
            "ControlMaster=no",
            "ControlPath=none",
            "ForwardAgent=no",
        ] {
            assert!(has_option(&args, option), "{option}");
        }
        assert!(!args.contains(&"-i".to_owned()));
        let [
            ..,
            port_flag,
            port,
            user_flag,
            user,
            separator,
            address,
            command,
        ] = args.as_slice()
        else {
            bail!("short argv: {args:?}");
        };
        assert_eq!(
            [
                port_flag, port, user_flag, user, separator, address, command
            ],
            ["-p", "2222", "-l", "root", "--", "203.0.113.7", "hostname"]
        );
        Ok(())
    }

    #[test]
    fn ssh_argv_for_passwords_and_keys() -> Result<()> {
        let remote = remote("/tmp/box/known_hosts");
        let password = Login::Password("secret".into());
        let args = strings(&Ssh::new(&remote, "ubuntu", &password).argv(false, Some("true"))?);
        for option in [
            "PubkeyAuthentication=no",
            "BatchMode=no",
            "NumberOfPasswordPrompts=1",
        ] {
            assert!(has_option(&args, option), "{option}");
        }
        assert!(!args.iter().any(|arg| arg.contains("secret")));
        let key = Login::Key("/tmp/box dir/id_ed25519".into());
        let args = strings(&Ssh::new(&remote, "difu", &key).argv(true, None)?);
        assert_eq!(args.first().map(String::as_str), Some("-t"));
        assert!(args.windows(2).any(
            |pair| matches!(pair, [flag, path] if flag == "-i" && path == "/tmp/box dir/id_ed25519")
        ));
        assert!(has_option(&args, "IdentitiesOnly=yes"));
        assert!(has_option(&args, "BatchMode=yes"));
        // A shell has no command after the address.
        assert_eq!(args.last().map(String::as_str), Some("203.0.113.7"));
        Ok(())
    }

    #[test]
    fn ssh_refuses_paths_it_would_misread() {
        for path in ["/tmp/100%/known_hosts", "/tmp/a\"b/known_hosts"] {
            assert!(
                Ssh::new(&remote(path), "root", &Login::Agent)
                    .argv(false, None)
                    .is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn script_substitutes_every_parameter() -> Result<()> {
        let script = bootstrap(Step::setup(true), "ssh-ed25519 AAAAC3Nza difu@mac").render()?;
        assert!(!script.contains(PARAMETERS));
        let version = format!("DIFU_VERSION='{VERSION}'");
        for line in [
            "DIFU_STEPS='ssh packages firewall updates tailscale difu tools sandbox service'",
            &version,
            "DIFU_TARGET='x86_64-unknown-linux-musl'",
            "DIFU_PUBLIC_KEY='ssh-ed25519 AAAAC3Nza difu@mac'",
            "DIFU_PORT='3438'",
            "DIFU_SSH_PORT='2222'",
            "DIFU_TAILSCALE='1'",
            "DIFU_SANDBOX='0'",
        ] {
            assert!(script.lines().any(|candidate| candidate == line), "{line}");
        }
        assert!(script.contains("DIFU_UNIT='# Installed by `difu server add`"));
        for name in [
            "DIFU_STEPS",
            "DIFU_VERSION",
            "DIFU_TARGET",
            "DIFU_PUBLIC_KEY",
            "DIFU_PORT",
            "DIFU_SSH_PORT",
            "DIFU_TAILSCALE",
            "DIFU_SANDBOX",
            "DIFU_UNIT",
        ] {
            assert!(
                BOOTSTRAP.contains(&format!("${name}"))
                    || BOOTSTRAP.contains(&format!("${{{name}")),
                "{name} is never used"
            );
        }
        // Parameters come right after `set -Eeuo pipefail`, before any code.
        let set = script.find("set -Eeuo pipefail").context("no set")?;
        let steps = script.find("DIFU_STEPS=").context("no steps")?;
        assert!(set < steps && steps < script.find("say()").context("no say")?);
        Ok(())
    }

    #[test]
    fn script_parameters_survive_hostile_values() -> Result<()> {
        let hostile = "ssh-ed25519 AAAA x'; touch /tmp/difu-pwned; echo '$(id) `id` \\ \"";
        let parameters = bootstrap(vec![Step::Keys, Step::Difu], hostile).parameters();
        let output = run_bash(
            &format!(
                "set -eu\n{parameters}\n\
                 printf '%s\\0' \"$DIFU_PUBLIC_KEY\" \"$DIFU_STEPS\" \"$DIFU_UNIT\"\n"
            ),
            &["-s"],
        )?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let values: Vec<&[u8]> = output.stdout.split(|byte| *byte == 0).collect();
        let [key, steps, unit, rest] = values.as_slice() else {
            bail!("unexpected output {:?}", output.stdout);
        };
        assert_eq!(*key, hostile.as_bytes());
        assert_eq!(*steps, b"keys difu");
        assert_eq!(*unit, UNIT.as_bytes());
        assert!(rest.is_empty());
        Ok(())
    }

    #[test]
    fn rendered_scripts_are_valid_bash() -> Result<()> {
        for steps in [
            vec![Step::Keys],
            Step::setup(false),
            Step::setup(true),
            vec![Step::Lockdown],
            Step::upgrade(),
        ] {
            let script = bootstrap(steps, "ssh-ed25519 AAAA difu").render()?;
            let output = run_bash(&script, &["-n"])?;
            ensure!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    #[test]
    fn script_knows_every_step_and_unit_placeholders() -> Result<()> {
        let case = BOOTSTRAP
            .lines()
            .find(|line| line.trim_start().starts_with("keys |"))
            .context("no step list in the script")?;
        let known: Vec<&str> = case
            .split(|c: char| c == '|' || c == ')' || c.is_whitespace())
            .filter(|name| !name.is_empty())
            .collect();
        for step in [
            Step::Keys,
            Step::Ssh,
            Step::Packages,
            Step::Firewall,
            Step::Updates,
            Step::Tailscale,
            Step::Lockdown,
            Step::Difu,
            Step::Tools,
            Step::Sandbox,
            Step::Image,
            Step::Service,
        ] {
            assert!(
                BOOTSTRAP.contains(&format!("step_{}() {{", step.name())),
                "{step:?}"
            );
            assert!(known.contains(&step.name()), "{step:?}");
        }
        assert_eq!(Step::setup(false).first(), Some(&Step::Ssh));
        assert!(!Step::setup(true).contains(&Step::Lockdown));
        assert!(Step::setup(true).contains(&Step::Tailscale));
        assert!(!Step::setup(false).contains(&Step::Tailscale));
        for line in [
            "User=difu",
            "ExecStart=/usr/local/bin/difu serve",
            "KillMode=mixed",
            "Restart=always",
            "Environment=XDG_RUNTIME_DIR=/run/user/@UID@",
            "Environment=CLAUDE_CONFIG_DIR=@HOME@/.claude",
            "EnvironmentFile=-@HOME@/.config/difu/server.env",
        ] {
            assert!(UNIT.lines().any(|candidate| candidate == line), "{line}");
        }
        Ok(())
    }

    #[test]
    fn upgrades_refresh_the_tools_image_and_unit_without_touching_settings() -> Result<()> {
        assert_eq!(
            Step::upgrade(),
            [Step::Difu, Step::Tools, Step::Image, Step::Service]
        );
        // The sandbox step writes server.sandbox.enabled; upgrades keep it.
        assert!(!Step::upgrade().contains(&Step::Sandbox));
        assert!(!Step::setup(true).contains(&Step::Image));
        let script = bootstrap(Step::upgrade(), "").render()?;
        assert!(
            script
                .lines()
                .any(|line| line == "DIFU_STEPS='difu tools image service'"),
            "{script}"
        );
        // The service step installs the current unit, then restarts it.
        let service = BOOTSTRAP
            .split("step_service() {")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .context("no service step")?;
        for part in [
            "/etc/systemd/system/difu-agent.service",
            "daemon-reload",
            "restart_service",
        ] {
            assert!(service.contains(part), "{part}");
        }
        Ok(())
    }

    #[test]
    fn the_image_step_follows_the_servers_sandbox_setting() -> Result<()> {
        let jq = Command::new("jq")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !jq.is_ok_and(|status| status.success()) {
            eprintln!("Skipping the sandbox setting test: jq is not installed");
            return Ok(());
        }
        let body = bootstrap_function("sandbox_enabled")?;
        let directory = tempfile::tempdir()?;
        let config = directory.path().join("config.json");
        for (contents, enabled) in [
            (None, false),
            (Some(""), false),
            (Some("{}"), false),
            (Some("not json"), false),
            (Some(r#"{"server":{"sandbox":{"enabled":false}}}"#), false),
            (
                Some(r#"{"server":{"enabled":true,"sandbox":{"enabled":true}}}"#),
                true,
            ),
        ] {
            match contents {
                Some(contents) => fs::write(&config, contents)?,
                None => {
                    let _ = fs::remove_file(&config);
                }
            }
            let output = run_bash(
                &format!("{body}sandbox_enabled \"$1\"\n"),
                &["-s", &config.to_string_lossy()],
            )?;
            assert_eq!(output.status.success(), enabled, "{contents:?}");
        }
        Ok(())
    }

    #[test]
    fn ssh_logins_share_the_services_claude_code_config_once() -> Result<()> {
        let body = bootstrap_function("share_claude_config")?;
        let home = tempfile::tempdir()?;
        fs::write(home.path().join(".profile"), "PATH=\"$HOME/bin:$PATH\"\n")?;
        for _ in 0..2 {
            let output = run_bash(
                &format!("set -eu\nas_difu() {{ \"$@\"; }}\n{body}share_claude_config \"$1\"\n"),
                &["-s", &home.path().to_string_lossy()],
            )?;
            ensure!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let profile = fs::read_to_string(home.path().join(".profile"))?;
        assert!(profile.starts_with("PATH="), "{profile}");
        assert_eq!(
            profile
                .lines()
                .filter(|line| *line == "export CLAUDE_CONFIG_DIR=\"$HOME/.claude\"")
                .count(),
            1,
            "{profile}"
        );
        Ok(())
    }

    #[test]
    fn setup_checks_survive_producers_that_die_of_sigpipe() -> Result<()> {
        // Like a real `sshd -T`, the stand-ins keep writing after the line
        // the check looks for, far more than a pipe buffers.
        let sshd = |answer: &str| {
            format!(
                "sshd() {{ printf 'port 22\\npasswordauthentication {answer}\\n'; seq 1 200000; }}\n"
            )
        };
        let run = |script: String| run_bash(&format!("set -Eeuo pipefail\n{script}"), &["-s"]);
        // The old check: grep -q exits at the match, sshd dies of SIGPIPE and
        // pipefail fails the pipeline, so the warning showed on every server.
        let naive = run(format!(
            "{}sshd -T 2>/dev/null | grep -qx 'passwordauthentication no'\n",
            sshd("no")
        ))?;
        assert!(!naive.status.success(), "the SIGPIPE was not reproduced");
        let check = bootstrap_function("password_logins_off")?;
        for (stand_in, off) in [
            (sshd("no"), true),
            (sshd("yes"), false),
            ("sshd() { return 1; }\n".to_owned(), false),
        ] {
            let output = run(format!("{stand_in}{check}password_logins_off\n"))?;
            assert_eq!(output.status.success(), off, "{stand_in}");
        }
        let groups = bootstrap_function("in_group")?;
        let id = "id() { printf 'difu sudo'; seq -f ' group%g' 1 200000 | tr -d '\\n'; printf '\\n'; }\n";
        for (group, member) in [
            ("sudo", true),
            ("difu", true),
            ("sud", false),
            ("wheel", false),
        ] {
            let output = run(format!("{id}{groups}in_group difu {group}\n"))?;
            assert_eq!(output.status.success(), member, "{group}");
        }
        // No other check pipes into a reader that stops early.
        for line in BOOTSTRAP.lines().map(str::trim) {
            assert!(
                line.starts_with('#') || !(line.contains("| grep -q") || line.contains("| head")),
                "{line}"
            );
        }
        Ok(())
    }

    #[test]
    fn claude_code_in_the_setup_shares_the_config_only_once_it_moved() -> Result<()> {
        let body = bootstrap_function("as_claude")?;
        let home = tempfile::tempdir()?;
        let home_path = home.path().to_string_lossy().into_owned();
        let run = || -> Result<String> {
            let output = run_bash(
                &format!(
                    "set -eu\nunset CLAUDE_CONFIG_DIR\ntest_home=$1\ndifu_home() {{ printf '%s' \"$test_home\"; }}\n\
                     as_difu() {{ \"$@\"; }}\n{body}\
                     as_claude sh -c 'printf %s \"${{CLAUDE_CONFIG_DIR:-unset}}\"'\n"
                ),
                &["-s", &home_path],
            )?;
            ensure!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        };
        // Before the move, ~/.claude.json is the real file.
        fs::write(home.path().join(".claude.json"), "{}")?;
        assert_eq!(run()?, "unset");
        fs::remove_file(home.path().join(".claude.json"))?;
        std::os::unix::fs::symlink(".claude/.claude.json", home.path().join(".claude.json"))?;
        assert_eq!(run()?, format!("{home_path}/.claude"));
        Ok(())
    }

    #[test]
    fn slugs_names_from_host_names() {
        assert_eq!(slug("Ubuntu-4GB-HEL1-1"), "ubuntu-4gb-hel1-1");
        assert_eq!(slug("  My  Server!! "), "my-server");
        assert_eq!(slug("Café_box"), "caf-box");
        assert_eq!(slug("---"), "");
        assert_eq!(slug(&format!("{}-tail", "a".repeat(39))), "a".repeat(39));
        assert_eq!(slug(&"b".repeat(60)).len(), 40);
        assert_eq!(
            name_for_host("ip-172-31-1-2.ec2.internal\n", "203.0.113.7"),
            "ip-172-31-1-2"
        );
        assert_eq!(name_for_host("10.0.0.5", "203.0.113.7"), "10-0-0-5");
        assert_eq!(name_for_host("", "203.0.113.7"), "203-0-113-7");
        assert_eq!(name_for_host("", "::"), "server");
    }

    #[test]
    fn chosen_and_unused_names() -> Result<()> {
        assert_eq!(chosen_name("Dev Box")?, "dev-box");
        assert!(chosen_name("!!!").is_err());
        let directory = tempfile::tempdir()?;
        fs::create_dir(directory.path().join("box"))?;
        let saved = SavedServer {
            name: "box-2".into(),
            id: "id".into(),
            hosts: Vec::new(),
            port: DEFAULT_PORT,
            fingerprint: "f".into(),
            device: "d".into(),
            token: "t".into(),
            ssh: None,
        };
        assert_eq!(unused_name("box", directory.path(), &[saved]), "box-3");
        assert_eq!(unused_name("other", directory.path(), &[]), "other");
        Ok(())
    }

    #[test]
    fn server_files_live_under_the_config_directory() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let storage = Storage {
            config: directory.path().join("config.json"),
            cache: directory.path().join("cache"),
        };
        let servers = servers_dir(&storage)?;
        assert_eq!(servers, directory.path().join("servers"));
        let dir = servers.join("box");
        private_dir(&dir)?;
        assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o700);
        let key = dir.join(KEY_FILE);
        assert_eq!(key, directory.path().join("servers/box/id_ed25519"));
        assert_eq!(
            public_key_path(&key),
            directory.path().join("servers/box/id_ed25519.pub")
        );
        let access = SshAccess {
            address: "203.0.113.7".into(),
            port: 22,
            key,
        };
        assert_eq!(
            Remote::saved(&access).known_hosts,
            directory.path().join("servers/box/known_hosts")
        );
        assert_eq!(known_name(&storage, &servers, "203.0.113.7"), None);
        fs::write(dir.join(ADDRESS_FILE), "203.0.113.7\n")?;
        assert_eq!(
            known_name(&storage, &servers, "203.0.113.7"),
            Some("box".into())
        );
        assert_eq!(known_name(&storage, &servers, "203.0.113.8"), None);
        Ok(())
    }

    #[test]
    fn generates_one_key_per_server() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let key = ensure_key(directory.path())?;
        let public = read_public_key(&key)?;
        assert!(public.starts_with("ssh-ed25519 "));
        assert_eq!(fs::metadata(&key)?.permissions().mode() & 0o077, 0);
        assert_eq!(ensure_key(directory.path())?, key);
        assert_eq!(read_public_key(&key)?, public);
        fs::remove_file(public_key_path(&key))?;
        assert_eq!(
            read_public_key(&key)?.split_whitespace().nth(1),
            public.split_whitespace().nth(1)
        );
        Ok(())
    }

    #[test]
    fn destinations_reject_option_like_addresses() -> Result<()> {
        assert_eq!(
            destination(" 203.0.113.7 ", "root")?,
            ("root".into(), "203.0.113.7".into())
        );
        assert_eq!(
            destination("ubuntu@ec2.example.com", "root")?,
            ("ubuntu".into(), "ec2.example.com".into())
        );
        assert_eq!(
            destination("[2001:db8::1]", "root")?,
            ("root".into(), "2001:db8::1".into())
        );
        for address in ["", "-oProxyCommand=sh", "a b", "host;id", "root@"] {
            assert!(destination(address, "root").is_err(), "{address}");
        }
        for user in ["Root", "-l", "a b", ""] {
            assert!(destination("203.0.113.7", user).is_err(), "{user}");
        }
        Ok(())
    }

    #[test]
    fn parses_probe_answers_and_targets() -> Result<()> {
        assert_eq!(
            parse_facts("Welcome!\nbox\nx86_64\nroot\n")?,
            Facts {
                hostname: "box".into(),
                arch: "x86_64".into(),
                root: true
            }
        );
        assert!(!parse_facts("box\naarch64\nuser")?.root);
        assert!(parse_facts("box\n").is_err());
        assert_eq!(release_target("x86_64\n")?, "x86_64-unknown-linux-musl");
        assert_eq!(release_target("aarch64")?, "aarch64-unknown-linux-musl");
        assert!(release_target("riscv64").is_err());
        Ok(())
    }

    #[test]
    fn classifies_ssh_failures() {
        assert_eq!(
            classify("root@203.0.113.7: Permission denied (publickey,password)."),
            Failure::Credentials
        );
        assert_eq!(classify("Host key verification failed."), Failure::HostKey);
        assert_eq!(
            classify("ssh: connect to host 203.0.113.7 port 22: Connection refused"),
            Failure::Connection
        );
    }

    #[test]
    fn spots_servers_that_refuse_passwords() {
        assert!(accepts_passwords(
            "root@203.0.113.7: Permission denied (publickey,password)."
        ));
        assert!(accepts_passwords(
            "Permission denied (publickey,keyboard-interactive)."
        ));
        assert!(!accepts_passwords(
            "ubuntu@203.0.113.7: Permission denied (publickey)."
        ));
        assert!(accepts_passwords("Too many authentication failures"));
    }

    #[test]
    fn claude_token_store_replaces_only_its_line_privately() -> Result<()> {
        let home = tempfile::tempdir()?;
        let config = home.path().join(".config/difu");
        fs::create_dir_all(&config)?;
        let env = config.join("server.env");
        fs::write(&env, "OTHER=1\nCLAUDE_CODE_OAUTH_TOKEN=old\n")?;
        for token in ["sk-ant-oat01-first", "sk-ant-oat01-second"] {
            let mut child = Command::new("bash")
                .args(["-c", CLAUDE_STORE])
                .env("HOME", home.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()?;
            child
                .stdin
                .take()
                .context("bash stdin")?
                .write_all(format!("{token}\n").as_bytes())?;
            let output = child.wait_with_output()?;
            ensure!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(
            fs::read_to_string(&env)?,
            "OTHER=1\nCLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-second\n"
        );
        assert_eq!(fs::metadata(&env)?.permissions().mode() & 0o077, 0);
        Ok(())
    }

    #[test]
    fn parses_fingerprints_tailscale_and_pairing_answers() -> Result<()> {
        assert_eq!(
            fingerprints("256 SHA256:abcDEF difu-server (ED25519)\nnoise\n"),
            ["SHA256:abcDEF (ED25519)"]
        );
        assert_eq!(
            parse_tailscale("100.101.102.103\nbox.tail1234.ts.net.\n")?,
            ("100.101.102.103".into(), Some("box.tail1234.ts.net".into()))
        );
        assert_eq!(
            parse_tailscale("100.101.102.103\n")?,
            ("100.101.102.103".into(), None)
        );
        assert!(parse_tailscale("\n").is_err());
        // An older server: only the expiry by its own clock.
        assert_eq!(
            pairing_answer("note\n{\"link\":\"difu://pair?c=1\",\"expires\":1790000000000}\n")?,
            ("difu://pair?c=1".to_owned(), Some(1_790_000_000_000))
        );
        // `expiresIn` counts from now on this computer, whatever the server's clock.
        let before = chrono::Utc::now().timestamp_millis();
        let (_, expires) =
            pairing_answer("{\"link\":\"difu://pair?c=1\",\"expires\":1,\"expiresIn\":600}")?;
        let expires = expires.context("expiry")?;
        assert!(expires >= before + 600_000, "{expires}");
        assert!(expires <= chrono::Utc::now().timestamp_millis() + 600_000);
        assert!(pairing_answer("{\"link\":\"https://example.com\"}").is_err());
        assert!(pairing_answer("nothing").is_err());
        Ok(())
    }

    #[test]
    fn commands_quote_what_they_pass_on() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(
            remote_config("aws", &["203.0.113.7".into(), "box.ts.net".into()]),
            "difu remote config --name 'aws' --host '203.0.113.7' --host 'box.ts.net'"
        );
        let mut custom = options();
        assert_eq!(
            retry_command(&custom, "root", "203.0.113.7", "box"),
            "difu server add 203.0.113.7 --name box"
        );
        custom.ssh_port = 2222;
        custom.tailscale = true;
        custom.sandbox = false;
        assert_eq!(
            retry_command(&custom, "ubuntu", "203.0.113.7", "box"),
            "difu server add 203.0.113.7 --name box --user ubuntu --ssh-port 2222 --tailscale \
             --no-sandbox"
        );
    }

    #[test]
    fn root_password_and_token_checks() {
        assert!(root_has_password("root P 01/01/2024 0 99999 7 -1"));
        assert!(!root_has_password("root L 01/01/2024 0 99999 7 -1"));
        assert!(!root_has_password("root NP"));
        assert!(!root_has_password(""));
        assert_eq!(keychain_service("box"), "app.difu.server.box");
        assert!(valid_token("sk-ant-oat01-AbC_d.e~f+g/h="));
        for token in [
            "",
            "with space",
            "quote'",
            "dollar$",
            "back\\slash",
            "new\nline",
        ] {
            assert!(!valid_token(token), "{token:?}");
        }
    }
}
