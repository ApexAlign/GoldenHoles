# Wrist grapple (zipline)

A character ability, not a weapon: it lives in `CharacterSystem` and never touches the Loadout, so shurikens and
the katana stay usable while swinging or being reeled in.

## Input
`PlayerIntent.zip` (existing wire bit, one-tick edge, cleared on lost-packet repeat). `to_player_input` turns
it into `PlayerInput.grapple_aim: Option<Vector>` (view direction from quantised yaw/pitch, libm trig), so the
client predicts from exactly what the server simulates. No new wire bits.

## Rules (all in `character.rs`, deterministic, f32 + libm only)
- Fire: edge while detached, off cooldown. Ray from the eye (`centre + eye_above_center`) along the aim, max
  `grapple_range`. Only static geometry is hookable (collider has no parent body or a fixed one); sensors,
  players and enemies are ignored. Hit closer than `grapple_min_len` = no attach.
- Attached: rope length starts at the hit distance and only ever shortens. Each tick
  1. reel: velocity along the anchor direction is accelerated by `grapple_accel` up to `grapple_speed`
     (existing higher speed is kept, momentum is never clipped);
  2. taut rope: when distance >= length, outward radial velocity is removed (pendulum swing under gravity);
  3. rope length follows `min(length, distance)`.
- Release (rope vanishes, velocity kept): zip edge again, jump (consumes the jump buffer and adds a
  `grapple_jump_boost` hop; in a wall-run the jump stays a wall jump), within `grapple_arrive` of the anchor,
  `grapple_max_time`, or line of sight to the anchor lost. Release starts `grapple_cd`.
- Feel fixes found by test: while attached the stick never brakes the swing (`grapple_steer` of air accel, additive
  only), a reeling rope cancels gravity so shallow shots still lift you, and a grounded hook kicks `grapple_lift` up
  so the reel does not drag along the floor.
- Composition: wall-run keeps its own steering, the reel adds on top. Dash/roll override velocity as before and
  leave the rope slack for their duration (rope is not dropped). Shooting is unaffected.

## State (prediction/reconcile)
`CharacterSnapshot.grapple: Option<GrappleState{anchor, len, time}>` and `grapple_cd`. `OwnState` carries it
bit-exact: 1 flag bit, then (when attached) anchor 3x32, len 32, elapsed ticks 8; `grapple_cd` is timer slot 7.
Base own block grows by 9 bits (one flag bit + one timer byte, see own_state.rs docs).

## Events / visuals
`CharacterSystem.grapple_event` is a one-tick edge (`Attached{origin, anchor}` / `Released`). The tick loop
turns it into `CombatEvent::GrappleAttach{origin, anchor}` / `GrappleRelease{pos}` for Graphics (rope visual).
Own rope: the client reads `CharacterSystem.grapple` directly each frame (anchor + eye). Remote players' ropes
need the anchor on the wire: Network to add (snapshot flag `FLAG_GRAPPLE` + a reliable `NetEvent`), not done here.

## Client
`Action::Grapple` (key E) latches the `zip` edge in `ClientGame`; `RenderView.rope` = (eye, anchor) while attached,
from the prediction, for the rope visual.

## Aim accuracy (same change set)
The crosshair ray is `RenderView::eye` + `RenderView::look_angles()` (= the aim sent in intents). Fixed leaks:
FX look-kick (up to 0.8 deg) was added to the rendered view only; slide eye drop (0.35 m) lowered the camera but
not the shot origin (now `Cfg::slide_eye_drop` / `CharacterSystem::eye_above`, shared, and unscaled by feel).
Residual by design: frame drawn `alpha` into a tick shows the eye up to one tick of travel ahead of the shot origin
(<= 0.15 m at run speed); thrust/landing eye dips (cosmetic, <~0.1 m, transient). Tests: `tests/aim_accuracy.rs`.

## Upgrades (Cfg keys, defaults on; no wire change; tests `shinobi-sim/tests/grapple_upgrades.rs`)
- **Anchor assist** (`grapple_assist_deg` 6, `grapple_assist_edge` 2.5; 0 = off): when the aim ray and the player hitboxes find
  nothing, `CharacterSystem::grapple_assist` casts 3 rings x 8 rays in the cone and takes the lowest score = angle off the aim
  minus bonuses (a rim within ~0.6 m of the face border: `edge`; a top face: `edge/2`). Same function in `grapple_fire` on server
  and client prediction (libm, fixed ray order). `grapple_probe` (crosshair "hookable") is unchanged = aim ray only;
  `grapple_target` / `grapple_assist` give the snapped point for a highlight.
- **Enemy hook** (`grapple_pull_max` 45 m/s, 0 = the old "reel to the player" behaviour; `grapple_pull_end` 2 m, `grapple_stagger`
  0.4 s, `grapple_player_cd` 3 s, `grapple_shield_drain` 0.4): a hook that hits a player latches (the rope anchor follows them, as
  before) but does NOT reel the hooker (`pulling_enemy()`, same on client and server). Server only (`resolve_hook_pulls`): every
  tick the non-allied player at the anchor is dragged toward the hooker at `grapple_reel_speed(gap)` capped by `grapple_pull_max`
  (never past the release radius) and kept staggered 0.4 s (no dash/act; outlasts the pull by 0.4 s). The hooker's rope releases
  within `grapple_pull_end` m, cooldown `grapple_player_cd`. A raised shield facing the hooker stops the pull, drains
  `grapple_shield_drain` of the guard (breaks it at 0) and drops the rope. Hook key again still yanks (old yank, `yank_cd`).
  Damage 0. The victim moves by ordinary reconciliation. The rope kind (`rope_player`) is not on the wire: set at the fire,
  re-derived after a reconcile restore.
- **Momentum**: `grapple_cd` is 0.4 s after any clean release (miss cooldown unchanged, 0.15 s) so hooks chain. A launching
  release (jump, dash / jump held, anchor lost) adds `grapple_sling` (25 %) of the speed along the velocity, capped at
  `grapple_sling_cap` 8 m/s, only above 6 m/s; jump-while-hooked still adds `grapple_jump_boost`.
- **Destruction**: a geometry anchor that vanishes (box broke, evicted) releases with the slingshot momentum; the rope is gone, so
  a box that re-forms later cannot catch it (anchors are never re-acquired). Only a player rope re-anchors onto a player.
- **Range**: `grapple_range` is already 1000 m (docs/traversal_melee.md; DEMO.md's "40 m" is stale); `grapple_speed_per_m`
  unchanged, so far hooks stay a ~2 s trip.

## Hardening (demo pass)
- Rope speed is capped at `grapple_max_speed` (28 m/s, under a dash), the pull eases in over 0.25 s, and the closing
  speed is limited inside the release radius (`grapple_arrive_speed` 14 m/s widening to the reel speed over 6 m), so
  hooks yank rather than slam. The stick on the rope steers but cannot add speed above a run.
- The anchor must still exist every tick (a block removed / evicted mid-swing releases cleanly); non-finite anchors
  release. Moving geometry (kinematic bodies), players and sensors are never hookable.
- Fired at nothing hookable: `GrappleEvent::Miss` -> `CombatEvent::GrappleMiss{origin,dir}` + `LocalFx.grapple_missed`;
  a miss starts `grapple_miss_cd` (0.15 s) so E-spam is rate limited.
- Death, respawn and anti-cheat snap-back clear the rope (`CharacterSystem::clear_grapple`).
- Hooking takes over wall-run and slide (both overwrite velocity every tick); a dash/roll in progress keeps going and
  leaves the rope slack.

## For HUD / Graphics (read from `RenderView.fx`, a `LocalFx`)
`grappling`, `grapple_ready` (off cooldown and not on the rope), `grapple_cooldown` (1.0 just used -> 0.0 ready),
`grapple_hookable` (client-side ray in the predicted world: tint the crosshair when true), edges since the last view:
`grapple_attached: Option<[f32;3]>` (anchor), `grapple_released`, `grapple_missed`. `RenderView.rope` = (eye, anchor).
Rules for "hookable" are `CharacterSystem::grapple_probe` (static geometry only, 2.5..40 m).

## Swing / orbit (DESIGN.md: pull AND swing)
Hold slide (C) while on the rope: the reel stops and the rope keeps its length (it can only shorten): a pendulum / orbit
under gravity, steered by the stick (adds sideways speed, never brakes; capped at 28 m/s). The taut-rope constraint kills
outward velocity and pulls integration drift back over a few ticks (rope never visibly stretches, no jitter). Tests: a
pendulum from rest never gains energy (rises <= 0.4 m past its start), rope length drifts < 0.35 m, velocity direction
changes < 0.25 rad per tick; a conical-pendulum start (v^2 = g r^2 / h) keeps circling for 9 s, radius within 3 m,
speed within 6 m/s. Max rope time is 10 s. No fall damage: the movement core has no health, landing only zeroes
velocity (test: a 400 m fall ends standing, finite); the only death from height is the kill plane (-20 m).
