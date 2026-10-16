# Power weapons: the yumi and the fuda tag

The **yumi** (digital bow) is a PRIMARY weapon: every player always has it, it is never picked up or dropped, and it has no ammo
count. The **fuda tag** is still a pickup: lying on pedestals (and on the floor where its owner died), it takes the bow's place for as
long as the bag lasts. Numbers live in Balance `combat.yumi.*` / `combat.tag.*` (`shinobi-sim/src/power.rs`, defaults below;
`Balance::default().to_text()` lists the keys).

| | YUMI (digital bow, primary) | FUDA TAG (explosive paper tag, pickup) |
|---|---|---|
| Pickup | none: everyone has it from spawn (`yumi` lines in level files are ignored) | pedestals where routes meet, 45 s respawn |
| Load / carry | one arrow, never lost | 2 tags, 60 s (`count`, `carry_s`) |
| Use | press **2** to raise the bow (**1** puts it away; the katana puts itself away with it — no blade shows while the bow is out), then **hold left click** 0.6 s (`draw_s`), release to loose. Letting go early cancels (no reload). After a shot the bow **reloads for 1.1 s** (`reload_s`; the HUD reload bar under the crosshair). | **press** Q: throws a tag (0.4 s gap). With the hands empty a press sets off your armed tags. At most 2 live (`max_live`). |
| While in use | walk -25 % (`move_mult`), -40 % at full draw (`focus_move_mult` 0.6), look speed x0.5 (`turn_mult`) and scaled with the zoom FOV, no stars while drawing; draw ring + reload bar on the HUD | no stars while tags are in hand or still ticking |
| Damage | 80 body, x1.5 head = 120, so **a full-draw headshot kills** (`damage`, `head_mult`), 280 m/s, 4 m/s^2 drop, 250 m range, no damage falloff. **Splash** (near-miss forgiveness, not a second blast weapon): within `splash_radius` **1.2 m** beyond the direct-hit band -- a flight graze, or embedded in a wall right next to someone -- still does a little: body-only (no head bonus), falling off linearly from `splash_frac` **40 %** of a direct hit at the inner edge to 0 at the outer edge (`arrow_splash_damage`, the same falloff shape as the tag's blast with `splash_frac` standing in for `edge_frac`) | blast 4 m (`radius`): 50 at the heart, 40 % at the edge (`damage`, `edge_frac`), thrower takes 30 % (`self_mult`), teammates 0 |
| Knock | none, including on a splash hit (it is a forgiveness band, not the tag's launch) | 14 m/s radial launch + hop (`knock`): ring-out credit, wall-splat window like a katana hit |
| Counters | **block**: damage x guard `front_mult`, guard loses 70 % (`block_drain`); a splash hit is blocked the same way but for proportionally less (`block_drain * splash_frac`). **Parry** (shield raised within the parry window): no damage, the arrow turns around toward the shooter and flies back at full speed (it can hurt them, and be parried again) -- a splash hit still in flight parries the same way (it is the same live arrow); a splash hit from a wall thud has no arrow left to turn around, so a parry there just absorbs it. **Katana** crossing the arrow cuts it out of the air (no splash triggers). | **Katana** swing (anyone but the owner) crossing the tag cuts it off, no blast. Not blockable or parryable (it is an area effect). |
| Telegraph | a drawn bow shows to everyone as a bow with its string back and a faint laser line along the aim; the line glints bright (and the screen frame pulses, "BOW DRAWN ON YOU") when it points at your head | a paper tag with a countdown ring of 12 pips; one stuck on **you** shows "TAG ON YOU 1.3" on the HUD and beeps faster; armed tags blink red |

Dying drops a tag bag where you fell (25 s, whatever was left) for anyone not holding one; the bow does not drop. A taken pedestal
call-out goes to the whole server. A spent or expired tag bag hands the bow back. Pedestals ignore players already holding a bag.

## Zoom (sniping)
Drawing the bow narrows the view toward a focused shot. Client-only camera (`shinobi-client/src/zoom.rs`, numbers in `YumiCfg`, helpers
in `power.rs`); the only sim part is the walk speed.
* **Ramp.** Nothing happens until the draw passes `zoom_start` (25 % of the draw = the 0.15 s tap), then the vertical FOV eases (smoothstep) from your base FOV
  (90 by default) to `zoom_fov` = **30 deg** at full draw. A quick release or a tap stays hip-fire. Releasing drops the zoom at once; it
  rises at most 5/s.
* **Focus** (full draw, not flinching): the reticle ring is at its tightest and gold, four hairlines reach in, the ring sway stops.
* **Costs.** Look speed = min(`turn_mult`, tan(fov/2) / tan(base/2)) (30 deg: x0.27, the same angular speed per screen pixel). Walk drops to
  `focus_move_mult` 0.6 at full draw (0.75 before; the server steps `PowerState` and the client predicts with the same `draw_walk_mult`, replays use the
  state at replay time). **A hit on you** (own health drops) cancels the zoom for `zoom_hurt_s` 0.35 s, then it eases back in while you still hold the draw.
* **Look.** Thin neon ring (radius 0.075 -> 0.018 of the screen height), range readout (`118 M`, `--- M` at open sky) to the aimed point (level raycast or a
  player within 0.7 m of the aim), a drop tick below the centre where the arrow lands at that range (`arrow_drop_angle`: ~0.3 deg at 200 m,
  ~0.0095 of the screen height at 30 deg FOV), a faint vignette (<= 0.2). No scope mask. The first-person katana, shield, star and wrist launcher sink out of frame
  (`viewmodel::zoom_fade`: gone at zoom 0.5; the overlay camera keeps its own 90 deg FOV); our own bow is pinned to its hip-fire screen size and leaves at zoom 0.35. The dash FOV kick fades out with the zoom.
* **Sniping numbers.** Range 250 m (the FFA maps are 224 x 224 / 336 x 336 m, the isles / sky maps ~230 m: diagonals are 300 to 475 m, so 250 m covers
  most sightlines without making the whole map a shooting range). 280 m/s and 4 m/s^2 (speed raised again from 180: a sniping weapon has to be
  near-flat to be worth lining up): at 200 m the arrow is in the air 0.71 s and falls 1.0 m (the drop tick shows it), a runner (9.5 m/s) moves 7 m
  meanwhile, barely a lead at all. Damage does not fall off: a full-draw headshot still kills at any range. The lock-on bracket's gold "fire ready"
  state is range-independent for this reason: it checks a fixed metre-drift window against the target's lateral speed and the arrow's actual flight
  time, so a correctly read lock stays trustworthy at long range, not just up close.
* Review frames: `docs/screens/bow_zoom_{hip,half,full}.png` (`SHOT_FX=bowzoom:D,lineup` with `SHOT_LINE=80 SHOT_LINE_PICK=1`, D = draw 0 / 0.65 / 1).

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
* Splash is checked two ways each tick, both using `splash_radius`/`splash_frac`: the flight sweep that already finds a direct hit is
  widened into a second, outer band (a near-miss resolves only once the true closest approach of the whole flight has already
  happened, so a shot about to land a direct hit keeps flying instead of being downgraded one tick early); and an arrow that embeds in
  a wall runs a small point-centred AOE against anyone else within the band of the impact point (mirrors the tag's blast loop, but on
  a point, not a flying tag, and with no line-of-sight check -- the band is small enough that it is always effectively adjacent).
* Match start / respawn gives you a loaded bow (and drops nothing but a tag bag). Intermission clears every arrow and tag.

## Netcode (protocol v23)
* `PlayerIntent.draw`: the bow's draw button held (one bit; the client sends it while the bow is up and left click is held). The server also
  needs the bow in the hands (no tag bag): a stray bit does nothing. The walk slowdown is a function of that bit alone (`PlayerInput.walk_slow`), so reconciliation replays reproduce it.
* `PowerState` (`power.rs`) is the carried state machine (draw timer, reload, tag ammo, tag carry timer). Its default is the loaded bow. Server and client predictor
  step the **same code** with the same inputs (`tests/power_parity.rs`); the owner block carries (kind, ammo, carry quarter-seconds)
  and the client re-syncs when kind / ammo change, never on a stale repeat (the bow reports kind 1, ammo 1; the reload runs from the loose on
  both sides). The bow has its own button, so the throw button always throws; no star energy is charged for a tag press.
* World pieces ride in the snapshot as entities from `POWER_ID_BASE` (0x6000): arrows (velocity, owner, reflected), tags (flying /
  stuck / on player, armed, fuse left in 1/15) and drawn bows (eye, aim, draw progress). See `shinobi-proto/src/snapshot.rs`.
* `NetEvent::PowerFx { what, owner, pos }` (`power_pass::FX_*`): pickup call-outs, draw, loose, hit / head / blocked / parried / cut /
  wall / splash, tag thrown / stuck / armed / blast / cut, drop. `PickupSpawned.kind` is 0 cube, 2 tag (1 was the yumi pedestal). Hit
  markers: `KIND_ARROW`, `KIND_TAG`. No wire change was needed for splash: it resolves server-side into the same `PvpHit` /
  `CombatEvent::Hit` stream a direct hit already uses, just with a smaller `amount` and a dedicated `FX_ARROW_SPLASH` cue.
* The client uses `PowerCfg::default()` for its predicted draw time and walk (like `StarCfg::default()`): a server tuned differently
  makes the prediction a little off until the next report.

## Levels
`tag x y z [respawn_secs]` lines (default 45 s), next to `pickup` (the data cube). Old `yumi x y z` lines still parse but spawn nothing
(the bow is not a pickup any more), so the bow pedestals of `levels/ffa_sky.lvl` / `ffa_small.lvl` are dead lines until the level owners
drop them.

## Bots (`shinobi-ai/src/power_bot.rs`)
Reads only what a client knows. **Yumi** (every bot has it): at a visible target 12 to 40 m away, loaded, it starts a draw now and then (about one every 5 s, with a
3 s pause after each shot on top of the reload; a readable telegraph), aims with lead and drop, and looses when the draw is
full and the aim has settled. With nothing to shoot at, a free moment goes to a tag pedestal within 30 m if the bot can get there on foot
(30 % of bots fancy a given pedestal). **Tags**: thrown at a target 5 to 18 m away (more eagerly at a cluster), set off by hand when a foe
is in the blast and the bot is clear. **Defence**: an arrow within 20 m closing on it sometimes raises the shield (a fresh one parries it
back); a tag stuck on it gets a swing.
