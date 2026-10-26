# Claude handoff guide

Read `AGENTS.md` first. It holds the repo rules, ownership, commands and acceptance gates.

## How work reaches you

`director` generates a self-contained package under `docs/handoffs/<date>-<slug>/`:

- `HANDOFF.md` - objective, scope, acceptance, commands already run, evidence, open questions.
- `changes.patch` - the complete diff against the sprint base.
- `logs/` - check, smoke and golden output.
- `shots/` - before/after screenshots or reference frames.

`docs/handoffs/LATEST.md` points at the newest package. If a handoff is incomplete, report exactly what is missing
instead of guessing.

## What to verify

1. Read `HANDOFF.md`, then `changes.patch`, then the relevant docs (`docs/LEVELS.md`, `docs/ART.md`,
   `docs/CONTENT_PLAN.md`, `docs/weapons/*.md`).
2. Confirm the change stays inside the stated ownership. Flag cross-domain edits.
3. Check determinism: server vs client prediction, fixed-point mover math, protocol version bump and roundtrip.
4. Run the stated commands. Prefer `scripts/check.sh --fast` first, then full `scripts/check.sh` when feasible.
5. For visual work, inspect the frames at the stated distances/tiers. Do not trust a render description alone.
6. Report: verdict, blocking defects, non-blocking risks, exact failing commands, and the smallest fix.

## Boundaries

- Do not commit, branch, merge or push unless the user explicitly asks you to.
- Do not rewrite another agent's files to make a check pass. Report the failure to `director`.
- Keep generated logs and screenshots out of commits unless they are deliberate goldens or reviewed references.
- Treat any wire change without a `VERSION` bump as a blocker.

## Useful commands

```bash
scripts/check.sh --fast
scripts/check.sh
cd crates/shinobi-game && cargo test
SECS=30 AI=3 CLIENTS=2 scripts/demo_headless.sh
LEVEL=levels/ffa_circuit.lvl SECS=30 AI=3 CLIENTS=2 scripts/demo_headless.sh
crates/shinobi-game/scripts/golden_check.sh
```
