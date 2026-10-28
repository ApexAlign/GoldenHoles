---
description: Build a self-contained Claude handoff package for a sprint or review.
agent: director
---

Create a Claude handoff for `$ARGUMENTS`.

1. Confirm the sprint branch, base commit and the intended scope.
2. Run `scripts/agent/handoff.sh "$ARGUMENTS"`.
3. Verify the package contains `HANDOFF.md`, `changes.patch`, `logs/` and any required `shots/`.
4. In `HANDOFF.md`, state: objective, acceptance, commands run, evidence, open questions, risks, and the exact
   files Claude should inspect first.
5. Update `docs/handoffs/LATEST.md` to point at the new package.
6. Report the package path and why the handoff is needed (visual judgment, gameplay feel, or an unreliable check).

Do not commit unless the user explicitly asks. Do not include secrets or generated logs in the commit.
