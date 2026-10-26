# Vector Vector

Status: design brief for Phase 3. No level file exists yet. Y-up only; no gravity mechanics.

## Identity

An interlocking ring arena of floating neon cubes, mirror-finish prism pillars and cyan refraction panels. Routes
twist through ring platforms; stars ricochet and arrows split. Ring rotation and a singularity pulse are visual and
knockback flavour, not gravity changes.

## Footprint

- 100 x 100 m footprint over a dark void.
- Three horizontal ring platforms at `y = 0`, `y = 8` and `y = 16`, radius 30 m, 24 m and 18 m, tube 2.5 m.
- Ring gaps and bridges create a spiral route; all traversal uses jump, wall-run and grapple.
- 12 floating neon cubes as cover and grapple anchors.
- 8 prism pillars on the ring lines.
- 4 refraction panel walls on the outer ring.

## Layout

### Rings

- Concentric horizontal rings, connected by four radial bridges at different angles.
- Interlocking look comes from offset ring segments, not from non-Y-up collision.
- Ring top faces are walkable; the outer rim is a wall-run surface.
- A ring-to-ring jump is within the normal jump + double-jump budget; no gravity assist.
- Ring rotation is a slow visual drift only. Collision rings remain static in v1.

### Prism pillars

- Mirror obsidian pillars, 2 m square, 6-10 m tall, with sharp highlight edges.
- Stars and arrows reflect off the flat faces; the reflected path is deterministic.
- Prisms are solid cover and can be grappled.
- A ricochet cannot chain infinitely; each projectile has a bounce budget.

### Refraction panels

- Cyan grid walls, 8 x 6 m, placed on the outer ring.
- A Rail-Arrow passing through a panel splits into three homing ricochets with reduced damage.
- The split has a clear lane and a visible panel state; it is not a random spray.
- Counterplay: cover, shield, and the panel's own cooldown/charge.

### Ring tether

- Tether Arrow can anchor between two ring anchors or between an enemy and a ring anchor.
- The tether yanks along the link and can shear a target off a ring edge.
- No gravity vector changes while tethered; wall-run and normal movement remain Y-up.
- Katana cuts the tether; the cut is a strong counterplay cue.

### Singularity pulse

- Every 60 s the central singularity emits a radial pulse for 10 s: an outward push and a strong visual/FX event.
- **No gravity flip, no upside-down camera, no inverted controls.**
- The pulse is a force/knockback zone with a clear ring telegraph; wall-run and tethered players are not exempt
  unless the movement contract says otherwise.

## Spawns and pickups

- 10 spawns on the outer ring, each with two routes inward.
- 4 data cubes on the inner ring.
- No weapon pedestals; all weapons and mods are owned from spawn.
- Spawn platforms are permanent.

## Art and readability

- Interlocking rings, floating neon cubes, prism obsidian, cyan grid panels.
- Ring routes must read at 5/25/60 m; no fake gravity horizon.
- Prism faces have a specular cue that distinguishes them from normal walls.
- Refraction panels have a visible split lane.
- Ring tether is a taut bright line; the cut produces sparks.
- Singularity pulse is a ring shockwave, not a screen flip.

## Tests

- `arena_vector.rs`: spawn clearance, ring routes, bridge walkability, prism placement, refraction lanes.
- Prism ricochet test: deterministic reflection with a bounce budget.
- Refraction test: three splits, damage reduction, counterplay.
- Ring tether test: yank, cut, no gravity-state change.
- Singularity test: pulse timing, push direction, no camera/gravity change.
- 30 s headless smoke with bots on the map.

## Contract needs

- `ring` primitive with deterministic segmented collider and render data.
- Surface tags for prism and refraction panels.
- `zone` for the singularity pulse.
- Tether anchor on ring geometry.
- Projectile bounce budget and split state on the wire.

## Open questions

- Should ring rotation ever affect collision, or remain visual only for all of v1?
- How many prism bounces before a star expires?
- Split-arrow homing strength and damage split.
- Whether the singularity pulse should affect wall-running players.
