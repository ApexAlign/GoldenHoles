# Total destruction: design, parameters, environmental factors

Direction: the maps are (almost) completely destructible. Every structure is built from bricks (`scripts/lvl_lib.py`); weapons
chew through them, whatever loses its support comes down, and nothing is safe for long. Decks and ramps are bricks too, so
the fight reshapes the arena. This is the design contract; the engine pieces are `breaks.rs` (sim), the `World*` events
(protocol v22) and `destructible_fx.rs` / `destructible_audio.rs` (game).

## The rules of the world

1. **Everything is bricks.** Walls, floors, columns, girders are `dbox`es of a material (2 to 6 m on a side). A level keeps at most
   ~1800 boxes and >= 70 % of its volume destructible; only anchors the match logic needs stay permanent: a thin core of
   "bedrock" (a spawn plinth under each spawn, pedestals), so nobody spawns into the void.
2. **Support physics.** A brick with nothing left under it falls `destruct.collapse_ticks` (18 = 0.3 s) after the last support breaks;
   running-bond walls arch over holes, columns fall from the foot up. Falling bricks do not hurt players (v1; a crush
   hazard is a follow-up, see below).
3. **Damage sources.** Katana swings and thrown stars chip; a tag blast, a barrel or canister blast and an arrow are the real wall breakers
   (see the table in docs/destructible.md). Heavy / air / whirl katana moves hit a bigger volume.
4. **Re-forming is a dial.** `destruct.rez_s` = 40 s brings bricks back (bottom-up, not onto a player); `0` = permanent
   damage for the whole match (the arena erodes: late-match = rubble and long falls).

## Parameters (all live-tunable `Balance` keys, server-side; clients just obey the events)

| key | default | what it does | range / note |
| --- | --- | --- | --- |
| `destruct.hp_mult` | 1.0 | divides every hit: 2.0 = twice as tough | 0.25 (paper) .. 4 (bunker) |
| `destruct.rez_s` | 40 | seconds a broken brick stays gone | 0 = never; 15 fast-heal; 90 slow |
| `destruct.blast_mult` | 5.0 | tag / barrel / canister blast vs boxes = player damage x this | 0 = bombs spare walls |
| `destruct.chip_mult` | 1.0 | stars / katana / arrow chip x this | 0 = only blasts break things |
| `destruct.collapse_ticks` | 18 | delay before unsupported bricks fall | 0 instant, 60 slow domino |
| `destruct.retry_ticks` | 60 | wait when a re-form is blocked | |

Per-brick numbers live in the level files (`lvl_lib.MATERIALS`, hit points per m^3): glass 0.6, wood 1.8, brick 3.0, concrete 5.5,
steel 11. A 4 x 3 x 4 m concrete brick = 48 m^3 x 5.5 = 264 hp: one tag, or ~4 katana swings, or ~38 star hits. Katana piece
damage is 60 (`combat.katana.piece_damage`), star 7 (`combat.shuriken.piece_damage`), blast 250 at the centre.

Suggested match presets (files in `balance/`; `PRESET=demolition ./demo.sh`, or `--balance PATH` on the server; a file overlays the demo preset):

* **Skirmish (default):** hp_mult 1, rez_s 40, blast_mult 5.
* **Demolition:** hp_mult 0.5, rez_s 0, blast_mult 8, collapse 10: everything goes, the arena ends as rubble.
* **Fortress:** hp_mult 2.5, rez_s 20, chip_mult 0.5: you need tags to open a wall; it heals fast.

## Performance budget (why bricks are 2 to 6 m, not 1)

Per tick the sim loops the destructible set once (support + re-form), a blast loops it once, a collapse can emit dozens of
`WorldBreak` events (21 bits each, spread over datagrams by the event budget). The renderer draws one mesh per box. So:
<= 1800 boxes, bricks >= 2 m, debris capped at 300 chunks. If a level wants finer detail, merge into bigger bricks.

## Environmental factors (proposals; none built yet)

Each is a server-driven event with a telegraph (sound + HUD cue + visual) so it is dramatic and fair, replicated as one reliable
event, cosmetic parts on the client, physics parts in the shared sim so prediction matches:

1. **Quake:** the floor shakes for 3 s (camera shake, dust); every brick takes damage proportional to how little holds it up;
   weak towers and overhangs fall. Telegraph: rumble 2 s before.
2. **Lightning strike:** a bolt hits a marked column: blast-sized damage on bricks + shock to players inside 6 m. Marked
   2 s ahead by a glowing ring. Great for chewing up whoever camps one spot.
3. **Wind gusts:** a lateral push on airborne bodies, stars and arrows along a direction that rotates; telegraphed by streaking
   particles. Makes hooks and long jumps interesting, ring-outs likelier.
4. **Rising void (shrinking arena):** a kill line climbs from below, eating the bottom levels; bricks that touch it dissolve.
5. **Low-gravity pockets:** floating zones (a box volume with a glow) where gravity is 40 %: air fights and huge hook swings.
6. **Magnetic anchors:** a few glowing nodes that pull the hook harder and give a speed boost on release; they are bricks, so
   they can be shot away.
7. **Aftershock pulse:** every brick break near a pillar makes dust that blinds (a visibility factor like the flash star, local).
8. **Crush hazard:** falling bricks deal damage to a player they land on (scaled by drop height): turns pulling a tower
   down on someone into a weapon.

First pick I would build: Quake + Lightning (they feed the destruction loop, are cheap: one event type, damage goes through
`LevelBreaks::blast`), then Wind. Tell me which ones you want and the severity.
