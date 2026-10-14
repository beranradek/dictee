#!/usr/bin/env bash
# End-to-end test: feeds synthesized speech (espeak-ng) through a virtual PipeWire microphone into the running daemon,
# then checks the clipboard. Needs: running `dictee` daemon, pw-loopback, pw-play, wl-paste, espeak-ng.
# Usage: tests/e2e.sh   (set DICTEE_BIN to override the binary)
set -u
BIN=${DICTEE_BIN:-$(dirname "$0")/../target/release/dictee}
AUDIO=$(dirname "$0")/audio
fail=0

pw-loopback -m '[FL FR]' --capture-props='media.class=Audio/Sink node.name=dictee-test-sink' \
  --playback-props='media.class=Audio/Source node.name=dictee-test-mic' >/dev/null 2>&1 &
LOOP=$!
trap 'kill $LOOP 2>/dev/null' EXIT
sleep 2

cfg=${XDG_CONFIG_HOME:-$HOME/.config}/dictee/config.toml
cp "$cfg" "$cfg.e2e-backup"
trap 'mv "$cfg.e2e-backup" "$cfg"; kill $LOOP 2>/dev/null' EXIT
sed -i 's/^mic = .*/mic = "dictee-test-mic"/; s/^notify = .*/notify = false/' "$cfg"

check() { # name wav expected-regex [rewrite-engine]
  sed -i "s/^engine = .*/engine = \"${4:-none}\"/" "$cfg"
  wl-copy "" ; "$BIN" start >/dev/null || { echo "FAIL $1: cannot start"; fail=1; return; }
  sleep 0.5
  pw-play --target dictee-test-sink "$AUDIO/$2"
  sleep 0.5
  "$BIN" stop >/dev/null
  for _ in $(seq 1 120); do [ "$("$BIN" status)" = idle ] && break; sleep 0.5; done
  got=$(wl-paste -n 2>/dev/null)
  if echo "$got" | grep -Eiq "$3"; then echo "PASS $1: $got"; else echo "FAIL $1: '$got' !~ /$3/"; fail=1; fi
}

check en en_plain.wav 'kubernetes.*deployment'
check cs cs_plain.wav 'administr.*konzol'
check cs-terms-rewrite cs_terms.wav 'radek beran.*kubernetes.*etnetera\b' claude
exit $fail
