# Cloud agents

difu can run on a Linux server you rent (Hetzner, EC2, a home box). Its coding
agents keep working when your laptop is closed, and any number of clients (the
difu terminal app on a Mac, the iOS app) connect to any number of servers.

This document is the design and the wire contract. The phone protocol it extends
is in [remote-protocol.md](remote-protocol.md).

## Shape

```
 Mac (difu TUI) ─┐                         ┌─ server A: difu serve ─ sandboxes (one per session)
                 ├── TLS WebSocket, pinned ┤
 iPhone (app) ───┘   certificate + tokens  └─ server B: difu serve ─ sandboxes
```

* **One difu service per server.** `difu serve` runs the normal agent service in
  the foreground (systemd), with remote control always on and *server mode*
  enabled. Each server is self-contained: its own Codex/Claude/GitHub logins,
  repositories, repository rules, sessions, sandboxes and paired devices.
* **Clients connect directly to each server.** A client keeps a list of servers
  (name, hosts, port, certificate fingerprint, device token). The Mac's local
  difu service holds these connections and merges server sessions into the
  sidebar; the iPhone connects to every paired server while in the foreground.
* **One sandbox per session.** On servers with sandboxing enabled, every
  session's agent (Claude Code or Codex app-server) and its shells run in a
  rootless Podman container. difu itself stays on the host and drives the agent
  over stdio exactly as today; difu's own tools travel over that same stdio
  stream, so a sandbox needs no channel back to the service.

Everything here is opt-in: nothing changes for a difu installation until you run
`difu serve`, `difu server add` or `difu server join`.

## Commands

Client side (your Mac):

| Command | What it does |
| --- | --- |
| `difu server add <ip> [--name N] [--user root] [--ssh-port 22] [--tailscale] [--no-sandbox]` | One-time onboarding with the root password (or your SSH key). See *Onboarding*. |
| `difu server join <host[:port]> [--name N]` | Pair this computer with an already configured server using its server password. No onboarding. |
| `difu server list` | Saved servers and whether they are reachable. |
| `difu server remove <name>` | Revoke this computer on the server (when reachable) and forget it locally. |
| `difu server ssh <name>` | Open a shell on the server as the `difu` user. |
| `difu server pair <name>` | Show a phone pairing QR for that server in this terminal. |
| `difu server upgrade <name>` | Install this computer's difu version on the server and restart it. |

Server side (run on the server, usually over SSH):

| Command | What it does |
| --- | --- |
| `difu serve [--port P]` | Run the agent service in the foreground with server mode and remote control on. Default port **3438**. |
| `difu remote pair [--json]` | Print a pairing QR and `difu://pair` link from the running service. |
| `difu remote devices` | List paired devices with access level and last seen. |
| `difu remote forget <device>` | Revoke a device. |
| `difu remote password [--generate] [--disable]` | Set the server password (prompt or stdin), generate a strong one, or turn password pairing off. |
| `difu remote config [--host H]... [--clear-hosts] [--port P]` | Addresses to advertise to clients (public IP, DNS or Tailscale name) and a fixed port. |
| `difu repos [list]` / `difu repos add [owner/name]` / `difu repos remove owner/name` / `difu repos refresh [owner/name]` | Server repositories. `add` without a name opens a picker built from `gh repo list`. |
| `difu sandbox build` / `difu sandbox status` | Build the sandbox image / show runtime, image and running sandboxes. |

## Onboarding (`difu server add`)

1. Try SSH with your existing keys (`BatchMode=yes`). Otherwise ask for the root
   password once and feed it to the system `ssh` through `SSH_ASKPASS`
   (`SSH_ASKPASS_REQUIRE=force`, difu itself acting as the askpass helper with the
   secret in a 0600 temp file). No `sshpass`, no SSH library.
2. Generate a per-server ed25519 key on the Mac and install it for `root` and a
   new `difu` user (no password, no sudo). Host keys are pinned in a per-server
   `known_hosts` under `<config>/servers/<name>/` (`HostKeyAlias`, so the pin
   survives a move to the Tailscale address), and difu checks that its key works
   before turning password logins off.
3. Harden: `PasswordAuthentication no`, `PermitRootLogin prohibit-password`,
   firewall allowing only SSH and difu's port (or only Tailscale with
   `--tailscale`), automatic security updates.
4. Install git, gh, podman, the matching difu Linux release binary, the Codex and
   Claude Code CLIs; build the sandbox image; install and start
   `difu-agent.service` (`difu serve` as `difu`, lingering enabled for rootless
   Podman).
5. Replace the root password (when the root account has one) with a random one
   saved in the Mac's Keychain as service `app.difu.server.<name>`, account
   `root`, for the provider's web console.
6. Guided logins on the server, interactive over `ssh -t`: `gh auth login`,
   `codex login --device-auth`, and a Claude Code token (`claude setup-token`
   pasted into the server's 0600 `server.env`).
7. Choose the **server password** (or generate one) — the single password new
   clients use with the server's address.
8. Pick server repositories from the server's `gh repo list`; clone them.
9. Advertise the address used to connect (`difu remote config --host <ip>`), pair
   this Mac as a **full** device, save the server, and print a phone pairing QR.

## Repositories

* `config.server.repositories` lists `owner/name` entries. Each has one base
  clone at `<data>/repos/<owner>/<name>` made with `gh repo clone`.
* The service fetches every server repository in the background every
  `refresh_minutes` (default 10) and on `difu repos refresh` / the
  `server.repos.refresh` method. Base clones never auto-prune.
* Sessions use normal difu worktrees from the base clone, so they start without a
  network clone. Launching on an unlisted `owner/name` adds and clones it first.
  Sessions start from `origin/main`, as on a Mac; choosing another base branch is
  not supported yet.
* Repository rules keep their existing storage (`config.repository_rules`, keyed
  by the repository root). On a server the root is the base clone, so rules are
  effectively keyed by `owner/name`, live on the server, and every connected
  client reads and edits the same copy. Running sessions keep the copy they
  started with.

## Server mode instructions

When `config.server.enabled` is true, coding sessions get this instead of the
local "do not run tests" rule:

> You are running on a difu cloud server inside an isolated sandbox. You may run
> builds, linters and tests, but run only the tests relevant to your change (the
> affected package, module or test file). In large repositories do not run the
> whole test suite when it would take a long time; leave full validation to
> pull-request CI. Repository rules take precedence over this guidance.

Permission prompts are unchanged.

## Sandboxes

* Runtime: rootless Podman. One **pod** per session (`difu-<session id>`), so the
  agent and its shells share one network namespace (their own ports) while
  staying isolated from other sessions.
* Each agent process or shell runs as `podman run --rm -i --pod difu-<id>` (shells
  with a TTY) in the sandbox image, as container root (rootless Podman maps it to
  the `difu` user), with:
  * the session worktree and its base repository's `.git` mounted at the same
    absolute paths (worktree metadata keeps working; commits and pushes happen in
    the sandbox);
  * the server's logins mounted (`~/.codex`, `~/.claude`, `~/.claude.json`,
    `~/.config/gh`), and `~/.gitconfig` plus the host's CLI binaries
    (`~/.local/bin`, Claude's versions directory) mounted read-only;
  * secrets passed by variable name only, so they never appear in `ps`.
* CPU and memory limits from `config.server.sandbox` apply to the whole pod, one
  budget per session.
* Codex starts with `sandbox_mode="danger-full-access"` inside a sandbox: the
  container is the sandbox, and Codex's own bubblewrap sandbox cannot work under
  rootless Podman's AppArmor profile. Approval policy is unchanged.
* A guard removes the container when difu stops the process (difu kills process
  groups with SIGKILL, which `podman run` cannot forward). Deleting, clearing or
  cleaning up a session removes its pod, and leftover containers are swept when
  the service starts.
* `difu sandbox build` builds `localhost/difu-sandbox:latest` from a bundled
  Containerfile (Ubuntu LTS with git, gh, build tools, Python, Node, ripgrep).
* Sessions are always worktrees, exactly as on a local difu. Worktrees share the
  base clone's object store and refs, so a sandbox could rewrite another session's
  branch refs (not its files); this is accepted, as it is locally.

## Wire protocol additions

All additions are backwards compatible with protocol 1: unknown fields are
ignored and old clients never send the new messages.

### Default port

A server-mode service listens on **3438** unless configured otherwise. Clients
that are given only a host use 3438; `host:port` overrides it.

### Access levels

Every device has `access`: `"standard"` (the phone API) or `"full"` (also the
`rpc` methods below). Devices paired before this change are `standard`. A client
asks for a level when pairing; QR and password pairing both grant the requested
level. `welcome` reports it: `"access": "full"`.

### `pair` (QR) additions

```json
{"type":"pair","code":"...","device":{"id":"...","name":"..."},"access":"full"}
```

`access` is optional (default `standard`).

### Password pairing

Used when a client has only an address and the server password. The client does
not know the certificate fingerprint yet; the password proves both sides and
binds the certificate, so a man in the middle can neither impersonate the server
nor complete pairing.

1. Client → server

   ```json
   {"type":"password","device":{"id":"...","name":"..."},"access":"standard","nonce":"<cn>"}
   ```

2. Server → client (or `denied` when password pairing is off, the device is
   rate-limited, or no password is set)

   ```json
   {"type":"challenge","salt":"<salt>","iterations":600000,"nonce":"<sn>"}
   ```

3. Client derives `K = PBKDF2-HMAC-SHA256(password UTF-8, base64url-decode(salt), iterations, 32 bytes)`
   and sends

   ```json
   {"type":"proof","proof":"<base64url(HMAC-SHA256(K, \"difu-pair-v1|client|\" + F + \"|\" + sn + \"|\" + cn))>"}
   ```

   `F` is the fingerprint of the certificate the client actually received:
   base64url without padding of SHA-256 over the leaf certificate DER (the same
   format as the QR's `f`). `sn` and `cn` are the nonce strings exactly as sent
   (base64url of 32 random bytes).

4. Server verifies with its own fingerprint, then answers `welcome` (with
   `token`, as after QR pairing) plus

   ```json
   "proof": "<base64url(HMAC-SHA256(K, \"difu-pair-v1|server|\" + F + \"|\" + sn + \"|\" + cn))>"
   ```

   The client must verify `proof` before trusting the server, then pins `F` and
   stores the token. A wrong password gets `denied`.

Rate limits: failed pairing attempts (QR code or password) are limited per client
address (IPv6 per /64) and wrong passwords globally; while limited, pairing is
denied without checking. Token authentication is never blocked, and invalid
tokens do not count, so a forgotten phone retrying its old token cannot lock its
owner out. The server stores only the verifier
`{salt, iterations, key}`, never the password. Passwords must be at least 12
characters.

### `welcome` additions

```json
{"type":"welcome", "...": "...", "access":"full", "proof":"... (password pairing only)",
 "server": {"...": "...", "id":"<fingerprint>", "mode":"server", "passwordPairing": true}}
```

`mode` is `"server"` for `difu serve`, `"computer"` otherwise. Clients key saved
servers by `server.id`.

### Methods for every paired device

| Method | Params | Result |
| --- | --- | --- |
| `device.unpair` | – | `{}`; revokes the calling device, then the server closes the socket. Clients call this from "Forget". |
| `server.devices` | – | `{"devices":[{"id","name","access","paired","seen","connected","current"}]}` |
| `server.devices.forget` | `{"device"}` | `{}`; revokes another device. |
| `server.repos` | – | `{"repositories":[{"name":"owner/repo","path","cloned","fetchedAt","error"}],"refreshMinutes":10}` |
| `server.repos.available` | – | `{"repositories":[{"name":"owner/repo"}]}` from the server's `gh`. |
| `server.repos.add` | `{"name"}` | `{}`; clones in the background, progress visible in `server.repos`. |
| `server.repos.remove` | `{"name"}` | `{}`; stops listing it (the clone stays while sessions use it). |
| `server.repos.refresh` | `{"name"?}` | `{}`; fetches one or all. |
| `server.rules.get` | `{"repository"}` | `{"rules":"..."}`; `repository` is `owner/name` or a path from `agents.repositories`. |
| `server.rules.set` | `{"repository","rules"}` | `{}` |
| `server.logins` | – | `{"logins":[{"tool":"github"\|"codex"\|"claude","loggedIn":true,"account":"...","detail":"..."}]}` |
| `server.login.start` | `{"tool":"github"\|"codex"}` | `{"url","code","message"}` for a device-code login that finishes in the background; poll `server.logins`. |
| `server.login.token` | `{"tool":"claude","token"}` | `{}`; stores a `claude setup-token` token for the service. |
| `server.logout` | `{"tool"}` | `{}` |

`server.repos*` and `server.login*`/`server.logout*` require server mode, so a
phone paired with a Mac cannot change the Mac's logins; `server.rules.*` work
everywhere. `agents.repositories` on a server returns the server repositories
(`name` is `owner/name`). `agents.new` accepts `repository` as a path or
`owner/name` (an unlisted name is added and cloned first); `base` is reserved and
currently ignored.

Event to every other connected device after a pairing:

```json
{"event":"device.paired","data":{"id":"...","name":"Ali's iPad","access":"standard","paired":1790000000000}}
```

### Methods for `full` devices

`rpc` carries difu's internal service protocol (the `Request`/`Reply` JSON the
terminal app sends over the local Unix socket), so a Mac gets every terminal
feature without a second API.

| Method | Params | Result |
| --- | --- | --- |
| `rpc` | `{"request": Request}` | `{"reply": Reply}` |
| `rpc.watch` | `{"id","version"?}` | `{"stream": n}`, then events `{"event":"rpc.watch","data":{"stream":n,"reply":Reply}}` until `rpc.unwatch` or `{"event":"rpc.watch.closed","data":{"stream":n,"message":"..."}}` |
| `rpc.unwatch` | `{"stream"}` | `{}` |

`standard` devices get an error for these. A service error comes back as the
call's `error`; `Request::Watch` must use `rpc.watch`. Clients must check that the
server's `version` equals their own before using `rpc` (`difu server upgrade`
fixes a mismatch).

## Clients

### Mac

* Saved servers live in `<config>/servers.json` (0600): name, server id, hosts,
  port, fingerprint, device id and token.
* The local agent service connects to every saved server as a `full` device and
  merges their sessions into `Request::List` replies. Server session ids are
  namespaced as `<server name>:<session id>` and summaries carry `server` for a
  sidebar badge. Requests for a namespaced id are forwarded with `rpc`; watches
  with `rpc.watch`.
* The new-session flow offers a server picker (This Mac + saved servers);
  repositories come from the chosen server.
* Features that need the session's files on this machine (attachments, local
  file paths, HTML artifact previews) say they are unavailable for server
  sessions.
* The phone API of a Mac hides relayed server sessions, because phones connect to
  servers directly.

### iPhone

* Saved servers: a Keychain array keyed by server id (migrating the single saved
  computer).
* Add Server: scan QR, paste link, or type address + server password (password
  pairing, `standard` access).
* While active, connect to every saved server; one combined session list with a
  server badge; New Session picks a server, then its repositories.
* Per-server settings: logins (status, device-code login, Claude token), server
  repositories (add from `server.repos.available`, remove, refresh mains),
  repository rules, paired devices (revoke).
* Face ID / passcode app lock when opening and after 5 minutes in the background.
* "Forget" calls `device.unpair` before deleting the credential.
* `device.paired` events show a banner with a Revoke action.

## Security summary

* Access needs a device token (256-bit random, stored hashed) or a pairing secret
  (single-use 128-bit QR code, or the server password with the proof above).
* SSH is key-only after onboarding; the `difu` user has no sudo.
* Agents run in rootless containers with only the session's worktree and the
  server logins; standard devices cannot reach `rpc`.
* Revocation works from any paired client and from `difu remote forget`.
