---
description: Specify and implement a weapon, mod, mover, hazard or trigger system.
agent: combat-smith
---

Implement `$ARGUMENTS`.

1. Read `AGENTS.md`, `design/CONTENT_PLAN.md`, the current task brief and the relevant mechanics docs.
2. Write the spec first: controls, state, server authority, counterplay, tuning keys, wire impact and tests.
3. Implement the server path, then client prediction/reconciliation, then HUD hooks. Use `libm` and fixed-point
   tick math where the movement/obstacle path requires it.
4. If the wire changes, bump `VERSION` in `crates/shinobi-proto/src/datagram.rs` and update roundtrip and parity tests.
5. Add counterplay tests and a 1000-tick determinism replay.
6. Run:
   - `cargo test -p shinobi-sim`
   - `cargo test -p shinobi-proto`
   - `cargo test -p shinobi-server`
   - `cargo test -p shinobi-client`
   - `scripts/check.sh --fast`
7. Report the spec path, protocol version, balance keys, commands, determinism evidence and risks.

Do not edit levels, arena tests or rendering, and do not run git writes.
