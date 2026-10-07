#!/usr/bin/env bash
# One-command demo: local server + game window.
#   ./demo.sh              build, start a local server, open the game window on this machine's display
#   ./demo.sh              free-for-all against 7 AI opponents (the default game)
#   BOTS=3 ./demo.sh       fewer / more AI opponents (BOTS=0 for none)
#   ./demo.sh              the sky city: a floating vertical world, four tiers (levels/ffa_sky.lvl, the default); MAP=small|big|xl for the ground maps
#   TEAM_SIZE=8 ./demo.sh  8v8 Cyan vs Orange team deathmatch, bots fill empty slots (opt-in; instead of BOTS)
#   ./demo.sh --server-only   just run the server (connect the game yourself)
# Quit: close the window or press Ctrl-C; the server is stopped automatically. Game log: playtest/game-<time>.log
set -euo pipefail
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"
PORT="${PORT:-7777}"
BOTS="${BOTS:-5}"   # free-for-all on the 88 m arena: you + 5 bots = 6 players
TEAM_SIZE="${TEAM_SIZE:-0}"
JOBS="${JOBS:-8}"
# server and client MUST load the same level or client prediction desyncs.
# TEAM_SIZE (8v8) defaults to the 8v8 map; free-for-all defaults to the sky city (levels/ffa_sky.lvl); MAP=small (88 m ground arena), MAP=big (224 m) or LEVEL=<path> pick others.
# MAP=sky|small|big|xl picks a shipped level by name (LEVEL=<path> still wins)
if [ -z "${LEVEL:-}" ]; then case "${MAP:-}" in sky) LEVEL=levels/ffa_sky.lvl;; small) LEVEL=levels/ffa_small.lvl;; big) LEVEL=levels/ffa.lvl;; xl) LEVEL=levels/ffa_xl.lvl;; esac; fi
if [ -z "${LEVEL:-}" ]; then
  if [ "$TEAM_SIZE" != "0" ] && [ -f levels/8v8.lvl ]; then LEVEL=levels/8v8.lvl; elif [ -f levels/ffa_sky.lvl ]; then LEVEL=levels/ffa_sky.lvl; elif [ -f levels/ffa_small.lvl ]; then LEVEL=levels/ffa_small.lvl; elif [ -f levels/ffa.lvl ]; then LEVEL=levels/ffa.lvl; else LEVEL=levels/arena.lvl; fi
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
trap 'kill $SRV 2>/dev/null || true' EXIT INT TERM
sleep 1

if [ "${1:-}" = "--server-only" ]; then wait $SRV; exit 0; fi

# Release by default: the debug build runs at ~10 fps. DEBUG=1 ./demo.sh for a debug build.
# sound: the game crate's default `audio` feature is on (docs/AUDIO.md); no audio device = silent, no crash
if [ "${DEBUG:-0}" = "1" ]; then PROFILE=debug; REL=(); else PROFILE=release; REL=(--release); fi
echo ">> building game ($PROFILE; first time takes several minutes) ..."
(cd crates/shinobi-game && cargo build "${REL[@]}" --offline -j "$JOBS" -q)
# Hybrid-GPU laptops: the default adapter is the low-power iGPU. Prefer the discrete one (SHINOBI_GPU=igpu|auto overrides).
export SHINOBI_GPU="${SHINOBI_GPU:-dgpu}"
# AutoVsync runs at ~11 fps on NVIDIA + Wayland hybrid laptops (verified); no-vsync is full speed. SHINOBI_PRESENT=auto restores vsync.
export SHINOBI_PRESENT="${SHINOBI_PRESENT:-immediate}"
mkdir -p playtest
LOG="playtest/game-$(date +%Y%m%d-%H%M%S).log"

cat <<'EOF'

  CONTROLS
    click window   grab the mouse (Esc releases)
    W A S D        move            Space   jump / double-jump
    mouse          aim             Shift   dash          Q   roll          C   slide
    W into a wall  wall-cling / wall-run (keep speed up, hold Space or W)
    Left click     katana dash-slice        Right click   throw shuriken
    E              wrist grapple: hooks the crosshair (reels you in), press E again to release

  FREE-FOR-ALL (default): everyone for themselves, every player has their own neon colour, first to the frag limit wins.

  TEAMS (optional, TEAM_SIZE=8 ./demo.sh): Cyan vs Orange, 8 per side, bots fill the empty slots. You join the smaller team.
    Shurikens and the katana pass through teammates. First team to 50 team kills wins (tie = 60 s sudden death).
  BLOCKS: the server and the wire support player-built blocks, but the game window has no build key yet.

EOF
# no `exec`: the shell must stay alive so the trap stops the server when the window closes
echo ">> game log (GPU line + PERF frame times): $LOG"
crates/shinobi-game/target/$PROFILE/shinobi-game --server "127.0.0.1:$PORT" "${LEVEL_ARGS[@]}" 2> >(tee "$LOG" >&2) || true
grep -E "GPU|PERF" "$LOG" | tail -n 3 | sed 's/^/>> /' || true
