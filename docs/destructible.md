# Destructible maps (protocol v20)

Level boxes with hit points (`dbox` lines) wear down under weapons, **break** (the collider disappears on the server and on
every client's predicted world), and **re-form** 40 s later. Decks, ramps, stairs, the spires and anything that carries a
pickup or a pedestal are plain `box` lines and never break (bots, the walkability tests and the spawn logic depend on them).

## What breaks what

| source | how | notes |
| --- | --- | --- |
| thrown star (shuriken, variants) | `CombatEvent::PieceHit` on every bounce, `combat.shuriken.piece_damage` | chips; a 50 hp bamboo stalk takes a handful |
| katana | `PieceHit`, `combat.katana.piece_damage` (60) | one swing fells bamboo, three a cover wall |
| fuda tag blast | `LevelBreaks::blast`: tag player-damage at that distance x `BreakCfg::blast_mult` (5) | measured to the box's nearest point, radius = tag radius, shakes boxes behind cover too |
| support loss | a free-standing box resting only on destructible boxes falls `COLLAPSE_TICKS` (18) after the last one breaks | cut a tower at the foot and the rest comes down in a chain |

Arrows (yumi) and the mine blast do not damage boxes yet (follow-up).

## Level file

```
dbox  cx cy cz  hx hy hz  rot_z hp      # like `box`, rot_z always given (may be 0), hp > 0
```

`box` index = order among all box/dbox lines (the wire id of a destructible box is its box index, a `u16`).
`scripts/gen_isles_level.py` / `gen_sky_level.py` mark bamboo, cover walls, towers (stacked 6 to 12 m segments), canyon walls,
ring segments, rocks, shard columns, torii posts, lantern pylons and pagoda daises. A post-pass keeps every box in a column under a
pedestal permanent.

## Sim (crates/shinobi-sim/src/breaks.rs)

`Server::breaks: LevelBreaks` (built by `LevelDesc::apply_to_server`), `Server::break_events: Vec<BreakEvent>` (cleared each
frame): `Break { idx, cascade }`, `Rez { idx }`, `Damaged { idx, hp_q }`. Destructible colliders carry the block piece tag with the
reserved cell `[i32::MIN, idx, 0]` (`breaks::world_cell/world_idx`), so existing weapon code reports hits unchanged.
Re-form waits while a player stands within 1.6 m of the box or one of its supports is still missing.

## Wire and client

`NetEvent::WorldBreak { idx, cascade }` (27), `WorldRez { idx }` (28), `WorldCrack { idx, hp_q }` (29), reliable; a joining peer is
sent every broken box and every cracked box (`Replicator::add_peer`). `NetClient` keeps:

* `world_broken() -> &BTreeSet<u16>`: broken right now (the renderer hides these; the prediction world removes them each tick),
* `world_hp_q(idx) -> u8`: 255 untouched .. 1 nearly gone (cracks),
* `take_world_fx() -> Vec<WorldFx>`: `Break { idx, cascade } | Rez { idx } | Crack { idx, hp_q }` since the last call (cue for debris,
  dust, sound; a catch-up break on join arrives as `Break { cascade: false }` too: the renderer should hide it quietly when it is
  older than the first frame, i.e. do not spawn debris for breaks drained on the join frame).

`LevelDesc::boxes[i].hp > 0.0` tells the renderer which level boxes are destructible; its entity index = `i`.

## Renderer notes

* Landmark dressing is matched by exact half extents (`TORII_POST_HALF`, `PYLON_HALF`, pagoda daises on tower tops): hide the torii
  beam when either post is broken, the lantern cap with its pylon, the pagoda canopy with its tower top.
* Boxes cascade (`cascade: true`) a few ticks apart: stagger debris so a falling tower reads as a collapse.
* Do not feed broken boxes to anything that reads `LevelDesc` once (void grid, minimap, nav) as if they were gone for good.
