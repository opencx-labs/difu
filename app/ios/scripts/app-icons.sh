#!/bin/sh
# Renders the primary app icon (the Default theme) and one alternate icon per
# theme (AppIcon-<id>) into Difu/Assets.xcassets from DifuKit's themes.json:
# the theme's accent on its background. Run from anywhere after the themes
# change: app/ios/scripts/app-icons.sh
set -eu
ios=$(cd "$(dirname "$0")/.." && pwd)
assets="$ios/Difu/Assets.xcassets"
themes="$ios/DifuKit/Sources/DifuKit/Themes"
build=$(mktemp -d)
trap 'rm -rf "$build"' EXIT

swiftc -O "$ios/scripts/app-icon.swift" -o "$build/app-icon"
swiftc -O -parse-as-library "$ios/scripts/theme-icons.swift" \
    "$themes/TerminalTheme.swift" "$themes/ThemePalette.swift" -o "$build/theme-icons"
"$build/theme-icons" "$ios/DifuKit/Sources/DifuKit/Resources/themes.json" > "$build/icons"

rm -rf "$assets"/AppIcon-*.appiconset
while read -r name text background; do
    mkdir -p "$assets/$name.appiconset"
    "$build/app-icon" "$assets/$name.appiconset/$name.png" "$text" "$background"
    cat > "$assets/$name.appiconset/Contents.json" <<JSON
{
  "images" : [
    {
      "filename" : "$name.png",
      "idiom" : "universal",
      "platform" : "ios",
      "size" : "1024x1024"
    }
  ],
  "info" : {
    "author" : "xcode",
    "version" : 1
  }
}
JSON
    echo "$name: #$text on #$background"
done < "$build/icons"
