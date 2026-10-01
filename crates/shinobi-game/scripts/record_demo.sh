#!/usr/bin/env bash
# Record a gameplay clip without opening a window: local server + bots + scripted visitor -> MP4 via ffmpeg.
#   scripts/record_demo.sh [OUT.mp4]           defaults: 1280x720, 30 fps, up to 25 s
#   BOTS=3 SKILL=normal FPS=60 SECS=30 LEVEL=levels/demo.lvl scripts/record_demo.sh /tmp/clip.mp4
#   LEVEL= (empty) uses the built-in floor-only level
# Uses debug builds (-j capped) so the shared box is not starved. Never touches $DISPLAY.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
OUT="${1:-$ROOT/demo-clip.mp4}"; BOTS="${BOTS:-2}"; FPS="${FPS:-30}"; SECS="${SECS:-25}"; LEVEL="${LEVEL:-levels/arena.lvl}"; SKILL="${SKILL:-easy}"
PORT=$((20000 + RANDOM % 20000)); TMP="$(mktemp -d)"
export PATH="$HOME/.cargo/bin:$PATH"
command -v ffmpeg >/dev/null || { echo "ffmpeg not found"; exit 2; }
(cd "$ROOT" && cargo build -p shinobi-server --offline -j 6 -q)
(cd "$ROOT/crates/shinobi-game" && cargo build --offline -j 6 -q)

SRV_ARGS=("127.0.0.1:$PORT")
[ "$BOTS" != "0" ] && SRV_ARGS+=(--bots "$BOTS" --bot-skill "$SKILL")
[ -n "$LEVEL" ] && SRV_ARGS+=(--level "$ROOT/$LEVEL")
SHINOBI_STATS_SECS=3600 "$ROOT/target/debug/shinobi-server" "${SRV_ARGS[@]}" >"$TMP/server.log" 2>&1 & SRV=$!
trap 'kill $SRV 2>/dev/null || true; rm -rf "$TMP"' EXIT
sleep 1.5

GAME_LEVEL=(); [ -n "$LEVEL" ] && GAME_LEVEL=(--level "$ROOT/$LEVEL")
"$ROOT/crates/shinobi-game/target/debug/shinobi-game" "${GAME_LEVEL[@]}" --record "$OUT" --demo --fps "$FPS" --secs "$SECS" --server "127.0.0.1:$PORT" \
  2>&1 | sed 's/\x1b\[[0-9;]*m//g' | grep -E "ERROR|record:|panicked" || true

if [ -s "$OUT" ]; then
  ffprobe -v error -select_streams v:0 -show_entries stream=width,height,r_frame_rate,nb_frames,duration -of default=nw=1 "$OUT" 2>/dev/null || true
  ls -la "$OUT"
else
  echo "no clip written; ffmpeg log:"; cat "$OUT.ffmpeg.log" 2>/dev/null | tail -5; exit 1
fi
