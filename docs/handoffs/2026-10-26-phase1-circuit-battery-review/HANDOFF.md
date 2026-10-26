# Claude handoff: phase1-circuit-battery-review

- Date: 2026-10-26
- Branch: `main`
- Base: `HEAD` (`d70bb42`)
- Package: `docs/handoffs/2026-10-26-phase1-circuit-battery-review`
- Patch: `changes.patch`
- State: **uncommitted working tree**. No git writes were made.

## Why this is a handoff

Final review of the Phase 0/1 integration. The Circuit-Breaker feature layer, client prediction/render, and
speed-lane movement are complete and tested. The Kinetic Battery/core-vent slice is implemented in simulation,
server, and owner wire (v31), and its focused tests pass, but the task was interrupted before the full
workspace/game gate and before HUD/visual wiring. Claude should re-run all gates, review the battery slice, and
finish the HUD/visual and remaining Circuit mechanics.

## What is done

### Phase 0: agent scaffolding and specs

- `AGENTS.md`, `CLAUDE.md`, `opencode.json`.
- `.opencode/agent/{director,levelsmith,combat-smith,visual-lead}.md`.
- `.opencode/command/{sprint,level,weapon,visual,integrate,handoff}.md`.
- `scripts/agent/{heavy,handoff}.sh`, `docs/handoffs/TEMPLATE.md`, `docs/handoffs/LATEST.md`.
- `docs/LEVELS.md`, `docs/ART.md`, `design/CONTENT_PLAN.md`, `design/levels/{circuit,defrag,vector}.md`.
- Config validation and helper-script smoke tests were done during Phase 0.

### Phase 1: level features and Circuit-Breaker

- `crates/shinobi-sim/src/level.rs`: additive `zone` / `mover` / `hazard` directives, validation, exact `to_text`,
  `LevelFile::fingerprint()`, `LevelFeatures::speed_lane_for`.
- `crates/shinobi-sim/src/features.rs`: deterministic zone/mover/hazard runtime, solid mover blocking/push,
  hazard contacts, `FeatureEvent`.
- `crates/shinobi-server/src/setup.rs` and `tick.rs`: features carried from level load to server tick.
- `VERSION` history: 29 -> **30** for the combined level fingerprint, then **31** for the Battery/aura OwnState.
- `crates/shinobi-client`: `ClientGame::new_with_features`, predicted solid movers, predicted speed-lane movement,
  `UdpGame::new_with_features`; reconnect preserves features.
- `crates/shinobi-game`: shell loads `LevelFile`, passes features to `UdpGame`, renders movers/zones/hazards in
  `features_vis.rs`; `docs/ART.md` has the Circuit palette intent.
- `levels/ffa_circuit.lvl` + `scripts/gen_circuit_level.py`: 70 boxes (42 structural, 28 gameplay, 20 destructible,
  85.6% gameplay volume destructible), 10 spawns, 6 pickups, 8 solid piston movers in four pairs, four
  `speed_lane` zones, one `core_vent` zone. Feature fingerprint `0xfa02_e5e8_9d24_5248`. Zero authored `hazard`
  lines.
- `crates/shinobi-sim/tests/arena_circuit.rs` (12 tests) and `walkable.rs` cover the map, features, clearance,
  budget, destructible share, and live feature smoke.
- Speed lanes: wall-run-only ramp/cap boost and post-exit decay, documented in `docs/movement.md`; server/client
  bit-identical over 1000 ticks (`crates/shinobi-client/tests/speed_lane_parity.rs`).

### Phase 1D battery slice (needs review)

- `crates/shinobi-sim/src/shield.rs`, `docs/weapons/shield.md`: Kinetic Battery 0..=1000; charges from blocked
  enemy stars/arrows, any successful parry, and `core_vent` over `fill_ticks=90` with per-player
  `cooldown_ticks=1200`; full charge consumes the battery and starts a 300-tick katana aura (1.2x unguarded slash
  damage only); aura cannot retrigger while active, but a full battery can retrigger on expiry; death cancels aura.
- `balance.rs`: new `shield.*` keys, validation, and `to_text`.
- `own_state.rs`: v31 owner wire `battery_q` (10 bits), `aura_ticks_left` (16), `aura_total_ticks` (16); base
  OwnState grows 41 -> 47 bytes, worst case 116 bytes. Roundtrip/size tests updated.
- `pvp.rs`, `power_pass.rs`, `tick.rs`, `replicate.rs`: server charge/aura/vent integration.
- `crates/shinobi-sim/tests/shield_battery.rs` (6 tests): block/parry charges, melee block does not charge, vent
  fill/cooldown/pause, aura damage-only and expiry, movement unchanged, 1000-tick replay.
- Client exposes `shield_battery`, `shield_aura_ticks`, `shield_aura_total` through `ClientGame`/`RenderView`.
  **HUD draw list is not wired yet**, and remote-player aura FX is deferred.

## Acceptance

- [x] Targeted sim/proto/client tests for the completed slices pass.
- [x] Wire changes bumped `VERSION` (31) and have roundtrip plus prediction-parity coverage.
- [x] Battery/aura 1000-tick replay is identical; speed-lane 1000-tick server/client parity is exact.
- [x] 30 s Circuit headless smoke: 1800 ticks, 60 Hz, 8 kills, `problems: none`.
- [x] Full `scripts/check.sh` green on v32 tree (user ran it 2026-10-26: sim 261, game 175, all integration suites ok).
- [~] HUD battery meter + aura cue wired (hud.rs, hud_bridge.rs; compiled and green). Vent reveal/dimming still open.
- [~] Remote aura: FX_AURA snapshot bit (fx now 5 bits, VERSION 32), server sets it, remote katana edge swells (character_look.rs)..
- [ ] Piston parry/launch and glitch traps.
- [~] Runtime arsenal: Tether Arrow DONE, check.sh green on v34 ( PlayerIntent.tether level, ARROW_TETHER flag, 40% dmg + yank via Character::yank, star keys toggle arrow while bow up, HUD label; test in power_weapons.rs). Glitch mine DONE, check.sh green on v33 (StarKind::Mine=1, VERSION 33; sticks, arms 0.6s, 2 m trigger, 30 dmg, max 2, cycle Standard>Flash>Mine; step_mine in shuriken.rs, 2 tests in star_variants.rs; Flash + Mark kept). Still open: Rail/Tether arrow selection, Tether Arrow
      effect done. Vortex Slam written blind (character.rs dive step DIVE_SPEED 32 while slide held in a fall, shared with the client predictor; server vortex in tick.rs: 5 m radius, 20-40 dmg, 10 m/s knock; no wire change; tests/vortex_slam.rs). Dive HUD cue and FX still open.
- [ ] Standable mover carry.
- [ ] `tube` / `ring` primitives and De-Frag/Vector maps.
- [ ] Visual review frames at 5/25/60 m and all tiers; goldens.
- [ ] Commit/integration.

## Commands and evidence

```text
cargo test -p shinobi-sim --test shield_battery          # 6 passed
cargo test -p shinobi-client --test speed_lane_parity   # passed
cargo test -p shinobi-sim --test arena_circuit          # 12 passed
cargo test -p shinobi-proto                             # passed
LEVEL=levels/ffa_circuit.lvl SECS=30 AI=3 CLIENTS=2 scripts/demo_headless.sh
# 1800 ticks, 60 Hz, 8 kills, 0 overruns, problems: none
```

Full logs are in `logs/`. `logs/diff-stat.log` covers tracked changes only; `changes.patch` also contains the
untracked files.

Known prior evidence (from the speed-lane task, before the battery slice): `scripts/check.sh` full passed,
including 175 game-crate tests; 30 s Circuit smoke passed. That evidence does **not** cover the v31 battery
changes.

## Open questions

- Confirm the Battery aura shape: 5 s, 1.2x unguarded slash only, no movement/reach change, retrigger on expiry.
- Should remote players visibly show the aura? If yes, add a snapshot status bit or reliable event.
- Should `core_vent` drive a HUD reveal via `reveal_ticks`, and should the zone dim while on cooldown?
- Should Flash remain a third star kind, or does the design's Standard + Glitch mine replace it?
- The current repo still has legacy yumi/tag pedestals and pickups; the no-loadout decision needs an audit across
  non-Circuit levels.
- Piston parry currently has no launch/parry hook; decide whether client prediction is required before shipping it.

## Risks

- **Partial verification:** do not treat this as integration-ready until full `scripts/check.sh` and the game crate
  pass on the v31 tree.
- **HUD/visual gap:** the Battery is invisible in play; `features_vis` boxes are placeholders and have not had a
  reviewed visual pass.
- **OwnState grew by 6 bytes** (47 typical, 116 worst case); re-check snapshot bandwidth and any fixed buffers.
- **Speed-lane wall matching** assumes the player centre is between the wall and lane middle. The authored 1 m
  Circuit lanes are fine; thicker lanes may need a wall-contact query.
- **No commits:** the patch is 322 KB and includes untracked files; apply with `git apply --binary changes.patch`.
- **Remote aura/vent FX** are not replicated; only the owner sees the battery/aura state.
- **Feature rendering** reads `MoverDef::pos_at(local_tick)`; reconnect keeps the same features, but runtime level
  changes are not supported.

## Package contents

```text
HANDOFF.md
changes.patch
logs/diff-stat.log
logs/arena-circuit.log
logs/headless-circuit.log
logs/proto.log
logs/shield-battery.log
logs/speed-lane-parity.log
```
