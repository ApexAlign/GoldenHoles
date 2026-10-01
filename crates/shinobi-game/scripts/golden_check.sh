#!/usr/bin/env bash
# Deterministic offscreen renders vs golden images. NOT part of `cargo test --workspace` (needs a GPU and a
# full bevy debug build). No window is opened; no network is used.
#   scripts/golden_check.sh            compare every scenario against tests/golden/<name>.png
#   scripts/golden_check.sh --update   re-record all goldens (review the images before committing them!)
# Scenarios:  offline          desktop: high tier, full effects
#             mobile_touch     low tier, no bloom/CRT, scene pinned to 0.6x and upscaled, touch overlay on
#             arena            levels/arena.lvl from spawn 5 (neon-edged tiers, corridors, ceiling skylight)
set -euo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"; cd "$HERE"
export PATH="$HOME/.cargo/bin:$PATH"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
ROOT="$(cd "$HERE/../.." && pwd)"
# per-scenario game arguments as real arrays (the repo path contains a space)
scenario_args() {
  case "$1" in
    offline)      ARGV=(--level builtin) ;;
    mobile_touch) ARGV=(--level builtin --tier low --scale 0.6 --touch) ;;
    arena)        ARGV=(--level "$ROOT/levels/arena.lvl" --spawn 5) ;;
  esac
}
python3 scripts/compare_images.py --selftest
cargo build --offline -j 6 -q
rc=0
for name in offline mobile_touch arena; do
  echo "== $name"
  scenario_args "$name"
  timeout 180 target/debug/shinobi-game --shot-offline "$TMP/$name.ppm" --secs 2.0 "${ARGV[@]}" 2>&1 \
    | sed 's/\x1b\[[0-9;]*m//g' | grep -E "ERROR|panicked|shot:|level:" || true
  [ -s "$TMP/$name.ppm" ] || { echo "no frame rendered for $name"; exit 2; }
  python3 -c "from PIL import Image; Image.open('$TMP/$name.ppm').save('$TMP/$name.png')"
  GOLD="tests/golden/$name.png"
  if [ "${1:-}" = "--update" ]; then cp "$TMP/$name.png" "$GOLD"; echo "golden updated: $GOLD"; continue; fi
  [ -f "$GOLD" ] || { echo "no golden at $GOLD (run with --update once)"; exit 2; }
  python3 scripts/compare_images.py "$TMP/$name.png" "$GOLD" --diff "tests/golden/last_diff_$name.png" || rc=1
done
exit $rc
