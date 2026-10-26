#!/usr/bin/env bash
# Build a self-contained Claude handoff package.
#   scripts/agent/handoff.sh <slug>
#   HANDOFF_RUN=1 scripts/agent/handoff.sh <slug>   run the heavy checks first
#   HANDOFF_BASE=<rev> scripts/agent/handoff.sh <slug>
set -euo pipefail
cd "$(dirname "$0")/../.."

slug="${1:?usage: scripts/agent/handoff.sh <slug>}"
stamp="${HANDOFF_DATE:-$(date +%F)}"
case "$slug" in
  "$stamp"-*) dir="docs/handoffs/$slug" ;;
  *) dir="docs/handoffs/${stamp}-${slug}" ;;
esac

if [ "${HANDOFF_RUN:-0}" = "1" ]; then
  scripts/agent/heavy.sh "$slug"
fi

mkdir -p "$dir/logs" "$dir/shots"
base="${HANDOFF_BASE:-HEAD}"
branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
head="$(git rev-parse --short "$base" 2>/dev/null || echo unknown)"
patch="$dir/changes.patch"

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
git diff --binary "$base" > "$tmp" 2>/dev/null || true
while IFS= read -r f; do
  case "$f" in
    docs/handoffs/*|playtest/*|target/*|session-*.md) continue ;;
  esac
  git diff --no-index --binary -- /dev/null "$f" >> "$tmp" 2>/dev/null || true
done < <(git ls-files --others --exclude-standard)
mv "$tmp" "$patch"
trap - EXIT

git diff --stat "$base" > "$dir/logs/diff-stat.log" 2>/dev/null || true

notes="${HANDOFF_NOTES:-Fill in the objective, acceptance, open questions and risks before sending.}"
{
  echo "# Claude handoff: $slug"
  echo
  echo "- Date: $(date -Iseconds)"
  echo "- Branch: \`$branch\`"
  echo "- Base: \`$base\` ($head)"
  echo "- Package: \`$dir\`"
  echo "- Patch: \`changes.patch\`"
  echo
  echo "## Why this is a handoff"
  echo
  echo "$notes"
  echo
  echo "## Read first"
  echo
  echo "1. \`AGENTS.md\`"
  echo "2. \`design/CONTENT_PLAN.md\`"
  echo "3. \`changes.patch\`"
  echo "4. The relevant domain doc under \`docs/\`."
  echo
  echo "## Scope and ownership"
  echo
  echo "List the intended files and confirm no cross-domain edits. The director owns shared contracts and git."
  echo
  echo "## Acceptance"
  echo
  echo "- [ ] Domain tests pass."
  echo "- [ ] Wire changes bumped \`VERSION\` and have roundtrip plus prediction-parity tests."
  echo "- [ ] Determinism replay is identical."
  echo "- [ ] Headless smoke produces fights and no stalls."
  echo "- [ ] Visual changes have reviewed frames at 5 m, 25 m and 60 m."
  echo
  echo "## Commands and evidence"
  echo
  if [ -s "$dir/logs/status.txt" ]; then
    echo '```text'
    cat "$dir/logs/status.txt"
    echo '```'
  else
    echo "No heavy-check logs in this package. Run \`HANDOFF_RUN=1 scripts/agent/handoff.sh $slug\`."
  fi
  echo
  echo "## Open questions"
  echo
  echo "- "
  echo
  echo "## Risks"
  echo
  echo "- "
  echo
  echo "## Package contents"
  echo
  echo '```text'
  find "$dir" -maxdepth 2 -type f -printf '%P\n' | sort
  echo '```'
} > "$dir/HANDOFF.md"

rel="${dir#docs/handoffs/}"
cat > docs/handoffs/LATEST.md <<EOF
# Latest handoff

- [${rel}](${rel}/HANDOFF.md)
- Branch: \`$branch\`
- Base: \`$base\` ($head)
EOF

echo "handoff: $dir"
echo "patch:   $patch"
echo "latest:  docs/handoffs/LATEST.md"
