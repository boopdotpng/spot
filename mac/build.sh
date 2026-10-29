#!/bin/sh
# Builds Spot Launcher.app (⌥Space from anywhere: search and play through the running spot) and
# installs it to ~/Applications. It adds itself to Login Items on first launch.
# `build.sh --out DIR` only builds it, into DIR (what the release workflow does).
set -eu
out=""
if [ "${1:-}" = --out ]; then
  case "$2" in
    /*) out="$2" ;;
    *) out="$PWD/$2" ;;  # relative to where it was run, not mac/
  esac
fi
cd "$(dirname "$0")"
app="${SPOT_LAUNCHER_DIR:-$HOME/Applications}/Spot Launcher.app"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' ../Cargo.toml)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# macOS 14 and up, whatever this Mac runs (Liquid Glass on 26+, decided at run time)
swiftc -O -target arm64-apple-macos14 -o "$tmp/SpotLauncher" Launcher.swift 2>&1 | grep -v '^$' || true
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
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
</dict></plist>
PLIST
# ad hoc: enough for a build of your own, or one fetched with curl (install.sh), which macOS
# doesn't quarantine; a browser download would need a Developer ID signature
codesign -s - --force "$tmp/Spot Launcher.app"

if [ -n "$out" ]; then
  mkdir -p "$out"
  rm -rf "$out/Spot Launcher.app"
  mv "$tmp/Spot Launcher.app" "$out/"
  echo "built $out/Spot Launcher.app"
  exit
fi

pkill -x SpotLauncher 2>/dev/null || true
# wait for the old one to exit, or `open` fails (-600)
for _ in 1 2 3 4 5 6 7 8 9 10; do pgrep -x SpotLauncher >/dev/null || break; sleep 0.2; done
mkdir -p "$(dirname "$app")"
rm -rf "$app"
mv "$tmp/Spot Launcher.app" "$app"
open "$app"
echo "installed $app; ⌥Space opens it"
