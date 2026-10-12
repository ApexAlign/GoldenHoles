# Stars: the standard star and the flash star

Stars are a **close-range tool** (the bow is the long gun). Right click throws the **selected** star; **mouse wheel** or **R** switches
between the two (Q is the roll). Holding right click is still the MARK star. Both draw on the same star energy pool (6 charges, lockout
when it cannot afford a plain star). Numbers live in Balance `combat.shuriken.*` and `combat.star.*`. The HUD selector under the pips
shows the type, its cost, rings on the pips a throw spends, and a one-line blurb after a switch.

Protocol v23 removed the barrel and the canister (they were the explosive kinds 1 and 3 of v22). Wire values 1 and 3 now read as a standard
star; wire hit kind 5 and the `StarFx` cues 0-2 and 4-6 are retired.

| Type (wire) | Cost | Behaviour |
|---|---|---|
| STAR (0) | 1 | the ricocheting neon star (aim assist, homing). **Max range 14 m** (`combat.shuriken.range`, bounces count): it vanishes there. Full damage (25) inside 7 m (`full_range`), falling off linearly to 50 % (`far_mult`) at 14 m. Aim assist reaches 20 m (`assist_range`). |
| FLASH (2) | 2 | bursts on impact or after 0.5 s. Everyone within 14 m whose view is within 100 degrees of the burst is whited out (no line of sight needed) for up to 3.5 s (scales with facing and distance, fades out). The white-out also DAZES: no lock-on, a slower aim turn (client side: `flash_daze_turn` 0.45 of normal) and +25 % damage taken from every weapon (`flash_daze_damage`) - the "expose them for the katana" window. The thrower is only blinded when looking at the burst within 6 m. Allies are never blinded. No damage of its own. |

## Netcode
* Intent: `PlayerIntent.star` (2 bits, only on the wire while FIRE is set), carried with a lost packet's FIRE edge.
* Throw, flight and energy are predicted by the client with the same code (`combat/shuriken.rs`); tested for parity in `tests/star_parity.rs`.
* Snapshot: a flying star is an ordinary projectile entity (`flags` bits 2..4 = kind, `dir` = its direction). There are no landed star objects any more.
* Server-only effects (`star_pass.rs`): the flash white-out. Reported as `NetEvent::StarFx` (`what` = `STAR_FX_FLASH_BURST` 3) and
  `NetEvent::StarBlind` (victim, strength, ticks: the daze lasts exactly that long).
* Client constants (cost, daze turn multiplier) are `StarCfg::default()` (like the MARK cost): a server with different `combat.star.*` values makes the
  HUD cost and the daze turn rate slightly off until the next energy report. The slower aim while dazed is applied by the client only.

## Bots
Bots poke with the plain star between 3 and 13 m, and now and then throw a flash at a target looking at them 5-13 m away (with energy to spare).

## Visuals and sounds (`star_fx.rs`, `audio.rs`, `synth.rs`)
* Flash: white shell + ring out to the 14 m reach + light flash, `FlashBang`. Dazed (`StarBlind`): the screen white-out holds for the
  first quarter of the daze then fades to nothing exactly as it ends (`whiteout_level`), a `Tinnitus` whine, the world muffled for the
  whole daze, and a DAZED label with a draining bar on the HUD (`RenderView::dazed`).
* Grapple on an enemy (`LocalFx::grapple_pulling_enemy`): the rope goes white-hot with a tension pulse that snaps on at the catch
  (`rope::hook_tension`), a `WhipCrack`. The crosshair lights from `grapple_target_kind` (aim assist counts) and is RED when the target
  is a player (`LocalFx::grapple_target_player`). No shield-block clang: the client cannot tell a block from the pull ending.

## Frames
`docs/screens/weapons_*.png` (`SHOT_WEAPONS=flash|dazed|hook|hookaim`, see the README there).
