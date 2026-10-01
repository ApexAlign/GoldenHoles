# Shinobi quickstart

Neon cyber-ninja arena shooter: run, wall-run, dash-slice and throw shuriken in a Tron-styled arena.
Server-authoritative, 60 Hz, UDP.

## Three commands

```bash
# 1. server   (UDP port 7777; add AI opponents with --bots)
cargo run --release --offline -p shinobi-server -- --bind 0.0.0.0 --bots 3

# 2. game     (a window; on another machine use the LAN address the server banner prints)
cd crates/shinobi-game && cargo run --offline -- --server 127.0.0.1:7777

# 3. extra scripted players (optional load / company; no window)
cargo run --release --offline -p shinobi-bot -- --addr 127.0.0.1:7777 --count 4 --secs 120
```

One-liner for a local match: `BOTS=3 ./demo.sh` (builds, starts the server, opens the window).
No window available? `scripts/demo_headless.sh` runs server + AI + scripted clients for 60 s and prints a health report.

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

## What to expect

- A match starts after a short warmup (one player is enough). Default rules (the demo preset): 100 hp,
  respawn in 3 s with brief spawn protection, katana kills in 2 hits, first to 10 kills or 5 minutes wins,
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
- `--team-size N` (e.g. `--team-size 8` for 8v8): implies `--teams` and keeps every team at N players with bots;
  a human joining takes a bot's slot, leaving brings a bot back. Use it instead of `--bots` (max N = 27).
  `TEAM_SIZE=8 ./demo.sh` starts a local 8v8.
- Level `spawn x y z [team]`: team 0/1 = that team's spawn, omitted = either team uses it.
- `--balance PATH`: `key = value` lines (hp, respawn time, damage, match rules). Start from the built-in preset:
  the keys are listed by `Balance::demo().to_text()`; typos are reported with line numbers.

## Network events (protocol v6, for client authors)

Reliable, ordered, acked events ride the snapshot datagrams (`shinobi_sim::events::NetEvent`). Besides the match
flow (`Kill`, `Respawn`, `MatchCountdown`, `MatchStart`, `MatchEnd`, `MapChange`) and pickups:

| Event | Meaning | Client state (`NetClient`) |
|---|---|---|
| `TeamAssign { player, team }` | player joined / moved to team 0 Cyan or 1 Orange; `TEAM_NONE` (3) = left. Replayed to late joiners | `team_of(id)`, `my_team()` |
| `TeamScore { tick, score: [cyan, orange], result }` | team kills whenever they change; `result` 0 running, 1 Cyan won, 2 Orange won, 3 draw (only in team mode) | `team_scores()`, `team_result()` |
| `RopeAttach { tick, player, anchor }` / `RopeRelease` | a player's wrist grapple hooked (anchor in 1/512 m) / let go, also on death or leaving | `rope_anchor(id)` |
| `BlockPlaced { owner, cell }` / `BlockRemoved { cell }` | player-built block appeared / vanished (cell = grid cell, centre = (cell + 0.5) x size) | `blocks()` |
| `BlockRefused { why }` | your build press failed: 0 occupied, 1 body in the way, 2 cooldown (builder only) | via `take_events()` |

Shurikens are snapshot entities with ids `PROJECTILE_ID_BASE` (0x4000) and up: `yaw` = owner id, `flags` = bounce
count (at most 128 replicated). Hit markers (`OwnExtras.hits`) are resent in 3 consecutive datagrams with a sequence
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
