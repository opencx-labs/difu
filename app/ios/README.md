# difu for iOS

A native iPhone companion for [difu](../../README.md). Pair it with difu on
your Mac and control everything over your Wi-Fi: watch and steer coding
agents, answer their questions and approvals, start new sessions, and review
GitHub pull requests (overview, diff, guide, comment, approve, merge).

The phone talks to difu's background agent service using the protocol in
[`docs/remote-protocol.md`](../../docs/remote-protocol.md): a WebSocket over TLS,
pinned to the certificate fingerprint in the pairing QR code.

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

Pairing codes are single-use and expire after 10 minutes. The phone then
reconnects by itself whenever you open the app. To switch computers or stop,
open **Settings** in the app. To revoke the phone's access, remove it in
`/remote-control` on the Mac.

## Troubleshooting

- **"Can't reach your Mac — same Wi-Fi?"**: the phone and Mac must be on the
  same network. Guest networks and some office networks isolate devices from
  each other; a VPN on either device can also get in the way.
- **Local Network permission**: if you tapped **Don't Allow**, turn it on in
  iOS **Settings › Privacy & Security › Local Network › difu**.
- **macOS firewall**: when the Mac asks whether `difu` may accept incoming
  network connections, choose **Allow**. If you denied it, change it in
  **System Settings › Network › Firewall › Options**.
- **Remote control off**: difu only listens after `/remote-control` was
  enabled, and keeps listening until you disable it there.
- **"This phone is no longer paired"**: the Mac forgot this phone. Scan the
  QR code again.
- **Certificate mismatch**: the Mac's remote-control certificate changed, for
  example because its `agents/remote/` folder was deleted. Pair again.

## Project layout

| Path | What it is |
| --- | --- |
| `Difu/` | The SwiftUI app (a synchronized folder: new files are picked up automatically). |
| `Config/Info.plist` | Camera, Local Network and ATS keys, and the `difu://` URL scheme. Merged with the keys Xcode generates. |
| `DifuKit/` | Swift package with everything that doesn't need UIKit: protocol models, pairing-link parsing, the pinned WebSocket client, transcript delta application, Markdown and diff helpers. Builds and tests on macOS. |
| `DifuKit/Sources/difu-remote-probe/` | A command-line client for testing a real difu from the Mac. |

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

### Probing a real difu

`difu-remote-probe` pairs like a phone and exercises the protocol, printing
clear errors (TLS certificate mismatch, denied, per-host timeouts). It stores
its token in `~/.difu-remote-probe.json` (override with `--store <path>` or
`DIFU_PROBE_STORE`).

```sh
cd app/ios/DifuKit
swift run difu-remote-probe pair 'difu://pair?v=1&n=…'   # quote the link
swift run difu-remote-probe info
swift run difu-remote-probe sessions
swift run difu-remote-probe watch <session-id> 15       # events for 15 s
swift run difu-remote-probe send <session-id> "Run the tests" --queue
swift run difu-remote-probe inbox --refresh
swift run difu-remote-probe overview opencx-labs/difu 45
swift run difu-remote-probe call agents.models          # any method, raw JSON
swift run difu-remote-probe call session.entry '{"id":"…","entry":"item_12"}'
swift run difu-remote-probe forget
```
