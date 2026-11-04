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
  A server never stops accepting its clients: turning remote control off is
  refused from every path (`difu remote`, `rpc` from a full device, the
  terminal app's `/remote-control` on the server) with a message pointing at
  `difu remote forget`, `difu remote password --disable` or
  `systemctl stop difu-agent`.
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
| `difu server list` | Saved servers, whether they are reachable, and each one's free disk space. |
| `difu server remove <name>` | Revoke this computer on the server (when reachable) and forget it locally. |
| `difu server ssh <name>` | Open a shell on the server as the `difu` user. |
| `difu server pair <name>` | Show a phone pairing QR for that server in this terminal. |
| `difu server upgrade <name>` | Install this computer's difu version on the server, update Codex and Claude Code, rebuild the sandbox image when sandboxing is on, refresh `difu-agent.service` and restart it. Safe to repeat. |
| `difu session move <session> <server>` | Move a session on this computer to a saved server; prints the new `<server>:<id>`. See *Moving a session to a server*. |

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
| `difu sandbox build` / `difu sandbox status` | Build (or pull) the configured sandbox image / show runtime, image and running sandboxes. |
| `difu disk [status [--refresh]]` / `difu disk clean <archived-worktrees\|build-outputs\|sandbox>` | Free disk space and what uses it / run a cleanup. See *Disk space*. |

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

## Upgrades (`difu server upgrade`)

Run from the computer that set the server up (it needs difu's SSH key). Over
SSH as root, the bootstrap script runs four steps, each safe to repeat:

1. `difu`: download this computer's difu release for the server, verify its
   checksum and install it.
2. `tools`: install the latest Codex build and run `claude update` (a failed
   download keeps the installed CLI and says so), print both versions.
3. `image`: when the server's `config.server.sandbox.enabled` is on, rebuild
   the sandbox image (or pull a custom registry image) because the bundled
   Containerfile may have changed; running sandboxes keep their image. The
   setting itself is never changed.
4. `service`: install the current `difu-agent.service` (so new unit settings
   such as `CLAUDE_CONFIG_DIR` arrive), reload systemd and restart the
   service.

### Version drift

On a computer, the terminal app replaces an older background service with its
own version. On a server (`config.server.enabled`) it never does: systemd runs
`difu serve` with `Restart=always` and the unit's environment, so a
replacement started by a terminal app would fight systemd and run without that
environment. A terminal app (or `--refresh-agent-service`) of a newer version
keeps the server's service and warns once, naming both versions and
`difu server upgrade`; with no service running it reports that the service is
not running instead of starting one.

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
  * the server's logins mounted (`~/.codex`, `~/.claude`, `~/.config/gh`), and
    `~/.gitconfig` plus the host's CLI binaries (`~/.local/bin`, Claude's
    versions directory) mounted read-only;
  * `CLAUDE_CONFIG_DIR=~/.claude`, so Claude Code's global config is
    `~/.claude/.claude.json` inside the mounted directory (see *Claude Code's
    shared config*);
  * the session's attachment directory (`<data>/agents/attachments/<id>`)
    mounted read-only, so Codex can read images sent from a Mac or a phone by
    path and the agent can open mentioned videos;
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
* Deleting a session from any client (a Mac through `rpc`, a phone with
  `session.delete`) is the same service request: it stops the agent and its
  shells, removes the session's pod, its worktree (uncommitted files included),
  attachments and saved session.
* `difu sandbox build` builds `localhost/difu-sandbox:latest` from a bundled
  Containerfile (Ubuntu LTS with git, gh, build tools, Python, Node, ripgrep).
  A custom `config.server.sandbox.image` under `localhost/` is built from the
  same Containerfile and tagged with that name; any other image comes from a
  registry, so `difu sandbox build` pulls it instead (and says so).
  `difu sandbox status` reports the configured image and whether it is
  present.
* Sessions are always worktrees, exactly as on a local difu. Worktrees share the
  base clone's object store and refs, so a sandbox could rewrite another session's
  branch refs (not its files); this is accepted, as it is locally.

## Disk space

A server's sessions, worktrees, base clones and sandbox images share one disk,
and a full disk breaks every session at once. Each server reports its own disk
space; clients only display what the server says.

* **Free space** is `statvfs` of the filesystem holding difu's agent data
  (`<data>/agents`, the `path` in `server.disk`), read on every request:
  `total`, `used`, and `free` (what difu can still use).
* **Status**: `low` when free space is below 15% **or** 20 GB, `critical` below
  5% **or** 5 GB, otherwise `ok` (constants in `cli/src/cloud/disk.rs`).
* **Breakdown**, measured on a background thread when the service starts, every
  5 minutes, after every cleanup and when a client asks (`refresh`); requests
  only read the last measurement and its `measuredAt`. Sizes are allocated
  blocks like `du -x`: symlinks are never followed, other filesystems are
  skipped and hard links count once.
  * `sessions`: each coding session's worktree (`<data>/agents/worktrees/<id>`)
    with its title, archived and active flags, biggest first, and
    `buildOutputs`, the part a `buildOutputs` cleanup could clear. Worktrees
    without a session count as other.
  * `repositories` (base clones), `attachments` (pasted media), `caches`
    (difu's cache directory).
  * `sandbox`: `podman system df --format json` (images, containers, volumes,
    and what Podman counts as reclaimable), when Podman is installed.
  * `other`: everything else on the filesystem (the system, logs, CLIs).

### Cleanups

Each cleanup is explicit, runs on the server one at a time, and reports the
bytes it freed and a summary. Running sessions are never interrupted.

| Action | What it does |
| --- | --- |
| `archivedWorktrees` | For every archived, inactive session with a difu-owned worktree: clear its build outputs (as below), then remove the worktree with the same per-session cleanup as the session's own "Clean up worktree" (pod removed, worktree removed with `git worktree remove` without `--force`, attachments removed). The chat, branch and commits stay. Worktrees that still hold modified, untracked or other ignored files are kept and named in the summary; active sessions are skipped. |
| `buildOutputs` | In idle sessions only (no turn, not preparing, clearing or being deleted, no open shell), under the session's action lock: delete the outermost directories named `target`, `node_modules`, `.next`, `.turbo`, `dist`, `build`, `.venv`, `__pycache__` or `.gradle` inside its difu-owned worktree, only where `git check-ignore` says Git ignores them and `git ls-files` finds no tracked file under them. The walk never enters symlinks, and each directory is checked again before removal to be a real directory whose canonical path is inside the worktree. |
| `sandbox` | `podman system prune -f` (stopped containers, dangling images, unused networks, build cache; not volumes), then `podman rmi` (without `--force`) of every image no container uses, except the configured sandbox image. |

### Guardrails

While a server is `critical` it refuses work that needs space: new sessions
(`NewAgent`, phone `agents.new`, checked before a repository is cloned for
them), imports from moves, and repository clones (`difu repos add`,
`server.repos.add`, the background refresh of a missing clone, launching on an
unlisted repository). The message names the free space and points at the
cleanups:

> Only 3.1 GB is free on this server (4% of 80 GB), so difu is not starting new
> sessions, moves or clones. Free up space in Cmd+K → /cloud or with `difu disk
> clean` on the server; running sessions continue.

Ordinary installations (not server mode) are never refused.

### Wire format

Phone API, server mode only (like `server.repos`), for every paired device:

```json
{"method": "server.disk", "params": {"refresh": false}}
→ {"path": "/home/difu/.config/difu/agents", "total": 80000000000, "used": 52000000000,
   "free": 28000000000, "status": "ok", "measuredAt": 1790000000000, "measuring": false,
   "breakdown": {
     "sessions": [{"id": "1790000000000-ab12cd34", "title": "Fix login", "archived": true,
                   "active": false, "bytes": 4100000000, "buildOutputs": 3200000000}],
     "repositories": 9000000000, "attachments": 12000000, "caches": 340000000,
     "sandbox": {"available": true, "images": 3000000000, "containers": 120000000,
                 "volumes": 0, "reclaimable": 900000000},
     "other": 30000000000}}

{"method": "server.disk.clean", "params": {"action": "archivedWorktrees"}}
→ {"freed": 4000000000, "summary": "Removed 3 archived worktrees (4.0 GB freed); their chats stay. Kept 1 with uncommitted, untracked or ignored files: Widget."}
```

* Bytes are integers. `measuredAt` is `null` (and the breakdown empty) until the
  first measurement finishes; `measuring` is true while one runs. `refresh`
  starts one and answers at once.
* `action` is `archivedWorktrees`, `buildOutputs` or `sandbox`. A cleanup can
  take minutes; clients should wait for the reply rather than time out early.
* The phone API asks the service with `Request::Disk {server: null, refresh}` →
  `Reply::Disk(report)` and `Request::DiskClean {server: null, action}` →
  `Reply::DiskCleaned({freed, summary})`.

A Mac's local service reads `server.disk` with `server.repos` while a terminal
app is open, at most every 30 seconds (the server answers from its cached
measurement), and `server.logins` every 5 minutes, into `Request::Servers`
(`disk`, `diskProblem`, `logins`, session counts, port). The terminal app's
`Request::Disk {server: "<name>", refresh}` and `Request::DiskClean {server:
"<name>", action}` are answered by the local service with these phone-API calls
on its connection, so they work whatever difu version the server runs; an older
server without `server.disk` shows "runs an older difu without disk status; run
`difu server upgrade <name>`". After a cleanup the local service reads the disk
status and session list again.

## Moving a session to a server

A coding session on this computer can continue on a saved server. The server
gets the conversation, the branch with its commits and the uncommitted files,
and continues them as a new server session. The local session is archived,
keeps its chat and its worktree (a move never deletes it), and refuses new
messages with a pointer to the server session.

Three ways to start a move:

* `difu session move <session> <server>` prints the new session's id, e.g.
  `hetzner:1790000100000-ef56ab78`.
* **Move to server…** in the session actions menu, then a saved server. difu
  opens the server session when the move finishes.
* The agent tool `difu_move_to_server` with `{"server": "<name>"}`, offered to
  Codex (a dynamic tool) and Claude Code (the difu MCP server) in local coding
  sessions, not on servers. Its description and instructions allow it only when
  the user explicitly asks, and it moves only the calling session. A session
  cannot move mid-turn, so the tool *schedules* the move and the agent ends its
  turn; the service runs the move once the turn has ended (status Ready) and
  reports the outcome in the chat (`Moved to <server> as <server>:<id>`, or an
  error entry). A turn that fails or is interrupted cancels the move. Scheduled
  moves are not persisted, so a service restart never replays one.

What moves:

| | |
| --- | --- |
| Chat | Every entry except transient sending markers, restored as is. |
| Settings | Title, provider, model, effort, first prompt, the permission mode (restored as the starting mode when the provider has it) and the Codex preset. |
| Repository | `owner/name` from the `origin` remote, which must be on GitHub. The server lists and clones it first when needed, as for new sessions. |
| Branch | The local branch name when it is free on the server, otherwise the managed `difu/agent-<id>`. Tracking `origin/<branch>` (as after `git push -u`) is restored, so pushes keep updating the branch and its pull request. |
| Commits | The commits on the branch that are not on `origin/main`, as a `git bundle`. |
| Uncommitted files | Modified, deleted and untracked files (binary-safe) as a scratch commit on top of HEAD, built with a temporary index so the local worktree and index do not change. They come back as unstaged changes and untracked files; staged changes become unstaged and ignored files stay behind. |
| Attachments | The session's private copies of pasted images and videos that the chat refers to (not unreferenced copies or the composer draft). They become the server session's private copies in its attachment directory, which its sandbox mounts read-only like any server session's, and every path to them in the chat (prompts, attachment lists, video path text) is rewritten, so resending, the handoff and the agent all find them. |
| HTML artifacts | Every registration with its title. Files in the worktree that git carries (tracked or untracked) arrive with the worktree; ignored files are written back to the same path in the new worktree; files registered in another workspace go to `artifacts/<n>/` in the session's attachment directory. Each registration points at its file on the server. |
| Pull requests | PRs attached by URL (`difu_attach_pr`, `difu attach-pr`) are attached to the server session. PRs found from the branch are found again from the preserved branch. |

A session cannot move while a turn is running, with pending questions or
approvals, with queued messages, while archived, preparing, clearing or being
deleted, after its worktree was removed, after it already moved, on a server, or
without a GitHub `origin` and an `origin/main`.

On the server the session gets a normal difu worktree exactly like a new session
(fetched `origin/main`, `<data>/worktrees/<id>` on `difu/agent-<id>`). Then the
bundle's objects are unpacked (`git bundle unbundle`, with object checks, no ref
changes), the branch is reset to the moved HEAD and the scratch commit's tree is
read into the worktree and the index reset to HEAD. If the server's clone lacks
the commits the bundle builds on (say, this computer's `origin/main` has commits
GitHub does not), the import fails and removes the worktree and branch it made.
The new session waits, idle, without a provider thread. Its first message starts
one and carries the prior conversation the way a provider switch does
(`provider_context`: the chat as JSON context, not instructions), followed by a
system entry saying where the session came from. Sandboxing follows the server's
configuration, as for any session.

### Wire format

The terminal app asks its local service with `Request::Move`; the local service
exports the session, sends `Request::Import` over the server's `rpc` (forwarded
like other server requests, full access and equal versions required) and records
the result.

```json
{"Move": {"id": "1790000000000-ab12cd34", "server": "hetzner"}}
→ {"Launched": "hetzner:1790000100000-ef56ab78"}

{"Import": {"package": {
  "source": "1790000000000-ab12cd34", "computer": "Ali's MacBook Pro",
  "title": "Widget work", "title_manual": true,
  "provider": "claude", "model": "claude/opus", "effort": "high",
  "mode": "plan", "preset": null, "prompt": "Build the widget",
  "entries": [{"id": "…", "kind": "userMessage", "text": "Build the widget", "data": null}],
  "repository": "acme/widgets", "branch": "difu/agent-1790000000000-ab12cd34", "upstream": true,
  "head": "<sha>", "wip": "<sha> | null", "bundle": "<base64 git bundle> | null", "commits": 2,
  "media": [{"path": "/Users/me/…/agents/attachments/1790000000000-ab12cd34/media-x1.png", "data": "<base64>"}],
  "artifacts": [
    {"path": "report.html", "title": "Report", "outside": false, "data": null},
    {"path": "build/out.html", "title": "Build output", "outside": false, "data": "<base64>"},
    {"path": "notes.html", "title": "Notes", "outside": true, "data": "<base64>"}
  ],
  "prs": [{"key": {"owner": "acme", "repo": "tools", "number": 42}, "state": "OPEN", "draft": false, "conflicts": false, "head_branch": "widget-tools", "head": "<sha>", "updated": "…", "summary": null}]
}}}
→ {"Launched": "1790000100000-ef56ab78"}
```

* `head` is the worktree's HEAD; `wip`, when present, is the scratch commit (its
  parent is `head`) whose tree is the worktree with its uncommitted files.
* `bundle` holds the commits from `origin/main` up to `wip` (or `head`) under one
  ref, `refs/difu/move/<source>`; it is `null` when there are none. `commits`
  counts the branch's own commits for the server's note.
* `media[].path` is the path the chat uses on this computer; the server stores
  the file under the same name in its own attachment directory and replaces that
  path everywhere in the chat.
* `artifacts[].data` is `null` when the worktree changes carry the file (or it
  no longer exists); `outside` marks a file registered in another workspace.
* `prs` are the attached pull requests as the PR cache stores them; the server
  attaches them without contacting GitHub.
* Only a server-mode service imports; anything else answers with an error. A
  failed import removes the worktree, branch and attachment copies it made.
* A move travels in one WebSocket message: the serialized package plus a 64 KiB
  envelope must fit in 48 MiB. A larger session is refused before anything is
  sent, with its size and a breakdown by part, largest first: attachments, HTML
  artifacts, commits and files, chat, and other.

Not moved: shells and their output, ignored files that are not artifacts, and
the composer draft. The handoff contains the whole chat, which for very long
sessions can be large for the provider, as with provider switching. Moving a
server session back to a computer or on to another server is not supported.

### Claude Code's shared config

Claude Code keeps its global config (account state, onboarding, user MCP
servers, per-project trust) in `$CLAUDE_CONFIG_DIR/.claude.json`, or
`~/.claude.json` when the variable is unset. It saves the file under a lock
directory beside it (`.claude.json.lock`, `mkdir`/`rmdir`) by writing
`.claude.json.tmp.<pid>.<hex>` next to it and renaming that over the file; when
the rename fails with `EXDEV`, `EPERM`, `EEXIST` or `EBUSY` it truncates and
rewrites the file in place. A sandbox with only the file `~/.claude.json`
mounted therefore took the lock in its own throwaway home (no exclusion between
sessions) and could not rename onto the mount point, so concurrent sessions
rewrote the shared file in place, where another process could read it half
written.

On a server every writer uses one directory instead:

* The service (`difu-agent.service` sets `CLAUDE_CONFIG_DIR=~/.claude`, and the
  service adds it to every agent's environment in server mode), every sandbox
  (the variable plus the `~/.claude` directory mount) and SSH logins (an
  `export` in the `difu` user's `~/.profile`) share `~/.claude/.claude.json`,
  its lock and its temporary files, so writes exclude each other and replace
  the file atomically.
* `difu serve` moves an existing `~/.claude.json` into `~/.claude/` when it
  starts (before any session runs), under Claude Code's own lock, keeps the
  original as `~/.claude.json.difu-backup-<time>`, and leaves `~/.claude.json`
  as a symlink to `.claude/.claude.json`, so a Claude Code started without the
  variable still reads the same file and replaces it atomically through the
  link. Starting a sandbox repeats the check. If both files exist (a Claude
  Code with the variable ran first), they are combined: the newer file's
  settings win and map settings such as `projects` and `mcpServers` are merged
  entry by entry; both originals are kept as backups. A symlink the user made
  is left alone.
* Server setup and `difu server upgrade` run the Claude Code installer and
  `claude update` with the same variable once the file has moved.
* Ordinary installations are unchanged: only server mode sets the variable.

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

Connection limits: a client address (IPv6 per /64) may hold 4 unauthenticated
connections and the server 32 connections in all. Each connection has 15
seconds from accept to finish TLS, the WebSocket handshake and authentication
(including the password challenge and proof); the server then closes it,
however slowly the client was still sending, so slow clients cannot hold the
slots.

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
| `server.disk` | `{"refresh"?}` | Disk space, status and breakdown; see *Disk space*. |
| `server.disk.clean` | `{"action"}` | `{"freed","summary"}`; `action` is `archivedWorktrees`, `buildOutputs` or `sandbox`. |

`server.repos*`, `server.login*`/`server.logout*` and `server.disk*` require
server mode, so a phone paired with a Mac cannot change the Mac's logins;
`server.rules.*` work everywhere. `agents.repositories` on a server returns the server repositories
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

Three service requests exist for clients whose sessions live elsewhere:

* `{"SessionPrs": {"id", "history"}}` → `{"SessionPrs": {"links", "branch", "error"}}`:
  the session's attached and discovered PRs, refreshed by the server with its
  worktree and `gh` under the same two-minute lease as its background refresh
  (`history` also checks closed and merged PRs, as opening a session does). On
  a failed refresh `links` are the cached ones and `error` says why.
* `{"UploadAttachment": {"id", "label", "kind": "Image"|"Video", "extension", "data": "<base64>"}}`
  → `{"Attachment": {"label", "path", "kind", "hash"}}`: stores pasted media as
  the session's private copy on the server (images are checked like a paste),
  at most 32 MB per file so the request stays within the WebSocket message
  cap. The reply is sent in `Control::Message*` `attachments`.
* `{"ReadArtifact": {"id", "path", "workspace"}}` → `{"ArtifactFile": "<base64>"}`:
  a presented HTML artifact (matched by its registered `path` and
  `workspace`, resolved with the same checks as opening it locally), at most
  32 MB. Nothing else in the worktree can be read this way.

## Clients

### Mac

* Saved servers live in `<config>/servers.json` (0600): name, server id, hosts,
  port, fingerprint, device id and token.
* The local agent service connects to every saved server as a `full` device and
  merges their sessions into `Request::List` replies. Server session ids are
  namespaced as `<server name>:<session id>` and summaries carry `server`: the
  sessions list shows a ☁️ after the worktree 🌳 and the server's name before
  the repository when the row has room. Requests for a namespaced id are forwarded with `rpc`; watches
  with `rpc.watch`.
* Every new session opens one form: **Location** (This Mac, then each saved
  server with its connection state or problem), **Repository**, **Model** and
  **Reasoning**, prefilled from the chosen location's saved defaults (a
  server's come from `Request::AgentDefaults`, cached with its repositories in
  `Request::Servers`). Enter starts right away; choosing another location
  reloads the other lists from it.
* Anything that consults a machine on a session's behalf asks the session's
  location. Requests with a session id are forwarded by id; the others carry
  `server`: `NewAgent`, `RepositoryRules`, `Defaults` (Codex's configured model
  for a directory), `AgentDefaults`, `DefaultModel` (saves a location's default
  model, so Alt+M on a server session sets that server's default) and `Models`
  (the Codex and Claude Code models that location's logins offer).
* Pull requests: the server is the source of truth. The terminal asks it for a
  visible server session's PRs (`SessionPrs`, every 15 s; the server only asks
  GitHub every two minutes), so badges, the chat footer and the Option+D diff
  selector work as for a local session. Published diffs (`PrChanges`) come
  from the server's `gh`; the local service caches PR states and diffs in this
  computer's GitHub store. The PR panel is the embedded review app and uses
  this computer's `gh` by `owner/repo#number`, like any pull request in
  Reviews; its Diff and Guide tabs need a local clone.
* Pasted images and videos stay private copies on the Mac (drafts included,
  under `attachments/<server>@<session>`) until Send. The local service then
  uploads each one with `UploadAttachment` and sends the message with the
  server's copies. A file over 32 MB fails the send with a clear error before
  anything reaches the server. Deleting or cleaning up the session removes the
  Mac's copies.
* Artifacts: badges and the artifact list come from the session as usual.
  Opening one downloads it (`ReadArtifact`) into
  `<cache>/server-artifacts/<server>@<session>/`, then previews that copy in
  the artifact pane or an external browser exactly like a local artifact.
  Each open and Refresh (button or Ctrl+R) downloads it again, so the preview
  follows changes on the server. Artifacts should be self-contained HTML:
  files they reference relatively are not downloaded. Deleting or cleaning up
  the session removes the copies.
* Shells (`!command`, Open shells, the shell pane) are `Request::Shell` /
  `Request::Shells` forwarded to the server, so they run there, in the
  session's sandbox when sandboxing is on. The pane's polling (`Exchange`,
  which also carries the pane size) and typed input (`Human`) are server round
  trips, so a distant server makes the pane less responsive than a local one.
  `difu_shell` runs on the server like every agent tool.
* A local session can move to a saved server (*Moving a session to a server*).
* **Cmd+K → `/cloud`** (also `/cloud <name>`) shows every saved server from
  `Request::Servers`: connection state or problem, difu version (a mismatch
  suggests `difu server upgrade <name>`), address, active/total sessions,
  GitHub/Codex/Claude Code logins, repositories, and a disk bar colored by
  status. Enter opens a server's breakdown (sessions biggest first) and actions:
  Refresh (`r`, `Request::Disk` with `refresh`, which also rereads logins and
  repositories), Clean archived worktrees (`a`), Clear build outputs (`b`) and
  Prune sandbox images (`p`), each confirmed with what it removes and the space
  it should free before `Request::DiskClean`, then the summary. Without saved
  servers it explains `difu server add` and `difu server join`.
* A low or critical server is marked where its sessions appear: the server name
  in the sessions list turns yellow or red (with "disk low"/"disk critical" when
  the row has room), the conversation title says "on <server> (disk low)", and
  the new-session form's Location shows the free space. The form refuses a
  critical server with the server's message before sending anything.
* The phone API of a Mac hides relayed server sessions, because phones connect to
  servers directly.

### iPhone

* Saved servers: a Keychain array keyed by server id (migrating the single saved
  computer).
* Add Server: scan QR, paste link, or type address + server password (password
  pairing, `standard` access).
* While active, connect to every saved server; one combined session list with a
  server badge; New Session picks a server, then its repositories.
* The session screen shows the session's PRs (`SessionSummary.prs`) as chips;
  tapping one opens the PR screen on the same server. Photos are sent with
  `session.send` straight to the server running the session.
* Typing `/` in the composer lists `session.commands` from the server running
  the session (difu's commands plus its provider's skills) and runs them with
  `session.command`; Model & Effort lists both providers' models and switches
  provider between turns.
* Per-server settings: disk space (bar, status, breakdown, and the three
  cleanups with confirmations), logins (status, device-code login, Claude
  token), server repositories (add from `server.repos.available`, remove,
  refresh mains), repository rules, paired devices (revoke).
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
