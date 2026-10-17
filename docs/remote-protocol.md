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
| `n` | Human-readable computer name, e.g. `Ali's MacBook Pro`. |
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
  "canReadChanges": true
}
```

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
| `session.send` | `id`, `text`, `queue` (bool, default false), `attachments` (optional `[{"name": "photo.jpg", "data": "<base64>"}]`, PNG/JPEG/WebP/GIF) | `{}` |
| `session.interrupt` | `id` | `{}` |
| `session.sendNow` | `id` | `{}` — interrupt and send queued messages now |
| `session.resume` | `id` | `{}` — "Continue session" |
| `session.compact` | `id` | `{}` |
| `session.unqueue` | `id`, `index` | `{}` — remove a queued message |
| `session.answer` | `id`, `request`, `question`, `answer` (string, or `null` to skip) | `{}` |
| `session.decide` | `id`, `request`, `choice` (a `choices[].id`) | `{}` |
| `session.model` | `id`, `model`, `effort` (either may be null) | `{}` |
| `session.rename` | `id`, `title` | `{}` |
| `session.archive` | `id`, `archived` | `{}` |
| `session.delete` | `id` | `{}` — removes chat and worktree (confirm first) |
| `session.clear` | `id` | `{}` — fresh conversation and worktree (confirm first) |
| `session.changes` | `id` | `{"files": [DiffFile]}` uncommitted + committed worktree changes |
| `agents.new` | `repository` (path or null for the default), `model`, `effort`, `text` (first message, optional) | `{"id": "..."}` |
| `agents.repositories` | – | `{"repositories": [{"path": "...", "name": "difu", "default": true}]}` |
| `agents.models` | – | `{"models": [{"id": "gpt-5.6-luna", "name": "GPT-5.6 Luna", "provider": "codex", "efforts": ["low","medium","high"]}]}` (slow: seconds) |
| `server.info` | – | `{"name", "version", "hosts", "port"}` |

`DiffFile`: `{"path": "src/a.rs", "oldPath": null, "status": "modified", "additions": 3, "deletions": 1, "patch": "@@ ...", "truncated": false}`;
`status` is `added | deleted | modified | renamed`. `patch` contains hunks only
(starting at the first `@@`), lines prefixed with `+`, `-`, or space.

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
