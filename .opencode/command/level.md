---
description: Author or revise a GoldenHoles map, generator and level tests.
agent: levelsmith
---

Work on level `$ARGUMENTS`.

1. Read `AGENTS.md`, `docs/LEVELS.md`, `design/CONTENT_PLAN.md`, the current task brief and the closest existing map.
2. Update `design/levels/<map>.md` first: route, verticality, sightlines, cover, spawn/pickup clearance, hazards,
   mover/zone data and acceptance checks.
3. Author through a generator under `scripts/` and keep the generated `.lvl` header honest.
4. Add or update `crates/shinobi-sim/tests/arena_<map>.rs`.
5. Run:
   - `cargo test -p shinobi-sim --test arena_<map>`
   - `cargo test -p shinobi-sim --test walkable`
   - `cargo test -p shinobi-sim --test runwalls`
   - `LEVEL=levels/ffa_<map>.lvl SECS=30 AI=3 CLIENTS=2 scripts/demo_headless.sh`
6. Report files, commands, box count, destructible ratio, clearances and blockers.

Do not edit simulation/protocol/client/game code and do not run git writes.
