# Traversal x melee: grapple, hook-strike, wall-run, wall clash

Everything lives in `shinobi-sim` (server-authoritative, bit-identical client prediction). Every number is a `Cfg` field
(`character.rs`) or a `combat.katana.*` Balance key. Tests that print the numbers below:
`cargo test -p shinobi-sim --test traversal --test wall_clash -- --nocapture`, `cargo test --release -p shinobi-ai --test hook_strike -- --nocapture`.

## The loop

hook something far away -> reel in fast -> **press the katana while on the rope** (hook-strike) -> land on a face and
run it, up or down -> **press the katana on the wall** (wall slash) -> hook the next building. Two players meeting on one
face fight a **wall clash**.

## Grapple (E)

| | before | now | key |
|---|---|---|---|
| range | 40 m | **400 m** | `grapple_range` |
| reel speed | 18 m/s flat | 18 m/s up close, **0.6 m/s per metre of rope up to 60 m/s** | `grapple_speed_per_m`, `grapple_far_speed` |
| 100 m of reel | 5.6 s | **1.8 s** | |
| speed ceiling on the rope | 20 m/s | `max(grapple_max_speed, reel + 2)` = 62 m/s | |
| arrival | fixed soft brake | brake and soft-arrival scale with the reel so a 60 m/s reel still slows before the wall | `grapple_soft_dist`, `grapple_brake` |

- **Players are hookable.** The hook takes the nearest living player's hitbox along the aim (if it is closer than the
  geometry). The anchor is their centre and follows them every tick (`player_near`, 2.5 m re-anchor radius). Geometry
  hooks behave exactly as before.
- A reel that drags a grounded player into a ledge pops them up (`grapple_hop`, 4.5 m/s) so the rope carries them over.
- After the release, speed above `grapple_max_speed` bleeds off (`overspeed_bleed`, 0.8 / s): a long hook is a fast
  flight that settles, not a permanent rocket.
- Anti-cheat: the per-tick displacement ceiling grows with the (server-side) velocity, never past the reel ceiling.

## Hook-strike (katana while hooked, or within 0.35 s after the rope lets go)

The katana swing becomes a **launch along your flat aim** instead of the plain dash lunge: horizontal speed is kept above
`hookstrike_speed` (24 m/s), the rope is cut, the double jump comes back. The blade reaches further and hits harder:

| | plain slash | hook-strike | key |
|---|---|---|---|
| reach | blade 2.0 m + lunge 3.3 m | blade + `hook_reach` 4 m + 0.15 m per m/s of speed | `combat.katana.hook_reach` |
| damage | x1 | **x1.5** | `combat.katana.hook_damage_mult` |
| window | | `hookstrike_window` 0.35 s after release | `Cfg` |

Measured (`wall_clash.rs`, katana 20): at 7 m a plain slash misses, a hook-strike hits for 30.

## Wall-run (hold Space or the cling key next to / into a face)

| | before | now | key |
|---|---|---|---|
| run speed | 7.0 m/s | **9.5 m/s** | `wall_speed` |
| max time | 2.2 s | **4.0 s** | `wall_max_time` |
| wall jump push / keep | 7.5 / 1.0 | 8.5 / 1.15, plus half your climb added to the hop | `wall_jump_push`, `wall_jump_keep` |
| corner / gap | dropped off | **coasts** along the old tangent while burning run time `wall_coast_burn` x faster | `wall_coast_burn` |

**Vertical faces.** A wall-run is now a 2D velocity in the wall's plane (sideways `a`, up/down `b`; both are in `vel`,
nothing new on the wire):
- Run **at** a face (head-on, closing speed >= `wall_front_min_speed` 2.5 m/s, three coplanar rays) and hold jump: you
  catch it and climb (`wall_up_enter` 6 m/s start, `wall_climb_speed` 8 m/s).
- The stick steers inside the plane: along the wall runs sideways, **into** the wall climbs, **away** runs down
  (`wall_steer` 45 m/s^2 turn rate). No stick = keep the current run (sticky, never slides off by itself).
- Climbing past the top edge pushes over it (`wall_topout_push`).
- Measured: a head-on run-up reaches 37.8 m on a 60 m face in 4 s; holding away descends at 8 m/s.

**Wall slash** (katana while wall-running): launches off the face along the aim (never into the wall), speed kept above
`wallslash_speed` 13 m/s with `wallslash_lift` 4.5, the wall-run ends, double jump returns. Reach `combat.katana.wall_reach`
3 m (+0.15 m per m/s), damage x`wall_damage_mult` 1.35.

## Wall clash (server only)

Two living, non-allied players wall-running the **same face** (normals within ~25 deg) within `clash_range` (2.2 m) open a
`clash_window` (0.6 s). The first katana hit inside it wins: damage x`clash_mult` (1.5) **on top of** the wall-slash bonus,
and the loser is thrown off the wall (`clash_knock` 11 m/s). If nobody lands a hit the window closes and both are knocked
off (70 % speed), no damage. The same pair cannot clash again for `clash_cd` (2.5 s). The knock reaches the client as an
ordinary reconciliation, so prediction is unaffected. Measured (katana 20): winner's slash 20 x 1.35 x 1.5 = **40.5**.
Strike-timing damage driven by the lock-on is owned by the melee-balance work; this file reads damage from `Balance`.

## Bots

Normal/hard bots hook a visible target at 16-90 m now and then (`HOOK_CHANCE` per tick, 7 s cooldown), and swing the moment
the target is inside the hook / wall reach (`select_phase`). 8 bots x 120 s: ffa_small 27 hooks / 17 hook-strikes; ffa 31 / 21
plus wall slashes. The stuck-bot tests stay green on ffa, ffa_xl and ffa_small.

## HUD and rendering

- While a katana press would be a strike, a pulsing gold ring and `HOOK STRIKE` / `WALL SLASH` sit on the crosshair
  (`RenderView.fx.strike_armed`).
- Long ropes space their light pulses over the length (about 18 packets however long); width already follows eye distance.
- Review frames (offscreen): `docs/screens/traversal_long_hook_strike_ring.png`, `docs/screens/traversal_wall_slash_cue.png`
  (`SHOT_FX=longrope`, `SHOT_FX=wallslash`).

## Tuning cheatsheet

- Hook too fast / slow: `grapple_far_speed`, `grapple_speed_per_m`.
- Strikes too strong: `combat.katana.hook_damage_mult`, `wall_damage_mult`, `hook_reach`, `wall_reach` (Balance text keys).
- Wall-run too sticky / long: `wall_max_time`, `wall_coast_burn`, `wall_steer`.
- Climbing too easy: `wall_climb_speed`, `wall_front_min_speed`.
- Clash too punishing: `clash_mult`, `clash_knock`, `clash_window`.
