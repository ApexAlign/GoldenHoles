# Movement netcode contract (shinobi-sim)

One 60 Hz fixed step, run identically by the server and by the client's predictor. The server is authoritative; the
client predicts its own character and is corrected only when the two really diverge.

## Tick order (server, `Server::step`)

1. Refresh the full query pipeline (`world.queries`), run match/respawn/pickup passes, refresh again, and rebuild the
   **solid-only** movement pipeline once (`movement::refresh_solid`).
2. For each player: take the intent for this tick (`IntentQueue::take_for_tick`; a missing intent repeats the previous
   one with its edge bits stripped), clamp aim slew, run combat, then `movement::step_player_with` (movement query tree
   passed in).
3. Anti-cheat displacement check; snapshot / own-state build for tick `T` happens after the step of tick `T`.

`now` (sim time) is currently unused by movement. Nothing in movement reads wall-clock time.

## `movement::step_player` / `step_player_with`

`CharacterSystem::update` -> commit the kinematic target (`set_position(next_position)`) -> `sync_body_colliders`
(parented capsules do not follow their body without a pipeline step, so every `set_position` on a player body must be
followed by that call; `restore` and the anti-cheat SnapBack do it already).

`step_player` is the lone-player form (client predictor): it builds its own solid tree. `step_player_with` is the
server form: the caller refreshed the tree for the tick.

**Movement queries use a solid-only broad phase (`SolidAabb`), never `world.queries`.** The KCC breaks ties between
simultaneous contacts (floor + wall) in tree-layout order; sensors (hit boxes, pickups) exist only
on the server, and one of them at the player's feet moved a wall contact by 4e-5 m, which jump/landing thresholds
amplified to metres. Do not add movement queries against `world.queries`.

Level geometry must come from one `LevelDesc` (`level.rs`) so both sides build identical colliders in identical order.
Anything solid the server adds that the client does not know about is a guaranteed divergence.

## `OwnState` (owner-only, server -> client)

Bit-exact copy of everything `update` reads besides the world and the intent: raw f32 `pos`/`vel`/direction vectors,
timers as whole ticks, flags. Layout and sizes are at the top of `own_state.rs` (39 B grounded/airborne, 56 B in a
wall-run, 78 B theoretical maximum; the frame's length prefix is a u8).

Why exact: restoring it and replaying the buffered intents reproduces the server's continued run bit-for-bit
(`restore_then_replay_is_bit_identical`, `parity.rs`). A quantised blob does not: contact response amplifies
millimetre errors into centimetres.

Combat: only the katana cooldown is in it (`slice_cd`, mirrored into `CharacterSystem` after each combat tick),
because that, plus `dash_ready`, decides whether a slice lunges. `combat::katana::predict_slice` reproduces that
decision for the client's replays (pinned to the real `Katana::fire` by a test); the live tick uses the real
combat code. Not in it: projectiles, cosmetics. Known divergence: `Katana::fire` cuts in place when a
target is already inside blade reach, which needs enemy positions the client lacks; that case is corrected.

## Ack semantics

`acked_tick` = the **intent tick the server had just executed** when the blob was built; `own` is the state AFTER that
tick. The client keeps `(intent, position after)` for each predicted tick (`Reconciler`). On an ack:

- `|own.pos - predicted pos at acked_tick| <= 2 cm` (deadband): nothing happens.
- Otherwise: restore the character to `own`, re-run every buffered intent after `acked_tick` through the same
  `step_player`, re-record them. The drawn position is `simulated + visual_offset`; the offset is set to
  `drawn_before - new_simulated` (so the screen does not move) and decays at 10 /s. Errors above 3 m (teleports:
  respawn, match-start reset) snap the offset to zero instead.
- Acks for older ticks than the history front, or for ticks never predicted, are ignored. Lost acks are harmless; the
  next one pops everything before it.

## `lead_ticks` (client)

Each intent is stamped with the tick it should execute on: server-clock estimate + `lead_ticks`. The estimate lags the
real server tick by one one-way trip (it is anchored on snapshot arrival), and the intent needs another trip to get
there, so the margin at the server is `lead - 2 * one_way - jitter` ticks. It must be positive or the server repeats
the previous intent for that tick (a divergence the reconciler then has to fix), and `lead` must stay below
`MAX_FUTURE_TICKS` (12) or the server drops the packet as a time-skip. Default 8 covers 80 ms RTT with +-15 ms jitter
(`tests/e2e_prediction.rs`: 0 corrections over 20 s at 5% loss each way).

## Guard rails (keep green)

- `shinobi-client/tests/parity.rs`: server and predictor bit-identical over 900 scripted ticks; restore from the wire
  blob bit-exact for 60 ticks; a dropped dash packet is detected and replayed away.
- `shinobi-client/tests/e2e_prediction.rs`: real `Server` + `ClientGame` through lossy/laggy in-memory links; predicted
  vs server error, zero visible snaps, no anti-cheat strikes; match-start teleport is followed.
- `shinobi-sim/tests/perf_step.rs` (`--ignored`, release): 16 players, 1400 solid boxes, ~0.6 ms per step.

## Known limits

- Bit-exactness holds on one CPU/arch. Cross-arch (ARM client vs x86 server) can drift in the last bits (FMA, libm);
  exact-restore self-heals it, so it only costs correction frequency.
- Other players' capsules are as of the start of the tick for movement culling on the server; the client has none.
