# DEMO v0.1 plan: decisions, scope, checklist (owner delegated all design decisions to Engine until the demo)

Pitch: a Tron grid FFA arena shooter where you run on walls, grapple, lock on with a Predator triangle, throw ricocheting
discs, slice with a katana, and print floors/walls out of the grid that others can de-rez.

## Locked decisions (change only via Engine)
- Mode: FREE-FOR-ALL. Demo default: 1 human + 5 bots on the 88 m `levels/ffa_small.lvl` (`BOTS=5 ./demo.sh`). Up to 16 players. Match: first to 10 kills
  or 6 minutes, 2.5 s respawn (a death animation fills it: flash, collapse camera, the body bursts into fragments), 1.5 s spawn protection, 8 s intermission. Team mode stays hidden (`--teams` only).
- Map: `levels/ffa.lvl`, fixed (no shrinking, no void-start in the demo). Default `levels/ffa.lvl` = 224 m across (HALF=112). `levels/ffa_xl.lvl` = 336 m across (HALF=168) is optional; measured
  density favours the 224 m map (18.7 vs 8.7 kills/min with 16 players, Network). Layered: ground, mid towers, sky layer. Void edges + kill
  plane. 24+ spread spawn pads, no mutual sightlines.
- Building: N = floor/ramp (tap cycles), M = wall. Grid snapped. ANY player can destroy ANY piece, including their
  own. Piece hp 100; shuriken 12 dmg, katana 60 dmg to pieces; older pieces tougher (`age_bonus_per_sec` 0.02);
  size scales with owner kills `scale_per_kill` 0.1, `scale_cap` 2.0 (never more than 2x). Cap 64 pieces/player,
  build cooldown 12 ticks, range 6 m. Pieces de-rez into data fragments. No fall damage (kill plane only).
- Aim: Predator three-dot lock-on, acquire -> lock over 0.35 s, pull full only at lock, enemies only, needs LOS.
  Katana lunges toward the lock. Hit-stop 60 ms on katana hit (client-only).
- Grapple: E fire/release, pull + swing/orbit, speed cap 28 m/s, range 40 m.
- HUD: cyber-computer look, console/menus interactive, radar without wallhack.
- Audio: procedural synth behind the `audio` cargo feature, OFF by default; it may slip past the demo.
- Colours: unique neon per player (`player_colour`).

## Demo = scope freeze
IN: everything above. OUT (post-demo): teams polish, void-start map, capture/hold modes, ranked, new weapons.
After freeze only: bugs, polish of IN items, tests. No new systems without Engine approval.

## Demo-ready checklist (Engine runs it; each owner reports status)
1. `scripts/check.sh` green on golden-shinobi-main (workspace tests + game compile). 2. `BOTS=5 ./demo.sh` boots into the FFA
map, bots fight, a match ends, the next starts. 3. Controls work: WASD, jump, dash/roll/slide, wall-run, E grapple,
N/M build, LMB katana, RMB shuriken. 4. Lock-on triangle shows and assists. 5. Hit/kill/damage feedback on every
weapon, HUD panels live. 6. Grapple + build stay in sync at 5% loss (e2e tests). 7. Release soak at 16 players: p99
tick < 5 ms. 8. QUICKSTART has the demo commands; a tag `demo-v0.1` is pushed by Engine when 1-7 pass.
