#!/bin/sh
# Builds and installs spot: the binary to ~/.local/bin/spot (SPOT_INSTALL_DIR to change) and, on a
# Mac with the Swift compiler, the ⌥Space launcher to ~/Applications/Spot Launcher.app. A player
# already running is restarted on the new build, playing on if it was.
#   --bin PATH       install a spot binary already built instead of running cargo
#   --no-launcher    leave the launcher out
set -eu
bin=""
launcher=yes
while [ $# -gt 0 ]; do
  case "$1" in
    --bin) bin="$2"; shift 2 ;;
    --no-launcher) launcher=no; shift ;;
    *) echo "install.sh: unknown option $1" >&2; exit 2 ;;
  esac
done
root="$(cd "$(dirname "$0")" && pwd)"
if [ -z "$bin" ]; then
  (cd "$root" && cargo build --release --locked)
  bin="$root/target/release/spot"
fi

# spot
bindir="${SPOT_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$bindir"
was=""
if "$bindir/spot" status >/dev/null 2>&1; then
  # what the old player was doing, to carry on with on the new one
  was="$("$bindir/spot" status 2>/dev/null | head -1 | cut -d: -f1)"
  "$bindir/spot" quit >/dev/null 2>&1 || true
fi
# rm first: overwriting a binary in place makes macOS kill it (stale code signature)
rm -f "$bindir/spot"
cp "$bin" "$bindir/spot"
echo "installed $bindir/spot"
if [ -n "$was" ]; then
  "$bindir/spot" start >/dev/null
  [ "$was" = playing ] && "$bindir/spot" set paused=no >/dev/null 2>&1 || true
  echo "restarted the player on the new build (a TUI that was open needs \`spot\` again)"
fi

# the launcher: macOS 14 and up, Liquid Glass on 26 and up (decided when it runs)
if [ "$launcher" = yes ] && [ "$(uname)" = Darwin ]; then
  if ! command -v swiftc >/dev/null 2>&1; then
    echo "skipped the ⌥Space launcher: no swiftc (xcode-select --install)"
  else
    app="$HOME/Applications/Spot Launcher.app"
    version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml")"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    contents="$tmp/Spot Launcher.app/Contents"
    mkdir -p "$contents/MacOS"
    swiftc -O -swift-version 5 -target arm64-apple-macos14 -o "$contents/MacOS/SpotLauncher" "$root/mac/Launcher.swift"
    cat > "$contents/Info.plist" <<PLIST
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
    # ad hoc: enough for an app built on this Mac
    codesign -s - --force "$tmp/Spot Launcher.app" 2>/dev/null
    pkill -x SpotLauncher 2>/dev/null || true
    # wait for the old one to exit, or `open` fails (-600)
    for _ in 1 2 3 4 5 6 7 8 9 10; do pgrep -x SpotLauncher >/dev/null || break; sleep 0.2; done
    mkdir -p "$(dirname "$app")"
    rm -rf "$app"
    mv "$tmp/Spot Launcher.app" "$app"
    open "$app"
    echo "installed $app: ⌥Space opens it (it starts at login)"
  fi
fi

case ":$PATH:" in
  *":$bindir:"*) ;;
  *) echo "note: $bindir is not on your PATH; add it, e.g. echo 'export PATH=\"$bindir:\$PATH\"' >> ~/.zshrc" ;;
esac
[ -z "$was" ] && echo "run spot in a terminal to open it (the first time, it logs you in)"
exit 0
