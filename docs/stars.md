# Star variants

Right click throws the **selected** throwable; **mouse wheel** or **R** cycles it (Q is the roll). Holding right click is still the MARK star.
All four draw on the same star energy pool (6 charges, lockout when it cannot afford a plain star). Numbers live in Balance `combat.star.*`
(`combat.shuriken.energy_cost` is the plain star's 1). The HUD selector under the pips shows the type, its cost, rings on the pips a throw
spends, and a one-line blurb after a switch; a hint appears on the first plain throws (3 times, stops once you cycle); the controls strip has a row.

Protocol v22 changed the meaning of star kinds 1 and 3 (wire layout unchanged, still 2 bits): 1 was MINE, now BARREL; 3 was SMOKE, now CANISTER.

| Type (wire) | Cost | Behaviour |
|---|---|---|
| STAR (0) | 1 | the existing ricocheting neon star (aim assist, homing) |
| BARREL (1) | 3 | a thrown explosive barrel: heavy lob (22 m/s, gravity 22), bounces off the floor and walls (`barrel_bounce`, `barrel_grip`) until it comes to rest, then burns a 1.5 s fuse. It goes off at once when it strikes a player, and EARLY when anything touches it: a katana cut, a flying star (within `barrel_hit_radius` 0.9 m), an arrow, a fuda tag blast or another explosion, in the air or at rest. Blast: 6 m radius, 90 damage at the centre falling off linearly to 15 % at the edge, radial knockback 16 m/s + 9 m/s launch (same falloff), wall-splat window like a tag. Needs line of sight (walls shield). Shakes destructible boxes (`blast_mult`) and sets off neighbouring barrels / canisters inside the radius (chain reactions). |
| FLASH (2) | 2 | bursts on impact or after 0.5 s. Everyone within 14 m whose view is within 100 degrees of the burst is whited out (no line of sight needed) for up to 3.5 s (scales with facing and distance, fades out). The white-out also DAZES: no lock-on, a slower aim turn (client side: `flash_daze_turn` 0.45 of normal) and +25 % damage taken from every weapon (`flash_daze_damage`) - the "expose them for the katana" window. The thrower is only blinded when looking at the burst within 6 m. Allies are never blinded. No damage of its own. |
| CANISTER (3) | 2 | small and fast (55 m/s, flat), STICKS to the first surface or player it hits and rides along with that player, 1.2 s fuse. Blast: 4 m radius, 60 damage at the centre (15 % at the edge), knockback 22 m/s + 6 m/s launch. Can be shot / cut / chained like a barrel. |

Blast rules (barrel and canister, `star_pass.rs`):
* The thrower takes the same blast (no immunity: "too close hurts yourself"). A raised shield facing the blast takes it as a blocked hit
  (`guard.front_mult` of the damage, guard drained like any blocked hit, knockback cut to a third). Self kills are suicide-style (no kill credit).
* Teammates of whoever set it off are spared (except that player themselves).
* Credit goes to whoever set it off: the thrower when the fuse runs out or the barrel hits somebody, the shooter / cutter / tag owner otherwise
  (so a thrower can be baited into their own barrel, and the bait scores). A chain keeps the original attacker.
* Damage kind `DamageKind::Explosive` (wire hit kind 5, `KIND_EXPLOSIVE`).

## Netcode
* Intent: `PlayerIntent.star` (2 bits, only on the wire while FIRE is set), carried with a lost packet's FIRE edge.
* Throw, flight (barrel arc, bounces, rest) and energy are predicted by the client with the same code (`combat/shuriken.rs`); tested for parity in
  `tests/star_parity.rs`. A predicted landed barrel / canister is dropped after 0.5 s; the authoritative copy comes in the snapshot.
* Snapshot: a flying barrel / canister is an ordinary projectile entity (`flags` bits 2..4 = kind, `dir` = its velocity direction). A resting barrel or
  stuck canister is a world object with a stable id from `STUCK_ID_BASE`: `flags` = kind | `DISC_STUCK` | `DISC_ARMED` (the fuse is burning), `vel` =
  surface normal * `STUCK_NORMAL_SPEED`, `fx` = fuse left in 1/15ths, `yaw` = owner id. A canister stuck to a player moves with them.
* Server-only effects (`star_pass.rs`): blasts and white-outs. Reported as `NetEvent::StarFx` (`what` = `STAR_FX_BARREL_LANDED` 0, `CANISTER_STUCK` 1,
  `BARREL_BURST` 2, `FLASH_BURST` 3, `BARREL_GONE` 4, `CANISTER_GONE` 5, `CANISTER_BURST` 6; burst radius is `combat.star.barrel_radius` /
  `canister_radius`) and `NetEvent::StarBlind` (victim, strength, ticks: the daze lasts exactly that long). Blast damage arrives as normal hit markers
  (kind 5) and kill-feed entries.
* Client constants (cost, daze turn multiplier) are `StarCfg::default()` (like the MARK cost): a server with different `combat.star.*` values makes the
  HUD cost and the daze turn rate slightly off until the next energy report. The slower aim while dazed is applied by the client only.
* Barrels and canisters vanish when their owner dies (`reset`).

## Bots
Now and then (per-kind gaps): a flash at a target looking at them 7-16 m away, a lobbed barrel at a target 9-20 m away (outside its own blast;
the bot raises its aim by the low-arc angle first). Only with energy to spare. Bots no longer use smoke.

## Sounds (existing synth sounds, `audio.rs`)
Barrel / canister landed Print, burst AirSlam, flash CounterFlash, own white-out LockBreak, cycling UiClick.

## Frames
`docs/screens/star_world_*.png`, `hud_cpu_star_sel_*.png` (see the README there; the mine / smoke frames predate the rework).
