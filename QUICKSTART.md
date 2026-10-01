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

`shinobi-server [BIND] [--bind ADDR] [--port N] [--max-players N] [--bots N] [--bot-skill easy|normal|hard]
[--level PATH] [--balance PATH] [--obstacles] [--seed N] [--stats-secs S] [--idle-ms N]` — flags override `SHINOBI_*` environment
variables (`SHINOBI_BOTS`, `SHINOBI_PORT`, ...). `--bind 0.0.0.0` means "reachable from the network" and the port
defaults to 7777. Humans get ids 1..=200 (max 200 players), bots 201..=255 (max 55).

- `--obstacles`: opt in to waves of moving neon obstacles that hurt on contact (off by default).
- `--level PATH`: text file, one directive per line (`box cx cy cz hx hy hz [roll]`, `spawn x y z`,
  `pickup x y z [respawn_secs]`). Default: `levels/arena.lvl` when present, else the built-in arena.
- `--balance PATH`: `key = value` lines (hp, respawn time, damage, match rules). Start from the built-in preset:
  the keys are listed by `Balance::demo().to_text()`; typos are reported with line numbers.

## Troubleshooting

- **Game says "connecting" forever / "client out of date"**: the server and game must be built from the same tree
  (protocol version is checked). Rebuild both.
- **Works on this machine, not from another one**: open **UDP** port 7777 in the server's firewall
  (`sudo ufw allow 7777/udp`) and use the LAN IP from the server banner, not `127.0.0.1`.
- **"server full"**: raise `--max-players` (bots do not count against it).
- **"kicked: packet flood / cheat detection"**: you were banned for 10 s; wait and reconnect.
- **Choppy on a slow machine**: the server stats line shows `step_avg_ms` and `overruns`; debug builds are much slower
  than `--release`.
