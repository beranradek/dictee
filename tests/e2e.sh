#!/usr/bin/env bash
# End-to-end test: feeds synthesized speech (espeak-ng) through a virtual PipeWire microphone into the running daemon,
# then checks the clipboard. Covers Whisper, Qwen3-ASR (llama.cpp) and the rewrite engines.
# Needs: running `dictee` daemon, downloaded models (small + qwen3-1.7b-q4), pw-loopback, pw-play, wl-paste, python3.
# Optional: claude and codex CLIs (E2E_SKIP_CLAUDE=1 / E2E_SKIP_CODEX=1 to skip). Usage: tests/e2e.sh   (DICTEE_BIN overrides the binary)
set -u
HERE=$(dirname "$0")
BIN=${DICTEE_BIN:-$HERE/../target/release/dictee}
AUDIO=$HERE/audio
fail=0

pw-loopback -m '[FL FR]' --capture-props='media.class=Audio/Sink node.name=dictee-test-sink' \
  --playback-props='media.class=Audio/Source node.name=dictee-test-mic' >/dev/null 2>&1 &
LOOP=$!
python3 "$HERE/fake_openai.py" 4999 >/dev/null 2>&1 &
FAKE=$!
cfg=${XDG_CONFIG_HOME:-$HOME/.config}/dictee/config.toml
cp "$cfg" "$cfg.e2e-backup"
trap 'mv "$cfg.e2e-backup" "$cfg"; kill $LOOP $FAKE 2>/dev/null' EXIT
sleep 2

setkey() { sed -i "s|^$1 = .*|$1 = \"$2\"|" "$cfg"; }                      # top-level keys
setrw() { sed -i "/^\[rewrite\]/,\$ s|^$1 = .*|$1 = \"$2\"|" "$cfg"; }   # keys of the [rewrite] section
sed -i 's/^mic = .*/mic = "dictee-test-mic"/; s/^notify = .*/notify = false/' "$cfg"

check() { # name wav expected-regex
  wl-copy ""; "$BIN" start >/dev/null || { echo "FAIL $1: cannot start"; fail=1; return; }
  sleep 0.5; pw-play --target dictee-test-sink "$AUDIO/$2"; sleep 0.5
  "$BIN" stop >/dev/null
  for _ in $(seq 1 240); do [ "$("$BIN" status)" = idle ] && break; sleep 0.5; done
  got=$(wl-paste -n 2>/dev/null)
  if echo "$got" | grep -Eiq "$3"; then echo "PASS $1: $got"; else echo "FAIL $1: '$got' !~ /$3/"; fail=1; fi
}

setrw engine none
setkey stt_engine whisper; setkey language auto
check whisper-en en_plain.wav 'kubernetes.*deployment'
check whisper-cs cs_plain.wav 'administr.*konzol'

setkey stt_engine qwen; setkey language en
check qwen-en en_plain.wav 'keycloak.*kubernetes.*deployment'
setkey language cs
check qwen-cs cs_plain.wav 'administr'

setkey stt_engine whisper; setkey language auto
setrw engine openai; setrw base_url "http://localhost:4999/v1"; setrw model fake-model
OPENAI_API_KEY=x check api-rewrite en_plain.wav '^PLEASE OPEN THE .*KUBERNETES DEPLOYMENT\.$'
if [ -z "${E2E_SKIP_CLAUDE:-}" ]; then
  setrw engine claude; setrw model ""
  check claude-rewrite cs_terms.wav 'radek beran.*kubernetes.*etnetera\b'
fi
if [ -z "${E2E_SKIP_CODEX:-}" ] && command -v codex >/dev/null; then
  setrw engine codex-app; setrw model ""
  check codex-app-rewrite cs_terms.wav 'radek beran.*kubernetes.*etneter'
fi
exit $fail
