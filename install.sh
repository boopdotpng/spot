#!/bin/sh
# Installs the latest spot release: curl -fsSL https://raw.githubusercontent.com/boopdotpng/spot/master/install.sh | sh
# SPOT_INSTALL_DIR overrides where it goes (default ~/.local/bin).
# On macOS, `| sh -s -- --launcher` also installs the ⌥Space launcher app to ~/Applications
# (SPOT_LAUNCHER_DIR to change); once it's installed, running this again updates it too.
set -eu

launcher=0
for arg in "$@"; do
  case "$arg" in
    --launcher) launcher=1 ;;
    *) echo "spot install: unknown option $arg (only --launcher)" >&2; exit 2 ;;
  esac
done

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Linux-x86_64 | Linux-amd64) target=x86_64-unknown-linux-gnu ;;
  *)
    echo "spot: no prebuilt binary for $(uname -s) $(uname -m) (only Apple Silicon macOS and x86_64 Linux)" >&2
    echo "build it instead: cargo install --locked --git https://github.com/boopdotpng/spot" >&2
    exit 1 ;;
esac

dir="${SPOT_INSTALL_DIR:-$HOME/.local/bin}"
releases="https://github.com/boopdotpng/spot/releases/latest/download"
url="$releases/spot-$target.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "downloading $url"
curl -fsSL "$url" | tar -xz -C "$tmp"
mkdir -p "$dir"
# rm first: overwriting a binary in place makes macOS kill it (stale code signature)
rm -f "$dir/spot"
mv "$tmp/spot" "$dir/spot"
chmod +x "$dir/spot"
echo "installed spot to $dir/spot"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "note: $dir is not on your PATH; add it, e.g. echo 'export PATH=\"$dir:\$PATH\"' >> ~/.zshrc" ;;
esac
if [ "$target" = x86_64-unknown-linux-gnu ]; then
  echo "linux: needs ALSA, OpenSSL 3 and D-Bus libraries (libasound2, libssl3, libdbus-1-3)"
  if [ "$launcher" = 1 ]; then
    echo "note: the launcher is a macOS app; skipped" >&2
  fi
  exit
fi

app="${SPOT_LAUNCHER_DIR:-$HOME/Applications}/Spot Launcher.app"
if [ "$launcher" = 1 ] || [ -d "$app" ]; then
  url="$releases/Spot-Launcher.zip"
  echo "downloading $url"
  # fetched with curl, so macOS doesn't quarantine it and its ad hoc signature is enough
  curl -fsSL "$url" -o "$tmp/launcher.zip"
  ditto -x -k "$tmp/launcher.zip" "$tmp/launcher"
  pkill -x SpotLauncher 2>/dev/null || true
  # wait for the old one to exit, or `open` fails (-600)
  for _ in 1 2 3 4 5 6 7 8 9 10; do pgrep -x SpotLauncher >/dev/null || break; sleep 0.2; done
  mkdir -p "$(dirname "$app")"
  rm -rf "$app"
  mv "$tmp/launcher/Spot Launcher.app" "$app"
  open "$app"
  echo "installed $app: ⌥Space opens it (it adds itself to Login Items)"
fi
