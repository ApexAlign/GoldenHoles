#!/usr/bin/env bash
# One command to playtest: builds the server and the game, starts a free-for-all (you + 5 bots) on levels/ffa_small.lvl,
# opens the game window, prints the controls, and records playtest telemetry for tuning.
#
#   scripts/play.sh                  you + 5 bots on levels/ffa_small.lvl
#   BOTS=3 scripts/play.sh           fewer opponents        BOT_SKILL=hard scripts/play.sh   (easy | normal | hard)
#   LEVEL=levels/ffa_xl.lvl scripts/play.sh                 the other map (server and game must load the same level)
#   scripts/play.sh --server-only    just run the server (connect the game yourself, see docs/PLAYTEST.md)
#   OFFLINE=1 scripts/play.sh        pass --offline to cargo (no crate downloads); the first Bevy build takes minutes
#   HOST=1 scripts/play.sh           also let other machines join: binds 0.0.0.0, prints your LAN IP and the join command
#                                    (friends run scripts/join.sh <ip>); see docs/ONLINE.md for the internet / firewall
#   PORT=7777  (UDP)                 DEBUG=1 builds the game unoptimised (slow: do NOT judge smoothness with it)
#
# The game is built --release (a debug Bevy build runs at a fraction of the speed and feels jerky). Environment is passed
# straight through, so WGPU_ADAPTER_NAME=... / WGPU_BACKEND=... work. The game's stderr (incl. the PERF frame-time lines)
# goes to playtest/game-<time>.log, the server's (incl. "strike:" anti-cheat lines) to playtest/server-<time>.log:
# paste both when something feels wrong. Ctrl-C or closing the window stops the server cleanly.
#
# Telemetry (shots, hit %, lock-on, grapple, building, deaths by cause...) is printed here every 60 s and when a
# match ends, and appended to playtest/telemetry-<time>.txt (+ JSON lines for scripts). The full server log is
# playtest/server-<time>.log. Both are plain text: send them with your feedback (docs/PLAYTEST.md).
set -euo pipefail
# custom character: CHARACTER=<file> (or SHINOBI_CHARACTER) is made absolute before we change directory; the game reads it
_CH="${CHARACTER:-${SHINOBI_CHARACTER:-}}"
if [ -n "$_CH" ] && [ -f "$_CH" ]; then export SHINOBI_CHARACTER="$(cd "$(dirname "$_CH")" && pwd)/$(basename "$_CH")"; fi
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
PORT="${PORT:-7777}"; BOTS="${BOTS:-5}"; BOT_SKILL="${BOT_SKILL:-normal}"; JOBS="${JOBS:-8}"
# MAP=sky|isles|small|big|xl picks a shipped level by name (LEVEL=<path> still wins)
if [ -z "${LEVEL:-}" ]; then case "${MAP:-}" in sky) LEVEL=levels/ffa_sky.lvl;; isles) LEVEL=levels/ffa_isles.lvl;; hive) LEVEL=levels/ffa_hive.lvl;; clock) LEVEL=levels/ffa_clock.lvl;; split) LEVEL=levels/ffa_split.lvl;; small) LEVEL=levels/ffa_small.lvl;; big) LEVEL=levels/ffa.lvl;; xl) LEVEL=levels/ffa_xl.lvl;; esac; fi
LEVEL="${LEVEL:-levels/ffa_small.lvl}"
[ -f "$LEVEL" ] || { echo "level $LEVEL not found (run from the repo; try LEVEL=levels/arena.lvl)"; exit 1; }
OFFLINE_ARG=(); [ "${OFFLINE:-0}" = "1" ] && OFFLINE_ARG=(--offline)
STAMP="$(date +%Y%m%d-%H%M%S)"
mkdir -p playtest
TELEMETRY="playtest/telemetry-$STAMP.txt"; SERVER_LOG="playtest/server-$STAMP.log"; GAME_LOG="playtest/game-$STAMP.log"

echo ">> building the server (release) ..."
cargo build --release "${OFFLINE_ARG[@]}" -j "$JOBS" -p shinobi-server -q
if [ "${1:-}" != "--server-only" ]; then
  # sound: the game crate's default `audio` feature is on (docs/AUDIO.md); no audio device = silent, no crash
  if [ "${DEBUG:-0}" = "1" ]; then PROFILE=debug; PROFILE_ARG=(); else PROFILE=release; PROFILE_ARG=(--release); fi
  echo ">> building the game ($PROFILE; the first build takes several minutes) ..."
  (cd crates/shinobi-game && cargo build "${PROFILE_ARG[@]}" "${OFFLINE_ARG[@]}" -j "$JOBS" -q)
  GAME_BIN="crates/shinobi-game/target/$PROFILE/shinobi-game"
  [ -x "$GAME_BIN" ] || { echo "game binary $GAME_BIN missing after the build"; exit 1; }
fi

if [ "${HOST:-0}" = "1" ]; then BIND="0.0.0.0:$PORT"; else BIND="127.0.0.1:$PORT"; fi
echo ">> starting the server on $BIND  level $LEVEL  $BOTS bot(s) ($BOT_SKILL)"
SHINOBI_STATS_SECS=3600 target/release/shinobi-server "$BIND" --level "$LEVEL" --bots "$BOTS" --bot-skill "$BOT_SKILL" \
  --telemetry "$TELEMETRY" 2> "$SERVER_LOG" &
SRV=$!
TAIL=""
stop_all() {
  trap - EXIT INT TERM
  [ -n "$TAIL" ] && kill "$TAIL" 2>/dev/null || true
  if kill -0 $SRV 2>/dev/null; then kill -INT $SRV 2>/dev/null || true; for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 $SRV 2>/dev/null || break; sleep 0.3; done; kill $SRV 2>/dev/null || true; fi
  wait $SRV 2>/dev/null || true
}
trap stop_all EXIT
trap 'exit 130' INT TERM
# wait until the server printed its banner (it is listening then); fail fast if it died
for _ in $(seq 1 50); do
  grep -q 'SHINOBI SERVER' "$SERVER_LOG" 2>/dev/null && break
  kill -0 $SRV 2>/dev/null || { echo "server failed to start:"; tail -n 20 "$SERVER_LOG"; exit 1; }
  sleep 0.2
done
grep -q 'SHINOBI SERVER' "$SERVER_LOG" || { echo "server did not come up in 10 s:"; tail -n 20 "$SERVER_LOG"; exit 1; }
if [ "${HOST:-0}" = "1" ]; then
  LAN="$(hostname -I 2>/dev/null | awk '{print $1}')"
  [ -n "$LAN" ] || LAN="$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="src") print $(i+1)}')"
  echo
  echo "  HOSTING for friends on your network (UDP port $PORT):"
  echo "    friend runs :  scripts/join.sh ${LAN:-<your-ip>} $PORT"
  echo "    over internet: forward UDP $PORT on your router to ${LAN:-this machine}; firewall: sudo ufw allow $PORT/udp  (docs/ONLINE.md)"
fi

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
  echo "   cd crates/shinobi-game && cargo run --release -- --server 127.0.0.1:$PORT --level ../../$LEVEL"
  wait $SRV
  exit 0
fi

# the game's stderr (PERF lines, errors) goes to the log; the last PERF line is also shown on exit
"$GAME_BIN" --server "127.0.0.1:$PORT" --level "$LEVEL" 2> "$GAME_LOG" || true

stop_all
echo
echo ">> game closed. Last telemetry:"
grep '^TELEMETRY' "$SERVER_LOG" | tail -n 16 || echo "(none: telemetry prints every 60 s and at match end; play a minute or two)"
grep '^strike:\|kick player' "$SERVER_LOG" | tail -n 5 | sed 's/^/>> anti-cheat: /' || true
grep 'PERF' "$GAME_LOG" | tail -n 3 | sed 's/^/>> /' || true
echo ">> files (paste these with feedback): $TELEMETRY  $SERVER_LOG  $GAME_LOG"
