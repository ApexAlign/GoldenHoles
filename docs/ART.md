# GoldenHoles art direction

This is the visual contract for `visual-lead`. Read `AGENTS.md` first. Update this document before a material visual
change, and attach before/after frames when handing work off.

## Language

Neon ninja arena: dark structural mass, saturated emissive edges, CRT/scanline atmosphere, readable silhouettes over
ornament. Gameplay information always wins over decoration. Red is danger, cyan/blue is traversal and energy, orange
is a lethal sweep, magenta is glitch/corruption.

## Tiers

| Tier | Intent | Must keep |
| --- | --- | --- |
| High | Full bloom, CRT, particles, trails | All gameplay cues readable at 60 m |
| Medium | Reduced bloom, fewer particles | Lane, hazard and tether cues still distinct |
| Low | No bloom/CRT, flat emissive, reduced FX | Geometry, hazard telegraphs and HUD legibility |

Low tier is the floor, not an afterthought. Check every map and HUD state on Low before calling a visual pass done.

## Map art

### Circuit-Breaker Arena

- Motherboard stadium: dark PCB deck (`[0.02, 0.03, 0.06]` near-black), copper traces (`[0.85, 0.5, 0.15]`), cyan data lanes (`[0.15, 0.8, 1.0]`), white component silkscreen (`[0.9, 0.95, 1.0]`).
- Central CPU-fan nexus: layered circular heatsink silhouettes, upper wireframe ring platforms, sunken lower pit.
- Kinetic solenoids: chunky copper coils with a clear charging glow; the piston face reads as a hammer, not a wall. Piston material is dark forged copper (`[0.55, 0.28, 0.12]`); charge colour drives the emissive from copper to hot orange/white (`[1.0, 0.9, 0.3]`) over the first half of the cycle and fades back during retract. For a 180-tick (3 s) piston the charge ramps over 90 ticks and peaks at the slam — readable charge, not a surprise hit.
- Core vent: blue plasma column (`[0.2, 0.45, 1.0]`), upward motes, strong vertical silhouette; visible from the far lanes.
- Glitch traps: magenta wire cluster with a short arc; never confuse with a pickup.
- Feature rendering: zones, movers and hazards are unlit translucent cuboids drawn from `LevelFeatures` (not from simulation state). On Low tier they stay flat and readable without bloom or CRT.
- Readability: lane direction, piston charge, vent active state and pit edge at 5/25/60 m.

### The De-Frag Assembly

- Vertical chasm: near-black depth fog, distant grid fragments, no visible bottom.
- Velocity cylinders: hollow glass with copper stripping; the interior wall-run line must read through the glass.
- Grid conveyors: dark panels with bright directional chevrons that animate in the travel direction.
- Data-purge sweeper: orange plane with a hard leading edge and a descending countdown band; safe pockets are visibly
  darker and calmer.
- Overload pillars: copper/steel columns with intermittent blue arcs; a charged state is unmistakable.
- Readability: next route, cylinder entry, conveyor direction and sweeper gap at 5/25/60 m.

### Vector Vector

- Interlocking rings and floating neon cubes over a dark void; prism obsidian with sharp mirror highlights.
- Cyan refraction panels with a visible grid; the split-arrow lane must be readable without UI.
- Gravity rings rotate slowly as visual flavour only. Do not add a camera roll, inverted horizon or gravity-flip FX.
- Ring tether: a taut bright line that snaps/shears with sparks; no fake gravity wells.
- Readability: ring route, prism face, refraction panel and tether anchor at 5/25/60 m.

## HUD

- Arsenal strip: stars (Standard, Glitch), arrows (Rail, Tether), shield battery, dive/vortex state. No loadout.
- Shield battery: segmented meter that fills from blocks/parries; at full, a bright aura and a short "AURA" cue.
- Glitch debuff: 2 s chromatic offset/inverted palette, but never hide the crosshair or the health value.
- Tether: anchor pip on the victim, line state, cut cue.
- Vortex: dive direction and impact band; head-hit and near-miss must look different.
- Keep the controls card and existing neon/CRT language. Do not add a pre-match loadout screen.

## Technical rules

- Do not change simulation, protocol or server state to make a frame.
- Use existing systems: `GridData`, shaders, destructible FX, HUD draw lists, env events.
- New primitives must be authored in the level contract; rendering reads the same data as collision.
- No per-frame allocation in hot render paths; reuse materials/meshes where possible.
- Generated screenshots and logs are not committed unless they are reviewed goldens or reference frames.

## Verification

```bash
cd crates/shinobi-game && cargo test
crates/shinobi-game/scripts/golden_check.sh
VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json WGPU_BACKEND=vulkan \
  target/debug/shinobi-game --shot-offline /tmp/frame.ppm --secs 2.0 --level levels/ffa_circuit.lvl --spawn 0
```

Capture:
- 5 m, 25 m and 60 m readability frames.
- Low/Medium/High tier variants.
- A clean fight frame and an FX frame for hazards, tether, battery and vortex.
- A before/after pair for every material change.

Goldens are re-recorded only after `visual-lead` reviews every changed image.
