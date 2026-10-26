# Level authoring

`crates/shinobi-sim/src/level.rs` is the source of truth for parsing and collision. This document is the workflow and
design contract for `levelsmith`. Read `AGENTS.md` first.

## Coordinates and budget

- Metres, Y up. Spawns use the capsule centre (`deck top + 0.81`).
- Open void is normal; the kill plane is below the map. Floating levels set `is_floating`.
- At most 1800 boxes per level.
- At least 70% of box volume destructible on maps designed for destruction. `scripts/lvl_lib.py` reports the ratio.
- Bricks are 2-6 m on a side. A full wall is at least 3 m; a step/lip is at most the character autostep.
- Spawns, pickups and weapon pedestals need at least 3.5 m clearance and must never sit on or under a breaking box.
- No unintended jump-only lips. A route must be walkable, rampable, wall-runnable or grappleable.

## Current directives

```text
box    cx cy cz  hx hy hz [rot_z_radians]
dbox   cx cy cz  hx hy hz rot_z hp
spawn  x y z [team]
pickup x y z [respawn_secs]
yumi   x y z [respawn_secs]
tag    x y z [respawn_secs]
zone   cx cy cz hx hy hz tag [key=value...]
mover  cx cy cz hx hy hz axis distance period_ticks solid|ghost tag [key=value...]
hazard cx cy cz hx hy hz period_ticks telegraph_ticks damage tag [key=value...]
tube   cx cy cz radius height thickness segments tag [key=value...]
ring   cx cy cz radius width thickness segments tag [key=value...]
```

`box`/`dbox` are cuboids. `rot_z` only; positive tilts +X up. Zones, movers and hazards are parsed into
`LevelFile.features`; `LevelDesc` stays geometry plus spawns.

### Zones, movers and hazards

- `zone`: server-side AABB trigger. `cooldown_ticks=N` (whole ticks, default 0) gates re-entry after exit.
- `mover`: one-axis ping-pong. `axis` is `x`, `y` or `z`; `distance` is one leg in metres; `period_ticks` is one leg,
  1..65535. `solid` creates a server collider; `ghost` is trigger-only. `phase_ticks=N` offsets the cycle.
- `hazard`: AABB damage volume. It is active for `active_ticks` (default 1) at the start of every `period_ticks`, and
  telegraphs for the last `telegraph_ticks`; `active_ticks + telegraph_ticks <= period_ticks`. Damage lands once per
  activation. `phase_ticks=N` offsets the cycle and must be `< period_ticks`.
- `tag` is a role name: letters/digits and `_ - . :`.
- Tuning is `key=value` with a finite f32 value; keys are `[A-Za-z_][A-Za-z0-9_]*`. Unknown keys are preserved.
- `hazard ... mover=N` rides mover `N` (0-based, in file order): its centre moves by that mover's displacement every
  tick, on the server and in the renderer. This is the glitch trap on a piston face. An index past the last `mover`
  line is a parse error.
- `tube`: a hollow vertical cylinder centred at `(cx, cy, cz)`, inner wall at `radius`, `height` tall and `thickness`
  thick. `ring`: a flat horizontal annulus whose centre line is at `radius`, `width` wide radially, `thickness` thick
  (`width < 2 * radius`). Both are static and built from `segments` (3..=256) straight cuboids rotated about Y
  (`ShapeDef::segments`), the same list on the server, in client prediction and in the renderer. Shapes only enter
  the feature fingerprint when a file has them.
- Shape tuning `door_start=i door_count=k` leaves segments `i..i+k` (mod `segments`) out: a doorway.
- Hazard tuning `sweep_y=d` lowers the hazard by `d` m linearly across each active window (a descending plane), and
  `shelter=1` spares any player with static geometry straight overhead, up to the hazard's authored centre (the top of
  the sweep).
- Hazard tuning `push=v` shoves a player inside it horizontally away from its centre at `v` m/s (plus a small lift),
  server-side; `pulse_every=n` re-arms it every `n` ticks within one active window. Zero damage is allowed.
- At most 4096 `hazard` lines. `parse_level_file(text).to_text()` is exact for all directives.

### Pending directives

These are shared contracts. `director` freezes the syntax and wire impact before `levelsmith` authors them.

| Directive | Purpose | Needed by |
| --- | --- | --- |
| `tube cx cy cz radius height thickness` | Hollow velocity cylinder | De-Frag |
| `ring cx cy cz radius tube_radius [rot_z]` | Ring platform / ring geometry | Vector |

Rules for authoring once approved:

- Mover/hazard periods use fixed-point tick math; never floating render positions.
- A mover that can be stood on must be marked solid and must carry player velocity.
  Implemented: a living player whose feet are within 0.08 m of a solid mover's top face (and over it) moves with
  the mover each tick, on every axis, before the sweep push. Server and client prediction run the same code
  (`features.rs` `carry_rider`). The carry is positional: jumping off keeps only the player's own velocity.
- A solid mover tagged `piston` strikes a player its leading face shoves, at most once per player per stroke. A
  shield raised inside the parry window (`guard.parry_s`) turns the strike into a launch along the face normal that
  carries `parry_reach` m (default 12) and peaks `parry_up` m high (default 4). Anything else takes `damage`
  (default 15, a `HazardSource::Obstacle(PISTON_SOURCE_ID)` hit) and a `stagger_ticks` stagger (default 30). The
  launch is server-only; client prediction corrects to it.
- Hazards need a telegraph window and a deterministic active phase.
- Zones are server-authoritative; the client only predicts the local response.
- Vector Vector may use ring rotation as visual flavour, but no gravity-vector or gravity-flip zones.
- Client features are loaded and rendered by `crates/shinobi-game/src/features_vis.rs` from `FeaturesRes`, and
  predicted by `ClientGame::new_with_features`; the combined level/feature fingerprint is checked in `WELCOME`.

## Generators

- Put new maps in `scripts/gen_<name>_level.py`.
- Import `lvl_lib.Level` for destructible fills and `runwalls.boxes(<preset>)` for tested wall-run lanes.
- The generated `.lvl` starts with the map title, a `GENERATED by ...` line, the generator path and the test path.
- Edit the generator, not the generated file. Re-run the generator after changing the shared library.
- Add the map preset to `scripts/runwalls.py` only if it needs run walls, and pin the positions in `runwalls.rs`.
- Print `Level.stats()` and fail the generator if it exceeds the box budget.

```bash
python3 scripts/gen_circuit_level.py
python3 scripts/gen_defrag_level.py
python3 scripts/gen_vector_level.py
```

## Map briefs

### Circuit-Breaker Arena (first)

Symmetrical motherboard stadium with a central two-story CPU-fan nexus, upper wireframe ring platforms, a sunken
lower pit, narrow glowing data lanes for wall-run speed, kinetic solenoid pistons on a 3 s cycle, a blue core vent,
and glitch traps on piston faces.

Phase 1C shipped: `levels/ffa_circuit.lvl` (70 boxes, 20 destructible, gameplay volume 85.6% destructible), generated
by `scripts/gen_circuit_level.py`; 12 tests in `arena_circuit.rs`, 9 in `level_features.rs`, and a clean 30 s headless
smoke. Feature layer: 8 solid `piston` movers in four pairs around `(±11, ±11)`, with 1 m thick x 4 m tall x 2 m wide
faces, a 6 m stroke, `period_ticks=90` per leg (3 s full round trip), `phase_ticks=0/90` so paired tracks alternate,
`y 0..4`; four `speed_lane` zones hugging one 28 m wall-run face per lane (`wall_run_m=10`, `mult=1.15`, `cap=1.6`,
`decay=0.9`); and one `core_vent` zone at `(0, -3.5, 0)` half `(4, 2.5, 4)` (`fill_ticks=90`, `cooldown_ticks=1200`,
`reveal_ticks=300`). Speed lanes are live, not server-trigger-only: a wall-run inside a tagged lane gets the ramp/cap
bonus and post-exit decay exactly as documented in `docs/movement.md`, and server and client prediction are
bit-identical over 1000 ticks. The `core_vent` remains a server trigger until the battery slice lands; its tuning is
still `fill_ticks=90`, `cooldown_ticks=1200`, `reveal_ticks=300`. No `hazard` lines ship yet: glitch-trap and vent
damage are the next slice. Feature fingerprint `0xfa02_e5e8_9d24_5248`.

Design targets:

- Contact within 8 s. At least three routes into the nexus: ground lane, upper ring, grapple line.
- Data lanes are long enough to build a wall-run multiplier but have an exit that is not a dead end.
- Pistons are readable for at least 0.6 s before firing; a perfect parry launches the player, a miss is punished.
- Core vent is contested, not spawn-adjacent; its HUD reveal is a real cost.
- Spawns have two exits and no line of sight to the vent.

### The De-Frag Assembly

Bottomless vertical chasm with four hollow velocity cylinders, sliding grid conveyors, a descending data-purge
sweeper every 45 s, overload pillars that discharge tethered enemies, and glitch traps inside the cylinders.

Design targets:

- Three vertical layers with wall-run or grapple routes between them; no single elevator bottleneck.
- Cylinders support a full up/down spiral without a seam that drops the wall-run.
- Conveyors are standable and their direction is readable from the top.
- Sweeper has a safe drop-grind pocket and a visible countdown.
- Overload pillars are placed so a tether line can reach them without crossing the void.

### Vector Vector (v1, no gravity mechanics)

Interlocking rings, floating neon cubes and prism pillars; cyan refraction panel walls; ring tethers that yank or
shear. Gravity rings rotate as visual flavour only.

Design targets:

- Ring routes are traversable with the normal Y-up jump, wall-run and grapple budget.
- Prism ricochet and refraction split have clear lanes and counterplay around cover.
- No geometry or trigger assumes a gravity flip.
- A tether between rotating rings is a movement/knockback tool, not a physics-orientation change.

## Tests

Every map gets `crates/shinobi-sim/tests/arena_<map>.rs` covering:

- Spawns and pickups have clearance and stand on solid, non-breaking geometry.
- At least 70% of gameplay box volume is destructible, and the box count is under budget.
- Walkable route from every spawn to the main objective.
- No unintended jump-only lip on the main route.
- Void edges are intentional and telegraphed.
- Zone/mover/hazard ids are unique and deterministic.
- A 30 s headless smoke produces fights and no stalls.

```bash
cargo test -p shinobi-sim --test arena_circuit
cargo test -p shinobi-sim --test walkable
cargo test -p shinobi-sim --test runwalls
LEVEL=levels/ffa_circuit.lvl SECS=30 AI=3 CLIENTS=2 scripts/demo_headless.sh
```

## Definition of done

- Design doc in `design/levels/<map>.md` matches the generated level.
- Generator is the source; generated file header is honest.
- Box budget, destructible ratio and clearances are reported.
- Map test, walkable, runwalls and headless smoke pass.
- No simulation/protocol edits from `levelsmith`.

## The De-Frag Assembly (`levels/ffa_defrag.lvl`)

Generated by `scripts/gen_defrag_level.py`; tests in `crates/shinobi-sim/tests/arena_defrag.rs`. Bottomless 120 x 120 m.
Central 20 x 20 m platforms at y = 5 / 25 / 50; four velocity tubes (inner r 10 m, 60 m tall, five stacked sections,
a centre-facing door on each layer, catch floor at the base); bridges into the doors on the top and low layers,
four 12 x 12 m conveyor panels (solid movers, 9 m stroke, 3 s legs) on the mid layer; one `data_purge` hazard
(45 s cycle, 8 s descent from y = 55 to y = 5, 3 s telegraph, lethal, `shelter=1`, first descent at 37 s); drop
ledges under the mid platform as safe pockets; six overload pillars with `overload_pillar` zones (the Tether
discharge runtime is not built yet); 10 spawns (top and mid); 4 data cubes. Open: conveyor end pauses, spiral
wall-run tolerance on 24-segment tubes, pillar discharge.

## Vector Vector (`levels/ffa_vector.lvl`)

Generated by `scripts/gen_vector_level.py`; tests in `crates/shinobi-sim/tests/arena_vector.rs`. Y-up only. Three
`ring` platforms (tops y = 0 / 8 / 16, centre-line r 30 / 24 / 18 m, 2.5 m wide) over the void; four radial ramps
along X (rot_z only) crossing over the centre at different heights; 12 floating neon cubes; 8 prism pillars between
the rings (`prism` zones); 4 refraction rim panels on the outer ring (`refraction_panel` zones); a `singularity`
push hazard (60 s cycle, 10 s pulse, first at 50 s, 14 m/s outward shove every 0.5 s, no damage, no gravity change);
10 spawns on the outer ring; 4 data cubes on the inner ring. Open: refraction split arrows and the prism surface
tag (stars already ricochet with their own bounce budget), a ring gap/spiral route, ramp angles other than X.
