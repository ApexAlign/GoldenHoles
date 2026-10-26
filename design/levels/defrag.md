# The De-Frag Assembly

Status: design brief for Phase 2. No level file exists yet.

## Identity

A bottomless vertical sandbox of glass velocity cylinders, sliding grid conveyors and a descending data-purge
sweeper. The map is about route choice under a timer, not about a safe floor.

## Footprint

- 120 x 120 m footprint, no ground plane. Kill plane below the lowest platform.
- Three layers: top `y = 50`, mid `y = 25`, low `y = 5`.
- Four hollow velocity cylinders at the corners, 10 m radius, 60 m tall.
- Central conveyor grid 40 x 40 m, four sliding panels.
- Sweeper plane spans the full footprint, descending from `y = 55` to `y = 5`.

## Layout

### Velocity cylinders

- Four glass tubes, each with copper stripping and a continuous interior wall-run surface.
- Spiral route: entry at one layer, one full turn, exit at the next layer. Up and down are both valid.
- Tube walls are authored as deterministic segments; seams must not drop the wall-run.
- A glitch mine can be placed on the interior wall; the blast is a trap for a pursuing player.
- Each tube has one clear entry and one exit per layer; no dead-end interior.

### Grid conveyors

- Four 12 x 1 x 12 m panels slide on linear paths between the central platforms.
- Standable and carry player velocity; the top face has directional chevrons.
- Grapple can hitch to a panel edge and slingshot; standing is the slower but safer route.
- Panels pause for 0.5 s at each end so a player can read the direction.

### Data-purge sweepers

- Every 45 s an orange plane descends from `y = 55` to `y = 5` over 8 s, then resets.
- Telegraph: a countdown band and a rising hum before the descent.
- Safe pockets: drop-grind ledges under the mid-layer platforms; visibly darker and calmer.
- Sweeper is lethal but never spawns on top of a player.

### Overload pillars

- Six copper/steel pillars on the mid and low layers, each with a charged state.
- A Tether Arrow through an enemy into a charged pillar discharges electricity: stun and shield fracture.
- Discharge consumes the pillar charge for 8 s, with a visible cooldown.
- Placement must allow a tether line that does not cross the void or a cylinder wall.

## Spawns and pickups

- 10 spawns split between the top and mid layers, each with two exits and no sweeper path within 3 s.
- 4 data cubes on the central platforms.
- No weapon pedestals; all weapons and mods are owned from spawn.
- Spawn platforms are permanent.

## Art and readability

- Near-black depth fog, distant grid fragments, no visible bottom.
- Glass tubes with copper stripping; the interior wall-run line remains visible through the glass.
- Conveyor chevrons animate in travel direction.
- Sweeper has a hard orange leading edge and a descending countdown; safe pockets are visibly darker.
- Overload pillars arc blue when charged.

## Tests

- `arena_defrag.rs`: spawn clearance, layer routes, tube entries/exits, conveyor paths, sweeper safe pockets.
- Cylinder wall-run test: enter at layer N, complete a spiral, exit at layer N+1 without losing the wall state.
- Conveyor carry test: standing player inherits panel velocity and can jump off.
- Sweeper test: deterministic descent, no spawn kill, safe pocket survives.
- 30 s headless smoke with bots on the map.

## Contract needs

- `tube` primitive with deterministic segmented collider and render data.
- `mover` with solid + carry.
- `hazard` with telegraph and period.
- `zone` for overload pillars and glitch-mine surfaces.
- Tether Arrow anchor and discharge interaction.

## Open questions

- Exact tube radius vs wall-run seam tolerance.
- Sweeper period after playtest; 45 s is the first target.
- Whether conveyor panels are solid on all faces or only the top.
- Overload pillar charge duration and stun length.
