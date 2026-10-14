#!/usr/bin/env bash
# End-to-end test with its own daemon and config (your settings and running daemon are not touched).
# Plays synthesized speech (espeak-ng WAVs) through a virtual PipeWire microphone and checks the clipboard (which it overwrites).
# Covers Whisper, Qwen3-ASR (llama.cpp) and the rewrite engines.
# Needs the models in your models folder (so it cannot run in CI): downloaded models (whisper "small", qwen3-1.7b-q4), pw-loopback, pw-play, wl-paste, python3.
# Optional: claude and codex CLIs (E2E_SKIP_CLAUDE=1 / E2E_SKIP_CODEX=1 to skip). DICTEE_BIN overrides the binary,
# DICTEE_MODELS_DIR the models folder. Do not copy anything while it runs.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
BIN=${DICTEE_BIN:-$HERE/../target/release/dictee}
AUDIO=$HERE/audio
MODELS=${DICTEE_MODELS_DIR:-$HOME/.local/share/dictee/models}
TMP=$(mktemp -d)
export DICTEE_CONFIG=$TMP/config.toml DICTEE_SOCKET=$TMP/dictee.sock DICTEE_NO_PORTAL=1
cfg=$DICTEE_CONFIG
fail=0

cat > "$cfg" <<CFG
mic = "dictee-test-mic"
models_dir = "$MODELS"
stt_engine = "whisper"
model = "small"
language = "auto"
notify = false
show_bubble = false
phrases = ["Radek Beran", "Etnetera", "Keycloak", "Kubernetes"]
[qwen]
model = "qwen3-1.7b-q4"
keep_alive_secs = 120
[rewrite]
engine = "none"
base_url = "http://localhost:4999/v1"
model = ""
CFG

pw-loopback -m '[FL FR]' --capture-props='media.class=Audio/Sink node.name=dictee-test-sink' \
  --playback-props='media.class=Audio/Source node.name=dictee-test-mic' >/dev/null 2>&1 &
LOOP=$!
python3 "$HERE/fake_openai.py" 4999 >/dev/null 2>&1 &
FAKE=$!
export OPENAI_API_KEY=x   # the daemon passes it to the API rewrite; the fake server answers 401 without it
"$BIN" daemon >"$TMP/daemon.log" 2>&1 &
DAEMON=$!
trap '"$BIN" quit >/dev/null 2>&1; kill $LOOP $FAKE $DAEMON 2>/dev/null; rm -rf "$TMP"' EXIT
for _ in $(seq 1 50); do [ -S "$DICTEE_SOCKET" ] && break; sleep 0.2; done
sleep 2

setkey() { sed -i "s|^$1 = .*|$1 = \"$2\"|" "$cfg"; }                      # top-level keys
setrw() { sed -i "/^\[rewrite\]/,\$ s|^$1 = .*|$1 = \"$2\"|" "$cfg"; }      # keys of the [rewrite] section

check() { # name wav expected-regex
  wl-copy --clear; "$BIN" start >/dev/null || { echo "FAIL $1: cannot start"; fail=1; return; }
  sleep 0.5; pw-play --target dictee-test-sink "$AUDIO/$2"; sleep 0.5
  "$BIN" stop >/dev/null
  for _ in $(seq 1 240); do [ "$("$BIN" status)" = idle ] && break; sleep 0.5; done
  got=$(wl-paste -n 2>/dev/null)
  if echo "$got" | grep -Eiq "$3"; then echo "PASS $1: $got"; else echo "FAIL $1: '$got' !~ /$3/"; fail=1; fi
}

check whisper-en en_plain.wav 'kubernetes.*deployment'
check whisper-cs cs_plain.wav 'administr.*konzol'

setkey stt_engine qwen; setkey language en
check qwen-en en_plain.wav 'keycloak.*kubernetes.*deployment'
setkey language cs
check qwen-cs cs_plain.wav 'administr'

setkey stt_engine whisper; setkey language auto
setrw engine openai; setrw model fake-model
check api-rewrite en_plain.wav '^PLEASE OPEN THE .*KUBERNETES DEPLOYMENT\.$'
if [ -z "${E2E_SKIP_CLAUDE:-}" ] && command -v claude >/dev/null; then
  setrw engine claude; setrw model ""
  check claude-rewrite cs_terms.wav 'radek beran.*kubernetes.*etneter'
fi
if [ -z "${E2E_SKIP_CODEX:-}" ] && command -v codex >/dev/null; then
  setrw engine codex-app; setrw model ""
  check codex-app-rewrite cs_terms.wav 'radek beran.*kubernetes.*etneter'
fi
exit $fail
