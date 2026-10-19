#!/bin/sh
# Installs the latest spot release: curl -fsSL https://raw.githubusercontent.com/boopdotpng/spot/master/install.sh | sh
# SPOT_INSTALL_DIR overrides where it goes (default ~/.local/bin).
set -eu

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Linux-x86_64 | Linux-amd64) target=x86_64-unknown-linux-gnu ;;
  *)
    echo "spot: no prebuilt binary for $(uname -s) $(uname -m) (only Apple Silicon macOS and x86_64 Linux)" >&2
    echo "build it instead: cargo install --locked --git https://github.com/boopdotpng/spot" >&2
    exit 1 ;;
esac

dir="${SPOT_INSTALL_DIR:-$HOME/.local/bin}"
url="https://github.com/boopdotpng/spot/releases/latest/download/spot-$target.tar.gz"
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
fi
