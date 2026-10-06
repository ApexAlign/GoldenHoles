# Melee: accuracy and action heighteners

All numbers are `Balance::demo()`; every knob is a `Balance` key (`Balance::demo().to_text()` lists them, `--balance FILE`
overrides them). Zero turns a heightener off; the default `Balance` has them all off so unit tests keep their timing.

## Controls (see also docs/DEMO.md)

| Input | Action |
|---|---|
| LMB | katana dash-slice (the lock triangle aims it) |
| RMB | shuriken (the poke) |
| Ctrl / F / V / middle or back mouse (right bumper on a pad) | hold: block + parry, view locked |

## Accuracy

* **Homing (client + server, no new wire).** The lunge goes where the aim points, on both sides. Pressing slice with a lock
  turns the aim onto the target (range 9 m, 52 degree half-cone: `HOME_RANGE`, `HOME_CONE` in `client_game.rs`); during the
  0.42 s swing cadence the aim keeps turning toward a target within 5 m by up to 2.3 degrees per tick, and only while you
  are not turning the view yourself. `ClientGameCfg::melee_home` switches it off.
* **Target-centred cone (server).** A swing also hits any enemy whose centre is within `combat.katana.cone_deg` (38) of the
  aim and within reach + `cone_extra` (1.0 m) of the eye, line of sight permitting; the locked target is forgiven a wider
  angle. The lag-compensated re-test uses a cylinder as wide as the cone at full reach.
* **Blade box** (`blade_len` 2.4, `half_width` 1.2) and the finisher reach (+0.6) are unchanged.
* **Hit confirmation:** hit-stop, spark, sound and the damage number already existed; the number is now big and gold for a
  boosted cut (27 or more). A miss only costs the 0.42 s swing cooldown (no extra whiff lockout).

Measured by `crates/shinobi-sim/tests/melee_feel.rs` (a human with 6-tick-late, noisy aim, standing and cutting every 0.5 s
at a target that circle-strafes at run speed and holds its distance; 160 s per distance):

| distance | before (box only) | after (cone + homing) |
|---|---|---|
| 3 m | 96 % | 96 % |
| 6 m | 32 % | 65 % |
| 10 m | 0 % | 0 % (past any swing: blade + lunge + cone is about 7.5 m) |

Bots' katana hit rate (`katana_hit_rate`, bots deliberately whiff a quarter of their dashes, `slice_miss`): 68-75 %.
Only one swing in three carries the dash lunge (the dash has its own 1.2 s cooldown), so beyond the 2.4 m blade the other two
cut in place.

## Action heighteners (`crates/shinobi-sim/src/melee.rs`, `melee.*` keys)

| Heightener | Demo numbers | What it does |
|---|---|---|
| Momentum | +4 % per m/s of your speed, cap +60 % | a lunging cut deals about 32 instead of 20; a standing cut stays 20 |
| Knockback + juggle | 8 m/s launch, 4 m/s pop; airborne victims keep at least 5 m/s up | a cut throws the victim about 3 m; a victim already in the air is kept up for the next cut |
| Wall-splat | within 0.5 s of a launch, 15 damage + 0.6 s stun | a launched victim stopped dead in the air by a wall (measured displacement, so landing never counts) |
| Ring-out | 3 s | a fall into the void within 3 s of being hit is a kill (and a frag) for whoever hit them; the log reads "knocked by pN" |
| Kill streak | 8 s window, +8 % per stack (cap 4), +20 hp per katana kill | katana kills chain: harder cuts and a heal while the streak lives |

Splat and ring-out arrive as obstacle-source hazard hits (ids `u16::MAX - 1` and `u16::MAX`), so the existing wire carries them;
no protocol change. The strike window (block.rs) multiplies on top of all of these.

Not done: client-side slow-motion on parry / kill, clash sparks, a finisher flash, dash-cancel / cooldown refund on a hit
(the katana cooldown is predicted by the client, a server-side refund would desync the lunge decision).

## Tests

`melee_duel.rs` (each heightener through the real `Server`), `melee_feel.rs` (accuracy), `guard_duel.rs` (block / parry /
strike window), `full_loop_e2e.rs` (homing and block through `ClientGame <-> Server`).
