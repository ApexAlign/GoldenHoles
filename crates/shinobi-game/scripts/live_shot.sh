#!/usr/bin/env bash
# Offscreen render against a throwaway LOCAL server + one moving headless client. No window is opened.
# usage: scripts/live_shot.sh OUT.png [secs] [--idle]   (--idle: no slash/throw, shows resting viewmodel)    (debug builds; -j capped so shared-box tests are not starved)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
OUT="${1:?usage: live_shot.sh OUT.png [secs]}"; SECS="${2:-2.5}"; IDLE="${3:-}"
PORT=$((20000 + RANDOM % 20000)); TMP="$(mktemp -d)"
export PATH="$HOME/.cargo/bin:$PATH"
(cd "$ROOT" && cargo build -p shinobi-server --offline -j 6 -q)
(cd "$ROOT/crates/shinobi-game" && cargo build --offline -j 6 -q)
SHINOBI_STATS_SECS=3600 "$ROOT/target/debug/shinobi-server" "127.0.0.1:$PORT" >"$TMP/server.log" 2>&1 & SRV=$!
trap 'kill $SRV 2>/dev/null || true; rm -rf "$TMP"' EXIT
sleep 1
G="$ROOT/crates/shinobi-game/target/debug/shinobi-game"
"$G" --headless --secs 12 --server "127.0.0.1:$PORT" >/dev/null 2>&1 & MOVER=$!
sleep 0.3
"$G" --shot "$TMP/shot.ppm" --secs "$SECS" $IDLE --server "127.0.0.1:$PORT" 2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "ERROR|shot:" || true
kill $MOVER 2>/dev/null || true
python3 -c "from PIL import Image; Image.open('$TMP/shot.ppm').save('$OUT'); print('wrote $OUT')"
