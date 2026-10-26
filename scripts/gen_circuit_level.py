#!/usr/bin/env python3
"""Generates levels/ffa_circuit.lvl: CIRCUIT-BREAKER ARENA, the Phase 1 blockout plus its zone/mover layer.

    python3 scripts/gen_circuit_level.py [out.lvl]

The level is a 100 x 100 m motherboard stadium (deck top y = 0, void beyond the perimeter wall), a 30 x 30 m
two-storey nexus (pit floor y = -6, ground ring y = 0, upper ring top y = 9 with four segments and 2 m corner
gaps, fan column to y = 14), four 10 m wide cardinal data lanes with 8 m wall-run faces, a 7 m wide outer spawn
ring, and six data-cube pickups. Structural geometry is permanent (`box`); the breakable data racks are `dbox`.
The destructible-share test only counts the boxes under `# --- gameplay:` section headers.

The feature layer is authored at the bottom: four piston pairs (8 solid `mover` faces on a 3 s cycle), four
data-lane `speed_lane` zones and one `core_vent` zone. There are no `hazard` lines yet: glitch-trap and vent
damage behaviour belongs to the next combat/runtime slice. Their volumes are reserved and listed in the file.

Tests: crates/shinobi-sim/tests/arena_circuit.rs, walkability in crates/shinobi-sim/tests/walkable.rs
"""
import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lvl_lib import Level  # noqa: E402

HALF = 50.0             # deck half extent (100 x 100 m)
PIT = 8.0               # lower pit half extent (16 x 16 m)
PIT_FLOOR = -6.0
NEXUS = 15.0            # ground-ring half extent (30 x 30 m)
UPPER = 9.0             # upper-ring top
RING_INNER = 7.0        # upper-ring inner edge
RING_OUTER = 10.0       # upper-ring outer edge
WALL = 41.0             # spawn-ring inner wall face
LANE_IN = 13.0          # lane starts at the nexus mouth
LANE_OUT = WALL         # lane ends at the spawn ring
LANE_HALF = 5.0         # 10 m lane interior
WALL_TOP = 8.0          # wall-run faces are 8 m tall
SPAWN_R = 45.5          # outer spawn ring, 3.5 m from each wall face

L = Level("SHINOBI FFA CIRCUIT - CIRCUIT-BREAKER ARENA")


def ramp_x(xa, ya, xb, yb, zc, hz, th=0.4):
    """A ramp whose TOP surface runs (xa, ya) -> (xb, yb), `th` thick, width 2 hz (rot_z only, the current format)."""
    if xb < xa:
        xa, ya, xb, yb = xb, yb, xa, ya
    angle = math.atan2(yb - ya, xb - xa)
    length = math.hypot(xb - xa, yb - ya)
    L.perm((xa + xb) / 2 + th * math.sin(angle), (ya + yb) / 2 - th * math.cos(angle), zc, length / 2, th, hz, angle)


# ------------------------------------------------------------------------------------------------------------------ deck
L.section("structural: deck slabs (top y = 0), pit floor (y = -6) and pit walls")
L.perm(0, -0.5, -29, 50, 0.5, 21)   # north deck, outside the 16 x 16 pit
L.perm(0, -0.5, 29, 50, 0.5, 21)    # south deck
L.perm(-29, -0.5, 0, 21, 0.5, 8)    # west deck
L.perm(29, -0.5, 0, 21, 0.5, 8)     # east deck
L.perm(0, PIT_FLOOR - 0.5, 0, PIT, 0.5, PIT)
for sx, sz in ((-1, 0), (1, 0), (0, -1), (0, 1)):
    L.perm(sx * (PIT + 0.5), -3.0, sz * (PIT + 0.5), 0.5 if sx else PIT + 0.5, 3.0, 0.5 if sz else PIT + 0.5)

# ------------------------------------------------------------------------------------------------------------------ perimeter
L.section("structural: perimeter wall (void beyond it) and the spawn-ring inner wall")
L.perm(0, 1.5, -49.5, 50.0, 1.5, 0.5)
L.perm(0, 1.5, 49.5, 50.0, 1.5, 0.5)
L.perm(-49.5, 1.5, 0, 0.5, 1.5, 50.0)
L.perm(49.5, 1.5, 0, 0.5, 1.5, 50.0)
SEG_HALF = ((WALL + 1.0) - LANE_HALF) / 2.0
SEG_CENTRE = (LANE_HALF + (WALL + 1.0)) / 2.0
for side in (-1, 1):
    for along in (-1, 1):
        # sides parallel to x: gaps at x = -5..5 for the north / south lanes
        L.perm(along * SEG_CENTRE, 1.5, side * (WALL + 0.5), SEG_HALF, 1.5, 0.5)
        # sides parallel to z: gaps at z = -5..5 for the east / west lanes
        L.perm(side * (WALL + 0.5), 1.5, along * SEG_CENTRE, 0.5, 1.5, SEG_HALF)

# ------------------------------------------------------------------------------------------------------------------ nexus
L.section("structural: pit ramps (four, x-running) and the central fan column")
for zc in (-6.5, -5.0, 5.0, 6.5):
    if zc < 0:
        ramp_x(4.0, PIT_FLOOR, -PIT, 0.0, zc, 0.75)
    else:
        ramp_x(-4.0, PIT_FLOOR, PIT, 0.0, zc, 0.75)
L.perm(0, (14.0 + PIT_FLOOR) / 2.0, 0, 1.5, (14.0 - PIT_FLOOR) / 2.0, 1.5)

# ------------------------------------------------------------------------------------------------------------------ lanes
L.section("structural: four cardinal data lanes with 8 m wall-run faces")
for along in (-1, 1):
    cz = along * (LANE_HALF + 0.5)
    L.perm((LANE_IN + LANE_OUT) / 2.0, WALL_TOP / 2.0, cz, (LANE_OUT - LANE_IN) / 2.0, WALL_TOP / 2.0, 0.5)
    L.perm(-(LANE_IN + LANE_OUT) / 2.0, WALL_TOP / 2.0, cz, (LANE_OUT - LANE_IN) / 2.0, WALL_TOP / 2.0, 0.5)
    cx = along * (LANE_HALF + 0.5)
    L.perm(cx, WALL_TOP / 2.0, (LANE_IN + LANE_OUT) / 2.0, 0.5, WALL_TOP / 2.0, (LANE_OUT - LANE_IN) / 2.0)
    L.perm(cx, WALL_TOP / 2.0, -(LANE_IN + LANE_OUT) / 2.0, 0.5, WALL_TOP / 2.0, (LANE_OUT - LANE_IN) / 2.0)

# ------------------------------------------------------------------------------------------------------------------ upper ring
L.section("structural: upper ring (top y = 9, four segments, 2 m corner gaps) and fan braces")
L.perm(0, UPPER - 0.3, -(RING_INNER + RING_OUTER) / 2.0, 6.0, 0.3, 1.5)
L.perm(0, UPPER - 0.3, (RING_INNER + RING_OUTER) / 2.0, 6.0, 0.3, 1.5)
L.perm((RING_INNER + RING_OUTER) / 2.0, UPPER - 0.3, 0, 1.5, 0.3, 6.0)
L.perm(-(RING_INNER + RING_OUTER) / 2.0, UPPER - 0.3, 0, 1.5, 0.3, 6.0)
L.perm(0, UPPER - 0.3, -4.25, 0.5, 0.3, 2.75)
L.perm(0, UPPER - 0.3, 4.25, 0.5, 0.3, 2.75)
L.perm(4.25, UPPER - 0.3, 0, 2.75, 0.3, 0.5)
L.perm(-4.25, UPPER - 0.3, 0, 2.75, 0.3, 0.5)

# ------------------------------------------------------------------------------------------------------------------ gameplay
L.section("gameplay: destructible data racks at the four plazas (2..6 m bricks)")
for sx in (-1, 1):
    for sz in (-1, 1):
        cx, cz = sx * 28.0, sz * 28.0
        for ox, oz in ((-3.0, -3.0), (3.0, -3.0), (-3.0, 3.0), (3.0, 3.0)):
            L.dbox(cx + ox, 2.0, cz + oz, 2.0, 2.0, 2.0, 320.0)
        L.dbox(cx, 6.0, cz, 2.0, 2.0, 2.0, 320.0)

L.section("gameplay: permanent hard cover in the plazas (3 m full walls)")
for sx in (-1, 1):
    for sz in (-1, 1):
        cx, cz = sx * 28.0, sz * 28.0
        L.perm(cx + 8.0, 1.5, cz, 1.5, 1.5, 1.5)
        L.perm(cx, 1.5, cz + 8.0, 1.5, 1.5, 1.5)

# ------------------------------------------------------------------------------------------------------------------ spawns / pickups
spawns = [
    (-30.0, -SPAWN_R), (-10.0, -SPAWN_R), (30.0, -SPAWN_R),
    (-30.0, SPAWN_R), (10.0, SPAWN_R), (30.0, SPAWN_R),
    (SPAWN_R, -30.0), (SPAWN_R, 30.0),
    (-SPAWN_R, -30.0), (-SPAWN_R, 30.0),
]
for x, z in spawns:
    L.spawn(x, 0.81, z)

for x, z in ((-17.0, 0.0), (17.0, 0.0), (0.0, -17.0), (0.0, 17.0)):
    L.pickup(x, 1.0, z, 25)       # four lane-mouth data cubes
L.pickup(0.0, UPPER + 1.0, -8.5, 30)   # two upper-ring data cubes
L.pickup(0.0, UPPER + 1.0, 8.5, 30)

# ------------------------------------------------------------------------------------------------------------------ features
# Four piston pairs on the nexus diagonals. Each pair is two parallel hammer faces on a 3 s ping-pong cycle
# (90 ticks per leg): the faces start at opposite ends of the shared stroke and the 90-tick phase offset swaps
# them, so the pair reads as a charging/firing rhythm rather than a single moving wall. Faces are 1 m thick,
# 4 m tall and 2 m wide; the full 6 m stroke stays above the deck, between the pit edge and the lane mouths.
PISTON_PERIOD = 90
PISTON_STROKE = 6.0
PISTON_HALF_Y = 2.0
PISTON_HALF_THIN = 0.5
PISTON_HALF_WIDE = 1.0
PISTON_Y = 2.0

def piston_pair(axis, sweep_start, tracks):
    """Two parallel hammers: `axis` is the stroke axis; `tracks` are the two transverse lane centres."""
    for track, phase in zip(tracks, (0, PISTON_PERIOD)):
        if axis == "x":
            centre = (sweep_start, PISTON_Y, track)
            half = (PISTON_HALF_THIN, PISTON_HALF_Y, PISTON_HALF_WIDE)
        else:
            centre = (track, PISTON_Y, sweep_start)
            half = (PISTON_HALF_WIDE, PISTON_HALF_Y, PISTON_HALF_THIN)
        L.mover(*centre, *half, axis, PISTON_STROKE, PISTON_PERIOD, "piston", solid=True, phase_ticks=phase)

# (+, +) serves the east lane; the two tracks sit inside the reserved 6 x 6 m column at (11, 11).
piston_pair("x", 8.0, (9.5, 12.5))
# (+, -) serves the north lane; stroke runs from z=-14 (lane end) to z=-8 (pit edge).
piston_pair("z", -14.0, (9.5, 12.5))
# (-, -) serves the west lane; stroke runs from x=-14 (lane end) to x=-8 (pit edge).
piston_pair("x", -14.0, (-9.5, -12.5))
# (-, +) serves the south lane.
piston_pair("z", 8.0, (-9.5, -12.5))

# Data-lane speed zones hug one 28 m wall-run face per lane (1 m deep), never the whole floor. The wall side
# rotates with the lane so the four zones are symmetric under a quarter turn.
SPEED_TUNING = dict(wall_run_m=10, mult=1.15, cap=1.6, decay=0.9)
L.zone(27.0, WALL_TOP / 2.0, 4.5, (LANE_OUT - LANE_IN) / 2.0, WALL_TOP / 2.0, 0.5, "speed_lane", **SPEED_TUNING)
L.zone(4.5, WALL_TOP / 2.0, 27.0, 0.5, WALL_TOP / 2.0, (LANE_OUT - LANE_IN) / 2.0, "speed_lane", **SPEED_TUNING)
L.zone(-27.0, WALL_TOP / 2.0, -4.5, (LANE_OUT - LANE_IN) / 2.0, WALL_TOP / 2.0, 0.5, "speed_lane", **SPEED_TUNING)
L.zone(-4.5, WALL_TOP / 2.0, -27.0, 0.5, WALL_TOP / 2.0, (LANE_OUT - LANE_IN) / 2.0, "speed_lane", **SPEED_TUNING)

# Core vent: an 8 m diameter, 5 m tall trigger around the pit column (y=-6..-1). The fan column is inside it.
L.zone(0.0, (PIT_FLOOR - 1.0) / 2.0, 0.0, 4.0, (-1.0 - PIT_FLOOR) / 2.0, 4.0, "core_vent",
       fill_ticks=90, cooldown_ticks=1200, reveal_ticks=300)

# Reserved `hazard` volumes (no hazard lines yet: glitch traps and vent damage are the next combat/runtime slice):
# - vent mantle: the core_vent zone, 8 m diameter, y=-6..-1. It must never be filled by permanent boxes.
# - piston glitch pads: one 1.5 x 1.5 m pad on each hammer's leading face; the six-metre stroke above stays clear.

out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "levels", "ffa_circuit.lvl")
total = L.write(out, header=[
    "GENERATED by scripts/gen_circuit_level.py. Edit the script, not this file; tests: crates/shinobi-sim/tests/arena_circuit.rs",
    "CIRCUIT-BREAKER ARENA, Phase 1 blockout with its zone/mover layer. 100 x 100 m deck (top y=0), void beyond the",
    "perimeter wall; 30 x 30 m nexus (pit floor y=-6, ground y=0, upper ring y=9, fan column to y=14); four 10 m data",
    "lanes with 8 m wall-run faces; 7 m spawn ring; 10 spawns; 6 data-cube pickups.",
    "Features: 8 solid piston movers (4 pairs, 90-tick legs, 6 m stroke), 4 speed_lane zones and 1 core_vent zone.",
    "No hazard lines yet: glitch traps and vent damage are reserved for the next combat/runtime slice.",
])

g_total = g_dest = 0
gv_total = gv_dest = 0.0
struct = 0
for title, items in L.sections:
    gameplay = title.startswith("gameplay:")
    for (cx, cy, cz, hx, hy, hz, rot, hp) in items:
        v = 8.0 * hx * hy * hz
        if gameplay:
            g_total += 1
            gv_total += v
            if hp:
                g_dest += 1
                gv_dest += v
        else:
            struct += 1
print("structural boxes: %d; gameplay boxes: %d (%d destructible, %.1f%% of gameplay volume)" % (struct, g_total, g_dest, 100.0 * gv_dest / gv_total))
if total > 1800:
    raise SystemExit("box budget exceeded: %d > 1800" % total)
