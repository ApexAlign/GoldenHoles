#!/usr/bin/env bash
# No-window health check: release server (with AI opponents) + scripted bot clients for N seconds, then a summary.
#   scripts/demo_headless.sh                 60 s, 3 AI opponents, 4 scripted clients
#   SECS=20 AI=0 CLIENTS=8 scripts/demo_headless.sh
# Prints the server's stats lines + kill feed and the bots' table. Exit status is non-zero if any scripted client
# was kicked, failed to connect, or saw undecodable datagrams, or if the server's tick rate left 60 Hz +- 1%.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
SECS="${SECS:-60}"; AI="${AI:-3}"; CLIENTS="${CLIENTS:-4}"; JOBS="${JOBS:-8}"
PORT=$((20000 + RANDOM % 20000)); TMP="$(mktemp -d)"
trap 'kill $SRV 2>/dev/null || true; rm -rf "$TMP"' EXIT

echo ">> building release server + bot ..."
cargo build --release --offline -j "$JOBS" -q -p shinobi-server -p shinobi-bot
echo ">> server on 127.0.0.1:$PORT with $AI AI opponent(s), $CLIENTS scripted client(s), ${SECS}s"
SHINOBI_STATS_SECS=$(( SECS < 30 ? 5 : 10 )) target/release/shinobi-server "127.0.0.1:$PORT" --bots "$AI" 2> "$TMP/server.log" &
SRV=$!
sleep 1
set +e
target/release/shinobi-bot --addr "127.0.0.1:$PORT" --count "$CLIENTS" --behaviour mixed --secs "$SECS" | tee "$TMP/bots.txt"
BOT_RC=${PIPESTATUS[0]}
set -e
kill $SRV 2>/dev/null || true
wait $SRV 2>/dev/null || true

echo; echo "---- server stats ----"
grep '^stats:' "$TMP/server.log" || echo "(no stats lines)"
echo; echo "---- kill feed / match flow (last 15) ----"
grep -E 'KILL|MATCH|PICKUP|RESPAWN' "$TMP/server.log" | tail -15 || true
echo; echo "---- problems ----"
grep -E 'PANIC contained|^kick player|^timeout player|: stall, discarded' "$TMP/server.log" || echo "none"

RATE_BAD=$(grep '^stats:' "$TMP/server.log" | sed -n 's/.* hz=\([0-9.]*\) .*/\1/p' | awk '$1 < 59.4 || $1 > 60.6 { bad=1 } END { print bad+0 }')
if [ "$BOT_RC" -ne 0 ] || [ "$RATE_BAD" -ne 0 ]; then
  echo; echo "FAIL: bots exit=$BOT_RC, tick-rate outside 1%: $RATE_BAD (dev-class machines under load can miss this; see stats above)"
  exit 1
fi
echo; echo "OK: all clients stayed connected, tick rate within 1% of 60 Hz"
