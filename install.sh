#!/bin/sh
# Builds spot (a release build) and installs it to ~/.local/bin/spot (SPOT_INSTALL_DIR to change).
# `--bin PATH` installs a binary already built instead of running cargo. A Spot.app from 2.0 (the
# window version) is removed, with its login item: `spot` in a terminal is the TUI again.
set -eu
bin=""
while [ $# -gt 0 ]; do
  case "$1" in
    --bin) bin="$2"; shift 2 ;;
    *) echo "install.sh: unknown option $1" >&2; exit 2 ;;
  esac
done
root="$(cd "$(dirname "$0")" && pwd)"
if [ -z "$bin" ]; then
  (cd "$root" && cargo build --release --locked)
  bin="$root/target/release/spot"
fi

bindir="${SPOT_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$bindir"
# rm first: overwriting a binary in place makes macOS kill it (stale code signature), and the old
# `spot` may be a link into Spot.app
rm -f "$bindir/spot"
cp "$bin" "$bindir/spot"
echo "installed $bindir/spot"

# the window version (2.0): its app, its login item, and the ⌥Space launcher from before it
agent="$HOME/Library/LaunchAgents/com.github.boopdotpng.spot.plist"
if [ -f "$agent" ]; then
  launchctl bootout "gui/$(id -u)" "$agent" 2>/dev/null || true
  rm -f "$agent"
  echo "removed the login item"
fi
for app in "$HOME/Applications/Spot.app" "$HOME/Applications/Spot Launcher.app"; do
  if [ -d "$app" ]; then
    rm -rf "$app"
    echo "removed $app"
  fi
done

case ":$PATH:" in
  *":$bindir:"*) ;;
  *) echo "note: $bindir is not on your PATH; add it, e.g. echo 'export PATH=\"$bindir:\$PATH\"' >> ~/.zshrc" ;;
esac
if "$bindir/spot" status >/dev/null 2>&1; then
  echo "an older spot is running: spot quit, then spot"
else
  echo "run spot in a terminal to open it (the first time, it logs you in)"
fi
