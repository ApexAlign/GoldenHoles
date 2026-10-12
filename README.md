# GoldenHoles

**Golden Shinobi** — a neon, Tron-styled cyber-ninja arena shooter. Run, wall-run, slide, double-jump, grapple across the
map, dash-slice with a katana, throw shuriken, and pick up power weapons. Free-for-all by default, optional 8v8 team
deathmatch, and bots to fill the empty slots. Server-authoritative, 60 Hz, over UDP. Written in Rust (Bevy for the game window).

> ## ⚠️ Early and moving fast
> This project is **under active development and changes constantly**. Expect **rolling updates**:
> new **maps**, new **weapon systems**, new **gameplay** mechanics and modes, balance changes, and the occasional broken build.
>
> - **Everyone in a match must be on the same version.** Network protocol changes often; if you see *"client out of date"*,
>   run `git pull` and rebuild (instructions below). Always `git pull` before playing with friends.
> - Levels, controls and settings may change between updates. Check this README and `docs/` after pulling.
> - Found a bug or have an idea? Open an issue.

---

## Contents

1. [What you need](#1-what-you-need)
2. [Install: Windows](#2-install-windows)
3. [Install: Linux](#3-install-linux)
4. [Install: macOS](#4-install-macos)
5. [Play solo (local game vs bots)](#5-play-solo-local-game-vs-bots)
6. [Multiplayer with friends](#6-multiplayer-with-friends)
7. [Controls](#7-controls)
8. [Maps and game modes](#8-maps-and-game-modes)
9. [Weapons and power-ups](#9-weapons-and-power-ups)
10. [Custom character](#10-custom-character)
11. [Updating](#11-updating)
12. [Troubleshooting](#12-troubleshooting)
13. [More documentation](#13-more-documentation)

---

## 1. What you need

- **Rust** (stable toolchain) — installed from <https://rustup.rs>
- **Git**
- A GPU with **Vulkan** (Linux/Windows) or **Metal** (macOS) support
- About **10 GB** of free disk space (the build cache is large) and a few minutes for the **first build** — it compiles
  several hundred crates (roughly 10 minutes; later builds are incremental and fast)

The project has two parts you will run:

| Part | Crate | What it is |
|---|---|---|
| **Server** | `shinobi-server` | Runs the match: physics, hits, bots, scoring. One per game. |
| **Game** | `shinobi-game` | The window you play in. Connects to a server. |

Even solo play uses a (local) server — the helper scripts start it for you.

---

## 2. Install: Windows

1. **Install Git for Windows:** <https://git-scm.com/download/win>
2. **Install the C++ build tools:** download *Visual Studio Build Tools* from
   <https://visualstudio.microsoft.com/visual-cpp-build-tools/> and tick **"Desktop development with C++"**.
3. **Install Rust:** download and run `rustup-init.exe` from <https://rustup.rs> (accept the defaults).
4. Open a **new** PowerShell window and get the code:

```powershell
git clone https://github.com/ApexAlign/GoldenHoles.git
cd GoldenHoles
```

5. Build everything once (this is the slow step):

```powershell
cargo build --release -p shinobi-server
cd crates\shinobi-game
cargo build --release
cd ..\..
```

You now have `target\release\shinobi-server.exe` and `crates\shinobi-game\target\release\shinobi-game.exe`.
Keep PowerShell in the repo folder (`GoldenHoles`) for the commands below — the game and server look for `levels\` there.

> The `.sh` helper scripts (`demo.sh`, `scripts/*.sh`) need bash. On Windows either use the plain commands in this README or run them from
> **Git Bash** / **WSL**.

---

## 3. Install: Linux

**Debian / Ubuntu:**

```bash
sudo apt update
sudo apt install -y build-essential pkg-config git curl \
    libasound2-dev libudev-dev libxkbcommon-x11-0 libwayland-dev \
    libx11-dev libxcursor-dev libxi-dev libxrandr-dev \
    mesa-vulkan-drivers vulkan-tools
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh     # accept defaults, then restart the shell
```

(Fedora: `sudo dnf install gcc pkgconf-pkg-config alsa-lib-devel systemd-devel libxkbcommon-x11 wayland-devel mesa-vulkan-drivers`;
Arch: `sudo pacman -S base-devel alsa-lib vulkan-icd-loader`. NVIDIA users: install the proprietary driver for your card.)

Then:

```bash
git clone https://github.com/ApexAlign/GoldenHoles.git
cd GoldenHoles
cargo build --release -p shinobi-server
(cd crates/shinobi-game && cargo build --release)
```

Sound is on by default (procedural synth, no asset files) and needs the ALSA headers above to *build*. No audio device at runtime
just means silence, not a crash. For a silent build: `cargo build --release --no-default-features --features wayland` inside `crates/shinobi-game`.

---

## 4. Install: macOS

```bash
xcode-select --install                                            # compiler tools
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh    # Rust; accept defaults, restart the terminal
git clone https://github.com/ApexAlign/GoldenHoles.git
cd GoldenHoles
cargo build --release -p shinobi-server
(cd crates/shinobi-game && cargo build --release)
```

> macOS is less tested than Linux/Windows. If something is off, please report it with your Mac model and macOS version.

---

## 5. Play solo (local game vs bots)

### Easiest (Linux / macOS / Git Bash / WSL)

```bash
./demo.sh                 # builds, starts a local server with 5 bots, opens the game
BOTS=3 ./demo.sh          # fewer opponents (BOTS=0 for none)
MAP=small ./demo.sh       # pick a map: sky | isles | runs | small | big | xl
TEAM_SIZE=8 ./demo.sh     # 8v8 team deathmatch with bots filling empty slots
```

Close the window or press Ctrl-C to quit; the server is stopped for you.

### Manual (any OS, two terminals, both in the repo folder)

**Terminal 1 — server** (free-for-all with 7 bots):

```bash
# Linux / macOS
target/release/shinobi-server --bind 127.0.0.1 --level levels/ffa_small.lvl --bots 7
```
```powershell
# Windows PowerShell
.\target\release\shinobi-server.exe --bind 127.0.0.1 --level levels\ffa_small.lvl --bots 7
```

**Terminal 2 — game:**

```bash
# Linux / macOS
crates/shinobi-game/target/release/shinobi-game --server 127.0.0.1:7777 --level levels/ffa_small.lvl
```
```powershell
# Windows PowerShell
.\crates\shinobi-game\target\release\shinobi-game.exe --server 127.0.0.1:7777 --level levels\ffa_small.lvl
```

> **The server and every game window must load the same `--level`**, or movement prediction desyncs and the game feels broken.
> Always pass `--level` to both, with the same file.

Click the game window to grab the mouse. **Esc** releases it and opens the pause screen.

---

## 6. Multiplayer with friends

One person **hosts** (runs the server); everyone else **joins**. The host can also play. Up to ~16 players per server is
the tested range (bots count toward that; see [Troubleshooting](#12-troubleshooting)).

**Before you start, everyone must:**
1. `git pull` and rebuild so you're all on the **same version** (the server rejects mismatched clients).
2. Agree on **one map** (`--level ...`) — see [Maps](#8-maps-and-game-modes).

### Step 1 — Host: start the server

Pick **one** of these. `--bind 0.0.0.0` means "accept connections from other computers". `--bots 0` = humans only.

```bash
# Linux / macOS (easiest: builds, starts the server, prints your LAN IP, opens your game)
HOST=1 BOTS=0 scripts/play.sh

# Linux / macOS / Git Bash (manual)
target/release/shinobi-server --bind 0.0.0.0 --level levels/ffa_small.lvl --bots 0
```
```powershell
# Windows PowerShell (manual)
.\target\release\shinobi-server.exe --bind 0.0.0.0 --level levels\ffa_small.lvl --bots 0
```

The server prints a banner with your address. The default port is **UDP 7777** (`--port N` to change it).
Windows will pop up a firewall prompt the first time — **allow it on Private networks**.

Want bots too? Use `--bots 5` (or any number). Want teams? Add `--team-size 8 --level levels/8v8.lvl` instead of `--bots`.

### Step 2 — Host: let people in

#### Same home network / LAN (easiest)

Friends only need your **LAN IP** (the banner shows it, e.g. `192.168.1.20`). If they can't connect, your firewall is blocking
UDP 7777:

```bash
sudo ufw allow 7777/udp                                                    # Ubuntu
sudo firewall-cmd --add-port=7777/udp --permanent && sudo firewall-cmd --reload   # Fedora
```
```powershell
# Windows, run PowerShell as Administrator
New-NetFirewallRule -DisplayName "GoldenHoles" -Direction Inbound -Protocol UDP -LocalPort 7777 -Action Allow
```

#### Over the internet

Pick **one**:

1. **Port forwarding:** in your router, forward **UDP 7777** to the host machine's LAN IP (give it a fixed IP / DHCP
   reservation first). Friends join your **public** IP (search "what is my IP").
2. **Tailscale or ZeroTier (recommended — no router setup):** everyone installs the same free mesh VPN and joins the same
   network. Friends then connect to the host's VPN address exactly like a LAN. This also works when your ISP uses
   carrier-grade NAT (where port forwarding is impossible).
3. **A small VPS:** run the server on a cheap cloud machine with UDP 7777 open and have everyone join its address.

Playing well needs roughly **under 80 ms ping** to the server.

### Step 3 — Everyone else: join

Replace `HOST_IP` with the address the host gives you.

```bash
# Linux / macOS (builds the game if needed, then connects)
scripts/join.sh HOST_IP                  # default port 7777
scripts/join.sh HOST_IP 7777             # explicit port
LEVEL=levels/ffa_small.lvl scripts/join.sh HOST_IP    # when the host picked a specific map
```
```bash
# Linux / macOS / Git Bash (manual)
crates/shinobi-game/target/release/shinobi-game --server HOST_IP:7777 --level levels/ffa_small.lvl
```
```powershell
# Windows PowerShell (manual)
.\crates\shinobi-game\target\release\shinobi-game.exe --server HOST_IP:7777 --level levels\ffa_small.lvl
```

Click the window to grab the mouse and you're in. A match starts after a short warm-up (one player is enough); first to the
frag limit or the time limit wins, then there's an intermission and the next round.

### Team deathmatch with friends (optional)

Host:
```bash
target/release/shinobi-server --bind 0.0.0.0 --level levels/8v8.lvl --team-size 8
```
Everyone joins with `--level levels/8v8.lvl`. Players are placed on the smaller team (Cyan vs Orange); bots fill the empty slots, and
a human joining takes a bot's place. First team to 50 team kills wins. Shurikens and the katana pass through teammates.

### Handy host options

```
--bind ADDR         0.0.0.0 = reachable from the network (127.0.0.1 = this PC only)
--port N            UDP port (default 7777)
--level PATH        map file — MUST match on every client
--bots N            AI opponents (0 = none)
--bot-skill easy|normal|hard
--max-players N     raise if you see "server full"
--teams / --team-size N   team deathmatch
--obstacles         waves of moving neon obstacles that hurt on contact
--balance PATH      tweak hp, damage, respawn time, match rules (key = value file)
```

Full list in [`QUICKSTART.md`](QUICKSTART.md); online play notes in [`docs/ONLINE.md`](docs/ONLINE.md).

---

## 7. Controls

| Input | Action |
|---|---|
| Click window | grab the mouse (Esc releases it and opens the pause screen) |
| W A S D, mouse | move, aim |
| Space | jump / double-jump |
| Shift | dash |
| Q | roll |
| C | slide |
| W into a wall (hold Space or W) | wall-cling / wall-run |
| Left click | katana dash-slice |
| Right click | throw the selected star (standard or flash; 14 m close-range tool) |
| 1 / 2 | katana / yumi bow up (bow up: hold left click to draw, release to loose; 2.2 s reload) |
| E | wrist grapple: hook the crosshair and reel in; E again releases |
| Tab (hold) | scoreboard (frees the cursor) |
| `` ` `` (backquote) | in-game console |

Controls will change as new systems land — check the in-game help and the changelog in the commit history after updates.

---

## 8. Maps and game modes

Maps live in `levels/`. Pass the **same** file to the server and every client with `--level`.

| File | What it is |
|---|---|
| `levels/ffa_sky.lvl` | the **sky city**: a floating, vertical, four-tier world (default of `demo.sh`) |
| `levels/ffa_small.lvl` | 88 m ground arena — good for 2–6 players |
| `levels/ffa.lvl` | 224 m big arena |
| `levels/ffa_xl.lvl` | extra-large arena for bigger lobbies |
| `levels/8v8.lvl` | team deathmatch map (Cyan vs Orange) |

**Modes:** free-for-all (default; every player has their own neon colour) and team deathmatch (`--team-size 8` / `--teams`).
Default rules: 100 hp, respawn after 3 s with brief spawn protection, katana kills in 2 hits, first to 10 kills or 6 minutes
wins. Falling off the map kills you. Glowing grid-beam cubes are pickups and respawn after 20 s.

**More maps, modes and mechanics are coming.** Maps are plain text (`box`, `spawn`, `pickup` lines), so you can make your own —
see `scripts/gen_*_level.py` for generators.

---

## 9. Weapons and power-ups

- **Katana** — dash-slice (left click). Two hits kill.
- **Shuriken** — thrown stars (right click); they bounce. A close-range tool: 14 m max, full damage inside 7 m. Standard or flash star only.
- **Wrist grapple** (E) — swing and reel in; combos with dash and wall-run.
- **Block / parry shield** — time it right to stop damage and punish attackers.
- **Yumi (digital bow)** — everyone's primary weapon. Press 2, hold left click to draw, release a full draw to loose; a headshot kills; 2.2 s reload. Can be parried back at the shooter.
- **Fuda tag (explosive paper tag)** — sticks to surfaces *or players* and detonates with a launching blast.

Details and tuning values: [`docs/powerups.md`](docs/powerups.md), [`docs/melee.md`](docs/melee.md), [`docs/grapple.md`](docs/grapple.md),
[`docs/movement.md`](docs/movement.md). **More weapon systems are on the way** and existing ones will be rebalanced.

---

## 10. Custom character

You can play as your own look. Give an AI chatbot a photo plus the prompt in [`docs/CHARACTER_SPEC.md`](docs/CHARACTER_SPEC.md);
it returns one small JSON file. Save it as `characters/me.json` (see `characters/example_kitsune.json`), then:

```bash
crates/shinobi-game/target/release/shinobi-game --server HOST_IP:7777 --level levels/ffa_small.lvl --character characters/me.json
```

Or type `/character characters/me.json` in the in-game console (`/character off` returns to the stock ninja). Everyone in the match
sees your look; hitbox, speed and weapons are identical for all characters.

---

## 11. Updating

Because things change often, update before every play session — and **make sure your friends do too**:

```bash
git pull
cargo build --release -p shinobi-server
(cd crates/shinobi-game && cargo build --release)
```

If the build fails after a pull, try `cargo clean` in the repo root and in `crates/shinobi-game`, then rebuild. If you
see *"client out of date"* when joining, you and the host are on different commits.

---

## 12. Troubleshooting

| Problem | Fix |
|---|---|
| Stuck on "connecting" / *"client out of date"* | Host and players must be on the **same commit**. `git pull` + rebuild both. |
| Works locally, friends can't connect | Open **UDP** (not TCP) port 7777 in the host's firewall; use the host's LAN/public IP, not `127.0.0.1`; for internet play forward the port or use Tailscale/ZeroTier. |
| Movement feels rubber-banded / wrong | Server and client loaded **different levels**. Pass the same `--level` to both. |
| *"server full"* | Host raises `--max-players`. Bots don't count against it. |
| *"kicked: packet flood / cheat detection"* | Temporary 10 s ban; wait and reconnect. |
| Very low FPS | Use a `--release` build (debug is ~10 fps). Hybrid-GPU laptop? Set `SHINOBI_GPU=dgpu`. NVIDIA + Wayland vsync issue? Set `SHINOBI_PRESENT=immediate`. |
| Mouse won't capture | Click the window once. Alt-Tab releases it; click again. |
| Linux build error about `alsa` / `libudev` | Install the dev packages listed in [Install: Linux](#3-install-linux). |
| Windows: `link.exe` not found | Install "Desktop development with C++" (Visual Studio Build Tools) and open a new terminal. |
| No sound | Linux needs ALSA at build time; no output device = silent by design. See [`docs/AUDIO.md`](docs/AUDIO.md). |

When reporting a bug, include the logs from the `playtest/` folder (`server-*.log`, `game-*.log`, `telemetry-*.txt`).

---

## 13. More documentation

- [`QUICKSTART.md`](QUICKSTART.md) — compact command reference and every server option
- [`DESIGN.md`](DESIGN.md) — design overview
- [`docs/ONLINE.md`](docs/ONLINE.md) — LAN / internet play details
- [`docs/PLAYTEST.md`](docs/PLAYTEST.md) — playtesting and the lag-proxy tool
- [`docs/`](docs/) — movement, melee, grapple, power-ups, audio, character spec, and more

## License

No license has been chosen yet (all rights reserved by default). Add a `LICENSE` file before accepting outside contributions.
