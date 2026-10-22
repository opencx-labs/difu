#!/bin/sh
set -eu
# Build on the release runner; customers never need Swift or an extra install.
arch=${1:?usage: build-notifications.sh arm64\|x86_64 output-directory}
out=${2:?missing output directory}
case "$arch" in arm64|x86_64) ;; *) echo 'unsupported architecture' >&2; exit 2;; esac
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
app="$out/DifuNotifications.app"
mkdir -p "$app/Contents/MacOS"
cp "$root/native/notifications/Info.plist" "$app/Contents/Info.plist"
xcrun swiftc -target "$arch-apple-macosx11.0" -O -framework AppKit -framework UserNotifications "$root/native/notifications/main.swift" -o "$app/Contents/MacOS/DifuNotifications"
codesign --force --sign - "$app"
