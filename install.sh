#!/bin/sh
# Installs the latest spot release: curl -fsSL https://raw.githubusercontent.com/boopdotpng/spot/master/install.sh | sh
# Spot.app goes to ~/Applications (SPOT_APP_DIR to change) and `spot` to ~/.local/bin, linked to
# the binary inside the app (SPOT_INSTALL_DIR to change). spot starts at login, without its
# window, so ⌥Space works from the start (`--no-login` leaves that out).
# `--skill` also copies the Claude Code skill to ~/.claude/skills/spot, replacing any copy there.
set -eu

skill=0
login=1
for arg in "$@"; do
  case "$arg" in
    --skill) skill=1 ;;
    --no-login) login=0 ;;
    --launcher) ;; # the old ⌥Space launcher is part of spot now
    *) echo "spot install: unknown option $arg (only --skill and --no-login)" >&2; exit 2 ;;
  esac
done

if [ "$(uname -s)-$(uname -m)" != Darwin-arm64 ]; then
  echo "spot: prebuilt for Apple Silicon Macs only" >&2
  echo "build it instead: git clone https://github.com/boopdotpng/spot && spot/macos/bundle.sh" >&2
  exit 1
fi

apps="${SPOT_APP_DIR:-$HOME/Applications}"
dir="${SPOT_INSTALL_DIR:-$HOME/.local/bin}"
releases="https://github.com/boopdotpng/spot/releases/latest/download"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

url="$releases/Spot.zip"
echo "downloading $url"
# fetched with curl, so macOS doesn't quarantine it and its ad hoc signature is enough
curl -fsSL "$url" -o "$tmp/spot.zip"
ditto -x -k "$tmp/spot.zip" "$tmp/app"
mkdir -p "$apps" "$dir"
rm -rf "$apps/Spot.app"
mv "$tmp/app/Spot.app" "$apps/"
rm -f "$dir/spot"
ln -s "$apps/Spot.app/Contents/MacOS/spot" "$dir/spot"
echo "installed $apps/Spot.app, and $dir/spot"

# the separate ⌥Space launcher app from before is part of spot now
old="${SPOT_LAUNCHER_DIR:-$HOME/Applications}/Spot Launcher.app"
if [ -d "$old" ]; then
  pkill -x SpotLauncher 2>/dev/null || true
  rm -rf "$old"
  echo "removed the old Spot Launcher.app"
fi

agent="$HOME/Library/LaunchAgents/com.github.boopdotpng.spot.plist"
if [ "$login" = 1 ]; then
  mkdir -p "$(dirname "$agent")"
  cat > "$agent" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.github.boopdotpng.spot</string>
  <key>ProgramArguments</key><array><string>$apps/Spot.app/Contents/MacOS/spot</string><string>--app</string></array>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict></plist>
PLIST
  echo "spot starts at login (remove $agent to stop that)"
fi

if [ "$skill" = 1 ]; then
  url="https://raw.githubusercontent.com/boopdotpng/spot/master/docs/skills/spot/SKILL.md"
  echo "downloading $url"
  # spot is in already, so a failed download here shouldn't stop the rest
  if curl -fsSL "$url" -o "$tmp/SKILL.md"; then
    mkdir -p "$HOME/.claude/skills/spot"
    mv "$tmp/SKILL.md" "$HOME/.claude/skills/spot/SKILL.md"
    echo "installed the Claude Code skill to ~/.claude/skills/spot"
  else
    echo "note: couldn't download the Claude Code skill; skipped" >&2
  fi
fi

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "note: $dir is not on your PATH; add it, e.g. echo 'export PATH=\"$dir:\$PATH\"' >> ~/.zshrc" ;;
esac
if "$dir/spot" status >/dev/null 2>&1; then
  echo "an older spot is running: spot quit, then spot"
else
  echo "run spot to open it (the first time, it logs you in)"
fi
