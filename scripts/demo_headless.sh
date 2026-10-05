#!/usr/bin/env bash
# No-window health check: release server (with AI opponents) + scripted bot clients for N seconds, then a summary.
#   scripts/demo_headless.sh                 free-for-all, 60 s, 7 AI opponents + 4 scripted clients (11 players)
#   SECS=20 AI=0 CLIENTS=8 scripts/demo_headless.sh
#   LAG=50:40:5 scripts/demo_headless.sh     the scripted clients connect THROUGH shinobi-lagproxy: 50 ms one way, +-40 ms
#                                            jitter, 5% loss each way (LAG_DELAY / LAG_JITTER / LAG_LOSS / LAG_BW_KBPS also work);
#                                            runs 70 s so the 60 s telemetry report (ping, late / rejected input) is printed
#   LEVEL=levels/arena.lvl scripts/demo_headless.sh   (default: levels/ffa_small.lvl when it exists, else levels/arena.lvl)
# Prints the server's stats lines + kill feed and the bots' table. Exit status is non-zero if any scripted client
# was kicked, failed to connect, or saw undecodable datagrams, if nobody fights, or if a stats window after warmup has
# hz outside 60 +-1%, step_max >= 8 ms or overruns > 0 (SKIP_PERF=1 turns the performance guard into a note).
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
if [ -n "${LAG:-}" ]; then IFS=: read -r LAG_DELAY LAG_JITTER LAG_LOSS <<< "$LAG"; SECS="${SECS:-70}"; fi
LAG_DELAY="${LAG_DELAY:-}"; LAG_JITTER="${LAG_JITTER:-0}"; LAG_LOSS="${LAG_LOSS:-0}"; LAG_BW_KBPS="${LAG_BW_KBPS:-0}"
SECS="${SECS:-60}"; AI="${AI:-7}"; CLIENTS="${CLIENTS:-4}"; JOBS="${JOBS:-8}"
if [ -z "${LEVEL:-}" ]; then if [ -f levels/ffa_small.lvl ]; then LEVEL=levels/ffa_small.lvl; else LEVEL=levels/arena.lvl; fi; fi
PORT=$((20000 + RANDOM % 20000)); TMP="$(mktemp -d)"
PROXY=""
trap 'kill $SRV $PROXY 2>/dev/null || true; rm -rf "$TMP"' EXIT

echo ">> building release server + bot + lagproxy ..."
cargo build --release --offline -j "$JOBS" -q -p shinobi-server -p shinobi-bot
echo ">> free-for-all server on 127.0.0.1:$PORT, level $LEVEL, $AI AI opponent(s), $CLIENTS scripted client(s), ${SECS}s"
SHINOBI_STATS_SECS=$(( SECS < 30 ? 5 : 10 )) target/release/shinobi-server "127.0.0.1:$PORT" --level "$LEVEL" --bots "$AI" --telemetry "$TMP/telemetry.txt" 2> "$TMP/server.log" &
SRV=$!
sleep 1
BOT_PORT=$PORT
if [ -n "$LAG_DELAY" ]; then
  BOT_PORT=$((PORT + 1))
  echo ">> lag proxy 127.0.0.1:$BOT_PORT -> :$PORT  ${LAG_DELAY} ms +- ${LAG_JITTER} ms one way, ${LAG_LOSS}% loss each way"
  target/release/shinobi-lagproxy --listen "127.0.0.1:$BOT_PORT" --to "127.0.0.1:$PORT" --delay-ms "$LAG_DELAY" --jitter-ms "$LAG_JITTER" \
    --loss "$LAG_LOSS" --bw-kbps "$LAG_BW_KBPS" --stats-secs 30 2> "$TMP/proxy.log" &
  PROXY=$!
  sleep 0.5
fi
set +e
target/release/shinobi-bot --addr "127.0.0.1:$BOT_PORT" --count "$CLIENTS" --behaviour mixed --secs "$SECS" | tee "$TMP/bots.txt"
BOT_RC=${PIPESTATUS[0]}
set -e
kill $SRV 2>/dev/null || true
wait $SRV 2>/dev/null || true

echo; echo "---- server stats ----"
grep '^stats:' "$TMP/server.log" || echo "(no stats lines)"
echo; echo "---- kill feed / match flow (last 15) ----"
grep -E 'KILL|MATCH|PICKUP|RESPAWN' "$TMP/server.log" | tail -15 || true
echo; echo "---- playtest telemetry (last report; every 60 s and at match end) ----"
grep "^TELEMETRY" "$TMP/server.log" | tail -n 14 || echo "(none yet: first report after 60 s)"
[ -f "$TMP/telemetry.txt" ] || echo "(telemetry file not written)"
[ -f "$TMP/proxy.log" ] && { echo; echo "---- lag proxy ----"; tail -n 4 "$TMP/proxy.log"; }
echo; echo "---- problems ----"
grep -E 'PANIC contained|^kick player|^timeout player|: stall, discarded' "$TMP/server.log" || echo "none"

KILLS=$(grep -c 'KILL ' "$TMP/server.log" || true)
STARTED=$(grep -c 'MATCH   START' "$TMP/server.log" || true)
# Performance guards on every stats window AFTER the first (warmup: cold caches, match-start reset):
#   hz within 60 +- 1%, step_max < 8 ms (about 4x headroom on a 16.7 ms tick), overruns == 0.
# SKIP_PERF=1 reports them but does not fail (slow / loaded dev boxes).
STEP_MAX_MS_LIMIT=8
PERF_BAD=$(grep '^stats:' "$TMP/server.log" | tail -n +2 | awk -v lim="$STEP_MAX_MS_LIMIT" '
  { hz=0; smax=0; ov=0
    for (i = 1; i <= NF; i++) {
      if ($i ~ /^hz=/) hz = substr($i, 4) + 0
      if ($i ~ /^step_max_ms=/) smax = substr($i, 13) + 0
      if ($i ~ /^overruns=/) ov = substr($i, 10) + 0
    }
    if (hz < 59.4 || hz > 60.6) { bad = bad " hz=" hz }
    if (smax >= lim) { bad = bad " step_max_ms=" smax }
    if (ov > 0) { bad = bad " overruns=" ov }
  }
  END { print bad }')
echo; echo "---- fight ----"
echo "kills: $KILLS   matches started: $STARTED   (a free-for-all with bots must produce kills; a match ends at 10 kills or 6 minutes)"
if [ "$AI" -gt 0 ] && [ "${SECS}" -ge 30 ] && { [ "$KILLS" -eq 0 ] || [ "$STARTED" -eq 0 ]; }; then
  echo; echo "FAIL: no match started or nobody died in ${SECS}s with $AI bots"
  exit 1
fi
if [ -n "$PERF_BAD" ]; then
  if [ "${SKIP_PERF:-0}" = "1" ]; then
    echo; echo "note: performance guard tripped but SKIP_PERF=1:$PERF_BAD"
  else
    echo; echo "FAIL: performance guard (hz 60 +-1%, step_max < ${STEP_MAX_MS_LIMIT} ms, overruns 0) tripped after warmup:$PERF_BAD"
    echo "      dev-class machines under load can miss this: re-run with SKIP_PERF=1, or see the stats above"
    exit 1
  fi
fi
if [ "$BOT_RC" -ne 0 ]; then
  echo; echo "FAIL: scripted clients: exit=$BOT_RC (kicked, failed to connect, or undecodable datagrams)"
  exit 1
fi
echo; echo "OK: all clients stayed connected; after warmup hz within 1% of 60, step_max < ${STEP_MAX_MS_LIMIT} ms, no overruns"
