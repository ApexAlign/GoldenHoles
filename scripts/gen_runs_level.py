#!/usr/bin/env python3
"""Generates levels/ffa_runs.lvl: THE RUN GAUNTLET (MAP=runs), a free-for-all course built to be flown through at speed.

    python3 scripts/gen_runs_level.py [out.lvl]

A 300 x 152 m ground floor (void edges, kill plane y = -20) under a raised SERPENTINE: five 180 m legs of 10 m wide decks at y 9.6,
joined by turn pads, each leg broken into platforms by GAPS that grow from 10 m (leg 0) to 18 m (leg 4). Every gap is bridged
by a RUN WALL: a 7 m tall panel standing at the deck edge, 4 m past the platform on each side, on alternating sides. Jump on the
face, wall-run (11 m/s for 1.5 s = 16.5 m) over the gap, and hook the ANCHOR slab hung 11 m over the middle of each gap to carry on
(the long gaps of legs 3 and 4 outlast a single wall-run: hook, or dash / double jump at the end). Falling short drops you on the
ground floor, where twelve HOOK TOWERS (20 m; the central pair is 30 m and carries the bows) and six 22 degree ramps at the turn pads
bring you back up. Two small FLOW CHAIN anchor plates sit between the towers on each of the z=-14 / z=14 rows, at 25 m
(between the 20 m and 30 m tower tops), so a hook -> hook -> hook run can cross a whole row of towers without a ground
stop (docs/traversal_melee.md "Flow"). Spawns sit on the decks, so a fight starts in the air.

Cfg limits used: single jump 1.6 m, jump + double ~2.8 m, wall-run 11 m/s for 1.5 s, ramps <= 24 degrees, grapple 1000 m.
Tests: crates/shinobi-sim/tests/arena_runs.rs
"""
import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lvl_lib import Level  # noqa: E402

L = Level("SHINOBI FFA RUNS - THE RUN GAUNTLET")

HALF_X, HALF_Z = 150.0, 76.0
DECK = 9.6  # deck top (8 x 1.2 m)
DECK_T = 1.2  # deck thickness
DECK_HW = 5.0  # deck half width
LEG_Z = [-56.0, -28.0, 0.0, 28.0, 56.0]
LEG_X = 90.0  # legs run x -90 .. 90
PLAT = 16.0  # platform length
GAPS = [10.0, 12.0, 14.0, 16.0, 18.0]
PANEL_H = 7.0
PANEL_PAST = 4.0  # a panel reaches this far over the deck on each side of a gap
ANCHOR_UP = 11.0  # anchor centre above the deck

PLATFORMS = []  # (leg, index, x0, x1)
GAP_LIST = []  # (leg, index, x0, x1)


def deck(x0, x1, zc, hz, top=DECK):
    L.perm((x0 + x1) / 2, top - DECK_T / 2, zc, (x1 - x0) / 2, DECK_T / 2, hz)


def ramp(xa, ya, xb, yb, zc, hz, th=0.4):
    """A ramp whose TOP surface runs (xa, ya) -> (xb, yb), thickness th, width 2 hz."""
    if xb < xa:
        xa, ya, xb, yb = xb, yb, xa, ya
    t = math.atan2(yb - ya, xb - xa)
    assert abs(t) <= math.radians(24.2), "ramp too steep: %.1f deg" % math.degrees(t)
    ln = math.hypot(xb - xa, yb - ya)
    L.perm((xa + xb) / 2 + th * math.sin(t), (ya + yb) / 2 - th * math.cos(t), zc, ln / 2, th, hz, t)


# ---------------------------------------------------------------------------------------------- ground
L.section("ground floor (void edges, kill plane y = -20)")
L.perm(0, -0.5, 0, HALF_X, 0.5, HALF_Z)

# ---------------------------------------------------------------------------------------------- legs
L.section("serpentine legs: platforms split by gaps (the gap grows with every leg)")
for k, zc in enumerate(LEG_Z):
    g = GAPS[k]
    n = int((2 * LEG_X + g) // (PLAT + g))
    used = n * PLAT + (n - 1) * g
    extra = (2 * LEG_X - used) / 2.0
    x = -LEG_X
    for i in range(n):
        ln = PLAT + (extra if i in (0, n - 1) else 0.0)
        PLATFORMS.append((k, i, x, x + ln))
        deck(x, x + ln, zc, DECK_HW)
        x += ln
        if i < n - 1:
            GAP_LIST.append((k, i, x, x + g))
            x += g
    assert abs(x - LEG_X) < 1e-6, (k, x)

# ---------------------------------------------------------------------------------------------- turn pads and ramps
L.section("turn pads (flat, same height as the legs) and 22 degree ramps down to the ground")
PADS = []  # (x0, x1, z0, z1, side)
PADS.append((-LEG_X - 20.0, -LEG_X, LEG_Z[0] - DECK_HW, LEG_Z[0] + DECK_HW, -1))  # start pad of leg 0
for k in range(4):
    side = 1 if k % 2 == 0 else -1  # leg k runs +x when k is even and ends on the +x pad
    z0, z1 = LEG_Z[k] - DECK_HW, LEG_Z[k + 1] + DECK_HW
    PADS.append((LEG_X, LEG_X + 20.0, z0, z1, 1) if side == 1 else (-LEG_X - 20.0, -LEG_X, z0, z1, -1))
PADS.append((LEG_X, LEG_X + 20.0, LEG_Z[4] - DECK_HW, LEG_Z[4] + DECK_HW, 1))  # finish pad of leg 4
RUN = DECK / math.tan(math.radians(22.0))
for (x0, x1, z0, z1, side) in PADS:
    deck(x0, x1, (z0 + z1) / 2, (z1 - z0) / 2)
    zc = (z0 + z1) / 2
    hz = min(6.0, (z1 - z0) / 2)
    if side == 1:
        ramp(x1, DECK, x1 + RUN, 0.0, zc, hz)
    else:
        ramp(x0 - RUN, 0.0, x0, DECK, zc, hz)

# ---------------------------------------------------------------------------------------------- run walls over the gaps
L.section("run walls: one 7 m panel over every gap, alternating sides; hook anchors hung over the gap middles")
for (k, i, gx0, gx1) in GAP_LIST:
    side = 1.0 if i % 2 == 0 else -1.0
    x0, x1 = gx0 - PANEL_PAST, gx1 + PANEL_PAST
    L.perm((x0 + x1) / 2, DECK + PANEL_H / 2, LEG_Z[k] + side * (DECK_HW + 0.5), (x1 - x0) / 2, PANEL_H / 2, 0.5)
    L.perm((gx0 + gx1) / 2, DECK + ANCHOR_UP, LEG_Z[k], 2.5, 0.35, 1.2)

# ---------------------------------------------------------------------------------------------- hook towers on the ground
L.section("hook towers between the legs (3 x 3 m shafts with an anchor plate on top; the central pair carries the bows)")
TOWERS = []  # (x, z, top)
for z in (-42.0, -14.0, 14.0, 42.0):
    for x in (-60.0, 0.0, 60.0):
        top = 30.0 if (x == 0.0 and z in (-14.0, 14.0)) else 20.0
        L.perm(x, top / 2, z, 1.5, top / 2, 1.5)
        L.perm(x, top + 0.35, z, 4.0 if top == 30.0 else 3.0, 0.35, 4.0 if top == 30.0 else 1.5)
        TOWERS.append((x, z, top + 0.7))

# ---------------------------------------------------------------------------------------------- flow chain
L.section("flow chain: two small floating anchor plates between the central hook towers on each side row, so a "
          "hook -> hook -> hook run crosses a whole row (-60 -> -30 -> 0 -> 30 -> 60) without ever touching the ground")
for z in (-14.0, 14.0):
    for x in (-30.0, 30.0):
        L.perm(x, 25.0, z, 1.6, 0.35, 1.6)

# ---------------------------------------------------------------------------------------------- spawns / pickups
for k, zc in enumerate(LEG_Z):
    pl = [p for p in PLATFORMS if p[0] == k]
    idx = sorted({1, len(pl) // 2, len(pl) - 2})
    for i in idx:
        _, _, x0, x1 = pl[i]
        L.spawn((x0 + x1) / 2, DECK + 0.81, zc)
L.spawn(-LEG_X - 10.0, DECK + 0.81, LEG_Z[0])
L.spawn(LEG_X + 10.0, DECK + 0.81, LEG_Z[4])

for (x0, x1, z0, z1, side) in PADS[1:5]:
    L.pickup((x0 + x1) / 2, DECK + 1.0, (z0 + z1) / 2, 25)  # the four turn pads
for (x, z, top) in TOWERS:
    if top < 25 and abs(x) == 60.0 and abs(z) == 14.0:
        L.pickup(x, top + 1.0, z, 25)  # four 20 m tower tops (hook only)
L.pickup(30.0, 1.0, -14.0, 20)  # ground, between the towers
L.pickup(-30.0, 1.0, 14.0, 20)
L.weapon("yumi", 0.0, 30.7 + 1.0, -14.0)
L.weapon("yumi", 0.0, 30.7 + 1.0, 14.0)
for (k, i) in [(1, 1), (3, 2)]:
    pl = [p for p in PLATFORMS if p[0] == k]
    _, _, x0, x1 = pl[i]
    L.weapon("tag", (x0 + x1) / 2, DECK + 1.0, LEG_Z[k] + 2.0)
L.weapon("tag", LEG_X + 10.0, DECK + 1.0, LEG_Z[4] - 2.5)
L.weapon("tag", -30.0, 1.0, -14.0)

out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "levels", "ffa_runs.lvl")
L.write(out, header=[
    "GENERATED by scripts/gen_runs_level.py. Edit the script, not this file; tests: crates/shinobi-sim/tests/arena_runs.rs",
    "THE RUN GAUNTLET: a serpentine of five 180 m decks at y 9.6 broken by 10..18 m gaps, a run wall over every gap, a hook anchor over",
    "every gap middle, 20 / 30 m hook towers and ramps from the ground floor (void edges, kill plane y=-20). x right, y up.",
])
