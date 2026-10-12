#!/usr/bin/env python3
"""Generates levels/ffa_sky.lvl: a floating vertical city (232 x 190 m, 200 m tall, no ground, kill plane y = -20).

    python3 scripts/gen_sky_level.py [out.lvl]

The ground-level structures of the old maps (towers, courtyards, canyons, terraces, bridges) lifted into the air and
stacked in FOUR TIERS whose decks sit at y = 24 / 55 / 86 / 117. Each tier is a "band" 40 m deep and ~135 m long made of two
rows of floating plates (a street between them), the plates joined by flat catwalks (a few gaps are left open: jump or
grapple). The bands are chained into one walkable serpentine by long 24 degree ramps (x direction only: the level format
rolls boxes about Z) and turn plazas:

    tier 0 (y 24, z -95..-55)  ->  ramp east  ->  plaza  ->  tier 1 (y 55, z -45..-5)
    tier 1  ->  ramp west  ->  plaza  ->  tier 2 (y 86, z 5..45)  ->  ramp east  ->  plaza  ->  tier 3 (y 117, z 55..95)

so a bot (or a player who does not want the hook) can WALK to every tier. Nothing needs a jump: every obstacle is a lip
under the step height, has a ramp beside it, or is a full wall (>= 3 m); see crates/shinobi-sim/tests/walkable.rs.
Above and around the bands: crown towers up to y = 200, five free-floating satellite towers (grapple / wall-run only),
and a central spire. Hanging plate bodies and tower flanks give 14 to 80 m wall-run faces everywhere.

Cfg limits used: step 0.35 m, ramps 24 degrees (slope limit 45), double jump ~2.8 m, grapple 400 m.
Tests: crates/shinobi-sim/tests/arena_sky.rs, walkable.rs, crates/shinobi-ai/tests/sky.rs, sky_nav.rs
"""
import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import runwalls  # noqa: E402

H = [24.0, 55.0, 86.0, 117.0]  # deck heights per tier
ZC = [-75.0, -25.0, 25.0, 75.0]  # band centre z
ROW_HD = 8.5  # row half depth (rows are 17 m deep)
ROW_OFF = 11.5  # row centre offset from the band centre (street = 6 m)
GAP = 7.0  # open gap between plates in a row
BRIDGE_HW = 2.5  # catwalk half width
BAND_X = [(-100.0, 34.0), (-30.0, 104.0), (-100.0, 30.0), (-30.0, 100.0)]
RAMP_DY = H[1] - H[0]  # 31 m per tier

BOXES = []  # (title, [boxes])
_cur = None


def section(title):
    global _cur
    _cur = []
    BOXES.append((title, _cur))


def add(cx, cy, cz, hx, hy, hz, rot=0.0, hp=0.0):
    """hp > 0 makes the box DESTRUCTIBLE (a `dbox` line, crates/shinobi-sim/src/breaks.rs); 0 = permanent."""
    _cur.append((cx, cy, cz, hx, hy, hz, rot, hp))


def f(v):
    s = ("%.4f" % v).rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def slab(x0, x1, z0, z1, top, depth, hp=0.0):
    x0, x1 = min(x0, x1), max(x0, x1)
    z0, z1 = min(z0, z1), max(z0, z1)
    add((x0 + x1) / 2, top - depth / 2, (z0 + z1) / 2, (x1 - x0) / 2, depth / 2, (z1 - z0) / 2, 0.0, hp)


def block(cx, cz, hx, hz, y0, y1, hp=0.0):
    add(cx, (y0 + y1) / 2, cz, hx, (y1 - y0) / 2, hz, 0.0, hp)


def tower(cx, cz, hx, hz, y0, y1, hp_per_seg, seg=12.0):
    """A destructible tower stacked from segments of at most `seg` m (cut at the foot, the rest falls in a chain)."""
    n = max(1, int(math.ceil((y1 - y0) / seg - 1e-6)))
    h = (y1 - y0) / n
    for i in range(n):
        block(cx, cz, hx, hz, y0 + i * h, y0 + (i + 1) * h, hp_per_seg)


def ramp(xa, ya, xb, yb, zc, hz, th=0.4):
    """A ramp whose TOP surface runs from (xa, ya) to (xb, yb), thick `th`, width 2 hz (rolled about Z)."""
    if xb < xa:
        xa, ya, xb, yb = xb, yb, xa, ya
    t = math.atan2(yb - ya, xb - xa)
    ln = math.hypot(xb - xa, yb - ya)
    add((xa + xb) / 2 + th * math.sin(t), (ya + yb) / 2 - th * math.cos(t), zc, ln / 2, th, hz, t)


class Rng:
    def __init__(self, seed):
        self.s = seed

    def u(self):
        self.s = (self.s * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        return (self.s >> 33) / float(1 << 31)

    def pick(self, seq):
        return seq[int(self.u() * len(seq)) % len(seq)]

    def rng(self, a, b):
        return a + (b - a) * self.u()


rng = Rng(0x534b59)

# decoration landmarks: the renderer recognises these exact half extents (shinobi_sim::level::TORII_POST_HALF / PYLON_HALF)
TORII_POST = (0.4, 2.75, 0.4)
PYLON = (0.35, 1.8, 0.35)
gate_sites = []  # (axis of walking, cx, cz, deck top): torii candidates on catwalks


def occupied(cx, cz, y0, y1, hx, hz):
    """Does any existing box overlap this footprint and height span? (rolled boxes by their bounding box)"""
    for _, items in BOXES:
        for (bx, by, bz, bhx, bhy, bhz, rot, _hp) in items:
            ex = abs(math.cos(rot)) * bhx + abs(math.sin(rot)) * bhy
            ey = abs(math.sin(rot)) * bhx + abs(math.cos(rot)) * bhy
            if abs(bx - cx) < ex + hx - 0.01 and abs(bz - cz) < bhz + hz - 0.01 and abs(by - (y0 + y1) / 2) < ey + (y1 - y0) / 2 - 0.01:
                return True
    return False


def tile(xa, xb, lens, gap=GAP):
    """Plates tiling [xa, xb] with `gap` between them; lens cycle, the last plate is stretched to end exactly at xb."""
    out, x, i = [], xa, 0
    while True:
        ln = lens[i % len(lens)]
        i += 1
        if x + ln + gap + 20.0 > xb:
            out.append((x, xb))
            return out
        out.append((x, x + ln))
        x += ln + gap


spawns = []
pickups = []
terrace_tops = []

# --------------------------------------------------------------------------------------------- the four bands
plates_all = []  # (tier, row, idx, x0, x1, zc) for the cross-checking below


def band(k):
    xa, xb = BAND_X[k]
    zc, top = ZC[k], H[k]
    rows = {"S": zc - ROW_OFF, "N": zc + ROW_OFF}
    # plate lengths differ between the rows and tiers so the skyline is not a grid
    lens = {"S": [28.0, 24.0, 32.0, 26.0], "N": [24.0, 30.0, 26.0, 34.0]}
    if k % 2:
        lens = {"S": lens["N"][::-1], "N": lens["S"][::-1]}
    # which plates must stay empty (ramp feet / heads and plaza joins)
    keep_clear = {(0, "S", "last"), (1, "S", "last"), (1, "N", "first"), (2, "S", "first"), (2, "N", "last"), (3, "S", "last")}
    section("tier %d plates, catwalks and buildings (deck y = %g)" % (k, top))
    row_plates = {}
    cand = []
    for r, rz in rows.items():
        out_sign = -1.0 if r == "S" else 1.0
        plates = tile(xa, xb, lens[r])
        row_plates[r] = plates
        for i, (x0, x1) in enumerate(plates):
            depth = 14.0 + 3.0 * (i % 3) + (2.0 if k else 0.0)
            depth = min(depth, top - 8.0)
            slab(x0, x1, rz - ROW_HD, rz + ROW_HD, top, depth)
            plates_all.append((k, r, i, x0, x1, rz))
            cx, ln = (x0 + x1) / 2, x1 - x0
            first, last = i == 0, i == len(plates) - 1
            clear = (k, r, "first") in keep_clear and first or (k, r, "last") in keep_clear and last
            # hanging keel: a wall-run spine under tiers 1..3
            if k and ln > 20:
                block(cx, rz, 2.5, 2.5, top - depth - 12.0, top - depth)
            if not clear:
                dress(k, r, rz, out_sign, i, x0, x1, top)
            # spawn candidates: plate centres on the inward side of the row (buildings sit on the outer side)
            if not clear and ln >= 24.0:
                cand.append((cx, rz - out_sign * 3.0, top))
        # catwalks across the open gaps (the second gap of the N row stays open: jump or grapple)
        for i in range(len(plates) - 1):
            g0, g1 = plates[i][1], plates[i + 1][0]
            open_gap = r == "N" and i == 1 and k != 2
            if not open_gap:
                slab(g0 - 0.5, g1 + 0.5, rz - BRIDGE_HW, rz + BRIDGE_HW, top, 1.0)
                gate_sites.append(("x", (g0 + g1) / 2, rz, top))
    # cross-street catwalks: every N plate gets one to an overlapping S plate; plus every other overlap
    n = 0
    for (x0, x1) in row_plates["N"]:
        best = None
        for (s0, s1) in row_plates["S"]:
            lo, hi = max(x0, s0), min(x1, s1)
            if hi - lo >= 8.0 and (best is None or hi - lo > best[1] - best[0]):
                best = (lo, hi)
        if best:
            cx = (best[0] + best[1]) / 2
            slab(cx - BRIDGE_HW, cx + BRIDGE_HW, zc - ROW_OFF + ROW_HD - 0.5, zc + ROW_OFF - ROW_HD + 0.5, top, 1.0)
            gate_sites.append(("z", cx, zc, top))
            n += 1
    # five spawns per tier, spread along the band (every plate is on the walkable network)
    cand.sort()
    step = max(1, len(cand) // 5)
    spawns.extend(cand[::step][:5])


FEATURE_W ={"tower": 7.0, "tower2": 6.0, "canyon": 20.0, "terrace": 24.0, "pillars": 14.0, "arcade": 13.0, "chicane": 14.0}
KINDS = ["tower", "canyon", "terrace", "pillars", "arcade", "chicane", "tower2"]


def dress(k, r, rz, out_sign, i, x0, x1, top):
    """Buildings on one plate, all on the OUTER side of the row so the walkway along the row and the catwalk mouths stay
    free. Greedy layout from the plate's two ends inward: each feature takes its width from the free span."""
    o = lambda off: rz + out_sign * off  # z at `off` metres outward of the row centre line
    lo, hi = x0 + 3.0, x1 - 3.0
    order = [KINDS[(i * 3 + 2 * k + (1 if r == "N" else 0) + j * 2) % len(KINDS)] for j in range(5)]
    left = (i + k) % 2 == 0
    if x1 - x0 >= 30.0 and (i + k) % 2 == 0:
        order = ["terrace"] + [kd for kd in order if kd != "terrace"]  # long plates get a raised roof with a ramp up
    placed = 0
    for kind in order:
        w = FEATURE_W[kind]
        if hi - lo < w + 2.0 or placed >= 4:
            continue
        cx = (lo + w / 2) if left else (hi - w / 2)
        th = rng.pick([14.0, 18.0, 24.0]) + 5.0 * k + (rng.rng(0, 16) if k == 3 else 0.0)
        if kind == "tower":
            if k == 3:  # the crown: tier 3 towers climb toward y = 200
                th = rng.pick([55.0, 70.0, 83.0])
            tower(cx, o(5.5), 3.0, 3.0, top, top + th, 320.0)
        elif kind == "tower2":
            tower(cx, o(5.8), 2.5, 2.5, top, top + th * 0.55, 280.0)
        elif kind == "canyon":  # two walls 4.5 m apart: a wall-run lane that is also a corridor (open ends, no roof)
            for off in (1.2, 7.4):
                block(cx, o(off), 9.0, 0.6, top, top + 12.0 + 2.0 * k, 260.0)
        elif kind == "terrace":  # a raised roof (top + 5) with a 22 degree ramp up to it along x
            pw, rise = 12.0, 5.0
            run = rise / 0.45
            if left:
                px0, px1 = lo, lo + pw
                ramp(px1, top + rise, px1 + run, top, o(6.0), 2.0)
            else:
                px0, px1 = hi - pw, hi
                ramp(px0 - run, top, px0, top + rise, o(6.0), 2.0)
            block((px0 + px1) / 2, o(5.5), pw / 2, 3.0, top, top + rise)
            terrace_tops.append(((px0 + px1) / 2, top + rise, o(5.5)))
            cx = (lo + w / 2) if left else (hi - w / 2)
        elif kind == "pillars":  # full-height cover walls (3.5 m: nothing waist high)
            for dx in (-4.0, 4.0):
                block(cx + dx, o(2.8), 3.0, 0.6, top, top + 3.5, 180.0)
        elif kind == "arcade":  # a colonnade of 2 m columns (6 m full walls) with 5 m between them
            for dx in (-5.0, 0.0, 5.0):
                block(cx + dx, o(4.0), 1.0, 1.0, top, top + 6.0, 140.0)
        else:  # chicane: two staggered baffle walls
            block(cx - 3.0, o(2.5), 4.0, 0.6, top, top + 3.5, 180.0)
            block(cx + 3.0, o(6.5), 4.0, 0.6, top, top + 3.5, 180.0)
        if left:
            lo += w + 2.0
        else:
            hi -= w + 2.0
        left = not left
        placed += 1
    # balconies: deck-level ledges jutting 4 m into the void on the outer edge (walkable, flush with the deck)
    ln = x1 - x0
    if ln >= 24.0:
        for fx in (0.28, 0.72):
            bx = x0 + ln * fx
            slab(bx - 4.0, bx + 4.0, min(o(ROW_HD), o(ROW_HD + 4.0)) - 0.5, max(o(ROW_HD), o(ROW_HD + 4.0)) + 0.0, top, 3.0)


for k in range(4):
    band(k)

# --------------------------------------------------------------------------------------------- ramps and plazas
section("ramps (24 degrees, x direction) and turn plazas that chain the four tiers into one walkable serpentine")
# tier 0 -> 1: east along the south row of band 0
ramp(34.0, H[0], 104.0, H[1], ZC[0] - ROW_OFF, 4.0)
slab(104.0, 116.0, -92.0, -28.0, H[1], 12.0)
# tier 1 -> 2: west along the north row of band 1
ramp(-30.0, H[1], -100.0, H[2], ZC[1] + ROW_OFF, 4.0)
slab(-116.0, -100.0, -18.0, 22.0, H[2], 12.0)
# tier 2 -> 3: east along the north row of band 2
ramp(30.0, H[2], 100.0, H[3], ZC[2] + ROW_OFF, 4.0)
slab(100.0, 116.0, 32.0, 72.0, H[3], 12.0)

# --------------------------------------------------------------------------------------------- crown, satellites, spire
section("satellite towers (free floating: grapple or wall-run only) and the central spire")
sats = [
    (78.0, -70.0, 14.0, 96.0, 5.0),
    (-72.0, -36.0, 26.0, 132.0, 6.0),
    (74.0, 14.0, 44.0, 158.0, 6.0),
    (-68.0, 72.0, 56.0, 178.0, 6.0),
    (0.0, 0.0, 38.0, 200.0, 3.5),
]
for cx, cz, y0, y1, hw in sats:
    block(cx, cz, hw, hw, y0, y1)
    pad = hw + 5.0
    slab(cx - pad, cx + pad, cz - pad, cz + pad, y1, 1.5) if hw > 4 else None
    # ledges on the faces (landing pads while wall-running / hooking), spaced up the shaft
    for j, yl in enumerate([y0 + (y1 - y0) * 0.35, y0 + (y1 - y0) * 0.68]):
        sgn = 1.0 if j == 0 else -1.0
        slab(cx + sgn * hw, cx + sgn * (hw + 7.0), cz - 3.5, cz + 3.5, yl, 1.0)
        pickups_ledge = (cx + sgn * (hw + 3.5), yl + 1.0, cz)
        if j == 1 and cz in (-36.0, 14.0, 72.0):
            pickups.append((pickups_ledge[0], pickups_ledge[1], pickups_ledge[2], 25))

section("floating islands (disconnected: grapple or wall-run chains; kept out of the columns above any walkable deck)")
islands = [  # (cx, cz, top)
    (62.0, -56.0, 44.0), (96.0, -60.0, 72.0),    # east of tier 0
    (-50.0, -30.0, 74.0), (-96.0, -38.0, 104.0),  # west of tier 1
    (50.0, 14.0, 104.0), (96.0, 18.0, 138.0),     # east of tier 2
    (-48.0, 84.0, 134.0), (-96.0, 70.0, 164.0),   # west of tier 3
]
for j, (cx, cz, top) in enumerate(islands):
    slab(cx - 9.0, cx + 9.0, cz - 7.0, cz + 7.0, top, 8.0)
    tower(cx - 3.0 if j % 2 else cx + 3.0, cz, 2.5, 2.5, top, top + 12.0 + 3.0 * (j % 3), 260.0)
    slab(cx + 9.0, cx + 14.0, cz - 3.0, cz + 3.0, top, 1.0)  # a walkable lip off the +x edge
    if j in (1, 4, 6):
        pickups.append((cx + (-3.0 if j % 2 else 3.0) + 5.0, top + 1.0, cz, 25))

# --------------------------------------------------------------------------------------------- shinobi architecture
section("pagoda daises (stepped 0.3 m: walkable) on tower tops, torii posts on catwalks and spawn decks, stone-lantern pylons")
# 1. pagoda daises: two inset 0.3 m steps on every tower top (the renderer floats a curved-eave pavilion canopy above)
towers = []
for _, items in BOXES[:-1]:
    for (bx, by, bz, bhx, bhy, bhz, rot, _hp) in items:
        if rot == 0 and abs(bhx - bhz) < 0.01 and 2.4 <= bhx <= 6.1 and bhy * 2 >= 7.0:
            towers.append((bx, by + bhy, bz, bhx, bhz))
for (tx, ttop, tz, thx, thz) in towers:
    if not occupied(tx, tz, ttop + 0.02, ttop + 0.6, thx - 0.5, thz - 0.5):
        add(tx, ttop + 0.15, tz, thx - 0.5, 0.15, thz - 0.5, 0.0, 40.0)
        add(tx, ttop + 0.45, tz, thx - 1.2, 0.15, thz - 1.2, 0.0, 40.0)

# 2. torii gates over every third catwalk: a small shrine pad widens the deck (flush with it) and the posts stand on the pad
#    OUTSIDE the 5 m walkway (3.1 m from the centre line), so the whole catwalk stays clear between them
for i, (axis, cx, cz, top) in enumerate(gate_sites):
    if i % 3 != 1:
        continue
    if axis == "x":
        pad = (cx - 1.4, cx + 1.4, cz - 3.9, cz + 3.9)
        posts = [(cx, cz - 3.1), (cx, cz + 3.1)]
    else:
        pad = (cx - 3.9, cx + 3.9, cz - 1.4, cz + 1.4)
        posts = [(cx - 3.1, cz), (cx + 3.1, cz)]
    slab(pad[0], pad[1], pad[2], pad[3], top, 1.0)
    for (px, pz) in posts:
        if not occupied(px, pz, top + 0.01, top + 5.5, TORII_POST[0], TORII_POST[2]):
            add(px, top + TORII_POST[1], pz, *TORII_POST, 0.0, 90.0)

# ... and framing every second spawn (posts 3.2 m either side along x)
for i, (sx, sz, top) in enumerate(spawns):
    if i % 2:
        continue
    for sgn in (-1, 1):
        px = sx + sgn * 3.2
        if not occupied(px, sz, top + 0.01, top + 5.5, TORII_POST[0], TORII_POST[2]):
            add(px, top + TORII_POST[1], sz, *TORII_POST, 0.0, 90.0)

# 3. stone-lantern pylons: one near the outer end of every third plate, on the outer rim
pyl = 0
for (k, r, i, x0, x1, rz) in plates_all:
    if i % 3 != 1 or r != "N":
        continue
    top = H[k]
    pz = rz + ROW_HD - 1.0
    px = x1 - 1.6
    if not occupied(px, pz, top + 0.01, top + 3.6, PYLON[0], PYLON[2]):
        add(px, top + PYLON[1], pz, *PYLON, 0.0, 60.0)
        pyl += 1

# --------------------------------------------------------------------------------------------- pickups
# on terrace roofs (reachable on foot by ramp): one per tier, the rest sit on the satellite ledges above
by_tier = {}
for (x, y, z) in terrace_tops:
    t = min(range(4), key=lambda i: abs(H[i] + 5.0 - y))
    by_tier.setdefault(t, []).append((x, y, z))
for t in range(4):
    if t in by_tier:
        x, y, z = by_tier[t][len(by_tier[t]) // 2]
        pickups.append((x, y + 1.0, z, 25))

# --------------------------------------------------------------------------------------------- run walls
# long tall wall-run panels, each with a hook anchor 8 m past its end and 2 m above its top (scripts/runwalls.py); permanent
section("run walls: 20 m x 7 m wall-run panels with a hook anchor beside each")
for _kind, _cx, _cy, _cz, _hx, _hy, _hz in runwalls.boxes("sky"):
    add(_cx, _cy, _cz, _hx, _hy, _hz)

# --------------------------------------------------------------------------------------------- power-weapon pedestals
# docs/powerups.md: two YUMI (digital bow) on the highest towers (the central spire and the east crown tower: grapple or wall-run
# up, rare, respawn 45 s) and four explosive TAG bags where the routes meet (the three turn plazas of the serpentine and the
# tier-1 terrace). Pedestal height = dais top + 1 m, like every pickup.
WEAPON_RESPAWN = 45
weapons = [
    ("yumi", 0.0, 200.0 + 0.6 + 1.0, 0.0),          # central spire (dais top 200.6)
    ("yumi", 48.5, 187.0 + 0.6 + 1.0, 92.0),        # crown tower of tier 3 (top 187 + dais)
    ("tag", 110.0, H[1] + 1.0, -60.0),              # plaza tier 0 -> 1
    ("tag", -108.0, H[2] + 1.0, 2.0),               # plaza tier 1 -> 2
    ("tag", 108.0, H[3] + 1.0, 52.0),               # plaza tier 2 -> 3
    ("tag", -5.0, H[1] + 5.0 + 1.0, -42.0),         # tier-1 terrace roof (the one no cube uses)
]

# --------------------------------------------------------------------------------------------- permanence
# a pedestal (pickup / bow / tag) must never lose its floor: every box in a column under one is made permanent
for title, items in BOXES:
    for k, (cx, cy, cz, hx, hy, hz, rot, hp) in enumerate(items):
        if not hp:
            continue
        for (px, py, pz) in [(p[0], p[1], p[2]) for p in pickups] + [(w[1], w[2], w[3]) for w in weapons]:
            if abs(px - cx) <= hx + 0.6 and abs(pz - cz) <= hz + 0.6 and py > cy - hy:
                items[k] = (cx, cy, cz, hx, hy, hz, rot, 0.0)
                break

# --------------------------------------------------------------------------------------------- write
lines = [
    "# SHINOBI FFA SKY - a floating vertical city: 232 x 190 m, 200 m tall, four tiers at y 24 / 55 / 86 / 117, no ground (kill plane y=-20).",
    "# GENERATED by scripts/gen_sky_level.py. Edit the script, not this file; tests: crates/shinobi-sim/tests/arena_sky.rs, walkable.rs",
    "# x right, y up. Plates + catwalks + towers per tier, 24 degree ramps and plazas chain the tiers on foot, crown towers to 200 m,",
    "# satellite towers and a central spire for the hook and wall-runs. Nothing needs a jump: lips <= step, ramps, or full walls.",
    "",
]
n = 0
for title, items in BOXES:
    lines.append("# --- %s ---" % title)
    for (cx, cy, cz, hx, hy, hz, rot, hp) in items:
        if hp:
            lines.append("dbox %s %s %s  %s %s %s  %s %s" % (f(cx), f(cy), f(cz), f(hx), f(hy), f(hz), f(rot), f(hp)))
            n += 1
            continue
        row = "box %s %s %s  %s %s %s" % (f(cx), f(cy), f(cz), f(hx), f(hy), f(hz))
        if rot:
            row += " " + f(rot)
        lines.append(row)
        n += 1
    lines.append("")
lines.append("# --- %d spawns (deck level, spread over all four tiers so contact is quick) ---" % len(spawns))
for x, z, top in spawns:
    lines.append("spawn %s %s %s" % (f(x), f(top + 0.81), f(z)))
lines.append("")
lines.append("# --- %d pickups ---" % len(pickups))
for x, y, z, r in pickups:
    lines.append("pickup %s %s %s %s" % (f(x), f(y), f(z), f(r)))
lines.append("")
lines.append("# --- %d power-weapon pedestals (yumi = bow, tag = explosive fuda tags; respawn %d s) ---" % (len(weapons), WEAPON_RESPAWN))
for kind, x, y, z in weapons:
    lines.append("%s %s %s %s %s" % (kind, f(x), f(y), f(z), f(WEAPON_RESPAWN)))

out = sys.argv[1] if len(sys.argv) > 1 else "levels/ffa_sky.lvl"
open(out, "w").write("\n".join(lines) + "\n")
print("wrote %s: %d boxes, %d spawns, %d pickups, %d weapon pedestals" % (out, n, len(spawns), len(pickups), len(weapons)))
