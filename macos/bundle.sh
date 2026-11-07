#!/bin/sh
# Builds Spot.app around the release binary and installs it to ~/Applications (SPOT_APP_DIR to
# change), with `spot` on your PATH linked to the binary inside it (SPOT_INSTALL_DIR, default
# ~/.local/bin), and has spot start at login without its window, so ⌥Space works from the start
# (`--no-login` leaves that out). `bundle.sh --out DIR` only builds the app, into DIR (what the
# release workflow does); `--bin PATH` uses a binary already built instead of running cargo.
set -eu
out="" bin="" login=1
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out="$2"; shift 2 ;;
    --bin) bin="$2"; shift 2 ;;
    --no-login) login=0; shift ;;
    *) echo "bundle.sh: unknown option $1" >&2; exit 2 ;;
  esac
done
root="$(cd "$(dirname "$0")/.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
if [ -z "$bin" ]; then
  (cd "$root" && cargo build --release --locked)
  bin="$root/target/release/spot"
fi
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
app="$tmp/Spot.app"
mkdir -p "$app/Contents/MacOS"
cp "$bin" "$app/Contents/MacOS/spot"
mkdir -p "$app/Contents/Resources"
cp "$root/macos/icon/spot.icns" "$app/Contents/Resources/spot.icns" # drawn in macos/icon/icon.svg
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>com.github.boopdotpng.spot</string>
  <key>CFBundleName</key><string>spot</string>
  <key>CFBundleDisplayName</key><string>spot</string>
  <key>CFBundleExecutable</key><string>spot</string>
  <key>CFBundleIconFile</key><string>spot</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
</dict></plist>
PLIST
codesign --force --sign - "$app" >/dev/null 2>&1 || true

if [ -n "$out" ]; then
  mkdir -p "$out"
  rm -rf "$out/Spot.app"
  mv "$app" "$out/"
  echo "built $out/Spot.app"
  exit
fi
dest="${SPOT_APP_DIR:-$HOME/Applications}"
mkdir -p "$dest"
# a running spot keeps playing from the old copy until it's quit; replace the files, then say so
rm -rf "$dest/Spot.app"
mv "$app" "$dest/"
bindir="${SPOT_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$bindir"
rm -f "$bindir/spot"
ln -s "$dest/Spot.app/Contents/MacOS/spot" "$bindir/spot"
echo "installed $dest/Spot.app, and $bindir/spot linked to it"

# the separate ⌥Space launcher app from before 2.0 is part of spot now
old="$dest/Spot Launcher.app"
if [ -d "$old" ]; then
  pkill -x SpotLauncher 2>/dev/null || true
  rm -rf "$old"
  echo "removed the old Spot Launcher.app"
fi

agent="$HOME/Library/LaunchAgents/com.github.boopdotpng.spot.plist"
if [ "$login" = 1 ]; then
  mkdir -p "$(dirname "$agent")"
  cat > "$agent" <<AGENT
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.github.boopdotpng.spot</string>
  <key>ProgramArguments</key><array><string>$dest/Spot.app/Contents/MacOS/spot</string><string>--app</string></array>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict></plist>
AGENT
  echo "spot starts at login (remove $agent to stop that)"
fi

case ":$PATH:" in
  *":$bindir:"*) ;;
  *) echo "note: $bindir is not on your PATH; add it, e.g. echo 'export PATH=\"$bindir:\$PATH\"' >> ~/.zshrc" ;;
esac
if "$bindir/spot" status >/dev/null 2>&1; then
  echo "an older spot is running: spot quit, then spot"
else
  echo "run spot to open it (the first time, it logs you in)"
fi
