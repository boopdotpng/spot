#!/bin/sh
# Builds Spot.app around the release binary and installs it to ~/Applications (SPOT_APP_DIR to
# change), with `spot` on your PATH linked to the binary inside it (SPOT_INSTALL_DIR, default
# ~/.local/bin). `bundle.sh --out DIR` only builds the app, into DIR (what the release workflow
# does); `--bin PATH` uses a binary already built instead of running cargo.
set -eu
out="" bin=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out="$2"; shift 2 ;;
    --bin) bin="$2"; shift 2 ;;
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
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>com.github.boopdotpng.spot</string>
  <key>CFBundleName</key><string>spot</string>
  <key>CFBundleDisplayName</key><string>spot</string>
  <key>CFBundleExecutable</key><string>spot</string>
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
echo "if an older spot is running: spot quit, then spot"
