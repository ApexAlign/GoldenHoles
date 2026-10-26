# GoldenHoles agent guide

This repository is a deterministic Rust arena game: a Bevy client plus a headless authoritative server and bots.
Read this file before editing. Domain agents exist under `.opencode/agent/`.

## Non-negotiables

- **One shared worktree.** All agents work in `/home/apex/GoldenHoles` on the same sprint branch. Do not create or
  switch worktrees.
- **Workers do not run git write commands.** Only `director` stages, commits, branches, merges, pushes, or creates
  handoffs. Workers may run `git status` and `git diff` read-only unless the director says otherwise.
- **Stay inside your ownership.** Do not edit another agent's files. If a change crosses domains, report it to the
  director instead of making it.
- **No comments in new code** unless the surrounding code uses a comment to explain a non-obvious invariant.
- **Determinism is a product feature.** Server and client prediction must produce the same result. Use `libm` on the
  movement/combat path, fixed-point tick math for obstacles, and keep wire/order changes explicit.
- **Protocol changes need a version bump.** `crates/shinobi-proto/src/datagram.rs` currently has `VERSION: u8 = 35`.
  Any wire change bumps it and updates roundtrip tests and prediction parity.
- **Server authority.** Damage, movement, mods, traps, tethers, triggers and rewards are validated on the server.
- **No secrets in the repo.** Never log or commit keys, tokens or credentials.
- **Do not commit generated screenshots/logs** unless they are deliberate goldens or reviewed reference frames.

## Current content decisions

- Build order: **Circuit-Breaker Arena first**, then **The De-Frag Assembly**, then **Vector Vector**.
- Vector Vector v1 has **no arbitrary gravity or gravity-flip mechanics**. It ships layout, prism ricochet,
  refraction and ring tether/shear as Y-up mechanics. Gravity rings are visual/rotational flavour only.
- Controls keep the current stance switch: `1` katana, `2` bow up. Bow up still disables the shield.
- **There is no loadout.** Every player owns every baseline weapon and every mod from spawn. Mods are selected
  in-match from the arsenal, not before the match. No pickup or lobby gating.
- Arsenal model:
  - Stars: Standard + Glitch mine; `R`/wheel cycles the throwable, `Q` throws. Max 2 mines.
  - Bow: Rail-Arrow + Tether Arrow; while bow is up, the cycle control toggles arrow type; full draw fires.
  - Shield: Aegis block is always active; Battery is passive and fills from blocked stars/parries.
  - Grapple: Vector-Link rope is always active; Vortex Slam is contextual while diving with grind held.
- Accepted engine extensions: level zones/movers/hazards, cylinder/ring authoring primitives, standable moving
  platforms, and runtime arsenal/mod state on the wire. There is no join-time loadout payload.

## Ownership

`director` owns shared integration and git:

```
AGENTS.md  CLAUDE.md  opencode.json  .opencode/**  Cargo.toml  Cargo.lock  README.md  QUICKSTART.md
DESIGN.md  docs/DEMO.md  scripts/check.sh  scripts/agent/**  .github/**
docs/handoffs/**  design/CONTENT_PLAN.md
```

`levelsmith` owns level authoring and tests:

```
levels/**  scripts/gen_*_level.py  scripts/lvl_*.py  scripts/runwalls.py
design/levels/**  docs/LEVELS.md
crates/shinobi-sim/tests/arena_*.rs  walkable.rs  runwalls.rs
```

`combat-smith` owns simulation, protocol, server and mod systems:

```
crates/shinobi-sim/**  crates/shinobi-proto/**  crates/shinobi-server/**  crates/shinobi-ai/**
crates/shinobi-client/src/{netclient,reconcile,input,client_game}.rs
crates/shinobi-client/src/{hud_power,hud_energy,power_view,power_fx,shot}.rs
balance/**  docs/{powerups,stars,melee,movement,grapple,grind,lock-on-and-hud}.md  docs/weapons/**
```

`visual-lead` owns rendering, game shell, FX, HUD art and visual QA:

```
crates/shinobi-game/**  crates/shinobi-client/src/{lib,render,viewmodel,hud*,...}.rs
crates/shinobi-client/src/{sky,grid,env_events,character_*,destructible_*,gore,...}.rs
tests/golden/**  docs/screens/**  docs/ART.md
crates/shinobi-game/scripts/**
```

Shared contract files (`Cargo.toml`, `Cargo.lock`, `opencode.json`, `AGENTS.md`, `CLAUDE.md`, `.github/**`,
`scripts/check.sh`, `docs/handoffs/**`) belong to the director. Ask before touching them.

## Where things live

- `crates/shinobi-sim/` - authoritative simulation: character, combat, hazards, levels, obstacles, env events.
- `crates/shinobi-proto/` - wire types and quantisation; `datagram.rs` owns the version.
- `crates/shinobi-server/` - setup/build, replication, match flow; `setup.rs` loads levels and pickups.
- `crates/shinobi-client/` - prediction, reconciliation, HUD draw lists, input mapping.
- `crates/shinobi-game/` - Bevy app outside the workspace; rendering, FX, screenshots, goldens.
- `crates/shinobi-ai/` and `crates/shinobi-bot/` - bots and scripted clients.
- `levels/*.lvl` - text levels: `box`, `dbox`, `spawn`, `pickup`, `yumi`, `tag`.
- `scripts/gen_*_level.py` - generators; generated levels say so in their header.
- `balance/` - `key = value` tuning overlays.
- `docs/` - mechanics contracts. Update the relevant doc with every behaviour change.

## Commands

Fast gate while iterating:

```bash
scripts/check.sh --fast
```

Full pre-integration gate (workspace + detached Bevy crate):

```bash
scripts/check.sh
```

Game crate only:

```bash
cd crates/shinobi-game && cargo test
```

Headless gameplay smoke (server + AI + scripted clients):

```bash
SECS=30 AI=3 CLIENTS=2 scripts/demo_headless.sh
LEVEL=levels/ffa_circuit.lvl SECS=30 AI=3 CLIENTS=2 scripts/demo_headless.sh
SKIP_PERF=1 scripts/demo_headless.sh
```

Visual goldens (needs software Vulkan / lavapipe; not part of CI):

```bash
crates/shinobi-game/scripts/golden_check.sh
crates/shinobi-game/scripts/golden_check.sh --update
```

Offline screenshot:

```bash
cargo build -p shinobi-game
VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json WGPU_BACKEND=vulkan \
  target/debug/shinobi-game --shot-offline /tmp/frame.ppm --secs 2.0 --level levels/ffa_circuit.lvl --spawn 0
```

Agent helper scripts:

```bash
scripts/agent/heavy.sh <slug>          # logs full checks, smoke and goldens under docs/handoffs/<slug>/logs
scripts/agent/handoff.sh <slug>        # self-contained Claude handoff with patch and evidence
```

## Level format

`crates/shinobi-sim/src/level.rs` is the source of truth. Today:

```text
box    cx cy cz  hx hy hz [rot_z_radians]
dbox   cx cy cz  hx hy hz rot_z hp
spawn  x y z [team]
pickup x y z [respawn_secs]
yumi   x y z [respawn_secs]
tag    x y z [respawn_secs]
```

`box`/`dbox` are cuboids. `rot_z` is the only rotation. Obstacles/movers are runtime data, not level directives
yet. New `zone`, `mover`, `hazard`, `tube` and `ring` directives are a shared contract: director reviews the
parser and wire impact before use.

Level acceptance:

- At most 1800 boxes per level.
- At least 70% of gameplay boxes destructible where the map design calls for destruction; structural primitives
  are excluded and the ratio is rechecked.
- Bricks are 2-6 m; no unintended jump-only lips; spawns/pickups have clearance.
- `walkable`, `arena_<map>` and `runwalls` tests pass.
- A 30 s headless smoke on the map produces fights and no stalls.

## Combat and mod acceptance

Write the spec first in `docs/weapons/<name>.md` or the relevant existing doc. Then:

- Server-authoritative and deterministic; 1000-tick replay produces identical state.
- Wire changes bump `VERSION`, roundtrip tests and prediction parity.
- AI can at least use or tolerate the mechanic without stalling.
- `perf_step` stays inside the existing budget; no per-tick allocation on the hot path.
- Balance keys go through `Balance` and are listed by `Balance::default().to_text()`.
- Tests cover the counterplay explicitly, not only the happy path.

## Visual acceptance

- Art direction is written in `docs/ART.md` before implementation.
- Before/after frames for each material change; readability checked at 5 m, 25 m and 60 m.
- Low/Medium/High tiers show no regression.
- Goldens are re-recorded only after `visual-lead` reviews the images.
- `visual-lead` is the final visual reviewer for both other agents.

## Agent workflow

1. `director` writes a task brief with scope, owned files, acceptance and non-goals.
2. The worker reads `AGENTS.md`, the relevant docs and the task brief before editing.
3. The worker implements, runs its domain tests, and reports: files changed, commands run, results, risks.
4. `director` reviews the diff, runs the integration gate, and commits with a domain prefix:
   `level:`, `combat:`, `visual:`, or `agents:`.
5. If a check cannot be trusted (visual judgment, flaky GPU, subtle gameplay feel), `director` runs
   `scripts/agent/handoff.sh` and hands the package to Claude.

## Definition of done

- The task brief's acceptance items are all checked off.
- `scripts/check.sh --fast` passes for domain work; full `scripts/check.sh` passes before integration.
- New behaviour is documented and tested.
- No cross-domain edits are left uncommitted or unexplained.
- Any unresolved risk is written into the sprint report or handoff.
