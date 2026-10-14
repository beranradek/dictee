# Dictee

Simple local speech-to-text dictation for Linux (Ubuntu / Fedora, Wayland first). Inspired by Mluva, without cloud STT.

Press a shortcut, speak, stop. The text goes to the clipboard (optionally auto-pasted), so it works in any app or terminal.

## Features

- Local Whisper models (whisper.cpp), Czech and English, automatic language detection or a fixed language
- Global shortcut through the xdg-desktop-portal GlobalShortcuts API, with hold-to-talk (stop on key release) or toggle
- Fallback for toggle mode: GNOME custom keybinding running `dictee toggle`
- Small floating bubble while dictating (see the focus note below)
- Optional final text improvement with the `claude` or `codex` CLI (your own login, no API key in this app)
- Known phrases (names of projects, repos, programs, people): given to Whisper as a prompt and to the rewrite LLM
- Settings window: microphone (with level test), model (download, folder), language, shortcuts, output, rewrite, phrases

## Install

```sh
bash install.sh
dictee settings
```

Build needs: Rust, cmake, a C++ compiler, libclang, GTK 4 and libadwaita dev files. Runtime needs: PipeWire (`pw-record`, `pw-dump`),
`wl-clipboard`. For auto-paste also `ydotool` (running `ydotoold`, access to `/dev/uinput`).

1. Open **Dictee** from the app menu, download a model (Small is the default, Turbo compressed is best for Czech).
2. Confirm the shortcut in the system dialog (default suggestion Ctrl+Shift+H). If it does not work, use the **Install** button for the
   GNOME keybinding fallback (Ctrl+Shift+H).
3. Dictate: press the shortcut, speak, press it again. The text is on the clipboard: Ctrl+V.

Config: `~/.config/dictee/config.toml`. Models: `~/.local/share/dictee/models` (change the folder in settings).

## Commands

```
dictee                 run the daemon (autostart does this)
dictee settings        open settings (starts the daemon if needed)
dictee toggle|start|stop|cancel|status|quit
dictee transcribe f.wav [--rewrite]
dictee models [--download small]
dictee mics
```

## Not supported yet

- Only Whisper (GGML) models. Parakeet (ONNX) and Qwen3-ASR (llama.cpp) are not supported, and Mluva's model files cannot be reused.
- No remote STT (by design for phase 1).

## Wayland notes

- Wayland does not let apps type into other windows, so the default is clipboard only. Auto-paste uses `ydotool` and is off by default.
- GNOME has no always-on-top overlay for normal apps. The bubble is a normal window and takes keyboard focus while visible. Dictee hides it
  before copying, so GNOME returns focus to your previous window. Never click it to paste. Turn the bubble off in settings if you dislike it.
  The result and errors are shown as desktop notifications, not in the bubble.
- Not tested: KDE, Sway and Hyprland. Fedora was not tested.

## Tests

```sh
cargo test                # unit tests
tests/e2e.sh              # needs the daemon running and a downloaded model; plays espeak-ng speech through a virtual microphone
```

`tests/audio/*.wav` were made with `espeak-ng` (English and Czech). The robotic voice gives imperfect raw text, so the tests check key words.
