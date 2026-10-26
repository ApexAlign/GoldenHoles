# GoldenHoles content plan

Status: Phase 1 in progress. Phase 0 scaffolding and baseline validation are complete.

## Locked decisions

- Build order: **Circuit-Breaker Arena**, then **The De-Frag Assembly**, then **Vector Vector**.
- Controls keep the stance switch: `1` katana, `2` bow up. Bow up disables the shield.
- **No loadout.** Every player owns every baseline weapon and every mod from spawn. Choices happen in-match through
  the arsenal; there is no pre-match payload, lobby gating or pickup gating.
- Vector Vector v1 is Y-up. No arbitrary gravity, gravity flip or gravity-ring traversal. Ring rotation is visual
  flavour only.
- Accepted engine extensions: level zones/movers/hazards, cylinder/ring authoring primitives, standable moving
  platforms, runtime arsenal/mod state on the wire.
- Workers do not run git writes. `director` owns shared contracts, branches, commits, merges and handoffs.

## Arsenal

| Category | Baseline | Alternate | Selection | Server state |
| --- | --- | --- | --- | --- |
| Stars | Standard | Glitch mine (max 2) | `R`/wheel cycle, `Q` throw | selected star, mine count, mine positions |
| Bow | Rail-Arrow | Tether Arrow | cycle while bow is up, full draw fires | selected arrow, tether link |
| Shield | Aegis block | Kinetic Battery | both installed; Battery is passive | battery meter, aura timer |
| Grapple | Vector-Link | Vortex Slam | both installed; Slam is contextual while diving | dive state, impact result |

No two mods conflict. Battery and Vortex are passive/contextual; only stars and arrows have an explicit cycle.

## Feature matrix

| Map / feature | Phase | Owner | Contract |
| --- | --- | --- | --- |
| Agent scaffolding and specs | 0 | director | `AGENTS.md`, agents, commands, this file |
| Level `zone`/`mover`/`hazard` directives | 1 | combat-smith | parser + server runtime + client prediction/render landed |
| `tube`/`ring` authoring primitives | 1-2 | combat-smith | deterministic segmented colliders; renderer reads same data |
| Standable movers and carry velocity | 1 | combat-smith | solid deterministic movers + push + client prediction landed; standable carry pending |
| Runtime arsenal state | 1 | combat-smith | `VERSION` bump, roundtrip, prediction parity |
| Circuit-Breaker layout | 1 | levelsmith | Phase 1C blockout + feature layer landed: `levels/ffa_circuit.lvl`, `arena_circuit.rs` |
| Data lanes / speed multiplier | 1 | combat-smith + levelsmith | four `speed_lane` zones live; wall-run speed effect server/client parity tested |
| Split-level nexus | 1 | levelsmith | walkable + void-edge tests |
| Kinetic solenoid pistons | 1 | combat-smith + levelsmith | 8 solid movers authored/predicted/rendered; parry launch pending |
| Core vent | 1 | combat-smith + levelsmith | `core_vent` zone authored; battery fill/aura/HUD reveal pending |
| Glitch trap on piston faces | 1 | combat-smith + levelsmith | volumes reserved; no hazard lines yet |
| De-Frag layout | 2 | levelsmith | `levels/ffa_defrag.lvl`, `arena_defrag.rs` |
| Velocity cylinders | 2 | combat-smith + levelsmith | tube primitive, spiral wall-run test |
| Grid conveyors | 2 | combat-smith + levelsmith | standable mover + carry test |
| Data-purge sweepers | 2 | combat-smith + levelsmith | 45 s hazard, telegraph, safe pocket |
| Overload pillars / tether discharge | 2 | combat-smith + levelsmith | tether link + pillar zone |
| Tether Arrow | 2 | combat-smith | anchor follow, chain, katana cut, yank |
| Vector Vector layout | 3 | levelsmith | `levels/ffa_vector.lvl`, `arena_vector.rs` |
| Prism ricochet | 3 | combat-smith + levelsmith | surface tag + bounce test |
| Refraction split arrow | 3 | combat-smith | three homing ricochets, counterplay |
| Ring tether yank/shear | 3 | combat-smith + levelsmith | Y-up tether, no gravity state |
| Integration, balance, QA | 4 | director + visual-lead | full gates, goldens, handoff |

## Engine contracts

### Level data

Additive fields on `LevelDesc` or a new shape list, so existing `boxes` tests keep working. Proposed:

- `zone`: axis-aligned trigger volume with a tag and key/value tuning. Server checks entry/exit and emits FX/state.
- `mover`: deterministic path (`Linear`/`PingPong`), extent, optional solid flag. Solid movers get a kinematic body
  and collider; non-solid movers are visual/hazard only.
- `hazard`: damage volume, period, telegraph, optional attached mover.
- `tube`: hollow vertical cylinder, authored as deterministic wall segments for KCC/wall-run.
- `ring`: ring platform, authored as deterministic curved segments for collision and nav.

The level file remains the shared source for server and client. No per-map Rust branches.

### Protocol

Runtime state only; no join-time loadout. Expected additions:

- selected star and arrow, battery meter/aura, mine count, tether target, dive/vortex phase;
- zone/mover/hazard events if the existing `ObstacleSpawn`/`NetEvent` path is not enough;
- `VERSION` bump (currently 30), roundtrip tests and prediction parity. `WELCOME.level_hash` is now
  `LevelFile::fingerprint()` (geometry + features, pickups excluded).

`director` freezes the exact wire shape before `combat-smith` edits `datagram.rs`.

### Determinism

- Fixed-point `ObstacleDesc` math for all movers/hazards.
- `libm` on movement/combat math.
- Server and client build the same colliders in the same order.
- 1000-tick replay for each new mechanic.
- No per-tick allocation on the hot path.

## Milestones

### Phase 0: scaffolding and specs

- `AGENTS.md`, `CLAUDE.md`, `opencode.json`, agents, commands, helper scripts.
- `docs/LEVELS.md`, `docs/ART.md`, `design/CONTENT_PLAN.md`.
- Three design docs in `design/levels/`.
- Validate config and scripts; run `scripts/check.sh --fast`.

Phase 0 status: complete. `opencode debug config` resolves `director` plus the three subagents and six commands.
`heavy.sh` and `handoff.sh` pass `bash -n`; `handoff.sh` produced a test package and was cleaned up.
`scripts/check.sh --fast` passes. The earlier `demo_smoke` scoreboard mismatch was a test bookkeeping gap:
bounty bonus frags were not counted in the event tally. `demo_smoke` now counts `MatchEvent::BountyKilled`
for both runs; both tests pass. No product code changed.

### Phase 1: foundations and Circuit-Breaker

- Level directives, runtime arsenal, standable movers.
- Circuit-Breaker blockout, systems, tests, art.
- Gate: playable map, full mod matrix on that map, headless smoke, overview frame.

Progress:
- `zone` / `mover` / `hazard` parsing, deterministic runtime, solid mover blocking/push and 1000-tick replay tests
  landed. `VERSION` is 30; `WELCOME.level_hash` is the combined `LevelFile` fingerprint (geometry + features,
  pickups excluded), with client prediction parity.
- Client prediction loads features and advances solid movers before combat/movement raycasts; the Bevy shell loads
  `LevelFile` and renders movers, zones and hazards in `crates/shinobi-game/src/features_vis.rs`.
- `levels/ffa_circuit.lvl` Phase 1C: 70 boxes, 20 destructible, 10 spawns, 6 pickups, 8 solid piston movers in four
  pairs, 4 `speed_lane` zones and 1 `core_vent` zone. Feature fingerprint `0xfa02_e5e8_9d24_5248`; 30 s headless
  smoke clean. No `hazard` lines yet.
- Speed lanes are live: only wall-running inside a tagged zone boosts speed, capped, with post-exit decay; server and
  client are bit-identical over 1000 ticks.
- Standable mover carry, vent consumption/battery/HUD reveal, piston parry/launch, glitch traps, `tube`/`ring`
  primitives and runtime arsenal state are still open.

### Phase 2: The De-Frag Assembly

- Tubes, conveyors, sweepers, overload pillars, tether.
- Gate: cylinder traversal, conveyor carry, sweeper telemetry, full tests.

### Phase 3: Vector Vector

- Rings, prisms, refraction, ring tether. No gravity.
- Gate: Y-up traversal, ricochet/refraction counterplay, no gravity-dependent test.

### Phase 4: integration

- Cross-map balance, bot nav, prediction parity, goldens, docs.
- Claude handoff for visual judgment or unreliable checks.

## Risks

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Curved geometry breaks KCC/nav/prediction | High | Author as deterministic segments; pin with tests before art |
| Standable movers destabilise reconciliation | High | Server-first + predictor parity tests; fallback grapple-only if blocked |
| All mods always on overloads controls/HUD | Medium | Contextual cycle, passive battery/vortex, compact arsenal strip |
| Vector Vector loses identity without gravity | Medium | Lean into prism/refraction/ring tether; add gravity later only if approved |
| Box budget or destructible ratio | Medium | Primitives are structural; generators report stats; tests enforce |
| Scope creep across ownership | High | Director task briefs; workers report cross-domain needs instead of editing |

## Definition of done

- Phase acceptance items are checked off.
- `scripts/check.sh --fast` per domain; full `scripts/check.sh` before integration.
- New behaviour documented and tested, including counterplay.
- Protocol version and parity tests correct.
- Visual frames reviewed by `visual-lead`.
- Remaining risks written into the sprint report or handoff.
