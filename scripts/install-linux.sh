#!/usr/bin/env sh
# Build the native desktop app and install it for the current user:
#   ~/.local/bin/kiro-ui, a desktop entry and an icon. No root needed.
set -eu
cd "$(dirname "$0")/.."

cargo build --release -p kiro-ui-desktop

prefix="${PREFIX:-$HOME/.local}"
install -Dm755 target/release/kiro-ui "$prefix/bin/kiro-ui"
install -Dm644 packaging/linux/kiro-ui.desktop "$prefix/share/applications/kiro-ui.desktop"
install -Dm644 build/icons/256x256.png "$prefix/share/icons/hicolor/256x256/apps/kiro-ui.png"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$prefix/share/applications" || true

echo "Installed $prefix/bin/kiro-ui ($(du -h target/release/kiro-ui | cut -f1))"
