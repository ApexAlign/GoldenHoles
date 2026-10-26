#!/usr/bin/env bash
# Run the expensive GoldenHoles checks and keep their logs in a handoff package.
#   scripts/agent/heavy.sh <slug>
#   HEAVY_FAST=1 scripts/agent/heavy.sh <slug>       workspace tests only
#   HEAVY_GOLDENS=0 scripts/agent/heavy.sh <slug>    skip lavapipe goldens
#   SECS=30 AI=3 CLIENTS=2 scripts/agent/heavy.sh <slug>
set -euo pipefail
cd "$(dirname "$0")/../.."
export PATH="$HOME/.cargo/bin:$PATH"

slug="${1:?usage: scripts/agent/heavy.sh <slug>}"
stamp="${HANDOFF_DATE:-$(date +%F)}"
case "$slug" in
  "$stamp"-*) dir="docs/handoffs/$slug" ;;
  *) dir="docs/handoffs/${stamp}-${slug}" ;;
esac
logs="$dir/logs"
mkdir -p "$logs"
status="$logs/status.txt"
: > "$status"
overall=0

run() {
  local name="$1"; shift
  echo ">> $name"
  set +e
  "$@" > "$logs/$name.log" 2>&1
  local rc=$?
  set -e
  printf '%-18s exit=%s\n' "$name" "$rc" | tee -a "$status"
  [ "$rc" -eq 0 ] || overall=1
}

{
  echo "slug=$slug"
  echo "date=$(date -Iseconds)"
  echo "branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
  echo "head=$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "fast=${HEAVY_FAST:-0}"
  echo "goldens=${HEAVY_GOLDENS:-1}"
} | tee "$logs/meta.txt"

git status --short > "$logs/git-status.log" 2>&1 || true
git diff --stat > "$logs/git-diff-stat.log" 2>&1 || true

if [ "${HEAVY_FAST:-0}" = "1" ]; then
  run check-fast scripts/check.sh --fast
else
  run check scripts/check.sh
fi

env_args=(SECS="${SECS:-30}" AI="${AI:-3}" CLIENTS="${CLIENTS:-2}")
[ -n "${LEVEL:-}" ] && env_args+=(LEVEL="$LEVEL")
[ -n "${MAP:-}" ] && env_args+=(MAP="$MAP")
[ -n "${SKIP_PERF:-}" ] && env_args+=(SKIP_PERF="$SKIP_PERF")
run headless env "${env_args[@]}" scripts/demo_headless.sh

if [ "${HEAVY_GOLDENS:-1}" != "0" ]; then
  if [ -z "${VK_ICD_FILENAMES:-}" ] && [ -f /usr/share/vulkan/icd.d/lvp_icd.json ]; then
    export VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json
  fi
  run goldens crates/shinobi-game/scripts/golden_check.sh
else
  echo "goldens            skipped (HEAVY_GOLDENS=0)" | tee -a "$status"
fi

echo
echo "handoff package: $dir"
echo "logs:            $logs"
echo "status:"
cat "$status"
exit "$overall"
