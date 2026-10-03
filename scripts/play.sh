#!/usr/bin/env bash
# One command to playtest: builds the server and the game, starts a free-for-all (you + 7 bots) on levels/ffa.lvl,
# opens the game window, prints the controls, and records playtest telemetry for tuning.
#
#   scripts/play.sh                  you + 7 bots on levels/ffa.lvl
#   BOTS=3 scripts/play.sh           fewer opponents        BOT_SKILL=hard scripts/play.sh   (easy | normal | hard)
#   LEVEL=levels/ffa_xl.lvl scripts/play.sh                 the other map (server and game must load the same level)
#   scripts/play.sh --server-only    just run the server (connect the game yourself, see docs/PLAYTEST.md)
#   OFFLINE=1 scripts/play.sh        pass --offline to cargo (no crate downloads); the first Bevy build takes minutes
#
# Telemetry (shots, hit %, lock-on, grapple, building, deaths by cause...) is printed here every 60 s and when a
# match ends, and appended to playtest/telemetry-<time>.txt (+ JSON lines for scripts). The full server log is
# playtest/server-<time>.log. Both are plain text: send them with your feedback (docs/PLAYTEST.md).
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
PORT="${PORT:-7777}"; BOTS="${BOTS:-7}"; BOT_SKILL="${BOT_SKILL:-normal}"; JOBS="${JOBS:-8}"
LEVEL="${LEVEL:-levels/ffa.lvl}"
[ -f "$LEVEL" ] || { echo "level $LEVEL not found (run from the repo; try LEVEL=levels/arena.lvl)"; exit 1; }
OFFLINE_ARG=(); [ "${OFFLINE:-0}" = "1" ] && OFFLINE_ARG=(--offline)
STAMP="$(date +%Y%m%d-%H%M%S)"
mkdir -p playtest
TELEMETRY="playtest/telemetry-$STAMP.txt"; SERVER_LOG="playtest/server-$STAMP.log"

echo ">> building the server (release) ..."
cargo build --release "${OFFLINE_ARG[@]}" -j "$JOBS" -p shinobi-server -q
if [ "${1:-}" != "--server-only" ]; then
  echo ">> building the game (the first build takes several minutes) ..."
  (cd crates/shinobi-game && cargo build "${OFFLINE_ARG[@]}" -j "$JOBS" -q)
fi

echo ">> starting the server on 127.0.0.1:$PORT  level $LEVEL  $BOTS bot(s) ($BOT_SKILL)"
SHINOBI_STATS_SECS=3600 target/release/shinobi-server "127.0.0.1:$PORT" --level "$LEVEL" --bots "$BOTS" --bot-skill "$BOT_SKILL" \
  --telemetry "$TELEMETRY" 2> "$SERVER_LOG" &
SRV=$!
TAIL=""
trap 'kill $SRV $TAIL 2>/dev/null || true' EXIT
sleep 1
kill -0 $SRV 2>/dev/null || { echo "server failed to start:"; tail -n 20 "$SERVER_LOG"; exit 1; }

cat <<EOF

  CONTROLS
    click window   grab the mouse (Esc releases)
    W A S D        move            Space  jump / double-jump     mouse  aim
    Shift dash     Q roll          C slide                       W into a wall: wall-run
    E              wrist grapple: hook the crosshair, E again releases
    N / M          print a floor-ramp / a wall (grid piece); anyone can shoot or slice pieces apart
    Left click     katana dash-slice         Right click  throw shuriken (hold an enemy in your cone: the lock-on triangle)
  Free-for-all: first to 10 kills or 6 minutes. Telemetry lines ("TELEMETRY ...") appear below every minute.

  Telemetry : $TELEMETRY
  Server log: $SERVER_LOG
  Feedback  : docs/PLAYTEST.md (what to try, what to report)

EOF
tail -n 0 -F "$SERVER_LOG" 2>/dev/null | grep --line-buffered '^TELEMETRY\|MATCH   START\|MATCH   END\|WINNER' &
TAIL=$!

if [ "${1:-}" = "--server-only" ]; then
  echo ">> server only: game command:"
  echo "   cd crates/shinobi-game && cargo run -- --server 127.0.0.1:$PORT --level ../../$LEVEL"
  wait $SRV
  exit 0
fi

crates/shinobi-game/target/debug/shinobi-game --server "127.0.0.1:$PORT" --level "$LEVEL" || true

kill $SRV 2>/dev/null || true
wait $SRV 2>/dev/null || true
echo
echo ">> game closed. Last telemetry:"
grep '^TELEMETRY' "$SERVER_LOG" | tail -n 16 || echo "(none: telemetry prints every 60 s and at match end; play a minute or two)"
echo ">> files: $TELEMETRY  $SERVER_LOG"
