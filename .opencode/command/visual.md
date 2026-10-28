---
description: Art-direct, implement and visually verify a GoldenHoles target.
agent: visual-lead
---

Do visual work for `$ARGUMENTS`.

1. Read `AGENTS.md`, `docs/ART.md`, `docs/screens/README.md`, the current task brief and the reference frames.
2. Update `docs/ART.md` with palette, materials, silhouettes and readability intent before implementation.
3. Implement only rendering, game-shell, FX or HUD art. Request state/API changes from `combat-smith`.
4. Capture before/after frames at 5 m, 25 m and 60 m; check Low/Medium/High tiers.
5. Run `cd crates/shinobi-game && cargo test` and `crates/shinobi-game/scripts/golden_check.sh`.
6. Re-record goldens only after reviewing every changed image.
7. Report frame paths, tier results, blocking defects and a visual verdict.

Do not edit simulation/protocol code and do not run git writes.
