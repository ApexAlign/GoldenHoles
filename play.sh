#!/usr/bin/env bash
# Easy start: a plain-text menu in front of demo.sh, for anyone who doesn't want to
# remember MAP=/BOTS=/TEAM_SIZE= env vars. Just run ./play.sh and answer the prompts.
# Non-interactive (CI, scripts, piping /dev/null in): falls straight through to demo.sh's own defaults.
set -euo pipefail
cd "$(dirname "$0")"

if [ ! -t 0 ]; then exec ./demo.sh; fi

echo "=== Golden Shinobi: quick start ==="
echo "Map:"
echo "  1) Sky city (default, vertical, 4 tiers)"
echo "  2) Skyfall Archipelago"
echo "  3) Run Gauntlet"
echo "  4) Ground arena (small)"
echo "  5) Ground arena (large)"
read -rp "Pick a map [1]: " map_choice
case "${map_choice:-1}" in
  2) MAP=isles ;;
  3) MAP=runs ;;
  4) MAP=small ;;
  5) MAP=big ;;
  *) MAP=sky ;;
esac

read -rp "Team deathmatch? Enter a team size (e.g. 4 or 8), or leave blank for free-for-all: " team_size
TEAM_SIZE="${team_size:-0}"

if [ "$TEAM_SIZE" = "0" ]; then
  read -rp "How many AI bots to fill the match? [5]: " bots
  BOTS="${bots:-5}"
  export BOTS
fi

export MAP TEAM_SIZE
echo "Starting: MAP=$MAP TEAM_SIZE=$TEAM_SIZE ${BOTS:+BOTS=$BOTS}"
exec ./demo.sh
