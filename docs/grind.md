# Grind (hold C)

Hold **C** on the ground with speed in hand, or land from a jump or fall with it held, and the ground does not stop
you: you GRIND. The horizontal speed is kept and topped up, so the momentum a normal landing would eat carries on along
the ground, a block top, a ramp, a deck. **C is the grind now; the crouch slide and the roll are gone.**

Sim: `CharacterSystem::grind` (`shinobi-sim/src/character.rs`), shared bit-for-bit by server and client predictor.
The grind has its own held button (`PlayerIntent.grind`, C) so it does not collide with jump / double jump: Space
stays purely the jump. On the wire it is one held intent bit, plus one flag bit and a 104-bit block in `OwnState`
(protocol v29).

## Rules (`Cfg::grind_*`)
- **Enter:** hold C while grounded (or land with it held), flat speed >= `grind_min_speed` (6 m/s), no rope, dash or
  slide. On a landing, speed becomes `speed + grind_fall_gain x fall speed + grind_boost`; on the ground, `speed +
  grind_boost`. Either way capped at `grind_max_speed` (15), and a speed already above the cap is kept, never cut.
- **While grinding:** `grind_friction` (2.5 m/s^2) bleeds speed; the stick steers the heading at `grind_turn` (3 rad/s),
  no stick keeps straight on. No slide while grinding.
- **Grind cuts the rope:** pressing C while on the grapple lets go of the rope at once (momentum kept, no hook press
  needed), so you land straight into a grind. Swinging into the floor with C already held drops the rope on touchdown
  too (if you are fast enough to grind). C held from before the hook never cuts a rope mid-air: only a fresh press does.
  The press edge replays exactly in client prediction (`CharacterSystem::grind_prev`, seeded from the acked intent).
- **Ends:** C released, speed under `grind_end_speed` (4.5), a roll, a rope, a hit that knocks / hooks / yanks you, a
  wall in front (see below), or leaving the ground (the speed stays in the air). No timer: friction takes the speed.
  Keep C held while running and it re-engages once you are back above `grind_min_speed`.
- **Jump out:** a jump keeps the whole grind speed (Space is free to jump out, no double-jump conflict). Land again still
  holding C and you grind again, so hop-grind-hop chains climb back up to the cap. That chain is the flow.
- **Walls:** steering into a face (W) while grinding hands over to the wall-run (climb / run along it), the same as
  holding W in the air; SPACE still enters a wall-run too, but it is no longer needed to keep a grind.
- **Dash / katana lunge** while grinding keeps the grind: the dash never drops below the grind speed.

## Melee
Any cut made while grinding does `combat.katana.grind_mult` x damage (1.4 in the demo balance, 1.0 = off in the base
balance). The whirl is flat damage and ignores it.

## Feel / FX
- Speed lines while grinding (`vm::streak_target`).
- Sound: `Sound::GrindStart` when it starts, `Sound::GrindScrape` grains every 0.11 s while it lasts (pitch follows speed).
- Footsteps are off while grinding.
- Other players do not see a grind state (the snapshot's move kind is unchanged); it is owner-only prediction + server rules.

## Tuning
Longer / faster: raise `grind_max_speed`, lower `grind_friction`. Less hop-chain snowball: lower
`grind_fall_gain` / `grind_boost`. Harder cut: `combat.katana.grind_mult`.
