#!/usr/bin/env python3
"""Generates levels/ffa_clock.lvl: THE CLOCKWORK, the inside of a huge octagonal brick drum (no ground, kill plane y = -20).

    python3 scripts/gen_clock_level.py [out.lvl]

A 54 m (flat to flat) stepped-octagon drum of 6 m bricks, 212 m tall (y 8..212), with a fat 12 m column of stacked bricks up the
middle (cap y 200 carries the two bows). A walkable SWITCHBACK spiral climbs the inside of the wall: 15 ramps of 21.8 degrees
(rolled about Z only, so they run along x in the north / south lanes) joined by landings on the east / west sides, every 12 m up
from y 14 to y 194. Gear-wheel cogs (cross-shaped plates) sit on the column: some mated to a landing or ramp (walkable, they carry
the tag bags), some floating free with only girder links (hook only), with hanging weights. Ceiling beams at y 201..205 carry four
pendulum weights; balconies jut from the walls into the bays. Everything is bricks (lvl_lib); only plinths under spawns and
pedestals are permanent. Tests: crates/shinobi-sim/tests/arena_clock.rs, walkable.rs; bots: crates/shinobi-ai/tests/clock.rs
"""
import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lvl_lib import Level, _cuts  # noqa: E402

L = Level("THE CLOCKWORK: a 54 m octagonal brick drum, 210 m tall, spiral ramps, cogs, pendulums, hooks everywhere", hp_scale=1.0)

Y0, BH, NL = 8.0, 6.0, 34  # wall layers: 8 .. 212
WALL_TOP = Y0 + BH * NL
COL_LAYERS = 32  # column top 200
COL_TOP = Y0 + BH * COL_LAYERS
RISE, N_LAND = 12.0, 16
HM = RISE / 2  # a ramp's height at x = 0 above its low landing
CT = HM + 0.15  # a cog mated to a ramp sits 0.15 m above it (a flush edge reads as no ground to the walkability validator)
LAND = [14.0 + RISE * k for k in range(N_LAND)]  # landing tops; even k west, odd k east
RAMP_HALF_RUN = 15.0  # ramps span x -15..15
TH = 0.6  # ramp half thickness


def wall_rect(x0, x1, z0, z1, axis):
    """A one-brick-thick wall piece, aligned courses with a half-shifted tie course every third layer."""
    for l in range(NL):
        ya = Y0 + BH * l
        if axis == "x":
            for (a, b) in _cuts(x0, x1, 6.0, 3.0 if l % 3 == 2 else 0.0):
                L.block(a, b, ya, ya + BH, z0, z1, "brick")
        elif axis == "z":
            for (a, b) in _cuts(z0, z1, 6.0, 3.0 if l % 3 == 2 else 0.0):
                L.block(x0, x1, ya, ya + BH, a, b, "brick")
        else:
            L.block(x0, x1, ya, ya + BH, z0, z1, "brick")


# ---------------------------------------------------------------------------------------------- drum wall
L.section("drum wall (stepped octagon: flat sides 30 m, two-step chamfers)")
wall_rect(-15, 15, 27, 33, "x")
wall_rect(-15, 15, -33, -27, "x")
wall_rect(27, 33, -15, 15, "z")
wall_rect(-33, -27, -15, 15, "z")
for sx in (-1, 1):
    for sz in (-1, 1):
        wall_rect(*sorted((sx * 15, sx * 21)), *sorted((sz * 21, sz * 27)), None)
        wall_rect(*sorted((sx * 21, sx * 27)), *sorted((sz * 15, sz * 21)), None)
        wall_rect(*sorted((sx * 21, sx * 27)), *sorted((sz * 21, sz * 27)), None)

# ---------------------------------------------------------------------------------------------- column
L.section("central column (12 m, stacked bricks; cap y 200)")
for l in range(COL_LAYERS):
    ya = Y0 + BH * l
    xs = _cuts(-6, 6, 6.0, 3.0) if (l % 3 == 2 and l < COL_LAYERS - 1) else _cuts(-6, 6, 6.0, 0.0)
    for (a, b) in xs:
        for (c, d) in ((-6, 0), (0, 6)):
            L.block(a, b, ya, ya + BH, c, d, "concrete")

# ---------------------------------------------------------------------------------------------- spiral
L.section("switchback spiral: landings (x +-15..21) and 22 degree ramps (z +-15..21)")


def landing(k):
    x0, x1 = (-21.0, -15.0) if k % 2 == 0 else (15.0, 21.0)
    for zc in (-18, -12, -6, 0, 6, 12, 18):
        L.block(x0, x1, LAND[k] - 2.0, LAND[k], zc - 3, zc + 3, "concrete")


def ramp(xa, ya, xb, yb, zc, hz, segs=6):
    """Top surface (xa, ya) -> (xb, yb) rolled about Z, cut into `segs` pieces (< 6 m along the slope)."""
    t = math.atan2(yb - ya, xb - xa)
    assert abs(t) <= math.radians(24.0)
    for i in range(segs):
        x0, x1 = xa + (xb - xa) * i / segs, xa + (xb - xa) * (i + 1) / segs
        y0, y1 = ya + (yb - ya) * i / segs, ya + (yb - ya) * (i + 1) / segs
        ln = math.hypot(x1 - x0, y1 - y0)
        L.dbox((x0 + x1) / 2 + TH * math.sin(t), (y0 + y1) / 2 - TH * math.cos(t), zc, ln / 2, TH, hz, L.hp_for(ln / 2, TH, hz, "concrete"), t)


for k in range(N_LAND):
    landing(k)
for k in range(N_LAND - 1):
    if k % 2 == 0:  # north lane, east-bound, from the west landing k to the east landing k+1
        ramp(-RAMP_HALF_RUN, LAND[k], RAMP_HALF_RUN, LAND[k + 1], 18.0, 3.0)
    else:  # south lane, west-bound, from the east landing k to the west landing k+1
        ramp(-RAMP_HALF_RUN, LAND[k + 1], RAMP_HALF_RUN, LAND[k], -18.0, 3.0)

# ---------------------------------------------------------------------------------------------- cogs
L.section("gear-wheel cogs on the column (mated = walkable, free = hook only with girder links)")
tags = []


def cog(top, mate=None, free=False, weights=False):
    """Four arms (+x -x +z -z) of 6 m plates from the column (or from x 9 with girder links when `free`) and four corner teeth.
    `mate` = arm direction reaching 15 m so it touches the landing / ramp beside it flush."""
    a0 = 9.0 if free else 6.0
    a1 = 14.0 if free else 12.0
    for d, (ux, uz) in {"+x": (1, 0), "-x": (-1, 0), "+z": (0, 1), "-z": (0, -1)}.items():
        end = 15.0 if d == mate else a1
        lo, hi = a0, end
        mid = lo + 6.0 if hi - lo > 6.0 else hi  # plates stay <= 6 m long
        for (s0, s1) in ((lo, mid), (mid, hi)) if mid < hi else ((lo, hi),):
            if ux:
                xa, xb = sorted((ux * s0, ux * s1))
                L.block(xa, xb, top - 2.0, top, -3, 3, "concrete")
            else:
                za, zb = sorted((uz * s0, uz * s1))
                L.block(-3, 3, top - 2.0, top, za, zb, "concrete")
        if free:
            ga, gb = sorted((ux * 6.0, ux * 9.0)) if ux else (-1, 1)
            za, zb = sorted((uz * 6.0, uz * 9.0)) if uz else (-1, 1)
            L.block(ga, gb, top - 1.75, top - 0.25, za, zb, "steel")
            if weights and d in ("+x", "-x"):
                cx = ux * (lo + hi) / 2
                L.block(cx - 2, cx + 2, top - 8.0, top - 2.0, -2, 2, "steel")
    for sx in (-1, 1):
        for sz in (-1, 1):
            xa, xb = sorted((sx * 6.0, sx * 9.0)) if not free else sorted((sx * 8.0, sx * 11.0))
            za, zb = sorted((sz * 6.0, sz * 9.0)) if not free else sorted((sz * 8.0, sz * 11.0))
            L.block(xa, xb, top - 2.0, top, za, zb, "concrete")


cog(LAND[2] + CT, mate="+z")  # 46.5 north ramp k=2
cog(LAND[5], mate="+x")  # 79 east landing
cog(LAND[7] + CT, mate="-z")  # 111.5 south ramp k=7
cog(LAND[8], mate="-x")  # 118 west landing
cog(LAND[11], mate="+x")  # 157 east landing
cog(LAND[12] + CT, mate="+z")  # 176.5 north ramp k=12
for t in (62.0, 92.0, 128.0, 158.0, 188.0):
    cog(t, free=True, weights=True)
tags += [(9.0, LAND[5] + 1.0, 0.0), (0.0, LAND[7] + CT + 1.0, -9.0), (-9.0, LAND[8] + 1.0, 0.0), (9.0, LAND[11] + 1.0, 0.0)]

# ---------------------------------------------------------------------------------------------- ceiling and pendulums
L.section("ceiling beams (y 199 and 205) and four hanging pendulum weights")
for zc in (-10.0, 10.0):
    for i in range(9):
        L.block(-27 + 6 * i, -21 + 6 * i, 202.8, 206.0, zc - 1, zc + 1, "steel")
for xc in (-20.0, 20.0):
    for i in range(7):
        L.block(xc - 1, xc + 1, 196.8, 200.0, -21 + 6 * i, -15 + 6 * i, "steel")
for px in (-10.0, 10.0):
    for pz in (-10.0, 10.0):
        L.block(px - 2, px + 2, 196.8, 202.8, pz - 2, pz + 2, "steel")
        L.block(px - 2, px + 2, 190.8, 196.8, pz - 2, pz + 2, "steel")
        L.block(px - 3, px + 3, 184.8, 190.8, pz - 3, pz + 3, "concrete")

# ---------------------------------------------------------------------------------------------- balconies
L.section("balconies jutting from the walls into the bays")


def snap(y):
    """Balcony tops sit on a brick course top so the wall bricks beside them stay walkable (no knee-high rise)."""
    return Y0 + BH * round((y - Y0) / BH)


for k in (0, 4, 8, 12):  # north bay, at the north ramp's mid height
    for xc in (-6, 0, 6):
        L.block(xc - 3, xc + 3, snap(LAND[k] + HM) - 1.85, snap(LAND[k] + HM) + 0.15, 21, 27, "wood")
for k in (1, 5, 9, 13):  # south bay
    for xc in (-6, 0, 6):
        L.block(xc - 3, xc + 3, snap(LAND[k] + HM) - 1.85, snap(LAND[k] + HM) + 0.15, -27, -21, "wood")
for k in (2, 6, 10, 14):  # east wall, at west landing levels (hook / jump only)
    for zc in (-6, 0, 6):
        L.block(21, 27, snap(LAND[k]) - 2.0, snap(LAND[k]), zc - 3, zc + 3, "wood")
for k in (3, 7, 11, 15):  # west wall, at east landing levels
    for zc in (-6, 0, 6):
        L.block(-27, -21, snap(LAND[k]) - 2.0, snap(LAND[k]), zc - 3, zc + 3, "wood")

# ---------------------------------------------------------------------------------------------- entities
for k in range(N_LAND):
    x = -18.0 if k % 2 == 0 else 18.0
    L.spawn(x, LAND[k] + 0.81, (-12.0, 12.0)[k % 2])
L.spawn(0.0, LAND[2] + CT + 0.81, 9.0)
for k in (1, 4, 6, 9, 10, 13):
    L.pickup(18.0 if k % 2 else -18.0, LAND[k] + 1.0, 0.0)
L.weapon("yumi", -3.0, COL_TOP + 1.0, -3.0)
L.weapon("yumi", 3.0, COL_TOP + 1.0, 3.0)
for (x, y, z) in tags:
    L.weapon("tag", x, y, z)

out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "levels", "ffa_clock.lvl")
L.write(out, header=[
    "GENERATED by scripts/gen_clock_level.py. Edit the script, not this file; tests: crates/shinobi-sim/tests/arena_clock.rs, walkable.rs",
    "A 54 m stepped-octagon brick drum, 210 m tall, no ground (kill plane y=-20): central column, switchback spiral, cogs, pendulums.",
])
