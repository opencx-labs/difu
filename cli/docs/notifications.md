# Service notifications

The agents service observes new successful turns, pending question/approval IDs, and failures. Native delivery and audio run on its notification worker, outside session locks. Closing terminal windows does not stop the service. Defaults remain in-app delivery, background sessions, sound off, Quiet off, and all three event types on.

Only the session title and event type enter notification content. Provider turns use session/thread/turn identity; question IDs additionally include their turn and method. Guide/Conflict jobs use the persisted session version of a new Completed edge because those jobs do not have provider turn IDs. Provider success evidence is recorded separately from Ready/Idle: cancellation that sends queued work is still silent.

Consumed identities are saved in `agents/notifications/dispatched.json` before policy filtering and external delivery. This is at-most-once: a crash between marking and delivery can lose an alert. Suppressed/failed alerts are not retried or replayed. Restoration completes before observations begin. In-app feed cursors are scoped to a service instance, reconnects establish a fresh baseline, and the in-memory feed retains at most 256 events. A malformed marker file disables automatic delivery and reports the error instead of replaying history.

Clients renew monotonic presence leases every two seconds; leases expire after six seconds. The main event loop renews during continuous input and with animation disabled. Background sessions excludes a conversation visible in any focused client. Reviews, Settings, palette, Changes and full Main helpers do not mark a remembered conversation as viewed. Right/Bottom helpers leave the conversation visible. App unfocused treats any focused live client as focused, including an overlay.

Preferences remain drafts until Save. Only a successful explicit change from In-app to native delivery requests authorization; startup, refresh, failed Save, no-op Save and Discard never do. Test uses draft choices without saving or changing a session. It bypasses session scope because it is synthetic, but respects Quiet, sound off and a disabled selected event. Testing Sound uses standalone audio and never creates a desktop alert. Settings shows permission, capability, delivery error and silent-preview state.

## macOS helper contract

Release runners build `DifuNotifications.app` with `sh cli/scripts/build-notifications.sh arm64 OUTPUT` or `x86_64 OUTPUT`. The deployment target and plist minimum are macOS 11.0. The bundle identifier is `app.difu.notifications`; its executable is `Contents/MacOS/DifuNotifications`. The build script performs ad-hoc signing; final distribution signing, archive/formula changes and final Mach-O inspection belong to release packaging. Customers must receive the compiled app, never a runtime compiler requirement.

Runtime canonicalizes `current_exe` before checking these locations:

- Archive: `difu` and `DifuNotifications.app` in the same directory.
- Homebrew: canonical Cellar `bin/difu` and sibling `libexec/DifuNotifications.app` (works through an external bin symlink and after relocation).

The helper reads one JSON object from stdin (`action`, optional `title`, optional `event`) and writes capability/error JSON. Actions are `status`, `authorize`, `deliver`, and `sound`. Only `authorize` calls UserNotifications authorization. Delivery uses UserNotifications without its built-in sound; standalone preview and real sound both use AppKit NSSound `Glass`. The helper remains alive until playback finishes, with a bounded timeout. Rust bounds helper processes and owns/reaps their process groups. Missing bundles, denial, malformed replies and playback errors remain visible. The Codex voice helper contract is unchanged.

## Linux

`notify-rust` uses its pure Rust `z`/zbus backend; libdbus is not required. Native notifications always carry `suppress-sound=true`, so they do not duplicate standalone audio. D-Bus capabilities are queried without sending an alert.

Standalone audio uses optional `paplay` (PulseAudio/PipeWire compatibility) with a generated mono 44.1kHz PCM WAV supplied through stdin. Both preview and real delivery use this path. The process has a five-second bound and no persisted audio asset. A missing player, missing audio server or playback failure is reported; no package is installed automatically. Sound support in a desktop notification server is not treated as standalone audio support.

## Verification boundaries

Automated tests use isolated storage, fake authorization/delivery, an exact helper process transport, fake audio executables, and private D-Bus servers. They never request actual macOS authorization, send a real desktop alert or play sound. Build verification proves compilation and deployment target, not visible/audible OS presentation. Release packaging must verify both architecture artifacts and actual helper placement; interactive OS presentation requires an explicitly authorized manual test.

Primary API references: [Apple UserNotifications](https://developer.apple.com/documentation/usernotifications/unusernotificationcenter), [NSSound](https://developer.apple.com/documentation/appkit/nssound), [freedesktop notification protocol](https://specifications.freedesktop.org/notification/latest/protocol.html), [notify-rust](https://docs.rs/notify-rust/4.18.2/notify_rust/), [paplay](https://manpages.ubuntu.com/manpages/noble/man1/paplay.1.html).
