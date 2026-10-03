#!/usr/bin/env bash
# No-window smoke test of the 8v8 team shooter: release server (--team-size 8, levels/8v8.lvl) + scripted clients that
# hook the wrist grapple and try to place blocks, for N seconds, then a pass/fail health report.
#   scripts/demo_8v8_headless.sh                       60 s, 8 scripted clients (the other 8 slots are AI bots)
#   SECS=20 CLIENTS=4 scripts/demo_8v8_headless.sh
#   OFFLINE=0 scripts/demo_8v8_headless.sh             allow cargo to fetch crates (default: --offline)
# PASS needs: every client connected and stayed (no kick / undecodable datagram), server tick rate within 1% of 60 Hz,
# no contained panic, teams on the wire (every client learned all 16 players' teams, both teams hold a client) and at
# least one grapple attach seen (RopeAttach events).
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
SECS="${SECS:-60}"; CLIENTS="${CLIENTS:-8}"; JOBS="${JOBS:-8}"; TEAM_SIZE=8
LEVEL="${LEVEL:-levels/8v8.lvl}"
OFFLINE_ARG=(--offline); [ "${OFFLINE:-1}" = "0" ] && OFFLINE_ARG=()
PORT=$((20000 + RANDOM % 20000)); TMP="$(mktemp -d)"
SRV=""
trap '[ -n "$SRV" ] && kill $SRV 2>/dev/null || true; rm -rf "$TMP"' EXIT

echo ">> building release server + bot ..."
cargo build --release "${OFFLINE_ARG[@]}" -j "$JOBS" -q -p shinobi-server -p shinobi-bot
echo ">> server on 127.0.0.1:$PORT: ${TEAM_SIZE}v${TEAM_SIZE}, level $LEVEL, $CLIENTS scripted client(s), ${SECS}s"
SHINOBI_STATS_SECS=$(( SECS < 30 ? 5 : 10 )) target/release/shinobi-server "127.0.0.1:$PORT" --level "$LEVEL" --team-size "$TEAM_SIZE" 2> "$TMP/server.log" &
SRV=$!
sleep 1
set +e
target/release/shinobi-bot --addr "127.0.0.1:$PORT" --count "$CLIENTS" --behaviour mixed --secs "$SECS" | tee "$TMP/bots.txt"
BOT_RC=${PIPESTATUS[0]}
set -e
kill $SRV 2>/dev/null || true
wait $SRV 2>/dev/null || true
SRV=""

echo; echo "---- server stats ----"
grep '^stats:' "$TMP/server.log" || echo "(no stats lines)"
echo; echo "---- kill feed / match flow (last 15) ----"
grep -E 'KILL|MATCH|PICKUP|RESPAWN' "$TMP/server.log" | tail -15 || true
echo; echo "---- problems ----"
grep -E 'PANIC contained|^kick player|^timeout player|: stall, discarded' "$TMP/server.log" || echo "none"

fail=0
check() { # name, ok(0/1), detail
  if [ "$2" -eq 1 ]; then echo "  ok   $1"; else echo "  FAIL $1 ($3)"; fail=1; fi
}
TOTALS=$(grep '^totals:' "$TMP/bots.txt" || true)
field() { echo "$TOTALS" | sed -n "s/.*$1=\([0-9]*\).*/\1/p"; }
ROPES=$(field rope_attaches); BLOCKS=$(field blocks_placed); CY=$(field bots_cyan); OR=$(field bots_orange); KNOWN=$(field players_with_known_team)
ROPES=${ROPES:-0}; BLOCKS=${BLOCKS:-0}; CY=${CY:-0}; OR=${OR:-0}; KNOWN=${KNOWN:-0}
RATE_BAD=$(grep '^stats:' "$TMP/server.log" | sed -n 's/.* hz=\([0-9.]*\) .*/\1/p' | awk '$1 < 59.4 || $1 > 60.6 { bad=1 } END { print bad+0 }')
PANICS=$(grep -c 'PANIC contained' "$TMP/server.log" || true)

echo; echo "---- health ----"
check "clients connected and stayed (no kick / bad datagram)" $([ "$BOT_RC" -eq 0 ] && echo 1 || echo 0) "bots exit=$BOT_RC"
check "server tick rate within 1% of 60 Hz" $([ "$RATE_BAD" -eq 0 ] && echo 1 || echo 0) "see stats above; loaded dev machines can miss this"
check "no contained panic" $([ "$PANICS" -eq 0 ] && echo 1 || echo 0) "$PANICS panic(s)"
check "server announced the teams" $(grep -q "teams: ${TEAM_SIZE}v${TEAM_SIZE}" "$TMP/server.log" && echo 1 || echo 0) "no 'teams:' line in server log"
check "clients learned all $((TEAM_SIZE * 2)) players' teams" $([ "$KNOWN" -ge $((TEAM_SIZE * 2)) ] && echo 1 || echo 0) "most known by one client: $KNOWN"
if [ "$CLIENTS" -ge 2 ]; then
  check "both teams hold a client (auto-balance)" $([ "$CY" -gt 0 ] && [ "$OR" -gt 0 ] && echo 1 || echo 0) "cyan=$CY orange=$OR"
fi
check "grapple attaches seen on the wire" $([ "$ROPES" -gt 0 ] && echo 1 || echo 0) "rope_attaches=$ROPES (needs a hook client: CLIENTS >= 4)"
echo "  info blocks placed seen: $BLOCKS (server validates range / overlap; 0 is possible in open space)"

echo
if [ "$fail" -ne 0 ]; then echo "FAIL: 8v8 smoke test"; exit 1; fi
echo "OK: 8v8 smoke test passed"
