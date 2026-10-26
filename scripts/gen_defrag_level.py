#!/usr/bin/env python3
"""Generates levels/ffa_defrag.lvl: THE DE-FRAG ASSEMBLY, the Phase 2 blockout plus its feature layer.

    python3 scripts/gen_defrag_level.py [out.lvl]

A bottomless 120 x 120 m vertical sandbox (no ground; the kill plane is below everything). Three layers of central
20 x 20 m platforms (tops y = 5 / 25 / 50). Four glass velocity tubes on the axes (centres at +-42 m, inner radius
10 m, 60 m tall) with one door per layer facing the centre and a catch floor at the base. The top and low layers
reach the tube doors over fixed bridges; the mid layer reaches them on four 12 x 12 m conveyor panels (solid
movers, riders are carried). Every 45 s the data-purge sweeper (a full-footprint hazard plane) drops from y = 55 to
y = 5 over 8 s; anyone with static geometry overhead is sheltered: the low platform under the mid platform, and the
drop ledges under the mid platform's edges. Six overload pillars stand on the mid and low platforms (geometry plus
`overload_pillar` zones; the Tether discharge runtime is not built yet).

Tests: crates/shinobi-sim/tests/arena_defrag.rs
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lvl_lib import Level  # noqa: E402

LOW, MID, TOP = 5.0, 25.0, 50.0
PLAT = 10.0                 # central platform half extent
TUBE_C = 42.0               # tube centre distance from the origin
TUBE_R = 10.0               # inner radius
TUBE_T = 0.5
TUBE_SEG = 24
DOOR = 3                    # segments left out per door (~8 m gap)
BRIDGE_HALF = 2.0           # bridges are 4 m wide
LEDGE_IN = TUBE_C - TUBE_R + 4.0   # bridges / ledges run 4 m into the tube
PANEL = 6.0                 # conveyor panel half extent (12 x 12 m)
PANEL_NEAR = PLAT + PANEL   # panel centre at the platform edge
PANEL_STROKE = 9.0
PANEL_LEG = 180             # 3 s per leg

L = Level("SHINOBI FFA DEFRAG - THE DE-FRAG ASSEMBLY")

# (tube centre x, z, door start segment): doors face the origin (segment i points at i * 15 degrees)
TUBES = [(TUBE_C, 0.0, 11), (-TUBE_C, 0.0, 23), (0.0, TUBE_C, 17), (0.0, -TUBE_C, 5)]

# ------------------------------------------------------------------------------------------------------------------ layers
L.section("structural: central platforms (tops y = 5 / 25 / 50)")
for top in (LOW, MID, TOP):
    L.perm(0, top - 0.5, 0, PLAT, 0.5, PLAT)

L.section("structural: fixed bridges into the tube doors on the top and low layers")
for top in (LOW, TOP):
    mid = (PLAT + LEDGE_IN) / 2.0
    half = (LEDGE_IN - PLAT) / 2.0
    L.perm(mid, top - 0.5, 0, half, 0.5, BRIDGE_HALF)
    L.perm(-mid, top - 0.5, 0, half, 0.5, BRIDGE_HALF)
    L.perm(0, top - 0.5, mid, BRIDGE_HALF, 0.5, half)
    L.perm(0, top - 0.5, -mid, BRIDGE_HALF, 0.5, half)

L.section("structural: mid-layer door ledges (the conveyors ferry players to them)")
ledge_out = PANEL_NEAR + PANEL_STROKE + PANEL - 0.5
lmid = (ledge_out + LEDGE_IN) / 2.0
lhalf = (LEDGE_IN - ledge_out) / 2.0
L.perm(lmid, MID - 0.5, 0, lhalf, 0.5, 3.0)
L.perm(-lmid, MID - 0.5, 0, lhalf, 0.5, 3.0)
L.perm(0, MID - 0.5, lmid, 3.0, 0.5, lhalf)
L.perm(0, MID - 0.5, -lmid, 3.0, 0.5, lhalf)

L.section("structural: drop ledges under the mid platform's edges (data-purge safe pockets)")
SAFE_TOP = MID - 4.0
L.perm(0, SAFE_TOP - 0.25, PLAT - 0.5, 8.0, 0.25, 3.5)
L.perm(0, SAFE_TOP - 0.25, -(PLAT - 0.5), 8.0, 0.25, 3.5)
L.perm(PLAT - 0.5, SAFE_TOP - 0.25, 0, 3.5, 0.25, 5.0)
L.perm(-(PLAT - 0.5), SAFE_TOP - 0.25, 0, 3.5, 0.25, 5.0)

L.section("structural: tube catch floors at the base (y = 0)")
for x, z, _ in TUBES:
    L.perm(x, -0.5, z, 7.0, 0.5, 7.0)

L.section("structural: overload pillars (3 on the mid platform, 3 on the low platform)")
PILLARS = [(7.0, MID, 7.0), (-7.0, MID, -7.0), (7.0, MID, -7.0), (-7.0, LOW, 7.0), (7.0, LOW, 7.0), (-7.0, LOW, -7.0)]
for x, top, z in PILLARS:
    L.perm(x, top + 1.5, z, 0.75, 1.5, 0.75)

# ------------------------------------------------------------------------------------------------------------------ spawns / pickups
for x, z in ((0.0, 0.0), (6.0, 6.0), (-6.0, 6.0), (6.0, -6.0), (-6.0, -6.0)):
    L.spawn(x, TOP + 0.81, z)
for x, z in ((0.0, 0.0), (5.0, 0.0), (-5.0, 0.0), (0.0, 5.0), (0.0, -5.0)):
    L.spawn(x, MID + 0.81, z)
L.pickup(8.0, TOP + 1.0, 0.0, 25)
L.pickup(-8.0, TOP + 1.0, 0.0, 25)
L.pickup(0.0, MID + 1.0, 8.0, 25)
L.pickup(0.0, MID + 1.0, -8.0, 25)

# ------------------------------------------------------------------------------------------------------------------ features
# Tubes: one 10 m section per layer with a centre-facing door, closed sections between them. Bases at y = 0.
SECTIONS = [(0.0, 10.0, True), (10.0, 20.0, False), (20.0, 30.0, True), (30.0, 45.0, False), (45.0, 60.0, True)]
for x, z, door in TUBES:
    for y0, y1, has_door in SECTIONS:
        tuning = dict(door_start=door, door_count=DOOR) if has_door else {}
        L.tube(x, (y0 + y1) / 2.0, z, TUBE_R, y1 - y0, TUBE_T, TUBE_SEG, "velocity_tube", **tuning)

# Mid-layer conveyors: each panel shuttles from the platform edge to the door ledge (9 m, 3 s per leg).
L.mover(PANEL_NEAR, MID - 0.5, 0, PANEL, 0.5, PANEL, "x", PANEL_STROKE, PANEL_LEG, "conveyor", solid=True)
L.mover(-(PANEL_NEAR + PANEL_STROKE), MID - 0.5, 0, PANEL, 0.5, PANEL, "x", PANEL_STROKE, PANEL_LEG, "conveyor", solid=True)
L.mover(0, MID - 0.5, PANEL_NEAR, PANEL, 0.5, PANEL, "z", PANEL_STROKE, PANEL_LEG, "conveyor", solid=True)
L.mover(0, MID - 0.5, -(PANEL_NEAR + PANEL_STROKE), PANEL, 0.5, PANEL, "z", PANEL_STROKE, PANEL_LEG, "conveyor", solid=True)

# Data purge: a 45 s cycle; 8 s (480 ticks) of descent from y = 55 to y = 5, 3 s (180 ticks) of telegraph before it.
# The phase puts the first descent at 37 s, so nobody spawns into it.
PURGE_PERIOD, PURGE_ACTIVE, PURGE_TELE = 2700, 480, 180
L.hazard(0.0, 55.0, 0.0, 60.0, 0.5, 60.0, PURGE_PERIOD, PURGE_TELE, 1000, "data_purge",
         active_ticks=PURGE_ACTIVE, sweep_y=50, shelter=1, phase_ticks=PURGE_PERIOD - PURGE_ACTIVE)

for x, top, z in PILLARS:
    L.zone(x, top + 1.5, z, 2.5, 1.5, 2.5, "overload_pillar", charge_ticks=480)

out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "levels", "ffa_defrag.lvl")
L.write(out, header=[
    "GENERATED by scripts/gen_defrag_level.py. Edit the script, not this file; tests: crates/shinobi-sim/tests/arena_defrag.rs",
    "THE DE-FRAG ASSEMBLY, Phase 2 blockout. Bottomless 120 x 120 m; central platforms at y = 5 / 25 / 50; four velocity",
    "tubes (r 10 m, 60 m tall, a door per layer); bridges on the top/low layers, conveyors on the mid layer; a 45 s",
    "data-purge sweeper (y 55 -> 5 over 8 s, sheltered players survive); six overload pillars; 10 spawns; 4 data cubes.",
])
