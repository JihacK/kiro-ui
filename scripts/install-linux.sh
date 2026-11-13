#!/usr/bin/env sh
# Build the native desktop app and install it for the current user:
#   ~/.local/bin/kiro-ui, a desktop entry and an icon. No root needed.
# With --with-server the binary also has browser mode (kiro-ui --server);
# that needs the wasm toolchain, see README.
set -eu
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--with-server" ]; then
  cargo xtask web
  cargo build --release -p kiro-ui --features server
else
  cargo build --release -p kiro-ui
fi

prefix="${PREFIX:-$HOME/.local}"
install -Dm755 target/release/kiro-ui "$prefix/bin/kiro-ui"
install -Dm644 packaging/linux/kiro-ui.desktop "$prefix/share/applications/kiro-ui.desktop"
install -Dm644 build/icons/256x256.png "$prefix/share/icons/hicolor/256x256/apps/kiro-ui.png"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$prefix/share/applications" || true

echo "Installed $prefix/bin/kiro-ui ($(du -h target/release/kiro-ui | cut -f1))"
