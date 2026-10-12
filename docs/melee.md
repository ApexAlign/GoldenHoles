# Melee: accuracy and action heighteners

All numbers are `Balance::demo()`; every knob is a `Balance` key (`Balance::demo().to_text()` lists them, `--balance FILE`
overrides them). Zero turns a heightener off; the default `Balance` has them all off so unit tests keep their timing.

## Controls (see also docs/DEMO.md)

| Input | Action |
|---|---|
| LMB | katana dash-slice (the lock triangle aims it); keep it down after a cut to charge the heavy cut |
| Q | shuriken (the poke); R / wheel switch the star type |
| RMB / Ctrl / middle or back mouse (right bumper on a pad) | hold: block; a timed raise is the parry |
| 1 / 2 / 4 | katana (spawn weapon) / bow / roll |

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

## Katana moves (`katana_moves.rs`, `combat/katana.rs`; `combat.katana.*` and `melee.*` keys)

One button, five moves. The server decides every one; none refunds a cooldown (the cooldown is client-predicted).
Order of choice: heavy (a charge released) > counter (parry window open) > whirl (sliding, or the shield came down within
0.2 s) > air (airborne, no rope or wall strike) > the plain cut.

| Move | How | Numbers (demo) | Keys |
|---|---|---|---|
| Heavy cut | press cuts as ever; keep holding, release after 0.5 s (full at 0.9 s) | up to 1.8x damage and +1.2 m blade, made in place (no lunge), 0.9 s recovery | `heavy_mult`, `heavy_reach`, `heavy_recover` |
| Air cut | cut while airborne | +25 % damage, taller blade box; the victim is driven into the floor and the landing is a shock-wave that throws bystanders back (ring-out credit to the slammer) | `air_mult`, `air_half_height`, `melee.slam_speed`, `shock_radius`, `shock_knock` |
| Counter cut | cut within 0.25 s of a parry | goes for the staggered attacker by itself (up to 7 m, no aim needed), hits for sure, damage x(1 + 0.5 x parried / base) | `counter_range`, `counter_scale`, `melee.counter_window_s` |
| Whirl cut | cut while sliding or just after lowering the shield | 360 degrees, 12 damage to everyone within 3 m, 0.6 s recovery | `whirl_radius`, `whirl_damage`, `whirl_recover` |
| Execute | any cut on a stunned (parried / splatted / guard-broken) player at or under 25 % hp | kills | `melee.exec_frac` |

A held button no longer auto-repeats: chain cuts by clicking. Zero on a damage/radius key turns that move off.

Client cues (`shinobi-client/src/katana_fx.rs`): a charge ring on the lock triangle (gold and labelled HEAVY when full),
an expanding shock ring on the air cut, a whirl arc, a COUNTER flash (our cut within the parry window), an EXECUTE flash
(a confirmed katana kill carrying the bonus flag). Screens: `docs/screens/katana_*.png`. The controls card reads
`KATANA (HOLD: HEAVY)`. Hint ids for the hint system: `katana.heavy|air|counter|whirl|execute` (`hint_ids`, `HudBridge::katana_hints`;
not wired to a display yet). Bots hold for heavy cuts (20 % of combo openers, at most one per 10 s), counter inside the window (50 %) and whirl
after dropping the shield with someone within 3 m (40 %).

Known gap: the client does not predict counter or post-shield whirl cuts (the client has no parry context); the server
result and the HUD flash still arrive with the hit confirm.

## Tests

`melee_duel.rs` (each heightener through the real `Server`), `katana_moves_duel.rs` (heavy, air + slam, counter, whirl, execute), `katana_fx.rs` unit tests, bot heavy-cut test in `shinobi-ai/tests/behaviour.rs`, `melee_feel.rs` (accuracy), `guard_duel.rs` (block / parry /
strike window), `full_loop_e2e.rs` (homing and block through `ClientGame <-> Server`).

## Skill combat (combo counter, wall chain, cancels)

All tunables are `combat.katana.*` (on in `Balance::demo`, off in the plain default). Everything lives in the sim (`combat/katana.rs`); the only
wire-visible effect is the damage number and the swing cooldown that already travel.

* **Combo counter**: every cut that connects raises it; a whiff, a hit taken (`CombatSystem::on_hurt`, called from `pvp::apply_hits`) or
  `combo_window` of idling drops it to 0. Each stack cuts `combo_speed` (3 %) off the recovery; once the combo carries past the finisher each further cut
  adds `combo_dmg` (5 %), up to `combo_max` stacks. The HUD shows `xN COMBO` from 2 (`HudState::combo`, read from the predicted `CombatSystem::combo_count`).
* **Wall chain**: up to `wall_chain_max` (3) connected wall-run slashes within `wall_chain_window` of each other, then the next wall-run cut is the
  wall-kick finisher (`wallkick_mult` damage, `wallkick_reach` m more blade). A non-wall cut or a whiff breaks the chain.
* **Hook / wall-jump into an air cut**: an air cut within `hook_air_window` s of the rope letting go (x`hook_air_mult`) or `walljump_air_window` s of a
  wall jump (x`walljump_air_mult`) hits harder (the larger applies). Needs `CharacterSystem::hook_age` / `walljump_age` (not on the wire).
* **Cancels** (`cancel` = 1): a dash, a hook or a wall jump started while the blade recovers frees it at once so the next cut can follow; it costs
  `cancel_cost` star charges (refused while the pool is locked or short; no pool = free).
