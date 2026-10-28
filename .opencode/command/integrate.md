---
description: Run the GoldenHoles integration gate and review all worker diffs.
agent: director
---

Run the integration gate for `$ARGUMENTS`.

1. Inspect `git status`, `git diff` and the sprint brief.
2. Confirm every change stays inside its owner's files and that any protocol change bumped `VERSION` and has
   roundtrip plus prediction-parity tests.
3. Run `scripts/check.sh`.
4. Run the map smoke in `AGENTS.md`; for visual work inspect the frames and get `visual-lead` approval.
5. Confirm docs, balance keys and tests cover the new behaviour and its counterplay.
6. Report pass/fail, exact failures, cross-domain edits and remaining risks.
7. Do not commit unless the user explicitly asks. If asked, stage only intended files and use the correct domain
   prefix.
