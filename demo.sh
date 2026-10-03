#!/usr/bin/env bash
# One-command demo: local server + game window.
#   ./demo.sh              build, start a local server, open the game window on this machine's display
#   BOTS=3 ./demo.sh       also ask the server for 3 AI opponents (once the server supports --bots)
#   TEAM_SIZE=8 ./demo.sh  8v8 Cyan vs Orange team deathmatch, bots fill empty slots (opt-in; instead of BOTS)
#   ./demo.sh --server-only   just run the server (connect the game yourself)
# Quit: close the window; the server is stopped automatically.
set -euo pipefail
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"
PORT="${PORT:-7777}"
BOTS="${BOTS:-0}"
TEAM_SIZE="${TEAM_SIZE:-0}"
JOBS="${JOBS:-8}"
# server and client MUST load the same level or client prediction desyncs.
# TEAM_SIZE (8v8) defaults to the 8v8 map, otherwise the classic arena.
if [ -z "${LEVEL:-}" ]; then
  if [ "$TEAM_SIZE" != "0" ] && [ -f levels/8v8.lvl ]; then LEVEL=levels/8v8.lvl; else LEVEL=levels/arena.lvl; fi
fi
LEVEL_ARGS=()
[ -f "$LEVEL" ] && LEVEL_ARGS=(--level "$LEVEL")

echo ">> building server (release) ..."
cargo build --release --offline -j "$JOBS" -p shinobi-server -q
SERVER=target/release/shinobi-server

BOT_ARGS=()
if [ "$BOTS" != "0" ]; then
  # the server has no --help; detect support from its source
  if grep -rq -- '"--bots"' crates/shinobi-server/src; then BOT_ARGS=(--bots "$BOTS"); else echo ">> server has no --bots yet, starting without"; fi
fi

TEAM_ARGS=()
if [ "$TEAM_SIZE" != "0" ]; then
  # --team-size fills BOTH teams with bots, so it replaces --bots
  TEAM_ARGS=(--team-size "$TEAM_SIZE"); BOT_ARGS=()
fi

echo ">> starting server on 127.0.0.1:$PORT ${LEVEL_ARGS[*]:-} ${BOT_ARGS[*]:-} ${TEAM_ARGS[*]:-}"
SHINOBI_STATS_SECS=3600 "$SERVER" "127.0.0.1:$PORT" "${LEVEL_ARGS[@]}" "${BOT_ARGS[@]}" "${TEAM_ARGS[@]}" &
SRV=$!
trap 'kill $SRV 2>/dev/null || true' EXIT
sleep 1

if [ "${1:-}" = "--server-only" ]; then wait $SRV; exit 0; fi

echo ">> building game (first time takes several minutes) ..."
(cd crates/shinobi-game && cargo build --offline -j "$JOBS" -q)

cat <<'EOF'

  CONTROLS
    click window   grab the mouse (Esc releases)
    W A S D        move            Space   jump / double-jump
    mouse          aim             Shift   dash          Q   roll          C   slide
    W into a wall  wall-cling / wall-run (keep speed up, hold Space or W)
    Left click     katana dash-slice        Right click   throw shuriken
    E              wrist grapple: hooks the crosshair (reels you in), press E again to release

  TEAMS (TEAM_SIZE=8 ./demo.sh): Cyan vs Orange, 8 per side, bots fill the empty slots. You join the smaller team.
    Shurikens and the katana pass through teammates. First team to 50 team kills wins (tie = 60 s sudden death).
  BLOCKS: the server and the wire support player-built blocks, but the game window has no build key yet.

EOF
exec crates/shinobi-game/target/debug/shinobi-game --server "127.0.0.1:$PORT" "${LEVEL_ARGS[@]}"
