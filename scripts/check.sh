#!/usr/bin/env bash
# The one pre-push check every agent runs (same as CI):
#   scripts/check.sh            workspace tests + Bevy game crate tests
#   scripts/check.sh --fast     workspace tests only (skip the game crate; first Bevy build takes minutes)
#   OFFLINE=1 scripts/check.sh  pass --offline to cargo (no crate downloads)
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
OFFLINE_ARG=(); [ "${OFFLINE:-0}" = "1" ] && OFFLINE_ARG=(--offline)
FAST=0; [ "${1:-}" = "--fast" ] && FAST=1

echo ">> cargo test --workspace"
cargo test --workspace "${OFFLINE_ARG[@]}"

if [ "$FAST" -eq 1 ]; then
  echo ">> skipped: shinobi-game cargo test (--fast)"
else
  echo ">> cd crates/shinobi-game && cargo test   (the Bevy app is outside the workspace)"
  (cd crates/shinobi-game && cargo test "${OFFLINE_ARG[@]}")
fi
echo "OK: check passed"
