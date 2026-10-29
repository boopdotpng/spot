#!/bin/sh
# Builds Spot Launcher.app (⌥Space from anywhere: search and play through the running spot) and
# installs it to ~/Applications. It adds itself to Login Items on first launch.
set -eu
cd "$(dirname "$0")"
app="${SPOT_LAUNCHER_DIR:-$HOME/Applications}/Spot Launcher.app"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

swiftc -O -o "$tmp/SpotLauncher" Launcher.swift 2>&1 | grep -v '^$' || true
test -x "$tmp/SpotLauncher"

mkdir -p "$tmp/Spot Launcher.app/Contents/MacOS"
mv "$tmp/SpotLauncher" "$tmp/Spot Launcher.app/Contents/MacOS/"
cat > "$tmp/Spot Launcher.app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>com.github.boopdotpng.spot.launcher</string>
  <key>CFBundleName</key><string>Spot Launcher</string>
  <key>CFBundleExecutable</key><string>SpotLauncher</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>1.0</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
</dict></plist>
PLIST
codesign -s - --force "$tmp/Spot Launcher.app"

pkill -x SpotLauncher 2>/dev/null || true
mkdir -p "$(dirname "$app")"
rm -rf "$app"
mv "$tmp/Spot Launcher.app" "$app"
open "$app"
echo "installed $app; ⌥Space opens it"
