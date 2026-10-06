#!/usr/bin/env bash
# Join someone else's game:  scripts/join.sh <server-ip> [port] [level]
#   scripts/join.sh 192.168.1.20            the host's LAN address (printed by `HOST=1 scripts/play.sh`)
#   scripts/join.sh play.example.com 7777   a public host / forwarded port
# Builds the game in --release (first build takes minutes). The level must match the host's (default levels/ffa_small.lvl;
# LEVEL=levels/ffa.lvl scripts/join.sh <ip> when the host runs another map). Environment passes through
# (WGPU_ADAPTER_NAME=... etc). The game's log (PERF frame-time lines) is playtest/game-<time>.log.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
# MAP=sky|small|big|xl picks a shipped level by name (LEVEL=<path> still wins)
if [ -z "${LEVEL:-}" ]; then case "${MAP:-}" in sky) LEVEL=levels/ffa_sky.lvl;; small) LEVEL=levels/ffa_small.lvl;; big) LEVEL=levels/ffa.lvl;; xl) LEVEL=levels/ffa_xl.lvl;; esac; fi
HOST_ARG="${1:-}"; PORT="${2:-7777}"; LEVEL="${3:-${LEVEL:-levels/ffa_small.lvl}}"
[ -n "$HOST_ARG" ] || { echo "usage: scripts/join.sh <server-ip> [port] [level]"; exit 1; }
[ -f "$LEVEL" ] || { echo "level $LEVEL not found (run from the repo)"; exit 1; }
OFFLINE_ARG=(); [ "${OFFLINE:-0}" = "1" ] && OFFLINE_ARG=(--offline)
echo ">> building the game (release) ..."
(cd crates/shinobi-game && cargo build --release "${OFFLINE_ARG[@]}" -j "${JOBS:-8}" -q)
# no vsync by default: vsync runs ~11 fps on NVIDIA + Wayland hybrid laptops (SHINOBI_PRESENT=auto restores it)
export SHINOBI_PRESENT="${SHINOBI_PRESENT:-immediate}"
mkdir -p playtest
LOG="playtest/game-$(date +%Y%m%d-%H%M%S).log"
echo ">> joining $HOST_ARG:$PORT  level $LEVEL   (log: $LOG)"
crates/shinobi-game/target/release/shinobi-game --server "$HOST_ARG:$PORT" --level "$LEVEL" 2> "$LOG" || true
grep PERF "$LOG" | tail -n 3 | sed 's/^/>> /' || true
echo ">> game closed. Paste $LOG with feedback."
