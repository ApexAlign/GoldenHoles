# Claude handoff template

`scripts/agent/handoff.sh <slug>` fills this package structure. Replace the prompts before sending.

## Package

```text
docs/handoffs/<date>-<slug>/
  HANDOFF.md
  changes.patch
  logs/
    meta.txt
    status.txt
    check.log
    headless.log
    goldens.log
  shots/
```

## Required content

- **Why this is a handoff**: visual judgment, gameplay feel, or an unreliable check.
- **Objective**: one paragraph, plus the sprint brief link.
- **Scope**: files changed, ownership check, non-goals.
- **Acceptance**: checked items and the exact commands that prove them.
- **Evidence**: test output, smoke summary, frame paths, before/after.
- **Open questions**: the decisions Claude must not guess.
- **Risks**: determinism, protocol, perf, AI, readability.
- **Read first**: `AGENTS.md`, `design/CONTENT_PLAN.md`, `changes.patch`, then the domain doc.

## Verification checklist

- [ ] `scripts/check.sh --fast` or full `scripts/check.sh` result is present.
- [ ] Headless smoke ran on the map under test.
- [ ] Protocol version bumped if the wire changed.
- [ ] Roundtrip and prediction-parity tests are present.
- [ ] 1000-tick determinism replay is identical.
- [ ] Visual frames exist for 5 m, 25 m and 60 m when relevant.
- [ ] Goldens were re-recorded only after review.
- [ ] No secrets, generated logs or unrelated screenshots are in the commit.

## Notes

- Do not commit, branch, merge or push unless the user explicitly asks.
- Report the smallest fix, not a rewrite of another agent's domain.
- If the package is incomplete, say exactly what is missing.
