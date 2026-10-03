# Shinobi quickstart

Neon cyber-ninja arena shooter: run, wall-run, grapple, dash-slice and throw shuriken in a Tron-styled arena.
The headline mode is **free-for-all**: everybody for themselves, each player has their own neon colour. Team
deathmatch (Cyan vs Orange, 8v8) is optional (`--teams`). Server-authoritative, 60 Hz, UDP.

## Three commands

```bash
# 1. server   (UDP port 7777; free-for-all with 7 AI opponents)
cargo run --release --offline -p shinobi-server -- --bind 0.0.0.0 --bots 7

# 2. game     (a window; on another machine use the LAN address the server banner prints)
cd crates/shinobi-game && cargo run --offline -- --server 127.0.0.1:7777
# with sound (procedural synth, no asset files; needs alsa dev headers on Linux): add `--features audio`
#   cd crates/shinobi-game && cargo run --offline --features audio -- --server 127.0.0.1:7777

# 3. extra scripted players (optional load / company; no window)
cargo run --release --offline -p shinobi-bot -- --addr 127.0.0.1:7777 --count 4 --secs 120
```

One-liner for a local free-for-all against 7 bots: `./demo.sh` (builds, starts the server, opens the window;
`BOTS=3 ./demo.sh` for fewer opponents).
No window available? `scripts/demo_headless.sh` runs server + AI + scripted clients for 60 s and prints a health report.
Playtesting? `scripts/play.sh` (builds, starts a free-for-all with 7 bots, records telemetry) and `docs/PLAYTEST.md`.
Before pushing code: `scripts/check.sh` (workspace tests + `cargo check` of the Bevy game crate; `--fast` skips the game crate).

## Windows / macOS / Linux without bash (plain cargo, two terminals)

The `.sh` scripts need bash; everything they do is plain cargo. Use the repo root as the working directory (the server and
the game find `levels/ffa.lvl` themselves; both MUST load the same level, so do not pass `--level` to only one of them).

**Once:** install Rust from https://rustup.rs. Windows: also the "Desktop development with C++" build tools (MSVC) and
Git for Windows. macOS: `xcode-select --install`. Linux: a Vulkan driver (`mesa-vulkan-drivers`) and `libxkbcommon-x11-0`.
The first game build compiles hundreds of crates (about 10 minutes); later builds are incremental.

**Windows PowerShell** (two windows, both in the repo folder):

```powershell
# window 1: server (free-for-all, 7 bots). Allow it through the Windows firewall if asked (private networks).
cargo run --release -p shinobi-server -- --bots 7

# window 2: game
cd crates\shinobi-game
cargo run --release -- --server 127.0.0.1:7777
```

**macOS / Linux terminal** (two tabs, repo folder):

```bash
cargo run --release -p shinobi-server -- --bots 7
cd crates/shinobi-game && cargo run --release -- --server 127.0.0.1:7777
```

Variants (same on every OS, flags instead of env vars): fewer bots `--bots 3`; bigger map `--level levels/ffa_xl.lvl` on BOTH
server and game; 8v8 `--team-size 8 --level levels/8v8.lvl` on both; LAN: server `--bind 0.0.0.0`, game `--server <server LAN IP>:7777`
(open UDP 7777 in the firewall). Sound: add `--features audio` to the game command (Linux needs alsa headers).

**Mouse / pointer lock:** click the window to grab the mouse (that click only grabs, it does not slash). Esc releases it
(and opens the pause screen); click again to resume. Losing focus (Alt-Tab, clicking another window) also releases it and
you must click to grab again. Holding Tab frees the cursor for the scoreboard; backquote opens the console (Esc closes the
console or radial menu first, a second Esc is the pause screen).

## Optional: team mode (8v8) + grapple

Free-for-all is the default. For Cyan vs Orange, 8 per side, with bots filling the empty slots (you join the smaller
team) start the server with `--team-size 8` (or `--teams` for no bot fill). Teams are opt-in;
the server and every game window MUST load the same level (`levels/8v8.lvl`) or client prediction desyncs.

**One machine** (builds, starts the server on the 8v8 map, opens the window):

```bash
TEAM_SIZE=8 ./demo.sh
```

**Server on one machine, game on another:**

```bash
# server machine: reachable from the network, UDP port 7777 open (Linux: sudo ufw allow 7777/udp)
cargo run --release -p shinobi-server -- --bind 0.0.0.0 --level levels/8v8.lvl --team-size 8

# every player machine: use the LAN IP the server banner prints (not 127.0.0.1)
cd crates/shinobi-game && cargo run --release -- --server 192.168.1.20:7777 --level levels/8v8.lvl
```

Press **E** to fire the wrist grapple at the crosshair (the rope reels you in; E again releases) and use it with
dash, wall-run and slide. Shurikens and the katana pass through teammates; first team to 50 team kills wins.
(Player-built blocks work on the server and the wire, but the game has no build key yet.)

**No window?** `scripts/demo_8v8_headless.sh` runs a release server with `--team-size 8` plus 8 scripted clients that
hook the grapple and try to place blocks for 60 s, then prints PASS/FAIL (connections, tick rate, teams on the wire,
grapple attaches seen). `SECS=20 CLIENTS=4 scripts/demo_8v8_headless.sh` for a quick run.

**First build:** the examples above drop `--offline`, so cargo may download crates. Add `--offline` only when the crate
cache is already populated (the scripts use it by default; `OFFLINE=0 scripts/demo_8v8_headless.sh` turns it off). The
very first Bevy game build compiles hundreds of crates and takes several minutes; later builds are incremental.

## Controls

| Input | Action |
|---|---|
| click window | grab the mouse (Esc releases) |
| W A S D, mouse | move, aim |
| Space | jump / double-jump |
| Shift / Q / C | dash / roll / slide |
| W into a wall (hold Space or W) | wall-cling / wall-run |
| Left click | katana dash-slice |
| Right click | throw shuriken |
| E | wrist grapple: hook the crosshair and reel in, E again releases |

## What to expect

- Free-for-all: no teams, anyone can hurt anyone; your colour is derived from your player id. A match starts after a
  short warmup (one player is enough). Default rules (the demo preset): 100 hp,
  respawn in 3 s with brief spawn protection, katana kills in 2 hits, first to 10 kills or 6 minutes wins,
  8 s intermission, then the next round.
- Grid-beam cubes (glowing pickups) respawn after 20 s. Fall off the map and you die (kill plane).
- The server console prints a kill feed (`KILL p3 sliced p7`), match flow and a stats line every 10 s.

## Server options

`shinobi-server [BIND] [--bind ADDR] [--port N] [--max-players N] [--bots N] [--teams] [--team-size N]
[--bot-skill easy|normal|hard] [--level PATH] [--balance PATH] [--obstacles] [--seed N] [--stats-secs S] [--idle-ms N]` — flags override `SHINOBI_*` environment
variables (`SHINOBI_BOTS`, `SHINOBI_PORT`, ...). `--bind 0.0.0.0` means "reachable from the network" and the port
defaults to 7777. Humans get ids 1..=200 (max 200 players), bots 201..=255 (max 55).

- `--obstacles`: opt in to waves of moving neon obstacles that hurt on contact (off by default).
- `--level PATH`: text file, one directive per line (`box cx cy cz hx hy hz [roll]`, `spawn x y z [team]`,
  `pickup x y z [respawn_secs]`). Default: `levels/arena.lvl` when present, else the built-in arena.
- `--teams`: opt-in team deathmatch, Cyan (team 0) vs Orange (team 1). Players join the smaller team (then the one
  with fewer kills). First team to 50 team kills wins, or the higher team score at the time limit; a tie goes to a
  60 s sudden death (first team kill wins, else a draw). Frags of players who leave stay on their team's score.
- `shinobi-lagproxy` (`shinobi-bot` crate): UDP relay that adds delay / jitter / loss / reorder / bandwidth cap between a
  game and the server, for testing over a bad link; see docs/PLAYTEST.md.
- `--telemetry FILE`: append playtest telemetry (`TELEMETRY` text + JSON lines: shots, hit %, lock-on, grapple, building,
  deaths by cause per player) to FILE; it is always printed to the console every 60 s and at match end.
- `--team-size N` (e.g. `--team-size 8` for 8v8): implies `--teams` and keeps every team at N players with bots;
  a human joining takes a bot's slot, leaving brings a bot back. Use it instead of `--bots` (max N = 27).
  `TEAM_SIZE=8 ./demo.sh` starts a local 8v8.
- Level `spawn x y z [team]`: team 0/1 = that team's spawn, omitted = either team uses it.
- `--balance PATH`: `key = value` lines (hp, respawn time, damage, match rules). Start from the built-in preset:
  the keys are listed by `Balance::demo().to_text()`; typos are reported with line numbers.

## Network events (protocol v12, for client authors)

Reliable, ordered, acked events ride the snapshot datagrams (`shinobi_sim::events::NetEvent`). Besides the match
flow (`Kill`, `Respawn`, `MatchCountdown`, `MatchStart`, `MatchEnd`, `MapChange`) and pickups:

| Event | Meaning | Client state (`NetClient`) |
|---|---|---|
| `TeamAssign { player, team }` | player joined / moved to team 0 Cyan or 1 Orange; `TEAM_NONE` (3) = left. Replayed to late joiners | `team_of(id)`, `my_team()` |
| `TeamScore { tick, score: [cyan, orange], result }` | team kills whenever they change; `result` 0 running, 1 Cyan won, 2 Orange won, 3 draw, 4 sudden death in progress (only in team mode) | `team_scores()`, `team_result()`, `in_sudden_death()` |
| `RopeAttach { tick, player, anchor }` / `RopeRelease` | a player's wrist grapple hooked (anchor in 1/512 m) / let go, also on death or leaving | `rope_anchor(id)` |
| `BlockPlaced { owner, cell, kind, axis, scale_q }` / `BlockRemoved { cell }` / `BlockDerez { cell }` | player-built piece appeared / vanished / was destroyed and should de-rez (cell = grid cell; kind 0 floor 1 wall 2 ramp; axis = wall normal 0 X 1 Z, or the direction a ramp rises toward 0 +X 1 +Z 2 -X 3 -Z; scale_q in 1/16ths; geometry via `Blocks::slab`) | `blocks()` / `block_pieces()` |
| `BlockDamaged { cell, hp_q }` | a piece is hurt but standing: remaining health in 1/255ths (255 untouched), at most one per cell per frame, replayed to late joiners; client `block_hp_q(cell)` | `block_hp_q()` |
| `MatchRules { frag_limit, team_frag_limit, time_limit_secs }` | match limits, sent to every joiner; client `frag_limit()` / `team_frag_limit()` / `time_limit_secs()` (all `Option`, `None` until catch-up) | `frag_limit()` |
| `BlockRefused { why }` | your build press failed: 0 occupied, 1 body in the way, 2 cooldown (builder only) | via `take_events()` |

Shurikens are snapshot entities with ids `PROJECTILE_ID_BASE` (0x4000) and up: `yaw` = owner id, `flags` = bounce
count (at most 128 replicated). Button edges (dash, jump, slice, zip, build, ultimate) are loss-proof: a client keeps sending the intents that carry
an edge until the server's owner block says every intent up to that tick arrived (`intent_complete`), and the server
applies an edge that arrives after its tick was simulated exactly once on the next tick. The owner block also reports
how much headroom the newest intent had (`lead_margin`); `NetClient` grows / shrinks its intent lead from that, so a
50 ms link with 80 ms of jitter needs no hand-tuned constant.
Hit markers (`OwnExtras.hits`) are resent in 3 consecutive datagrams with a sequence
number; `NetClient` de-duplicates them.

## Troubleshooting

- **Game says "connecting" forever / "client out of date"**: the server and game must be built from the same tree
  (protocol version is checked). Rebuild both.
- **Works on this machine, not from another one**: open **UDP** port 7777 in the server's firewall
  (`sudo ufw allow 7777/udp`) and use the LAN IP from the server banner, not `127.0.0.1`.
- **"server full"**: raise `--max-players` (bots do not count against it).
- **"kicked: packet flood / cheat detection"**: you were banned for 10 s; wait and reconnect.
- **Choppy on a slow machine**: the server stats line shows `step_avg_ms` and `overruns`; debug builds are much slower
  than `--release`.
