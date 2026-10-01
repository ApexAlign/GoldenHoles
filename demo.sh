#!/usr/bin/env bash
# One-command demo: local server + game window.
#   ./demo.sh              build, start a local server, open the game window on this machine's display
#   BOTS=3 ./demo.sh       also ask the server for 3 AI opponents (once the server supports --bots)
#   ./demo.sh --server-only   just run the server (connect the game yourself)
# Quit: close the window; the server is stopped automatically.
set -euo pipefail
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"
PORT="${PORT:-7777}"
BOTS="${BOTS:-0}"
JOBS="${JOBS:-8}"
# server and client MUST load the same level or client prediction desyncs
LEVEL="${LEVEL:-levels/arena.lvl}"
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

echo ">> starting server on 127.0.0.1:$PORT ${LEVEL_ARGS[*]:-} ${BOT_ARGS[*]:-}"
SHINOBI_STATS_SECS=3600 "$SERVER" "127.0.0.1:$PORT" "${LEVEL_ARGS[@]}" "${BOT_ARGS[@]}" &
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

EOF
exec crates/shinobi-game/target/debug/shinobi-game --server "127.0.0.1:$PORT" "${LEVEL_ARGS[@]}"
