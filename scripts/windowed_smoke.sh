#!/usr/bin/env bash
# Headless WINDOWED check of the real game client: Xvfb + software Vulkan (lavapipe), a real server (FFA, bots), the real
# game window driven by xdotool (click to grab, WASD, space, shift, E grapple, C, N/M build, LMB, RMB, Tab, backquote, Esc),
# screenshots at each step, and a scan of both logs for panics / errors. Output: playtest/windowed-<time>/
#
#   scripts/windowed_smoke.sh                 ~60 s on levels/ffa.lvl with 7 bots
#   SECS=30 BOTS=3 LEVEL=levels/arena.lvl scripts/windowed_smoke.sh
#   SKIP_BUILD=1 scripts/windowed_smoke.sh    reuse the binaries already built
#
# One-time setup (apt):
#   apt-get install -y --no-install-recommends mesa-vulkan-drivers libvulkan1 vulkan-tools xvfb x11-utils xdotool imagemagick
# Read the PNGs it writes (they are the actual frames); docs/PLAYTEST.md has the notes on what to look for.
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
PORT="${PORT:-7791}"; BOTS="${BOTS:-7}"; LEVEL="${LEVEL:-levels/ffa.lvl}"; SECS="${SECS:-60}"; W="${W:-1280}"; H="${H:-720}"
OUT="${OUT:-playtest/windowed-$(date +%Y%m%d-%H%M%S)}"; mkdir -p "$OUT"
for c in xvfb-run xdotool import; do command -v "$c" >/dev/null || { echo "missing $c (see the apt line at the top of this script)"; exit 2; }; done
if [ "${SKIP_BUILD:-0}" != "1" ]; then
  echo ">> building server (release) and game (dev profile) ..."
  cargo build --release -p shinobi-server -q || exit 1
  (cd crates/shinobi-game && cargo build -q) || exit 1
fi
SERVER=target/release/shinobi-server; GAME=crates/shinobi-game/target/debug/shinobi-game
export VK_ICD_FILENAMES="${VK_ICD_FILENAMES:-/usr/share/vulkan/icd.d/lvp_icd.json}" WGPU_BACKEND="${WGPU_BACKEND:-vulkan}"
echo ">> server on 127.0.0.1:$PORT level=$LEVEL bots=$BOTS -> $OUT/server.log"
"$SERVER" "127.0.0.1:$PORT" --level "$LEVEL" --bots "$BOTS" > "$OUT/server.log" 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null || true' EXIT
sleep 1

export OUT PORT LEVEL SECS GAME W H
xvfb-run -a -s "-screen 0 ${W}x${H}x24" bash -c '
  shot() { import -window root "$OUT/$1.png" 2>>"$OUT/driver.log"; }
  key()  { xdotool key --clearmodifiers "$@" 2>>"$OUT/driver.log"; }
  hold() { xdotool keydown "$1"; sleep "$2"; xdotool keyup "$1"; }
  "$GAME" --server "127.0.0.1:$PORT" --level "$LEVEL" > "$OUT/game.log" 2>&1 &
  GP=$!
  # wait for the window
  for i in $(seq 1 120); do WID=$(xdotool search --onlyvisible --pid $GP 2>/dev/null | head -1); [ -n "$WID" ] && break; sleep 1; done
  echo "window: ${WID:-none} after ${i}s" | tee -a "$OUT/driver.log"
  [ -z "${WID:-}" ] && { echo "no game window"; kill $GP; exit 3; }
  sleep 8; shot 00-connected
  xdotool mousemove $((W/2)) $((H/2)); xdotool click 1; sleep 1; shot 01-after-click-grab   # click grabs the pointer
  hold w 1.5; shot 02-forward
  key space; sleep 0.15; key space; sleep 0.4; shot 03-double-jump
  hold shift 0.1; shot 04-dash
  xdotool mousemove_relative -- -200 -60; sleep 0.2; key e; sleep 0.8; shot 05-grapple-e     # E: hook the crosshair
  hold c 0.6; shot 06-slide-or-swing
  key e; sleep 0.3
  key n; sleep 0.4; shot 07-build-floor-n
  key m; sleep 0.4; shot 08-build-wall-m
  xdotool click 1; sleep 0.3; shot 09-lmb-katana
  xdotool click 3; sleep 0.3; shot 10-rmb-shuriken
  xdotool keydown Tab; sleep 0.5; shot 11-tab-scoreboard; xdotool keyup Tab
  key grave; sleep 0.5; shot 12-backquote-console; key grave
  hold a 1; hold d 1; hold s 1
  END=$((SECS - 30)); [ $END -gt 0 ] && sleep $END
  shot 13-late; key Escape; sleep 0.6; shot 14-esc-pause
  kill $GP 2>/dev/null; wait $GP 2>/dev/null
' 2>&1 | tee -a "$OUT/driver.log"

echo ">> scanning logs"
grep -inE "panic|error|thread .* panicked|ERROR|warn" "$OUT/game.log" "$OUT/server.log" | grep -v "^$" | head -40 > "$OUT/problems.txt"
echo "problem lines: $(wc -l < "$OUT/problems.txt") (see $OUT/problems.txt)"; echo "screenshots:"; ls "$OUT"/*.png 2>/dev/null
