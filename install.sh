#!/usr/bin/env bash
# Builds Dictee and installs it for the current user (binary, app menu entry, autostart).
set -euo pipefail
cd "$(dirname "$0")"

missing=()
for c in cargo cmake pkg-config pw-record pw-dump notify-send; do command -v "$c" >/dev/null || missing+=("$c"); done
command -v wl-copy >/dev/null || command -v xclip >/dev/null || command -v xsel >/dev/null || missing+=("wl-copy (wl-clipboard) or xclip")
pkg-config --exists gtk4 libadwaita-1 || missing+=("gtk4 and libadwaita development files")
if [ ${#missing[@]} -gt 0 ]; then
  echo "Missing: ${missing[*]}"
  echo "Ubuntu: sudo apt install build-essential cmake pkg-config libclang-dev libgtk-4-dev libadwaita-1-dev pipewire-bin wl-clipboard libnotify-bin"
  echo "Fedora: sudo dnf install gcc-c++ cmake pkgconf clang-devel gtk4-devel libadwaita-devel pipewire-utils wl-clipboard libnotify"
  echo "Rust:   https://rustup.rs"
  exit 1
fi

cargo build --release
bin="$HOME/.local/bin/dictee"
install -Dm755 target/release/dictee "$bin"

# The template has @BIN@; a path with spaces or special characters is quoted for the Exec line.
quoted="\"$bin\""
template=$(<data/cz.etnetera.Dictee.desktop)
app="$HOME/.local/share/applications/cz.etnetera.Dictee.desktop"
auto="$HOME/.config/autostart/cz.etnetera.Dictee.desktop"
mkdir -p "$(dirname "$app")" "$(dirname "$auto")"
printf '%s\n' "${template//@BIN@ settings/"$quoted" settings}" > "$app"
printf '%s\n' "${template//@BIN@ settings/"$quoted" daemon}" > "$auto"   # autostart: daemon without a window
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
echo "Installed. Start it: dictee settings   (download a model there, then set your shortcut)"
