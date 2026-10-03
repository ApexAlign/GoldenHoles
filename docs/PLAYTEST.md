# Playtest guide

You: one human against 7 bots in a free-for-all on the big map. About 5 minutes of play gives useful data; 15 is better.

## Run it (one command)

```bash
scripts/play.sh
```

It builds the server (release) and the game, starts a free-for-all with 7 bots on `levels/ffa.lvl`, opens the game
window and prints the controls. The first build compiles the Bevy engine and takes several minutes; later runs start in
seconds. Add `OFFLINE=1` if the machine has no network but the crate cache is warm.

| Want | Command |
|---|---|
| fewer / smarter bots | `BOTS=3 scripts/play.sh`, `BOT_SKILL=hard scripts/play.sh` (easy, normal, hard) |
| the other (smaller) map | `LEVEL=levels/ffa_xl.lvl scripts/play.sh` |
| friends on the LAN | server: `cargo run --release -p shinobi-server -- --bind 0.0.0.0 --level levels/ffa.lvl --bots 4 --telemetry playtest/lan.txt` (UDP 7777 open); every player: `cd crates/shinobi-game && cargo run -- --server SERVER_IP:7777 --level ../../levels/ffa.lvl` |
| no window (smoke test) | `scripts/demo_headless.sh` |

Quit by closing the window; the server stops with it.

## Controls

WASD move, mouse aim, Space jump (again = double jump), Shift dash, Q roll, C slide, W into a wall = wall-run,
**E** wrist grapple (hook the crosshair, E again releases), **N** floor/ramp and **M** wall (grid pieces: anyone can shoot
or slice them apart), left click katana dash-slice, right click throw a shuriken (keep an enemy inside your view cone
and a three-dot triangle locks on to them). Esc releases the mouse.

## First 5 minutes: try each of these at least once

1. Run, jump, double-jump, dash, slide down a slope, wall-run along a long wall. Does anything feel sticky or floaty?
2. Throw shurikens at a bot from 10 m, 30 m and across the map. Does the lock-on triangle appear, and does it help or fight you?
3. Hit a bot with the katana from a dash. Do you feel the hit (flash, freeze, sound-less shake)? Does a miss feel like a miss?
4. Grapple to a ledge, swing off a pillar, release with a jump, grapple again. Try to cross the map without touching the ground.
5. Print floors and walls (N / M): a bridge over a gap, a wall to hide behind. Shoot your own wall until it breaks; watch the
   cracks. Do bots use your pieces or get stuck on them?
6. Die on purpose twice (fall off the edge, get shot). Is it clear why you died and where you respawn? Respawn is 3 s with
   1.5 s protection.
7. Finish a match (first to 10 kills or 6 minutes) and look at the end screen, the intermission and the next match starting.

## What gets recorded

The server prints `TELEMETRY` lines every 60 s and when a match ends, in the terminal and in
`playtest/telemetry-<time>.txt` (plain text, plus one JSON line per player for scripts). Per player:

- kills, deaths by cause (shuriken, katana, beam, void, hazard), average life;
- shuriken: thrown, hit %, headshot %, and hit % **locked vs unlocked** (the lock-on is server-authoritative, so this is
  the real effect of the aim assist);
- lock-on: times locked, seconds locked out of seconds alive;
- katana swings and hits, grapple fired / attached / missed, pieces built and lost;
- network: how far ahead of the server clock your inputs arrived (headroom) and how often one was late.

The bots are summed in one row so you can compare yourself with them. Numbers are per match (they reset when a match
starts). Definitions are at the top of `crates/shinobi-server/src/telemetry.rs` (a hit is counted when it lands, and
"locked" hits mean locked on the victim at the moment of the hit, so short windows are approximate).

## Feedback form (copy, fill in, send together with the two files in `playtest/`)

```
1. Date / machine / how long you played, map (ffa or ffa_xl), bots + skill:
2. First impression in one sentence:
3. Movement (run, wall-run, slide, grapple): best moment / worst moment:
4. Combat (shuriken, lock-on, katana): too easy / too hard / just right, and why:
5. Aim assist: did you notice the lock pulling your aim? Helpful, annoying, invisible?
6. Building (N / M, shooting pieces apart): fun / confusing / useless. What did you build?
7. Bots: smart enough? stuck anywhere? unfair? boring?
8. Map: got lost / too empty / too crowded / favourite spot / a spot that felt broken:
9. Bugs (what you did, what happened, what you expected; screenshots welcome):
10. Would you play another match right now? What one change would make you say yes?
```

Files to send: `playtest/telemetry-*.txt` and `playtest/server-*.log` (both are plain text, a few hundred KB at most).
