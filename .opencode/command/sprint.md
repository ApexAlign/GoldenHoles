---
description: Start or resume a GoldenHoles sprint with a written brief and worker dispatch.
agent: director
---

Start sprint `$ARGUMENTS`.

1. Read `AGENTS.md`, `design/CONTENT_PLAN.md` and the latest handoff.
2. Confirm the current branch and worktree are the shared `/home/apex/GoldenHoles` checkout.
3. Create `docs/handoffs/<date>-<slug>/HANDOFF.md` with the sprint objective, phase, owned files, acceptance,
   non-goals and verification commands.
4. Write one task brief per worker. Sequence shared contracts before dependent work; parallelise only non-overlapping
   files.
5. Dispatch `levelsmith`, `combat-smith` and `visual-lead` with their briefs. Workers must not run git writes.
6. Report the sprint state, the dispatch table, and the first integration checkpoint.

Do not commit unless the user explicitly asks.
