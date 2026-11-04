# difu remote-control protocol (v1)

The difu iOS app controls a difu installation on a computer on the same Wi-Fi.
The computer side is the background **agent service** (`difu --agent-service`,
`cli/src/agents/remote/`). It listens only after the user opens
**Cmd+K → `/remote-control`** in the terminal app, and keeps listening across
restarts until disabled there.

## Pairing QR code

The terminal shows a QR code that encodes a URI:

```
difu://pair?v=1&n=<computer name>&h=<host>,<host>,...&p=<port>&f=<fingerprint>&c=<code>
```

| Key | Meaning |
| --- | --- |
| `v` | Protocol version, currently `1`. |
| `n` | Human-readable computer name, e.g. `Ali's MacBook Pro`, or the name set with `difu remote config --name`. |
| `h` | Comma-separated host candidates, best first: the Bonjour name (`Alis-MacBook-Pro.local`) then IPv4 addresses. |
| `p` | TCP port. |
| `f` | Base64url (no padding) SHA-256 of the server's TLS certificate (DER). |
| `c` | Single-use pairing code, valid for 10 minutes. |

Values are `application/x-www-form-urlencoded` (spaces may appear as `+`).

## Transport

* `wss://<host>:<port>/` — a WebSocket over TLS 1.2+/1.3.
* The certificate is **self-signed**. Clients must not use system trust; they
  must accept the server only when SHA-256(leaf certificate DER), base64url
  encoded without padding, equals the pinned fingerprint `f`. On iOS: implement
  `urlSession(_:didReceive:completionHandler:)` for
  `NSURLAuthenticationMethodServerTrust`, compare `SecCertificateCopyData` of the
  leaf, then answer `.useCredential` with `URLCredential(trust:)`, or
  `.cancelAuthenticationChallenge`.
* All application messages are JSON **text** frames. Messages can be several MB
  (initial transcripts); set `URLSessionWebSocketTask.maximumMessageSize` to at
  least 64 MiB.
* The server sends WebSocket pings every 15 s. Try hosts in order with a short
  connect timeout (~4 s each) and remember the one that worked.

## Authentication (first message)

The first client frame must be one of:

```json
{"type":"pair","code":"<c from QR>","device":{"id":"<stable random id, 8-64 chars [A-Za-z0-9_-]>","name":"Ali's iPhone"}}
{"type":"auth","token":"<device token>"}
```

The server answers with exactly one of:

```json
{"type":"welcome","protocol":1,"device":"<device id>","token":"<device token, only after pair>",
 "server":{"name":"Ali's MacBook Pro","version":"0.20.0","hosts":["Alis-MacBook-Pro.local","192.168.1.20"],"port":51234,"fingerprint":"..."}}
{"type":"denied","message":"This phone is no longer paired. Scan the QR code in /remote-control again."}
```

After `denied` the server closes the socket. Store the token in the Keychain;
it is long-lived until the user forgets the device in `/remote-control` (the
server then disconnects it and later `auth` attempts are denied). Refresh the
stored hosts/port from every `welcome`.

## Cloud servers

The same protocol serves difu running on a Linux server (`difu serve`, port
**3438** by default). Those additions are backwards compatible and specified in
[cloud-agents.md](cloud-agents.md#wire-protocol-additions):

* `access` (`standard` or `full`) in the `pair` message and in `welcome`;
  devices paired before it are `standard`.
* Password pairing (`password` → `challenge` → `proof` → `welcome` with the
  server's `proof`) for clients that know only the address and server password.
* `welcome.server` gains `id` (the fingerprint), `mode` (`server` or `computer`)
  and `passwordPairing`. Hosts set with `difu remote config` come first in the
  QR code, `welcome` and `server.info`; a server with such hosts lists only
  them, leaving out the private addresses it discovers.
* Methods for every device: `device.unpair`, `server.devices`,
  `server.devices.forget`, `server.repos*`, `server.rules.*`, `server.login*`,
  `server.logout`, `server.disk`, `server.disk.clean`; the `device.paired`
  event; and `rpc`, `rpc.watch`, `rpc.unwatch` for `full` devices only.
* While a server is critically low on disk, `agents.new` (and moves and clones)
  fail with a message naming the free space; see
  [cloud-agents.md](cloud-agents.md#disk-space).
* Failed pairing attempts are rate-limited per client address, and wrong
  passwords also globally; token authentication is never limited.

## Requests, responses, and events

Client → server:

```json
{"id": 7, "method": "session.send", "params": {"id": "...", "text": "hi"}}
```

Server → client, exactly one response per request (any order):

```json
{"id": 7, "result": { ... }}
{"id": 7, "error": "Human-readable message"}
```

Server → client push events (no `id`):

```json
{"event": "sessions", "data": [SessionSummary, ...]}
{"event": "session", "data": SessionView}
{"event": "session.delta", "data": SessionDelta}
{"event": "session.closed", "data": {"id": "...", "message": "Session not found"}}
```

Unknown fields must be ignored; optional fields may be absent or `null`.
Timestamps are Unix **milliseconds** unless stated otherwise.

## Agents

### Types

`SessionSummary`

```json
{
  "id": "1791875325706-02acb407",
  "title": "Fix login redirect",
  "kind": "Coding",                 // "Coding" | "Orchestrator" | "Conflicts"
  "orchestrator": false,            // true for the pinned "Difu orchestrator" chat
  "status": "Running",              // Starting | Running | Waiting | Idle | Completed | Interrupted | Failed
  "statusLabel": "Working",         // Starting | Working | Needs input | Ready | Completed | Interrupted | Failed
  "active": true,                   // Starting, Running or Waiting
  "archived": false,
  "pinned": false,
  "updated": 1790000000000,
  "turnStartedAt": 1790000000000,   // null when no turn is running; show elapsed time while active
  "repository": "difu",             // display name, null for the orchestrator
  "repositoryPath": "/Users/ali/code/difu",
  "branch": "difu/agent-1791875325706-02acb407",
  "workspace": "/Users/ali/Library/Application Support/difu/agents/worktrees/...",
  "worktree": true,
  "pending": 1,                     // unanswered questions + approvals
  "queued": 0,
  "canReadChanges": true,
  "prs": [SessionPr]                // attached and discovered pull requests; may be absent (older computers)
}
```

`SessionPr` — a pull request the terminal shows as a badge for the session:

```json
{
  "repo": "opencx-labs/difu",       // with `number`, the `pr.overview` / `pr.diff` parameters
  "number": 45,
  "title": "Fix login redirect",    // null until GitHub was asked
  "state": "OPEN",                  // OPEN | MERGED | CLOSED, or "" until GitHub was asked
  "label": "Open",                  // Open | Draft | Has conflicts | Merged | Closed | Unknown
  "draft": false,
  "conflicts": false,
  "attached": false,                // attached with difu_attach_pr (maybe another repository), not found from the branch
  "url": "https://github.com/opencx-labs/difu/pull/45",
  "headBranch": "difu/agent-1791875325706-02acb407"   // may be null
}
```

`prs` is ordered like the terminal's badges: open pull requests for the
session's current branch first, then other open ones, then closed and merged
history. The list comes from the computer's PR cache (refreshed in the
background about every two minutes, and immediately when an agent attaches a
PR), so it can lag GitHub by that much. Open one with `pr.overview` / `pr.diff`
on the same connection: they run on that computer or server with its own `gh`.

The `sessions` array is already in display order (orchestrator, pinned, the
user's custom order, newest first) and excludes guide-generation jobs. Archived
sessions are included; filter them client-side (Active / Archived toggle).

`SessionView` — everything in `SessionSummary` plus:

```json
{
  "version": 412,
  "provider": "codex",              // "codex" | "claude"
  "model": "gpt-5.6-luna",          // null = provider default
  "effort": "high",
  "error": null,                    // session-level error, shown at the end in red
  "preparing": false,               // worktree/agent still starting; messages are queued
  "canSend": true,                  // composer enabled
  "canInterruptAndSend": false,     // there are queued messages that "Send now" would flush
  "betweenTurns": true,             // no turn, pending request or queued message; may be absent (older computers)
  "activity": {"label": "Running cargo test", "since": 1790000000000},  // or null
  "queue": [{"index": 0, "text": "Also update the docs"}],
  "pending": [PendingView],
  "entries": [EntryView]
}
```

`EntryView` — one transcript row, already normalized for display:

```json
{
  "id": "item_12",
  "kind": "commandExecution",       // raw provider kind (for icons); see role
  "role": "tool",                   // user | assistant | reasoning | tool | plan | error | system
  "text": "...",                    // user/assistant (Markdown)/system/error text; tool output; reasoning summary
  "title": "Ran cargo test",        // tool label; reasoning heading; null otherwise
  "detail": "cargo test --locked",  // command for commandExecution, arguments for other tools; may be null
  "state": "done",                  // tools: running | done | failed | interrupted; null for non-tools
  "startedAt": 1790000000000,
  "finishedAt": 1790000001000,
  "files": [{"path": "src/main.rs", "additions": 3, "deletions": 1, "diff": "@@ -1,3 +1,5 @@\n..."}],
  "plan": [{"step": "Add tests", "status": "completed"}],   // completed | inProgress | pending
  "delivery": null,                 // user rows: "sending" | "unsent" | "awaiting" | null (delivered)
  "truncated": false                // text/diff shortened; fetch with session.entry
}
```

Render `assistant` text as Markdown. Tools are collapsed rows (icon, title,
state, duration) that expand to `detail`, `text` (monospace output), and file
diffs. `plan` renders a checklist.

`PendingView` — a question set or approval the agent is waiting on:

```json
{
  "request": 3,                     // opaque JSON value; echo it back unchanged
  "kind": "questions",              // questions | approval | permissions | unsupported
  "async": false,                   // true: the agent keeps working while it waits
  "title": "Allow command?",
  "detail": "cargo test --locked",
  "questions": [{
    "id": "difu-decision", "header": "Permission", "question": "Allow Bash?\nrm -rf target",
    "options": [{"label": "Allow once", "description": ""}, {"label": "Deny", "description": ""}],
    "multiSelect": false, "secret": false, "answered": false
  }],
  "choices": [{"id": "accept", "label": "Allow once"}, {"id": "decline", "label": "Decline"}]
}
```

* `questions`: answer each unanswered question with `session.answer`. Offer the
  options plus free text ("Other"). For `multiSelect`, join chosen labels with
  `", "`. Permission questions (`id` = `difu-decision`) must be answered with
  an option label exactly.
* `approval` / `permissions`: show `choices` as buttons; send `session.decide`.
* `unsupported`: a single choice `stop` that interrupts the agent.

`SessionDelta` — apply to the last `SessionView` for that id:

```json
{"id": "...", "base": 411, "session": { /* SessionView without "entries" */ },
 "count": 58, "entries": [[57, EntryView], [56, EntryView]]}
```

If `base` differs from the stored `version`, re-watch the session. Otherwise:
truncate entries to `count`, replace/append each `[index, entry]`, then replace
all other fields with `session` (including `version`).

### Methods

| Method | Params | Result |
| --- | --- | --- |
| `sessions.subscribe` | – | `{"sessions": [SessionSummary]}`; then `sessions` events on every change |
| `sessions.list` | – | `{"sessions": [SessionSummary]}` |
| `session.watch` | `id` | `{}`; then a `session` event, followed by `session.delta` events. Several sessions may be watched at once. |
| `session.unwatch` | `id` | `{}` |
| `session.entry` | `id`, `entry` (entry id) | `{"entry": EntryView}` untruncated (up to ~4 MB) |
| `session.send` | `id`, `text`, `queue` (bool, default false), `attachments` (optional `[{"name": "photo.jpg", "data": "<base64>"}]`, PNG/JPEG/WebP/GIF, each under 32 MB), `skills` (optional names of Codex skills the text uses as `$name`; sent with the message as the terminal does) | `{}` |
| `session.interrupt` | `id` | `{}` |
| `session.sendNow` | `id` | `{}` — interrupt and send queued messages now |
| `session.resume` | `id` | `{}` — "Continue session" |
| `session.compact` | `id` | `{}` |
| `session.unqueue` | `id`, `index` | `{}` — remove a queued message |
| `session.answer` | `id`, `request`, `question`, `answer` (string, or `null` to skip) | `{}` |
| `session.decide` | `id`, `request`, `choice` (a `choices[].id`) | `{}` |
| `session.model` | `id`, `model`, `effort` (either may be null) | `{}` — a model of the other provider switches the session to it (see below) |
| `session.rename` | `id`, `title` | `{}` |
| `session.archive` | `id`, `archived` | `{}` |
| `session.delete` | `id` | `{}` — removes chat and worktree (confirm first) |
| `session.clear` | `id` | `{}` — fresh conversation and worktree (confirm first) |
| `session.changes` | `id` | `{"files": [DiffFile]}` uncommitted + committed worktree changes |
| `session.commands` | `id`, `refresh` (bool; lists the provider's skills again) | `{"commands": [Command], "errors": ["..."]}`; see *Slash commands* (slow the first time: seconds) |
| `session.command` | `id`, `name` (`/compact`, `$lint` or a bare name), `args` (string, optional), `confirm` (bool), `queue` (bool, provider commands) | `CommandOutcome`; see *Slash commands* |
| `agents.new` | `repository` (path or null for the default), `model`, `effort`, `text` (first message, optional) | `{"id": "..."}` — a `model` picks its own provider; the saved provider, effort and starting mode apply only when they belong to it |
| `agents.repositories` | – | `{"repositories": [{"path": "...", "name": "difu", "default": true}]}` |
| `agents.models` | – | `{"models": [{"id": "gpt-5.6-luna", "name": "GPT-5.6 Luna", "provider": "codex", "efforts": ["low","medium","high"]}]}` (slow: seconds) |
| `server.info` | – | `{"name", "version", "hosts", "port"}` |

**Switching providers.** `agents.models` lists both providers' models, and
`session.model` accepts any of them: a model of the other provider switches the
session there (the chat and workspace stay; the conversation is handed over
with the next message). The computer allows that only for coding sessions
between turns (`betweenTurns`), and changes a Claude Code session's model or
effort only between turns too; otherwise the call fails with "Finish the current
turn and pending requests before switching models or providers". A provider is
derived from a model the same way everywhere: `claude/…`, `claude-…`, `sonnet`,
`opus` and `haiku` are Claude Code, everything else Codex.

`DiffFile`: `{"path": "src/a.rs", "oldPath": null, "status": "modified", "additions": 3, "deletions": 1, "patch": "@@ ...", "truncated": false}`;
`status` is `added | deleted | modified | renamed`. `patch` contains hunks only
(starting at the first `@@`), lines prefixed with `+`, `-`, or space.

### Slash commands

`session.commands` lists what typing `/` in the composer offers for a session,
from the computer or server running it: difu's session commands that work on a
phone, then the session provider's skills and commands (Codex skills, Claude
Code commands and skills). Provider skills are cached per connection for two
minutes; `refresh` lists them again. Problems listing them come back in
`errors` while difu's commands are still listed.

`Command`:

```json
{
  "name": "model",                  // without the trigger
  "trigger": "/model",              // what to show and insert: "/name", or "$name" for a Codex skill
  "description": "Choose a model",
  "hint": "[model] [effort]",       // argument hint, or null
  "kind": "screen",                 // action | prompt | screen
  "screen": "model",                // for screen commands: model | rename | rules | changes | usage | status | permissions | new | skills
  "confirm": false,                 // destructive: session.command needs "confirm": true
  "arguments": "optional",          // none | optional | required
  "source": "difu"                  // difu, or the provider (codex | claude) for its skills and commands
}
```

difu's commands, in the terminal's order (those marked *coding* only in coding
sessions; `rules` and `repo` not in the orchestrator; `diff` only when
`canReadChanges`):

| Command | Kind | Behaviour |
| --- | --- | --- |
| `/compact` | action | Compacts the conversation. |
| `/model [model] [effort]` | screen `model` | Opens the model picker; with arguments switches like the terminal (a model of the session's provider keeps the effort). |
| `/effort [level]` | screen `model` | Same picker; with an argument sets the effort. |
| `/skills` | screen `skills` | *coding*: lists only the provider's entries. |
| `/status` | screen `status` | Session, model, mode and workspace. |
| `/rules` | screen `rules` | *coding*: the session repository's rules (`server.rules.*`). |
| `/permissions [mode]` | screen `permissions` | *coding*: provider modes and Codex approval presets; the argument (an id or label) selects one. |
| `/usage` | screen `usage` | *coding*: provider usage and rate limits. |
| `/diff` | screen `changes` | *coding*: `session.changes`. |
| `/new` | screen `new` | A new session on the same computer or server. |
| `/rename [title]` | screen `rename` | With a title renames the session. |
| `/clear` | action, confirm | Fresh conversation and worktree from main. |
| `/repo <path>` | action, confirm | *coding*: changes repository, discarding the workspace's work. On a server `owner/name` works too. |

Provider entries are `prompt`s with `arguments: "optional"`: insert the trigger
in the composer and send it as a message. Claude Code runs `/name args` from the
text; Codex skills are named `$name` in the text and sent in `session.send`'s
`skills`. A Claude Code command named like one of difu's is not listed (difu's
runs, as in the terminal). Text starting with a difu command's `/name` should be
run with `session.command`, not sent.

`session.command` runs a command by name. difu's run on the computer; screen
commands without arguments only name their screen, except `status`, `usage` and
`permissions`, which also return the screen's data. A provider skill or command
is sent as a message (`queue` as in `session.send`). Destructive commands fail
without `confirm: true`, commands that take no arguments fail with some.

`CommandOutcome` — every field optional:

```json
{
  "message": "Compacting the conversation",   // what an action did
  "screen": "usage",                         // the screen to open
  "repository": "/Users/ali/code/difu",       // rules: the session's repository
  "status": {"title": "Fix login", "provider": "claude", "providerLabel": "Claude Code",
             "model": null, "effort": "high", "state": "Ready", "repository": "/Users/ali/code/difu",
             "workspace": "/…/worktrees/…", "branch": "difu/agent-…", "providerSession": "…",
             "mode": "Plan", "preset": null},          // preset: Codex approval preset label
  "usage": {"provider": "codex", "title": "OpenAI Codex · Usage",
            "limits": [{"section": "Codex", "title": "Weekly limit", "percent": 96.0,
                        "remaining": true, "resetsAt": 1800000000000}],   // percent left (remaining) or used
            "details": [{"section": null, "label": "Account", "value": "pro"}]},
  "permissions": {"provider": "codex", "activeMode": "default", "pendingMode": null,
                  "activePreset": "Auto", "pendingPreset": null, "reloadPending": false,
                  "modes": [{"id": "plan", "label": "Plan", "description": "…", "available": true, "reason": null}],
                  "presets": [{"id": "Auto", "label": "Approve for me", "description": "…", "available": true, "reason": null}],
                  "message": null}
}
```

Computers without these methods answer `Unknown method`; clients then offer the
commands existing methods run (`session.compact`, `session.clear`,
`session.rename`, the model picker, changes and new session).

## Reviews

`Pr`:

```json
{"repo": "opencx-labs/difu", "number": 45, "title": "...", "author": "octocat",
 "updated": "2026-10-17T12:00:00Z", "created": "...", "draft": false,
 "state": "OPEN", "conflicts": false, "reviewers": ["alice"],
 "additions": 10, "deletions": 2, "changedFiles": 3, "url": "https://github.com/..."}
```

| Method | Params | Result |
| --- | --- | --- |
| `reviews.inbox` | `refresh` (bool; true fetches from GitHub, seconds) | `{"viewer": "octocat", "mine": [Pr], "reviewing": [Pr]}` open PRs you authored / were asked to review |
| `reviews.repositories` | – | `{"repositories": [{"name": "owner/repo", "pinned": true}]}` |
| `reviews.repository` | `repo` | `{"prs": [Pr]}` open PRs in that repository |
| `pr.overview` | `repo`, `number` | `PrOverview` |
| `pr.diff` | `repo`, `number` | `{"files": [DiffFile]}` |
| `pr.guide` | `repo`, `number` | `Guide` |
| `pr.generateGuide` | `repo`, `number` | `Guide` (status `running` or `ready`) |
| `pr.image` | `repo`, `number`, `head` (`PrOverview.head`; needed for repository paths), `url` (an image source as written in the description or a comment) | `{"data": "<base64>", "mime": "image/png", "width": 1280, "height": 720}`; see *PR images* |
| `pr.comment` | `repo`, `number`, `head`, `body` | `{"message": "Comment succeeded"}` |
| `pr.review` | `repo`, `number`, `head`, `event` (`APPROVE` \| `COMMENT` \| `REQUEST_CHANGES`), `body` | `{"message"}` |
| `pr.merge` | `repo`, `number`, `head`, `squash` (bool), `admin` (bool) | `{"message"}` |
| `pr.close` | `repo`, `number`, `head`, `body` (optional comment) | `{"message"}` |
| `pr.ready` | `repo`, `number`, `head`, `draft` (bool: true converts back to draft) | `{"message"}` |

`head` is `PrOverview.head`; writes fail if the PR changed since it was viewed
("PR head changed…") — reload the overview and retry.

`PrOverview`:

```json
{
  "repo": "opencx-labs/difu", "number": 45, "url": "...", "title": "...", "body": "<markdown>",
  "author": "octocat", "state": "open", "draft": false,
  "head": "<sha>", "base": "<sha>", "headBranch": "feature", "baseBranch": "main",
  "additions": 10, "deletions": 2, "changedFiles": 3,
  "mergeable": "MERGEABLE", "mergeState": "CLEAN",
  "reviewers": [{"name": "alice", "state": "approved"}],
  "checks": [{"name": "check (macos-latest)", "state": "pass", "url": "..."}],
  "timeline": [{"date": "...", "author": "alice", "kind": "review · approved", "body": "LGTM", "url": "..."}]
}
```

Check `state`: `pass | fail | pending | skipping | cancelled | expected`.

`Guide`:

```json
{
  "status": "ready",            // none | running | ready | failed
  "message": null,              // progress text while running, error when failed
  "session": "1790...-abcd",    // the guide job's session id, when one exists
  "outdated": false,            // generated for an older head commit
  "chapters": [{
    "category": "regular",      // schema | migrations | regular | generated | tests
    "title": "...", "explanation": "<markdown>",
    "hunks": [{"file": "src/a.rs", "header": "@@ -1,3 +1,4 @@",
               "lines": [{"kind": "add", "old": null, "new": 2, "text": "let x = 1;"}]}]
  }]
}
```

Line `kind`: `context | add | remove | meta`. Poll `pr.guide` every few seconds
while `running`.

### PR images

Descriptions and timeline bodies are Markdown that may contain images:
`![alt](src)`, linked images `[![alt](src)](link)`, and HTML `<img src alt>`
(entities decoded), never inside code. The phone has no GitHub login, so
`pr.image` makes the computer or server download one with the terminal's
preview rules and disk cache: HTTPS and public addresses only (also after
redirects), GitHub authentication (`gh`) only for `github.com`,
`api.github.com` and `raw.githubusercontent.com` (so `user-attachments`
images work and the signed URLs they redirect to never see the token), at most
10 MiB and 8192 px per side, PNG, JPEG, WebP or GIF (show the first frame).
`src` may be an absolute URL, a GitHub root path (`/owner/repo/raw/…`), or a
repository path resolved at `head`. `data` is the original file. Other schemes
(`file:`, `data:`, `javascript:`), plain HTTP, URLs with credentials,
oversized or undecodable images fail with a message; offer the image's browser
URL instead. SVG images are not drawn (as in the terminal); link to them.
