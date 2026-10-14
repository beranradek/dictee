#!/usr/bin/env bash
# Builds Dictee and installs it for the current user (binary, app menu entry, autostart).
set -euo pipefail
cd "$(dirname "$0")"

missing=()
for c in cargo cmake pkg-config pw-record wl-copy; do command -v "$c" >/dev/null || missing+=("$c"); done
pkg-config --exists gtk4 libadwaita-1 || missing+=("gtk4/libadwaita dev files")
if [ ${#missing[@]} -gt 0 ]; then
  echo "Missing: ${missing[*]}"
  echo "Ubuntu: sudo apt install build-essential cmake pkg-config libclang-dev libgtk-4-dev libadwaita-1-dev pipewire-bin wl-clipboard"
  echo "Fedora: sudo dnf install gcc-c++ cmake pkgconf clang-devel gtk4-devel libadwaita-devel pipewire-utils wl-clipboard"
  echo "Rust:   https://rustup.rs"
  exit 1
fi

cargo build --release
install -Dm755 target/release/dictee "$HOME/.local/bin/dictee"
desktop="$HOME/.local/share/applications/cz.etnetera.Dictee.desktop"
mkdir -p "$(dirname "$desktop")" "$HOME/.config/autostart"
sed "s|@BIN@|$HOME/.local/bin/dictee|" data/cz.etnetera.Dictee.desktop > "$desktop"
# Autostart the daemon in the background (no window)
sed "s|Exec=.*settings|Exec=$HOME/.local/bin/dictee daemon|" "$desktop" > "$HOME/.config/autostart/cz.etnetera.Dictee.desktop"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
echo "Installed. Start it: dictee settings   (download a model there, then set your shortcut)"
