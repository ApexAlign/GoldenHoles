# Star variants

Right click throws the **selected** throwable; **mouse wheel** or **R** cycles it (Q is the roll). Holding right click is still the MARK star.
All four draw on the same star energy pool (6 charges, lockout when it cannot afford a plain star). Numbers live in Balance `combat.star.*`
(`combat.shuriken.energy_cost` is the plain star's 1). The HUD selector under the pips shows the type, its cost, rings on the pips a throw
spends, and a one-line blurb after a switch; a hint appears on the first plain throws (3 times, stops once you cycle); the controls strip has a row.

| Type | Cost | Behaviour |
|---|---|---|
| STAR | 1 | the existing ricocheting neon star (aim assist, homing) |
| MINE | 3 | flies straight, sticks to the first surface, arms after 0.6 s, bursts when an enemy is within 3 m: 25 damage within 4 m + 9 m/s knockback. Max 2 per player (a third replaces the oldest), 20 s life, shot (standard star within 0.9 m of it) or cut (katana) = disarmed. Ignores teammates and its owner. |
| FLASH | 2 | bursts on impact or after 0.5 s: enemies within 12 m, inside a 70 degree cone around their view, with line of sight, are whited out up to 1.2 s (strength scales with facing and distance). No damage. White-out breaks lock-on, blocks acquiring a new one (aim assist is off), and washes the screen (scaled by effect strength / reduced motion). |
| SMOKE | 2 | flies straight, lands, releases a 4 m cloud for 5 s. Players inside cannot be locked onto by others and vanish from the lock triangle, radar and tags. They can still be hit. |

## Netcode
* Intent: `PlayerIntent.star` (2 bits, only on the wire while FIRE is set), carried with a lost packet's FIRE edge.
* Throw, flight, landing and energy are predicted by the client with the same code (`combat/shuriken.rs`); tested for parity in `tests/star_parity.rs`.
  A predicted landed mine / smoke is dropped after 0.5 s; the authoritative copy comes in the snapshot (stable entity ids from `STUCK_ID_BASE`,
  `flags` = kind / stuck / armed, `fx` = life left).
* Server-only effects (`star_pass.rs`): bursts, disarms, white-outs, smoke hiding. Reported as `NetEvent::StarFx` (sounds, FX) and `NetEvent::StarBlind`.
* Client constants for cost / smoke radius are `StarCfg::default()` (like the MARK cost): a server with different `combat.star.*` values makes the HUD
  cost and the client's smoke-lock prediction slightly off until the next energy report.
* Mines and smoke vanish when their owner dies (`reset`).

## Bots
Now and then (per-kind gaps): a mine at a target 8-22 m away, a flash at one looking at them 7-16 m away, smoke dropped at their own feet when hurt
or retreating. Only with energy to spare.

## Sounds (existing synth sounds, `audio.rs`)
mine landed Print, smoke landed Land, mine burst AirSlam, flash CounterFlash, mine disarmed Whiff, own white-out LockBreak, cycling UiClick.

## Frames
`docs/screens/star_world_*.png`, `hud_cpu_star_sel_*.png` (see the README there).
