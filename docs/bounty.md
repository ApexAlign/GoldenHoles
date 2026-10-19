# Bounty: a map-wide target that gives a match stakes

Problem it's aimed at: a plain deathmatch has nothing to chase beyond the frag counter. Bounty adds one
additive, in-match-only system on top of the existing frag race: get on a streak, become hunted by everyone,
and either cash in the bonus for killing the hunted player or survive long enough that the title moves on.

No persistent progression, no account-level stats — it resets with every match (`MatchState::reset_all`) exactly
like kills and deaths do.

**FFA only.** `MatchState::update_bounty` is a no-op whenever `team_mode` is on: a team match already has its
own race (team frag count), and this system is built around a target everyone, friend or foe, can see and hunt
— not a note on a teammate's own HUD. Turning `team_mode` on always clears an existing bounty the same tick.

## How it works

1. **Streak tracking.** Every player has a server-side `kill_streak` (`pvp::Vitals::kill_streak`): consecutive
   kills without dying. A kill increments it; dying zeroes it immediately. It is not itself a wire field — only
   the bounty's identity is replicated (below).
2. **Becoming the bounty.** When nobody currently holds the title and some player's streak reaches
   `match.bounty_streak` (default **3**), they become the BOUNTY (`MatchState::update_bounty`, called once per
   Live tick after that tick's kills are resolved). Ties (two players cross the threshold the same tick) go to
   the higher streak, then the lower player id — deterministic regardless of iteration order.
3. **Grace window.** The marker is not visible yet: for `match.bounty_grace_ticks` (default **2.5 s**) only the
   bounty themselves gets a heads-up (`"MARKED AS BOUNTY"` on their own HUD). This is the ramp-up the brief asks
   for — becoming the bounty is not an instant ambush. After the window elapses the marker goes **live**
   map-wide (`MatchEvent::BountyLive`).
4. **The buff.** While marked (grace or live), the bounty's own outgoing katana / star damage is multiplied by
   `match.bounty_dmg_mult` (default **1.1**, i.e. +10%), applied in `pvp::apply_hits` in the same place and
   under the same condition as the existing momentum/kill-streak bonus: only against an open target. A raised
   shield (block or parry) is judged on the base cut, same as it already is against momentum — the bounty buff
   never punches through it either. Arrows and fuda tags are resolved against shields earlier (`power_pass.rs`
   / `star_pass.rs`) and never reach this code, so they are not boosted — a +10% nudge on the katana and stars
   is enough to feel it without re-opening those weapons' own guard handling. Deliberately modest: tense, not a
   free pass. It never touches anyone else's numbers and never changes the base combat constants (parry, guard,
   combo, cloak, bow).
5. **Cashing in.** Killing the bounty grants the killer `match.bounty_bonus_kills` (default **1**) extra frags on
   top of the normal +1, plus a `MatchEvent::BountyKilled { killer, bounty }` for the announcer log / kill feed.
   This fires for a hazard kill too (ring-out, wall-splat, lightning...) — anything that already credits a
   killer the normal way. A kill with nobody credited (self-inflicted, environmental) clears the title the same
   way any death does, but pays no bonus to anyone, matching the server's own normal-kill crediting rule. The
   bounty's own death (by anyone) clears the title the same tick via the normal streak reset.
6. **Clearing.** The title clears when the bounty dies, disconnects, the match leaves Live (warmup/intermission),
   or `match.bounty_streak` is set to 0 (off). As soon as it clears, whoever next reaches the streak threshold
   becomes the new bounty — there is always at most one at a time.

## Tunables (`Balance`, `match.*` keys)

| Key | Default | Meaning |
|---|---|---|
| `match.bounty_streak` | 3 | Consecutive kills to become the bounty. `0` disables the whole system. |
| `match.bounty_grace_ticks` | 150 (2.5 s @ 60 Hz) | Ticks between being marked and the map-wide marker going live. |
| `match.bounty_bonus_kills` | 1 | Extra frags credited for the kill that takes down the bounty. |
| `match.bounty_dmg_mult` | 1.1 | Damage multiplier on the bounty's own hits while marked. Must stay >= 1 (validated). |

Set any of these from a balance text file the same way as every other key (`match.frag_limit = 20`, etc.);
`Balance::from_text` round-trips them and rejects `bounty_dmg_mult < 1.0`.

## On the wire

No new reliable event tag was added — the protocol's 5-bit event tag space (`shinobi_sim::events::NetEvent`)
was already full at exactly 32 variants. Instead the periodic scoreboard (`board` section of the SNAPSHOT
datagram, `shinobi_proto::gameplay::Board`) gained two fields:

* `bounty: u16` — 0 = none, else the marked player's id.
* `bounty_live: bool` — only present on the wire when `bounty != 0`; true once the grace window has elapsed.

The board is already re-sent at once on a match phase change; the server now does the same on any bounty
transition, so clients see a new bounty (or it clearing) within one datagram rather than waiting for the normal
~1 s cadence. This bumped the protocol to **v24** (`shinobi_proto::datagram::VERSION`); see its module doc for
the exact byte layout.

## Bots

`shinobi-ai` bots get a tiered chance to commit to hunting the current bounty (`SkillParams::bounty_hunt_skill`,
mirroring `parry_skill` / `cloak_skill`):

| Tier | `bounty_hunt_skill` |
|---|---|
| Easy | 0.2 |
| Normal | 0.5 |
| Hard | 0.85 |

The roll (`host::think_all`) only fires once the bounty is **live** (never during the grace window — bots know
no more than any other player does) and the bounty is inside the bot's normal reachable target pool (so it
respects the usual team / warmup-engager-cap filtering). It is rolled **once per bounty instance**, not once
per tick: the seed is `(bot's seed, bot id, the tick the bounty was marked)`, so every bot commits or doesn't for
that bounty's whole run, deterministically and reproducibly from a replay. A per-tick roll would have made the
tiers meaningless — even an Easy bot's 0.2 chance fails for a full second with probability 0.8^60 ~ 1e-6, so
everyone would converge on the bounty within about a second regardless of skill.

A bot that commits does not blindly beeline for the bounty regardless of what is in the way: the bounty's
*cost* in the normal nearest/weakest-enemy target scoring is halved (the same "wounded targets look closer"
scaling the selector already does for low-health enemies), a strong pull rather than a hard override. A bounty
standing right next to a normal, safely-reachable target still wins out over a bounty the bot would otherwise
have to path across a hazardous route to reach — the nav grid and the existing reachability filtering never get
bypassed. An earlier version of this *was* a hard override and reliably walked bots off `levels/ffa_clock.lvl`'s
islands chasing a bounty across the drum; the cost-discount version was checked clean (zero void deaths) against
that level and every other movement-safety level test (`isles`, `sky`, `hive`, `split`) with the system fully on.

A committed hunter also reaches for the rope harder (`fsm::Assignment::hunting`, `fsm::hook_decision`): its hook
window stretches from `HOOK_MAX` (90 m) out to `HOOK_HUNT_MAX` (140 m), and the per-tick chance to take the hook
roughly doubles at both the near and far bands (`HOOK_HUNT_NEAR_CHANCE` / `HOOK_HUNT_CHANCE` vs. the ordinary
`HOOK_NEAR_CHANCE` / `HOOK_CHANCE`). This only applies while that bot's assigned target *is* the live bounty it
committed to hunt — an ordinary target never gets the wider window. The wanted player gets actively run down
with traversal, not just flagged on everyone's HUD.

The bounty gets a counter to that, not just the hunt: while marked (grace or live), the bounty's own lock-on
reaches `combat::BOUNTY_ASSIST_RANGE_MULT` (1.5x) farther than usual (`CombatSystem::assist_boost`, set every
tick in `tick.rs` the same way `suppress_lock` is). A wanted player can pick off a pursuer at range with the
lock-assisted bow before their hook/wall-run closes the distance, instead of only being able to run.

## HUD

* The bounty's own screen gets `"MARKED AS BOUNTY"` during the grace window, then `"YOU ARE THE BOUNTY"` once
  live (pulsing gold). Nobody else sees anything until the grace window elapses — that is the whole point of the
  window.
* Everyone else sees `"BOUNTY: <callsign>"` once live, plus a small two-line beacon caret riding the compass
  strip at the bounty's bearing (or a `<<`/`>>` edge chevron, both in the HUD's own bitmap font, when they're
  outside the visible 120-degree window) — the same "always know roughly where" treatment the compass already
  gives cardinal directions.
* The kill feed appends **(BOUNTY)** to the victim's name when the bounty falls to an actual killer (inline,
  not a second line — the rows are packed too tightly under the brow for one). A self-inflicted or environmental
  death still clears the title but draws a plain, untagged void row, matching that it pays no bonus.

All derived client-side from the `Board` fields above plus whatever position the client already has for that
player id this frame (no extra wire cost for the beacon itself).

## Tests

* `crates/shinobi-sim/src/match_state.rs` (`#[cfg(test)] mod tests`): streak -> marked -> grace -> live,
  bonus-on-kill (a real katana kill through the full `Server`), bonus-on-ring-out (a hazard kill, same bonus,
  plus the no-killer case that pays nobody), clear-on-death, and the "off when `bounty_streak == 0`" case.
* `crates/shinobi-proto/src/gameplay.rs`: `Board` round-trips `bounty` / `bounty_live` (including the
  marked-but-not-live case).
* `crates/shinobi-ai/src/skill.rs`: `bounty_hunt_skill` climbs with the tier like every other skill number.
