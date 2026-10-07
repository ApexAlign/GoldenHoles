# Movement feel (shinobi-sim `Cfg`)

All movement lives in `shinobi_sim::character` and is shared bit-for-bit by the server and the client predictor
(`Cfg::default()` on both sides; nothing is on the wire). Every number below is a `Cfg` field.

## Goals
- Slower on foot (about -17 % ground speed), so fights are readable.
- Jump is the traversal verb: a held jump floats, carries forward, and a double jump redirects. About two inputs
  (jump, jump) cross a gap that used to take a run-up of bunny hops; add a dash for the rest.
- Lower gravity so the descent is a glide, not a drop, which also gives grapple release and wall-run exits more air.

## Move-faster round (user: "sluggish")
Run 5.4 -> 7.2 m/s; ground accel 45 -> 72 (top speed in 0.10 s), decel 72 -> 100, air accel 18 -> 28; gravity 20 -> 24
(quicker apex), jump 10 -> 11.0 m/s, double jump 7.2 -> 8.4, `air_speed_cap` 9.5 -> 11.5, dash 15.5 -> 19, roll 7.5 -> 9,
slide boost 7.6 -> 9.5, wall-run 9.5 -> 11, terminal 32 -> 36. Anti-cheat `max_step` and bot hop planning read `Cfg`.
Measured (run-in jump): 10 m sprint 1.91 s -> 1.43 s; tap 1.92 m / 0.85 s / 6.5 m -> 1.92 m / 0.78 s / 7.9 m;
held 3.25 m / 1.23 s / 9.0 m -> 3.27 m / 1.13 s / 11.0 m; held + double 4.91 m / 2.03 s / 15.9 m -> 5.16 m / 1.88 s / 19.3 m.
The table below is the older round (5.4 m/s era).

## Before / after (run-in jump, stick held forward; pinned by `feel_tests` in `character.rs`)

| | before | after |
|---|---|---|
| run speed | 6.5 m/s | 5.4 m/s (-17 %) |
| dash / roll / slide boost / wall-run | 18 / 9 / 9 / 8 m/s | 15.5 / 7.5 / 7.6 / 7.0 m/s |
| gravity | 28 m/s^2 | 20 (x0.8 rising with jump held, x1.5 rising released, x0.6 falling with jump held) |
| terminal fall speed | 40 m/s | 32 m/s |
| tap jump: apex / air time / reach | 1.53 m / 0.67 s / 4.4 m | 1.22 m / 0.63 s / 5.0 m |
| held jump: apex / air time / reach | 1.53 m / 0.67 s / 4.4 m (no hold effect) | 2.35 m / 1.02 s / 7.6 m |
| held jump + double jump | 2.75 m / 1.08 s / 7.2 m | 3.91 m / 1.78 s / 14.4 m |
| grapple release (horizontal momentum) | braked at `air_accel` 22 m/s^2 to run speed | kept: 19.2 m/s at release, 18.4 m/s 0.4 s later |

## How the jump works now
- **Variable height.** Rising with jump held: gravity x`rise_hold_mult`. Rising with jump released: x`rise_cut_mult`
  (a tap is a short hop). Falling with jump held: x`fall_glide_mult` (glide); release to fall at full weight.
  The rope ignores these (it uses plain gravity).
- **Forward carry.** A ground jump adds `jump_carry` m/s along the current heading (capped at `air_speed_cap`;
  a faster entry such as a slide keeps its speed).
- **Momentum.** In the air, speed above a run, or any speed with no stick, decays only at `air_drag` m/s^2.
  The stick steers sideways and can brake, but never adds forward speed past the carry. Under run speed with the stick
  held it accelerates as before.
- **Double jump** (at the apex, `peak_window`): momentum turns to the stick and gets `double_jump_carry` on top
  (at least run speed).
- **Wall-run exit** keeps `wall_jump_keep` (1.0) of the wall-run speed along the wall, plus `wall_jump_push` away.
- **Coyote / buffer:** 0.14 s / 0.15 s.

## Knobs (all in `Cfg`, `character.rs`)
`run_speed`, `gravity`, `terminal`, `jump_speed`, `double_jump_speed`, `rise_hold_mult`, `rise_cut_mult`,
`fall_glide_mult`, `jump_carry`, `double_jump_carry`, `air_speed_cap`, `air_drag`, `wall_jump_keep`, `wall_jump_push`,
`wall_speed`, `wall_max_time`, `dash_speed`, `slide_boost`, `grapple_speed`, `grapple_max_speed`.
Floatier: lower `fall_glide_mult` (or `gravity`). Longer jumps: raise `jump_carry` / lower `air_drag`.
Faster on foot: raise `run_speed` (and `ground_accel`/`ground_decel` with it).

## Bots
`shinobi-ai` hop planning reads `Cfg::max_hop_rise` / `jump_apex` / `double_jump_gain` (perch heights, ledge-hop range),
and bots hold jump during a hop, so a new `Cfg` retunes them with no edits.
