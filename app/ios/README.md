# difu for iOS

A native iPhone companion for [difu](../../README.md). Pair it with difu on
your Mac (over your Wi-Fi) and with difu cloud servers (from anywhere), and
control everything: watch and steer coding agents, answer their questions and
approvals, start new sessions, and review GitHub pull requests (overview with
its images, diff, guide, comment, approve, merge).

The phone talks to each difu's background agent service using the protocol in
[`docs/remote-protocol.md`](../../docs/remote-protocol.md) and its cloud
additions in [`docs/cloud-agents.md`](../../docs/cloud-agents.md): a WebSocket
over TLS, pinned to the certificate fingerprint from the pairing QR code (or
the one proven by the server password).

<p>
  <img src="screenshots/sessions.png" width="200" alt="Agents session list">
  <img src="screenshots/transcript.png" width="200" alt="Live session transcript">
  <img src="screenshots/approval.png" width="200" alt="Approving a command">
  <img src="screenshots/pr-guide.png" width="200" alt="PR guide">
</p>

More in [`screenshots/`](screenshots/), taken in the iOS Simulator against a
demo difu.

## Run it on your iPhone

Requirements: Xcode 26 or later with the iOS platform installed
(**Xcode › Settings › Components**), and an iPhone running iOS 17 or later.

1. Open `app/ios/Difu.xcodeproj` in Xcode.
2. Select the **Difu** target › **Signing & Capabilities** and choose your
   team. If Xcode says the bundle identifier `app.difu.ios` is unavailable,
   change it to something unique, such as `com.yourname.difu`.
3. Connect your iPhone, select it as the run destination, and press **Run**.
   The first time, you might need to trust the developer certificate on the
   phone (**Settings › General › VPN & Device Management**) and turn on
   Developer Mode.

## Pair with your Mac

1. On the Mac, open difu, press **⌘K**, and run **`/remote-control`**.
2. On the phone, tap **Scan QR Code** and point the camera at the code.
   You can also press **c** in `/remote-control` to copy the pairing link,
   then tap **Paste Pairing Link** on the phone (Universal Clipboard carries
   it across), or open the `difu://pair?…` link on the phone.
3. When iOS asks to find devices on your local network, tap **Allow**.

Pairing codes are single-use and expire after 10 minutes.

## Add a cloud server

A server set up with `difu server add` (see
[`docs/cloud-agents.md`](../../docs/cloud-agents.md)) can be added in two ways:

- **Address and password**: tap **Use Address and Password** and enter the
  server's IP address or DNS name (port 3438 unless you add `:port`) and its
  server password. The password proves the server is genuine before the phone
  trusts its certificate, so nobody in between can impersonate it.
- **QR code**: run `difu server pair <name>` on your Mac (or `difu remote pair`
  on the server) and scan the code.

Add more computers and servers from **Settings › Servers › Add Server**. The
app connects to all of them while it's open and shows their sessions in one
list (with a server badge once there's more than one). New sessions start by
picking a server. Each server's page in Settings shows its disk space (free
space, status, what uses it, and cleanups for archived worktrees, build outputs
and sandbox storage), logins (GitHub and Codex device-code login, Claude Code
token), repositories (add, remove, **Refresh Mains**), new session defaults,
repository rules and paired devices.

**New Session** starts from the chosen computer's or server's new session
defaults, as that computer or server works them out, so the phone, its
terminal and its agents start sessions the same way: its default repository,
model, effort and mode. When it saved no model, that's the newest Claude Opus
its Claude Code offers with extra-high reasoning (or the highest it supports);
without Claude Code there, the provider's configured model. **Mode** shows
how the session starts: the provider's modes, and for Codex its approvals
(Ask for approval, Approve for me, Full Access, Read Only). Servers start
Claude Code in **Auto** and give Codex **Approve for me** unless you saved
another mode; computers keep each provider's own default. In Auto and
Approve for me, Claude Code and Codex still review risky actions themselves;
bypass permissions and Full Access are never chosen automatically, and
starting in one asks first. Anything you don't
change is left to the computer or server; repositories, models and defaults
load independently, every control works while they do, and what you choose
stays when lists arrive. Type another repository (`owner/name` on a server, a
full path on a computer) to use one that isn't listed. Computers with an older
difu get the phone's own Claude Opus preselection and no Mode picker.

**Settings › (server) › New Session Defaults** changes those defaults on that
computer or server, where its terminal and agents use them too: provider and
model (**Automatic** shows what it picks), effort, starting mode, Codex
approvals and repository (each session gets its own worktree). **Use Claude
Opus · xhigh** saves that in one tap. Computers with an older difu don't show
the screen and still get the Opus preselection.

In a session, type **/** at the start of the message to see its commands:
difu's (compact, model and effort, status and usage, permissions, rules,
changes, rename, new session, clear, change repository) and the agent's own
skills and commands, each from the computer or server running the session.
Choosing one runs it (asking first for destructive ones), opens its screen, or
puts it in the message to add arguments. **Model & Effort** lists every
provider's models; picking another provider's switches the session there once
it's between turns.

The chip above the message field shows the session's mode (and Codex's
approvals), with "next turn" while a change waits. Tap it to switch: modes
that aren't available say why, and when the computer or server refuses a
change the message says what to do. Switching to bypass permissions or Full
Access asks first. **/permissions** and **⋯ › Permissions** do the same.
Session rows show what the terminal's sidebar and session controls show:
requests waiting, queued messages, HTML artifacts, open shells, sub-agents
(how many work out of how many), pull requests, and 🌳 for a session in its
own worktree and ☁️ for one on a cloud server. Folders from the terminal's
sidebar appear under **Folders**; open one to see its sessions (sessions
started there are filed in it) and rename, reorder or delete it from its
menu. Swipe right on a session to pin it, or long-press for **Pin**,
**Rename**, **Move to Folder…**, **Archive** and **Delete…** (which, as in
the terminal, also removes its difu worktree). Search matches titles,
repositories, branches, servers and folders. Pins and folders are saved on
that computer or server, so its terminal shows them too.

In a session, the bar above the chat holds **Artifacts**, **Shells**,
**Sub-agents** and the session's pull requests. Artifacts open in a web view
(**Refresh** reads them again; share one to open it elsewhere). A shell shows
its text and takes typed lines plus the keys a phone lacks (Ctrl-C, Tab, Esc,
arrows); it's shared with difu and the agent, which waits while you type. Open
a new shell or run a command from the Shells list, and swipe one to
terminate it. Start a message with **!** to run a command in the worktree, as
in the terminal. Sub-agents open read-only and follow along while they work.
Tap a queued message to edit or remove it. The session menu also pins it,
files it into a folder, attaches a pull request by URL, changes its
repository, and, on a computer with saved cloud servers, moves it to one.

When an agent asks a question, tap **Add a note** to send a note with the
option, options or "Other" answer you choose, as the terminal does. Permission
prompts, tool forms and secret answers take no note.

Images in a pull request's description and comments load through the computer
or server showing the PR, which uses its own GitHub login, so private
attachments work. Tap one to view it full screen and pinch to zoom; images that
can't be shown offer **Open in browser**.

**Forget** revokes the phone on that server first, then removes it from the
phone. When another device pairs with one of your servers, a banner offers to
revoke it.

## App lock

difu asks for Face ID (or your passcode) when it opens and after 5 minutes in
the background. Turn it off in **Settings › Security**.

## Themes

**Settings › Appearance › Theme** offers the same themes as difu in the
terminal, with the same names, and saves the choice on the phone. The theme
colours the accent, lists and cards, code and diffs, statuses, badges and the
composer, and changes the app icon to the theme's accent on its background
(Default keeps the Matrix green icon). iOS confirms each icon change with an
alert.

Themes with their own background (Dracula, Nord, GitHub Light and the rest)
switch the app to dark or light to match. Default, Amber and Ocean follow the
phone's appearance, as difu follows the terminal's background, and ANSI and
ANSI Light use iOS's system colours in dark and light.

## Troubleshooting

- **"Can't reach …"**: for a Mac, the phone and Mac must be on the same
  network. Guest networks and some office networks isolate devices from each
  other; a VPN on either device can also get in the way. For a cloud server,
  check that it's running (`difu server list` on your Mac) and that its
  firewall allows difu's port (3438 by default).
- **Local Network permission**: if you tapped **Don't Allow**, turn it on in
  iOS **Settings › Privacy & Security › Local Network › difu**.
- **macOS firewall**: when the Mac asks whether `difu` may accept incoming
  network connections, choose **Allow**. If you denied it, change it in
  **System Settings › Network › Firewall › Options**.
- **Remote control off**: difu only listens after `/remote-control` was
  enabled, and keeps listening until you disable it there.
- **"This phone is no longer paired"**: the Mac or server forgot this phone.
  Pair again.
- **Certificate mismatch**: the remote-control certificate changed, for
  example because difu's `agents/remote/` folder was deleted. Pair again.
- **Wrong server password**: repeated failures are rate-limited for a while.
  Change the password with `difu remote password` on the server.

## Project layout

| Path | What it is |
| --- | --- |
| `Difu/` | The SwiftUI app (a synchronized folder: new files are picked up automatically). |
| `Config/Info.plist` | Camera, Face ID, Local Network and ATS keys, and the `difu://` URL scheme. Merged with the keys Xcode generates. ATS is relaxed (`NSAllowsArbitraryLoads`) because difu servers use self-signed certificates on arbitrary hosts, which the app pins itself; the app makes no other web requests. For App Store or TestFlight review, the justification is: "connects only to user-owned servers at user-entered addresses, using self-signed certificates pinned by SHA-256 fingerprint." |
| `DifuKit/` | Swift package with everything that doesn't need UIKit: protocol models, pairing links, addresses and password pairing, the saved-server Keychain store, the pinned WebSocket client, transcript delta application, Markdown and diff helpers. Builds and tests on macOS. |
| `DifuKit/Sources/difu-remote-probe/` | A command-line client for testing a real difu from the Mac. |
| `DifuKit/Sources/DifuKit/Resources/themes.json` | The terminal's themes (`cli/src/theme.rs`), which a Rust test keeps identical. |
| `scripts/` | `app-icons.sh` renders the app icons from `themes.json` with `app-icon.swift`. |

## Development

Test the shared logic on the Mac:

```sh
cd app/ios/DifuKit
swift build
swift test
```

Build the app from the command line (the iOS platform must be installed):

```sh
xcodebuild -project app/ios/Difu.xcodeproj -scheme Difu \
  -destination 'generic/platform=iOS' CODE_SIGNING_ALLOWED=NO build
```

### Themes and icons

The themes come from the terminal. After changing `cli/src/theme.rs`, update
the phone's copy and the icons from the repository root (`cargo test` fails
until `themes.json` matches, and `swift test` until every theme has an icon):

```sh
(cd cli && DIFU_UPDATE_THEMES_JSON=1 cargo test iphone_themes_json)
app/ios/scripts/app-icons.sh
```

`app-icons.sh` renders the primary icon (Default) and an `AppIcon-<id>` set
per theme into `Difu/Assets.xcassets`: opaque 1024×1024 PNGs of "difu" in the
theme's accent on its background. The build includes every icon set
(`ASSETCATALOG_COMPILER_INCLUDE_ALL_APPICON_ASSETS`), so iOS can switch to it.

### Probing a real difu

`difu-remote-probe` pairs like a phone and exercises the protocol, printing
clear errors (TLS certificate mismatch, denied, per-host timeouts). It stores
its token in `~/.difu-remote-probe.json` (override with `--store <path>` or
`DIFU_PROBE_STORE`).

```sh
cd app/ios/DifuKit
swift run difu-remote-probe pair 'difu://pair?v=1&n=…'   # quote the link
swift run difu-remote-probe join 203.0.113.5            # server password (prompt or DIFU_PROBE_PASSWORD)
swift run difu-remote-probe info
swift run difu-remote-probe sessions
swift run difu-remote-probe watch <session-id> 15       # events for 15 s
swift run difu-remote-probe send <session-id> "Run the tests" --queue
swift run difu-remote-probe inbox --refresh
swift run difu-remote-probe overview opencx-labs/difu 45
swift run difu-remote-probe call agents.models          # any method, raw JSON
swift run difu-remote-probe call agents.defaults.set '{"model":"claude/opus","effort":"xhigh"}'
swift run difu-remote-probe call agents.defaults.set '{"provider":null,"model":null,"effort":null,"codexPreset":"Auto"}'
swift run difu-remote-probe call agents.new '{"repository":null,"startingMode":"plan","text":"Plan the change"}'
swift run difu-remote-probe call session.commands '{"id":"…"}'
swift run difu-remote-probe call session.command '{"id":"…","name":"/compact"}'
swift run difu-remote-probe call session.entry '{"id":"…","entry":"item_12"}'
swift run difu-remote-probe call session.shells '{"id":"…"}'
swift run difu-remote-probe call session.shell.write '{"id":"…","shell":"difu-shell-…","text":"ls","enter":true}'
swift run difu-remote-probe call folders.list
swift run difu-remote-probe forget --revoke             # device.unpair, then delete the token
```
