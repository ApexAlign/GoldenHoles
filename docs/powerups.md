# Power weapons: the yumi and the fuda tag

Nobody spawns with one. Two ninja-flavoured power weapons lie on **pedestals** (and on the floor where their owner died) and
replace the right mouse button while they are in your hands. Melee stays the main fight; these are the spice. Numbers live in
Balance `combat.yumi.*` / `combat.tag.*` (`shinobi-sim/src/power.rs`, defaults below; `Balance::default().to_text()` lists the keys).

| | YUMI (digital bow) | FUDA TAG (explosive paper tag) |
|---|---|---|
| Pickup | rare, glowing pedestal on the tallest towers, 45 s respawn | pedestals where routes meet, 45 s respawn |
| Load / carry | 3 arrows, 30 s (`arrows`, `carry_s`) | 2 tags, 60 s (`count`, `carry_s`) |
| Use | **hold** right button 0.6 s (`draw_s`), release to loose. Letting go early cancels (no arrow spent); a tap under 0.15 s (`tap_s`) is still a star throw. Next draw 0.5 s after a shot (`cooldown_s`). | **press** right button: throws a tag (0.4 s gap). With the hands empty a press sets off your armed tags. At most 2 live (`max_live`). |
| While in use | walk -25 % (`move_mult`), look speed x0.5 (`turn_mult`), no stars; draw ring + arrow pips on the HUD | no stars while tags are in hand or still ticking |
| Damage | 80 body, x1.5 head (`damage`, `head_mult`), 90 m/s, 5 m/s^2 drop, 140 m range | blast 4 m (`radius`): 50 at the heart, 40 % at the edge (`damage`, `edge_frac`), thrower takes 30 % (`self_mult`), teammates 0 |
| Knock | none | 14 m/s radial launch + hop (`knock`): ring-out credit, wall-splat window like a katana hit |
| Counters | **block**: damage x guard `front_mult`, guard loses 70 % (`block_drain`). **Parry** (shield raised within the parry window): no damage, the arrow turns around toward the shooter and flies back at full speed (it can hurt them, and be parried again). **Katana** crossing the arrow cuts it out of the air. | **Katana** swing (anyone but the owner) crossing the tag cuts it off, no blast. Not blockable or parryable (it is an area effect). |
| Telegraph | a drawn bow shows to everyone as a bow with its string back and a faint laser line along the aim; the line glints bright (and the screen frame pulses, "BOW DRAWN ON YOU") when it points at your head | a paper tag with a countdown ring of 12 pips; one stuck on **you** shows "TAG ON YOU 1.3" on the HUD and beeps faster; armed tags blink red |

Dying drops what you held where you fell (25 s, whatever ammo was left) for anyone with empty hands. A taken pedestal call-out
goes to the whole server ("ECHO FOX TOOK THE YUMI"). An empty bow or tag bag is gone; so is one that ran out of carry time.
Nobody can hold two weapons; pedestals ignore players whose hands are full.

## Rules worth knowing
* A tag sticks to the first surface **or player** it touches (its path is swept against solids and living foes, not just hit-tested),
  arms 0.5 s after sticking, detonates 2.0 s after sticking (`fuse_s`), or on your next press once armed. Its blast needs a clear line
  to the victim (a stuck-on victim is always in it). A tag on a player who dies fizzles; so do the tags of an owner who dies.
* The explosion launches everyone inside it away from the centre (a tag stuck to a body pushes away from the thrower). The launch
  sets the same `melee` bookkeeping a katana hit does: `last_hit` (a void death inside the ring-out window is the thrower's kill) and
  the wall-splat window (stopped dead by a wall in the air: splat damage + stagger).
* Arrows are server-authoritative flying objects (swept against solids and capsules each tick; no lag compensation), parried or
  blocked **at the victim's shield** using the same arc test as every other hit (`block::guard_vs`). Spawn-protected and rolling players
  are passed through.
* Match start / respawn empties your hands. Intermission clears every arrow and tag.

## Netcode (protocol v21)
* `PlayerIntent.draw`: the throw button held while a yumi is in hand (one bit). The server also needs the weapon: a stray bit does
  nothing. The walk slowdown is a function of that bit alone (`PlayerInput.walk_slow`), so reconciliation replays reproduce it.
* `PowerState` (`power.rs`) is the carried state machine (draw timer, ammo, carry timer, cooldown). Server and client predictor
  step the **same code** with the same inputs (`tests/power_parity.rs`); the owner block carries (kind, ammo, carry quarter-seconds)
  and the client re-syncs when kind / ammo change, never on a stale repeat. The client sends no FIRE edge for a real draw and no star
  energy is charged for a tag press.
* World pieces ride in the snapshot as entities from `POWER_ID_BASE` (0x6000): arrows (velocity, owner, reflected), tags (flying /
  stuck / on player, armed, fuse left in 1/15) and drawn bows (eye, aim, draw progress). See `shinobi-proto/src/snapshot.rs`.
* `NetEvent::PowerFx { what, owner, pos }` (`power_pass::FX_*`): pickup call-outs, draw, loose, hit / head / blocked / parried / cut /
  wall, tag thrown / stuck / armed / blast / cut, drop. `PickupSpawned.kind` is 0 cube, 1 yumi, 2 tag. Hit markers: `KIND_ARROW`,
  `KIND_TAG`.
* The client uses `PowerCfg::default()` for its predicted draw time and walk (like `StarCfg::default()`): a server tuned differently
  makes the prediction a little off until the next report.

## Levels
`yumi x y z [respawn_secs]` and `tag x y z [respawn_secs]` lines (default 45 s), next to `pickup` (the data cube). `levels/ffa_sky.lvl`:
two bows (central spire top, east crown tower top), four tag bags (three turn plazas, one terrace). `levels/ffa_small.lvl`: one bow
on the hub pylon, four tag bags (hub second tier x2, open ground x2). Generators: `scripts/gen_sky_level.py`, `scripts/gen_small_level.py`.

## Bots (`shinobi-ai/src/power_bot.rs`)
Reads only what a client knows. With empty hands and a free moment, a bot walks to a pedestal within 30 m if it can get there on foot
(55 % of bots fancy a given pedestal). **Yumi**: at a visible target 12 to 110 m away it draws (a readable telegraph), aims with lead and
drop, looses when the draw is full and the aim has settled. **Tags**: thrown at a target 5 to 18 m away (more eagerly at a cluster),
set off by hand when a foe is in the blast and the bot is clear. **Defence**: an arrow within 20 m closing on it sometimes raises
the shield (a fresh one parries it back); a tag stuck on it gets a swing.

## Sounds and FX (`shinobi-game/src/power_fx.rs`, `audio.rs`)
Existing synth sounds: draw HeavyCharge, loose Throw (low), arrow hit HitBody / HitHard (head), blocked Block, parried Parry, cut Clash,
wall Ricochet, tag throw Throw (high), stuck Print, armed Refill (high), blast AirSlam, drop Land, own pickup Refill, someone else's
pickup Execute (high), countdown RopePulse beeps when a tag is on you. FX: streak-trail arrows, the drawn bow with its string and laser,
paper-glyph tags with a pip ring, blast shell + floor ring, glowing pedestals.

## Tests
`shinobi-sim/tests/power_weapons.rs` (draw / damage / head / drop / range, block, parry-reflect, cut-down, stars vs bow, tag stick /
arm / fuse / hand detonation / falloff / launch / ring-out / splat marks / self and ally damage / disarm / live cap, pedestals: pickup,
respawn, full hands, drop on death), `power_parity.rs`, `power.rs` / `power_pass.rs` unit tests, `level.rs` (kinds),
`arena_sky.rs` / `arena_small.rs` / `walkable.rs` (pedestals supported, map stays walkable), `shinobi-ai/tests/power_bots.rs`,
`shinobi-server/tests/power_e2e.rs` (real UDP: pickup call-out, draw ring, arrow in the snapshot, ammo, run dry),
`shinobi-client` `hud_power` + goldens.

## Frames
`docs/screens/power_world_{yumi,arrows,tags,tagblast,pedestals}.png` (lavapipe, `SHOT_FX=yumi|arrows|tags|tagblast|pedestals`, recipe in
`docs/screens/README.md`) and CPU HUD frames `hud_cpu_power_{draw,full,tags_live,tag_on_me,glint}.png`.
