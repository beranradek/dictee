# Dictee

Simple local speech-to-text dictation for Linux (Ubuntu / Fedora, Wayland first). Inspired by Mluva, without cloud STT.

Press a shortcut, speak, stop. The text goes to the clipboard (optionally auto-pasted), so it works in any app or terminal.

## Features

- Two local speech engines, Czech and English, automatic language detection or a fixed language:
  - **Whisper** (whisper.cpp, built in): Tiny to Large v3 Turbo, GGML models from Hugging Face
  - **Qwen3-ASR** on **llama.cpp**: a managed `llama-server` (pinned release b11011, CPU or Vulkan GPU) with 0.6B Q8, 1.7B Q4 (default) or
    1.7B Q8 models. Starts when a recording starts, stays loaded for a configurable time, listens on 127.0.0.1 only with a random key.
    You can point it to your own `llama-server` binary. Your phrases are given to it as context
- Global shortcut through the xdg-desktop-portal GlobalShortcuts API, with hold-to-talk (stop on key release) or toggle
- Fallback for toggle mode: one-click keybinding running `dictee toggle`, written for GNOME (gsettings) or KDE Plasma 5/6
  (launcher file + kglobalshortcutsrc). Any other desktop: add a custom shortcut that runs `dictee toggle`
- Small floating bubble while dictating (see the focus note below)
- Optional final text improvement, one of:
  - `claude` CLI (tools disabled, your own login)
  - `codex exec` CLI and **Codex app-server** (`codex app-server`): both run text-only, with every Codex tool and feature disabled
  - **OpenAI-compatible chat API** (LiteLLM, OpenAI, Ollama, llama.cpp ...): base URL, API key from an environment variable or a key file.
    Remote servers need https; plain http only for localhost
- Known phrases (names of projects, repos, programs, people): given to Whisper as a prompt and to the rewrite LLM
- Settings window: microphone (with level test), model (download, folder), language, shortcuts, output, rewrite, phrases

## Install

```sh
bash install.sh
dictee settings
```

Build needs: Rust, cmake, a C++ compiler, libclang, GTK 4 and libadwaita dev files. Runtime needs: PipeWire (`pw-record`, `pw-dump`),
`wl-clipboard` (or `xclip` on X11), `notify-send` (libnotify). Qwen3-ASR downloads its own `llama-server` (about 17 MB CPU or 30 MB Vulkan) on first use. For auto-paste also `ydotool` (running `ydotoold`, access to `/dev/uinput`).

1. Open **Dictee** from the app menu, choose the speech engine and download a model (Whisper Small is the default; Qwen3-ASR 1.7B Q4
   is Mluva's recommended model and about 1.6 GB).
2. Confirm the shortcut in the system dialog (default suggestion Ctrl+Shift+H). If it does not work, use the **Install** button for the
   GNOME or KDE keybinding fallback (it uses the shortcut from the settings field).
3. Dictate: press the shortcut, speak, press it again. The text is on the clipboard: Ctrl+V.

Config: `~/.config/dictee/config.toml`. Models: `~/.local/share/dictee/models` (change the folder in settings).

## How it behaves

- While a dictation is being transcribed (bubble visible), a new start (shortcut, `dictee toggle|start`) is ignored. `dictee cancel`
  (or right click on the bubble) cancels the running transcription: nothing is copied. `dictee quit` works at any time.
- Recordings shorter than 0.4 s, and recordings with no signal at all (dead or muted microphone), are not sent to the models.
- A recording stops by itself after 15 minutes.
- Model downloads are pinned to a fixed commit and SHA-256, have timeouts, and resume after a failure.
- A rewrite reply is checked before it is used: control characters are removed, and a reply that is much longer than what was said
  (for example an LLM that followed instructions hidden in the audio) is rejected and the raw text is used instead.
- Errors (missing model, microphone not connected, invalid `config.toml`) show as a desktop notification and in the daemon log.
- Privacy: with a rewrite engine on, the transcript and your phrase list go to that engine (the CLI's service or your API server).
  The default phrases are generic tech terms; add your own names in Settings.

## Commands

```
dictee                 run the daemon (autostart does this)
dictee settings        open settings (starts the daemon if needed)
dictee toggle|start|stop|cancel|status|quit
dictee transcribe f.wav [--rewrite]
dictee models [--download small|qwen3-1.7b-q4|...]   list or download Whisper and Qwen3-ASR models
dictee mics
```

## Not supported yet

- Parakeet (ONNX) models. Mluva's model files cannot be reused (different formats and folders).
- No remote STT (by design for phase 1). Rewrite through a remote API is optional and off by default.

## Wayland notes

- Wayland does not let apps type into other windows, so the default is clipboard only. Auto-paste uses `ydotool` and is off by default.
- GNOME has no always-on-top overlay for normal apps. The bubble is a normal window and takes keyboard focus while visible. Dictee hides it
  before copying, so GNOME returns focus to your previous window. Never click it to paste. Turn the bubble off in settings if you dislike it.
  The result and errors are shown as desktop notifications, not in the bubble.
- Tested only on Ubuntu 26.04 / GNOME 50. **Not tested: KDE, Fedora, Sway, Hyprland.** The KDE keybinding writer follows the documented
  `kglobalshortcutsrc` method, but no Plasma session was available. Please report problems.

## Architecture

| Module | Job |
| --- | --- |
| `config`, `options` | TOML config with validation; fixed choices (engines, languages, devices) shared by UI and engines |
| `audio` | `pw-record` capture with level and length limit, microphone list, WAV reading |
| `stt` | `Transcribe` trait; Whisper in process; Qwen via `qwen`; silence and annotation filtering |
| `qwen` | model catalog, runtime download, `llama-server` lifecycle (warm-up, lease, keep-alive), requests |
| `models`, `download` | Whisper catalog; verified resumable downloads |
| `rewrite`, `rewrite_api`, `codex_app` | final text improvement engines |
| `controller` | state machine: record, transcribe, rewrite, deliver; `Output` trait for testable side effects |
| `deliver` | clipboard, optional `ydotool` paste, notifications |
| `shortcut`, `ipc` | portal shortcut with retry, GNOME/KDE keybinding writers; unix socket for `dictee toggle` |
| `ui` | libadwaita application, bubble, settings window (form, debounced autosave, downloads) |

Developer environment variables: `DICTEE_CONFIG` (config file), `DICTEE_SOCKET` (daemon socket), `DICTEE_NO_PORTAL=1` (no portal shortcut),
`DICTEE_DEBUG=1` (more logs), `DICTEE_SNAPSHOT_SETTINGS=file.png` (makes the window taller; `DICTEE_SNAPSHOT_PART=lower` hides the top groups) /
`DICTEE_SNAPSHOT_BUBBLE=dir` (render windows to PNG).

## Tests

```sh
cargo test                # unit tests (config, controller flow, downloads with resume, IPC, URL rules, keybinding quoting ...)
tests/e2e.sh              # own daemon and config; plays espeak-ng speech through a virtual microphone; overwrites the clipboard
```

`tests/audio/*.wav` were made with `espeak-ng` (English and Czech). The robotic voice gives imperfect raw text, so the tests check key words.
