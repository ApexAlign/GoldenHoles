#!/usr/bin/env python3
"""Generates levels/ffa_split.lvl: THE SPLIT, two sheer brick towers over a 24 m vertical canyon (floating, kill plane y = -20).

    python3 scripts/gen_split_level.py [out.lvl]

Two towers, 42 x 26 m footprint, 198 m tall (y 8 .. 206), face to face along z with a 24 m canyon between them (z -12 .. 12).
Everything is bricks from scripts/lvl_lib.py (<= 6 m), (almost) totally destructible; only the plinths under spawns / pedestals are bedrock.

Tower (u = metres from the canyon face outward, x = along the face):
  * canyon FACADE u 0..2, 6 m bricks: a flat wall-run face with window gaps, door gaps and balconies.
  * outer / end PIERS, so the outside is a lattice (hook and wall-run faces on every side).
  * a zig-zag of 21.8 degree RAMPS (12 m rise over 30 m, lane A u 6..12 going +x on even levels, lane B u 14..20 going -x on odd
    levels) with a LANDING at the arrival end of each of the 17 levels (y = 10 + 12 k): every floor is walkable, no hook needed.
  * GALLERIES (levels 2 5 8 11 14) run along the facade; the roof (level 16, y 202) has parapets, scaffolds, a gantry with a bow (north)
    and a crane (south) whose jib reaches over the canyon and carries the second bow on its cab (hook / wall-run only).
Canyon: 12 BRIDGES (wood / brick / steel plates, one per level pair, shoot a plate and the crossing is gone), balconies, 14 gondola
decks, 8 hanging counterweights, 8 girder tightropes and 6 cable stays floating mid-canyon as hook anchors and swing lines.

Tests: crates/shinobi-sim/tests/arena_split.rs, walkable.rs; bots: crates/shinobi-ai/tests/split.rs
"""
import math
import os
import random
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lvl_lib import Level  # noqa: E402

L = Level("THE SPLIT")
rnd = random.Random(0x5911)

# ------------------------------------------------------------------------------------------------ geometry constants
HALF_X = 21.0  # tower half length along x
GAP = 12.0  # canyon half width: facade at |z| = 12
LEVEL_Y = lambda k: 10.0 + 12.0 * k  # top of the floor of level k (0..16)
LAYERS = [(8.0, 10.0)] + [(10.0 + 6.0 * j, 16.0 + 6.0 * j) for j in range(32)]  # last layer ends at 202
CUT_U = [-21.0 + 6.0 * i for i in range(8)]  # unshifted facade cuts, 7 bricks
CUT_S = [-21.0, -18.0] + [-12.0 + 6.0 * i for i in range(7)] + [21.0]  # shifted (bond) cuts: -21 -18 -12 .. 18 21 (9 values)
CUT_S = [-21.0, -18.0, -12.0, -6.0, 0.0, 6.0, 12.0, 18.0, 21.0]
SIDES = (-1, 1)  # -1 = south tower (z < 0), +1 = north tower

BRIDGES = [  # (level, brick index 0..6 or "edge", material)
    (1, 6, "wood"), (2, 3, "steel"), (3, 6, "brick"), (5, 2, "wood"), (6, 0, "steel"), (8, 4, "steel"),
    (9, 6, "brick"), (11, 3, "wood"), (12, 0, "steel"), (14, 1, "steel"), (15, 6, "brick"),
    (13, 6, "wood"),
]
GALLERIES = [2, 5, 8, 11, 14]
BALCONIES = {2: (1, 5), 5: (4, 5), 8: (2, 5), 11: (1, 5), 14: (3, 5)}  # facade brick index -> door + balcony (galleries only)


def T(s, u0, u1, x0, x1, y0, y1, mat, dest=True):
    """Box in tower-local coordinates: u from the canyon face outward, x along the face."""
    za, zb = s * (GAP + u0), s * (GAP + u1)
    L.block(x0, x1, y0, y1, min(za, zb), max(za, zb), mat, dest)


def ramp(s, xa, ya, xb, yb, uc, hz, mat="concrete", th=0.4):
    """Walkable ramp, top surface from (xa, ya) to (xb, yb) along x, centred at u = uc, half width hz (rolled about Z)."""
    if xb < xa:
        xa, ya, xb, yb = xb, yb, xa, ya
    t = math.atan2(yb - ya, xb - xa)
    ln = math.hypot(xb - xa, yb - ya)
    assert abs(t) <= math.radians(24.0), "ramp too steep: %.1f deg" % math.degrees(t)
    cx = (xa + xb) / 2 + th * math.sin(t)
    cy = (ya + yb) / 2 - th * math.cos(t)
    L.dbox(cx, cy, s * (GAP + uc), ln / 2, th, hz, L.hp_for(ln / 2, th, hz, mat), t)


def door_set(k):
    """Facade brick indices opened at level k (doors for bridges and balconies)."""
    d = set()
    for (lv, idx, _m) in BRIDGES:
        if lv == k:
            d.add(idx)
    for i in BALCONIES.get(k, ()):
        d.add(i)
    return d


def windows(layer):
    """Brick indices (shifted layer, 1..7) opened as windows in an even layer."""
    if layer < 2 or layer > 30:
        return set()
    r = (layer * 5 + 2) % 7
    return {1 + r, 1 + (r + 3) % 7} if layer % 4 == 0 else {1 + (r + 1) % 7}


def facade(s):
    L.section("facade %s" % ("south" if s < 0 else "north"))
    for li, (y0, y1) in enumerate(LAYERS):
        if li % 2 == 1:  # door layer of level (li-1)/2
            k = (li - 1) // 2
            skip = door_set(k)
            cuts = CUT_U
        else:
            skip = windows(li)
            cuts = CUT_S
        for i in range(len(cuts) - 1):
            if i in skip:
                continue
            T(s, 0.0, 2.0, cuts[i], cuts[i + 1], y0, y1, "brick")
    # parapet on top of the roof edge
    for i in range(len(CUT_U) - 1):
        T(s, 0.0, 2.0, CUT_U[i], CUT_U[i + 1], 202.0, 206.0, "brick")


def piers(s):
    L.section("piers %s" % ("south" if s < 0 else "north"))
    outer = [(-21.0, -15.0), (-3.0, 3.0), (15.0, 21.0)]
    ends = [(2.0, 8.0), (18.0, 24.0)]
    for (y0, y1) in LAYERS:
        for (xa, xb) in outer:
            T(s, 24.0, 26.0, xa, xb, y0, y1, "concrete")
        for (ua, ub) in ends:
            T(s, ua, ub, -21.0, -19.0, y0, y1, "concrete")
            T(s, ua, ub, 19.0, 21.0, y0, y1, "concrete")
    for (xa, xb) in outer:
        T(s, 24.0, 26.0, xa, xb, 202.0, 206.0, "concrete")


UC = [2.0, 8.0, 14.0, 20.0, 24.0]


def plate_grid(s, xcuts, ucuts, k, mat="concrete", hole=None):
    top = LEVEL_Y(k)
    for i in range(len(xcuts) - 1):
        for j in range(len(ucuts) - 1):
            if hole and hole(xcuts[i], xcuts[i + 1], ucuts[j], ucuts[j + 1]):
                continue
            T(s, ucuts[j], ucuts[j + 1], xcuts[i], xcuts[i + 1], top - 2.0, top, mat)


def floors(s):
    L.section("floors %s" % ("south" if s < 0 else "north"))
    # lobby (level 0): the whole shaft floor, so a fall inside the tower is not a fall into the void
    plate_grid(s, [-19.0, -15.0, -9.0, -3.0, 3.0, 9.0, 15.0, 19.0], UC, 0)
    # landings: the arrival end of every other level
    for k in range(1, 16):
        xc = [-19.0, -15.0] if k % 2 == 0 else [15.0, 19.0]
        plate_grid(s, xc, UC, k)
    for k in GALLERIES:  # a walkway along the facade, level with the landing
        plate_grid(s, [-15.0, -9.0, -3.0, 3.0, 9.0, 15.0], [2.0, 6.0], k, "concrete")
    # roof (level 16): the whole deck, with the lane-B stair well left open
    def hole(xa, xb, ua, ub):
        return xa >= -15.0 - 1e-6 and xb <= 15.0 + 1e-6 and ua >= 14.0 - 1e-6 and ub <= 20.0 + 1e-6
    plate_grid(s, [-19.0, -15.0, -9.0, -3.0, 3.0, 9.0, 15.0, 19.0], UC, 16, "concrete", hole)
    # the ramps
    for k in range(16):
        if k % 2 == 0:
            ramp(s, -15.0, LEVEL_Y(k), 15.0, LEVEL_Y(k + 1), 9.0, 3.0)
        else:
            ramp(s, -15.0, LEVEL_Y(k + 1), 15.0, LEVEL_Y(k), 17.0, 3.0)


def balconies(s):
    L.section("balconies %s" % ("south" if s < 0 else "north"))
    for k, idxs in BALCONIES.items():
        top = LEVEL_Y(k)
        for i in idxs:
            T(s, -4.0, 0.0, CUT_U[i], CUT_U[i + 1], top - 2.0, top, "steel")


def bridges():
    L.section("bridges")
    out = []
    for (k, idx, mat) in BRIDGES:
        top = LEVEL_Y(k)
        if idx == 0:
            xa, xb = -19.0, -15.0
        elif idx == 6:
            xa, xb = 15.0, 19.0
        else:
            xa, xb = CUT_U[idx], CUT_U[idx + 1]
        for (za, zb) in [(-12.0, -6.0), (-6.0, 0.0), (0.0, 6.0), (6.0, 12.0)]:
            L.block(xa, xb, top - 2.0, top, za, zb, mat)
        out.append((k, idx, (xa + xb) / 2, top))
    return out


# ------------------------------------------------------------------------------------------------ boxes registry for overlap tests
def all_boxes():
    for _, items in L.sections:
        for it in items:
            yield it


def free(x0, x1, y0, y1, z0, z1, m=1.0):
    for (cx, cy, cz, hx, hy, hz, rot, hp) in all_boxes():
        e = abs(math.cos(rot)) * hx + abs(math.sin(rot)) * hy
        f = abs(math.sin(rot)) * hx + abs(math.cos(rot)) * hy
        if cx - e < x1 + m and cx + e > x0 - m and cy - f < y1 + m and cy + f > y0 - m and cz - hz < z1 + m and cz + hz > z0 - m:
            return False
    return True


def put(x0, x1, y0, y1, z0, z1, mat, m=1.0):
    if not free(x0, x1, y0, y1, z0, z1, m):
        return False
    L.block(x0, x1, y0, y1, z0, z1, mat)
    return True


# ------------------------------------------------------------------------------------------------ build
for s in SIDES:
    facade(s)
    piers(s)
    floors(s)
    balconies(s)
bl = bridges()

# ---- canyon: gondolas
L.section("gondolas (hook only)")
gondolas = []  # (x, top, z)
for m in range(1, 15):
    top = 16.0 + 12.0 * m
    for _try in range(30):
        x = rnd.uniform(-15.0, 15.0)
        z = rnd.uniform(-5.0, 5.0)
        if put(x - 3, x + 3, top - 2, top, z - 3, z + 3, rnd.choice(["steel", "wood", "concrete"])):
            gondolas.append((x, top, z))
            put(x - 3, x + 3, top, top + 3, z + 1, z + 3, "wood", 0.1)  # a wall on the back of the deck
            put(x - 1, x + 1, top + 2, top + 10, z - 1, z + 1, "steel", 0.1)  # mast / hook post
            break
# low extras so the bottom of the canyon has anchors too
for (x, top, z) in [(-10.0, 18.0, -4.0), (9.0, 40.0, 4.0), (-4.0, 52.0, -2.0)]:
    if put(x - 3, x + 3, top - 2, top, z - 3, z + 3, "steel"):
        gondolas.append((x, top, z))

# ---- hanging counterweights
L.section("counterweights")
for i in range(8):
    top = rnd.uniform(40.0, 190.0)
    x, z = rnd.uniform(-17.0, 17.0), rnd.uniform(-7.0, 7.0)
    if put(x - 2, x + 2, top - 8, top, z - 2, z + 2, "concrete", 1.5):
        L.block(x - 1, x + 1, top, top + 12.0, z - 1, z + 1, "steel")  # the cable (nothing holds it: it is a hook line)

# ---- girder tightropes across the canyon
L.section("girders")
for i in range(8):
    y = 20.0 + 22.0 * i + rnd.uniform(-3, 3)
    x = rnd.uniform(-16.0, 16.0)
    if put(x - 1, x + 1, y - 1, y + 1, -12.0, 12.0, "steel", 1.0):
        pass

# ---- cable stays: slanted beams in the x-y plane
L.section("cable stays")
n = 0
tries = 0
while n < 6 and tries < 200:
    tries += 1
    y = rnd.uniform(30.0, 185.0)
    x = rnd.uniform(-14.0, 14.0)
    z = rnd.uniform(-6.0, 6.0)
    ang = math.radians(rnd.choice([-1, 1]) * rnd.uniform(30.0, 55.0))
    ln = 9.0
    if free(x - 8, x + 8, y - 8, y + 8, z - 1.5, z + 1.5, 1.0):
        L.dbox(x, y, z, ln, 1.0, 1.0, L.hp_for(ln, 1.0, 1.0, "steel"), ang)
        n += 1

# ---- rooftops
L.section("roof: scaffolds, gantry, crane")
ROOF = LEVEL_Y(16)
for xs in (-9.0, 9.0):  # scaffolds on the south roof: four posts, top frame
    for (dx, du) in [(-3, -3), (3, -3), (-3, 3), (3, 3)]:
        T(-1, 8 + du - 1, 8 + du + 1, xs + dx - 1, xs + dx + 1, ROOF, ROOF + 6.0, "steel")
        T(-1, 8 + du - 1, 8 + du + 1, xs + dx - 1, xs + dx + 1, ROOF + 6.0, ROOF + 12.0, "steel")
    T(-1, 3, 13, xs - 5, xs + 5, ROOF + 12.0, ROOF + 14.0, "wood")
# north gantry: a lane-A ramp up from the roof to a bow platform (walkable)
SG = 1
ramp(SG, -15.0, ROOF, 15.0, ROOF + 12.0, 9.0, 3.0)
T(SG, 6.0, 12.0, 15.0, 21.0, ROOF + 10.0, ROOF + 12.0, "steel")
for du in (7.0, 11.0):
    T(SG, du - 1, du + 1, 19.0, 21.0, ROOF, ROOF + 6.0, "steel")
    T(SG, du - 1, du + 1, 19.0, 21.0, ROOF + 6.0, ROOF + 10.0, "steel")
L.weapon("yumi", 18.0, ROOF + 12.0 + 1.0, SG * (GAP + 9.0))
# south crane: a mast, a cab with the second bow, a jib reaching over the canyon
SC = -1
mx, mu = 0.0, 22.0
for i in range(4):  # mast 202 .. 226
    T(SC, mu - 2, mu + 2, mx - 2, mx + 2, ROOF + 6.0 * i, ROOF + 6.0 * (i + 1), "steel")
T(SC, mu - 3, mu + 3, mx - 3, mx + 3, ROOF + 24.0, ROOF + 26.0, "steel")  # the cab deck (y 228)
for (za, zb) in [(-31.0, -25.0), (-25.0, -19.0), (-19.0, -13.0), (-13.0, -7.0), (-7.0, -1.0), (-1.0, 5.0), (5.0, 10.0)]:
    L.block(-1.0, 1.0, ROOF + 24.0, ROOF + 26.0, za, zb, "steel")
L.block(-1.0, 1.0, ROOF + 12.0, ROOF + 24.0, -17.0, -15.0, "steel")  # the hook line hanging from the jib
L.block(-2.0, 2.0, ROOF + 8.0, ROOF + 12.0, -18.0, -14.0, "steel")  # the crane block
L.weapon("yumi", mx, ROOF + 26.0 + 1.0, SC * (GAP + mu))

# ---- spawns / pickups / tags
SP = []  # (x, level_top, z)
def land(s, k, u=13.0):
    return ((17.0 if k % 2 else -17.0), LEVEL_Y(k), s * (GAP + u))
SP += [land(-1, k) for k in (1, 4, 7, 10, 13)]
SP += [land(1, k) for k in (0, 3, 6, 9, 12, 15)]
SP += [(-9.0, ROOF, -1 * (GAP + 5.0)), (-9.0, ROOF, GAP + 4.0)]
SP += [(0.0, LEVEL_Y(2), -3.0), (-6.0, LEVEL_Y(5), 3.0), (-12.0, LEVEL_Y(14), -3.0)]
for (x, top, z) in SP:
    L.spawn(x, top + 0.81, z)
for (k, x, z) in [(6, -17.0, 3.0), (8, 6.0, 3.0), (11, 0.0, 3.0), (12, -17.0, 3.0)]:
    L.weapon("tag", x, LEVEL_Y(k) + 1.0, z)
cubes = [(g[0], g[1] + 1.0, g[2]) for g in gondolas[:3]]
cubes += [(-17.0, LEVEL_Y(8) + 1.0, -1 * (GAP + 18.0)), (17.0, LEVEL_Y(11) + 1.0, GAP + 8.0), (17.0, LEVEL_Y(7) + 1.0, -1 * (GAP + 8.0))]
for c in cubes:
    L.pickup(*c)

out = sys.argv[1] if len(sys.argv) > 1 else "levels/ffa_split.lvl"
L.write(out, header=[
    "SHINOBI FFA SPLIT: two 198 m brick towers 24 m apart (a vertical canyon, floating, kill plane y=-20). GENERATED by scripts/gen_split_level.py.",
    "Edit the script, not this file; tests: crates/shinobi-sim/tests/arena_split.rs, walkable.rs, crates/shinobi-ai/tests/split.rs.",
])
