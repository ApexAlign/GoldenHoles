#!/usr/bin/env python3
"""Generates levels/ffa_hive.lvl: THE HIVE, a close-quarters honeycomb shaft, 56 x 56 m and 190 m tall, no ground
(floating-style level, kill plane y = -20), built only from lvl_lib bricks and (almost) totally destructible.

    python3 scripts/gen_hive_level.py [out.lvl]

Nine ring floors, one every 20 m (deck tops y = 10, 30, ... 170) round an open 36 x 36 m WELL, closed by a roof ring at y 190.
Brick outer walls (12 m per story, windows in the 4 m slot under the next floor; three layers on the top story), a 24 degree
RAMP up the inside of the north / south wall to the next floor (alternating, so the climb spirals), partition bricks that cut each
side strip into honeycomb ALCOVES. In the well, only along two 4 m stripes (so the bots' heightmap edge guard still sees open
void everywhere else): walkable BRIDGES at floors 1, 3, 5, 7, hook-only steel SWING LINES 10 m above every floor, ROOF BEAMS, and a
CROWN platform on chains (hook only, carries a bow). Under every floor rim hang WALL-RUN CURTAINS and chains of STALACTITE
columns. Everything is a destructible brick except the plates under spawns / pedestals (bedrock).

Support rules (breaks.rs: a box rests on boxes whose top face is its bottom face): rim plates sit on the story walls under them;
hung things are tied in 1 m INSIDE the plate / beam above (or 0.3 m apart) so shooting them never drops a floor.
Tests: crates/shinobi-sim/tests/arena_hive.rs, walkable.rs; bots: crates/shinobi-ai/tests/hive.rs
"""
import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lvl_lib import Level  # noqa: E402

L = Level("SHINOBI FFA HIVE - a 56 x 56 m honeycomb shaft, 190 m tall, no ground (kill plane y=-20)")
NF = 9  # floors
H = 20.0  # floor pitch
D0 = 10.0  # deck top of floor 0
THICK = 2.0
D = [D0 + H * k for k in range(NF)]
ROOF_TOP = D[-1] + 20.0  # 190
WELL = 18.0  # well half width
OUT = 28.0  # outer half width
WALL = 26.0  # inner face of the outer walls
STORY_LAYERS = 2  # wall layers per story (6 m each)
LANE_Z = (22.0, 26.0)  # ramp lane depth (from the centre line), against the wall
RAMP_RUN = 45.0
RAMP_X = (-25.5, 19.5)  # south ramp climbs +x from -25.5 to 19.5; the north one is the mirror image
MATW, MATF, MATB = "brick", "concrete", "steel"


def xs_edges(a, b, n):
    return [a + (b - a) * i / n for i in range(n + 1)]


def plates(xe, ze, top, mat=MATF, thick=THICK):
    """Flush plates over the grid xe x ze (edge lists)."""
    n = 0
    for i in range(len(xe) - 1):
        for j in range(len(ze) - 1):
            L.block(xe[i], xe[i + 1], top - thick, top, ze[j], ze[j + 1], mat)
            n += 1
    return n


def ring_floor(top, cut=None, roof=False):
    """One ring deck. cut = 'S' or 'N': the 4 m ramp lane under the ramp that arrives here is left open (rim plates stay under the wall)."""
    X10 = xs_edges(-OUT, OUT, 10)
    for sgn in (-1, 1):  # sgn -1 = south strip, +1 = north strip
        def zr(a, b):
            return sorted([sgn * a, sgn * b])
        # ledge row (well side) and rim row
        plates(X10, zr(WELL, 22.0), top)
        sidecut = (cut == ("S" if sgn < 0 else "N"))
        if not sidecut:
            plates(X10, zr(22.0, OUT), top)
        else:
            plates(X10, zr(26.0, OUT), top)  # rim under the wall
            # lane row (22..26): only outside the ramp x range
            if sgn < 0:
                xa, xb = RAMP_X
            else:
                xa, xb = -RAMP_X[1], -RAMP_X[0]
            if xa > -OUT:
                plates([-OUT, xa], zr(22.0, 26.0), top)
            if xb < OUT:
                plates(xs_edges(xb, OUT, 2), zr(22.0, 26.0), top)
    # east / west strips between the N and S strips
    Z6 = xs_edges(-WELL, WELL, 6)
    for sgn in (-1, 1):
        def xr(a, b):
            return sorted([sgn * a, sgn * b])
        plates(xr(WELL, 24.0), Z6, top)
        plates(xr(24.0, OUT), Z6, top)


def ramp_boxes(k):
    """The 24 degree ramp from deck k to deck k+1 along the south (k even) or north (k odd) wall, in 8 rolled segments."""
    south = (k % 2 == 0)
    xa, xb = RAMP_X if south else (-RAMP_X[1], -RAMP_X[0])
    zc = -(LANE_Z[0] + LANE_Z[1]) / 2 if south else (LANE_Z[0] + LANE_Z[1]) / 2
    hz = (LANE_Z[1] - LANE_Z[0]) / 2
    # surface runs (x_lo, deck_lo) -> (x_hi, deck_hi) climbing in +x (south) or -x (north)
    p0 = (xa, D[k]) if south else (xb, D[k])
    p1 = (xb, D[k + 1]) if south else (xa, D[k + 1])
    n = 8
    th = 0.4
    for i in range(n):
        x0 = p0[0] + (p1[0] - p0[0]) * i / n
        y0 = p0[1] + (p1[1] - p0[1]) * i / n
        x1 = p0[0] + (p1[0] - p0[0]) * (i + 1) / n
        y1 = p0[1] + (p1[1] - p0[1]) * (i + 1) / n
        if x1 < x0:
            x0, y0, x1, y1 = x1, y1, x0, y0
        t = math.atan2(y1 - y0, x1 - x0)
        assert abs(t) <= math.radians(24.2), "ramp too steep %.1f" % math.degrees(t)
        ln = math.hypot(x1 - x0, y1 - y0)
        # a few cm overlap between neighbours so no crack opens at the seams
        L.dbox((x0 + x1) / 2 + th * math.sin(t), (y0 + y1) / 2 - th * math.cos(t), zc, ln / 2 + 0.05, th, hz, L.hp_for(ln / 2, th, hz, MATF), rot=t)


def story_walls(k):
    y0 = D[k]
    top = D[k + 1] - THICK - 4.0 if k < NF - 1 else ROOF_TOP - THICK
    layers = STORY_LAYERS if k < NF - 1 else 3
    h = (top - y0) / layers if k == NF - 1 else 6.0
    top = y0 + h * layers
    # north / south walls span the full width; east / west walls run between them. Running bond is done per layer by hand
    # (lvl_lib's own bond would also split the 2 m thickness).
    for l in range(layers):
        ya, yb = y0 + h * l, y0 + h * (l + 1)
        off = (OUT * 2 / 10) / 2 if l % 2 else 0.0
        for z in (-WALL - 1.0, WALL + 1.0):
            for (xa, xb) in ([(-OUT, -OUT + off), (-OUT + off, OUT)] if off else [(-OUT, OUT)]):
                L.bricks(xa, xb, ya, yb, z - 1.0, z + 1.0, MATW, (OUT * 2 / 10, h, 2.0), bond=False)
        off = (WALL * 2 / 10) / 2 if l % 2 else 0.0
        for x in (-WALL - 1.0, WALL + 1.0):
            for (za, zb) in ([(-WALL, -WALL + off), (-WALL + off, WALL)] if off else [(-WALL, WALL)]):
                L.bricks(x - 1.0, x + 1.0, ya, yb, za, zb, MATW, (2.0, h, WALL * 2 / 10), bond=False)


def partitions(k):
    """Honeycomb: brick partitions across the east / west strips (aisle x 18..22 stays open) form alcoves."""
    for sx in (-1, 1):
        for z in (-9.0, 9.0):
            L.block(sx * 22.0 if sx > 0 else -26.0, sx * 26.0 if sx > 0 else -22.0, D[k], D[k] + 6.0, z - 1.0, z + 1.0, MATW)


def hang_chain(x, z, hx, hz, y_top, y_bot, links, mat=MATF, gap=0.3):
    """Bricks hung from y_top (tied 1 m into whatever is above) down to y_bot with 0.3 m gaps: no support coupling, each link shootable."""
    total = y_top - y_bot - gap * (links - 1)
    ln = total / links
    y = y_top
    for _ in range(links):
        L.block(x - hx, x + hx, y - ln, y, z - hz, z + hz, mat)
        y -= ln + gap


# ============================================================================================================ floors
L.section("ring floors (concrete plates), one every 20 m")
for k in range(NF):
    cut = None
    if k >= 1:
        cut = "S" if (k - 1) % 2 == 0 else "N"  # ramp k-1 arrives here
    ring_floor(D[k], cut)
L.section("roof ring (y 188..190)")
ring_floor(ROOF_TOP, None)

L.section("outer brick walls (one story per floor)")
for k in range(NF):
    story_walls(k)

L.section("ramps (24 degrees) floor to floor")
for k in range(NF - 1):
    ramp_boxes(k)

L.section("honeycomb partitions")
for k in range(NF):
    partitions(k)

# ============================================================================================================ the well
BR_W = 4.0  # bridge / stripe width
L.section("walkable bridges across the well (floors 1, 3, 5, 7)")
for i, k in enumerate((1, 3, 5, 7)):
    if i % 2 == 0:  # along z at x = 0
        L.block(-BR_W / 2, BR_W / 2, D[k] - 1.0, D[k], -WELL - 1.0, WELL + 1.0, MATB)
    else:  # along x at z = 0
        L.block(-WELL - 1.0, WELL + 1.0, D[k] - 1.0, D[k], -BR_W / 2, BR_W / 2, MATB)

L.section("swing lines: steel girders 10 m above every floor (hook only)")
for k in range(NF - 1):
    y = D[k] + 10.0
    if k % 2 == 0:
        L.block(-WELL - 1.0, WELL + 1.0, y - 0.75, y + 0.75, -1.0, 1.0, MATB)
    else:
        L.block(-1.0, 1.0, y - 0.75, y + 0.75, -WELL - 1.0, WELL + 1.0, MATB)

L.section("ceiling beams under the roof (y ~187.6) and the crown chains")
BEAM_Y = ROOF_TOP - THICK - 0.4  # centre 187.6, top 188.35: 0.35 m into the roof plates (no support coupling)
L.block(-WELL - 1.0, WELL + 1.0, BEAM_Y - 0.75, BEAM_Y + 0.75, -BR_W / 2, BR_W / 2, MATB)
L.block(-BR_W / 2, BR_W / 2, BEAM_Y - 0.75, BEAM_Y + 0.75, -WELL - 1.0, WELL + 1.0, MATB)
CROWN_TOP = D[-1] - 2.0  # 168
L.perm(0.0, CROWN_TOP - 1.0, 0.0, 3.0, 1.0, 4.5)  # the crown platform (bedrock: carries a bow)
for z in (-3.0, 3.0):
    hang_chain(0.0, z, 1.0, 1.0, BEAM_Y + 0.5, CROWN_TOP - 1.0, 4, MATB)
# a weapon plinth on the floor 7 bridge (bow 2): the bridge there runs along x (i = 3)
BR7 = D[7]
L.perm(0.0, BR7 + 0.75, 0.0, 1.5, 0.75, 1.5)

L.section("pillars on the bridges (hook / wall-run posts)")
for i, k in enumerate((1, 3, 5, 7)):
    for s in (-1, 1):
        if k == 7 and s == 1:
            continue
        if i % 2 == 0:
            L.column(0.0, s * 9.0, 1.0, D[k], D[k] + 8.0, MATF, 4.0)
        else:
            L.column(s * 9.0, 0.0, 1.0, D[k], D[k] + 8.0, MATF, 4.0)

# ============================================================================================================ curtains and stalactites
L.section("wall-run curtains hung under the floor rims")
for k in range(1, NF):
    ytop = D[k] - THICK + 1.0  # tied 1 m into the plate
    ybot = D[k] - THICK - 11.0
    h = (ytop - ybot) / 2
    pos = [(7.0, 19.0), (-7.0, -19.0)]
    for (x, z) in pos:
        for r in range(2):
            L.block(x - 3.0, x + 3.0, ytop - h * (r + 1), ytop - h * r, z - 1.0, z + 1.0, MATF)
    for (x, z) in [(19.0, -7.0), (-19.0, 7.0)]:
        for r in range(2):
            L.block(x - 1.0, x + 1.0, ytop - h * (r + 1), ytop - h * r, z - 3.0, z + 3.0, MATF)

L.section("stalactite chains under the floors")
for k in range(1, NF):
    sh = [-13.0, 13.0] if k % 2 else [-14.0, 14.0]
    for a in sh:
        hang_chain(a, 19.0 if k % 2 else -19.0, 1.0, 1.0, D[k] - THICK + 1.0, D[k] - THICK - 11.0, 2)
        hang_chain(19.0 if k % 2 else -19.0, -a, 1.0, 1.0, D[k] - THICK + 1.0, D[k] - THICK - 11.0, 2)

# ============================================================================================================ entities
SPAWNS = [  # (floor, x, z)
    (0, 24, 0), (0, -24, -14), (1, -23, 23), (1, 24, 13), (2, 23, -23), (2, -24, 0), (3, 24, -13), (3, -23, 23),
    (4, -23, 23), (4, 24, 0), (5, -24, 13), (6, 23, -23), (6, -24, -13), (7, 24, 13), (8, -24, 0), (8, 23, -23),
]
for (k, x, z) in SPAWNS:
    L.spawn(x, D[k] + 0.81, z)
for (k, x, z) in [(0, -24, 14), (3, 24, 13), (4, -24, 0), (6, 24, 0), (7, -24, -13), (8, 24, 13)]:
    L.pickup(x, D[k] + 1.0, z, 25)
for (k, x, z) in [(1, -24, 0), (2, 24, 13), (4, -24, -13), (5, 24, -13)]:
    L.weapon("tag", x, D[k] + 1.0, z)
L.weapon("yumi", 0.0, CROWN_TOP + 1.0, 0.0)
L.weapon("yumi", 0.0, BR7 + 1.5 + 1.0, 0.0)

out = sys.argv[1] if len(sys.argv) > 1 else "levels/ffa_hive.lvl"
L.write(out, header=[
    "GENERATED by scripts/gen_hive_level.py (lvl_lib bricks). Edit the script, not this file. Tests: arena_hive.rs, walkable.rs, ai hive.rs",
    "Nine ring floors (deck tops y 10, 30 ... 170), a 36 m open well, roof y 190, ramps on the north / south walls, bridges, swing lines, crown.",
])
