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
