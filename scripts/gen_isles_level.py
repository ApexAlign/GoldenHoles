#!/usr/bin/env python3
"""Generates levels/ffa_isles.lvl: SKYFALL ARCHIPELAGO, a floating-islands arena (no ground, kill plane y = -20).

    python3 scripts/gen_isles_level.py [out.lvl]

One central ziggurat (HEAVEN'S GATE, deck y 60) with four walkable spokes to four themed islands, each at its own height:

    east   ramp  down to y 45   MOON GATE   a 34 m vertical ring hung over the deck, a floating shrine disc in its eye, a bow on its crown
    west   ramp  up   to y 80   CASCADE     rice terraces, a 46 m waterfall wall to wall-run, a pagoda on the high terrace
    north  stairs down to y 48  BAMBOO      a grid of thin (some leaning) 1 m stalks, 10 to 40 m tall: a wall-run / hook forest
    south  stairs down to y 50  DRAGON BOWL a sunken 24 m duel pit with a 24 degree ramp in on each side and a rim of cover

Above the ziggurat a 66 m SPIRE climbs to the SKY SANCTUM (an inverted-pagoda island at y 140 carrying the bow, a pavilion roof
to y 150) and a spiral of 4 m floating STONES winds round it (jump / dash / double jump parkour route, no hook needed).
Four free-floating SHARDS (rock spires on stacked cones) sit in the corners, and ~28 drifting rock mini-islands scatter the
void: stepping stones, wall-run faces and hook anchors. Spokes (ramps 24 degrees, stairs 0.3 m rise) join the five decks, so a
bot can WALK to every island; the sanctum, hub shrine, shards and ring are hook / wall-run / jump only.

Cfg limits used: step 0.35 m, ramps <= 24 degrees, jump ~2.8 m double, grapple 1000 m.
Tests: crates/shinobi-sim/tests/arena_isles.rs, walkable.rs
"""
import math
import sys

BOXES = []
_cur = None


def section(title):
    global _cur
    _cur = []
    BOXES.append((title, _cur))


def add(cx, cy, cz, hx, hy, hz, rot=0.0):
    _cur.append((cx, cy, cz, hx, hy, hz, rot))


def f(v):
    s = ("%.4f" % v).rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def slab(x0, x1, z0, z1, top, depth):
    x0, x1 = min(x0, x1), max(x0, x1)
    z0, z1 = min(z0, z1), max(z0, z1)
    add((x0 + x1) / 2, top - depth / 2, (z0 + z1) / 2, (x1 - x0) / 2, depth / 2, (z1 - z0) / 2)


def block(cx, cz, hx, hz, y0, y1):
    add(cx, (y0 + y1) / 2, cz, hx, (y1 - y0) / 2, hz)


def ramp(xa, ya, xb, yb, zc, hz, th=0.4):
    """Ramp whose TOP surface runs (xa, ya) -> (xb, yb), thickness th, width 2 hz (rolled about Z)."""
    if xb < xa:
        xa, ya, xb, yb = xb, yb, xa, ya
    t = math.atan2(yb - ya, xb - xa)
    ln = math.hypot(xb - xa, yb - ya)
    assert abs(t) <= math.radians(24.2), "ramp too steep: %.1f deg" % math.degrees(t)
    add((xa + xb) / 2 + th * math.sin(t), (ya + yb) / 2 - th * math.cos(t), zc, ln / 2, th, hz, t)


STEP = 0.3  # stair rise (autostep is 0.35)
RUN = 1.0  # stair run per step


def stairs_z(x, hx, z0, y0, direction, n, depth=1.6):
    """A floating staircase from (z0, y0) going in `direction` (+1/-1 along z), n steps descending (n<0: ascending).
    Returns (z_end, y_end): where the next flat deck starts. Step i has top y0 - sgn*(i+1)*STEP."""
    sgn = 1 if n > 0 else -1
    for i in range(abs(n)):
        ya = z0 + direction * i * RUN
        yb = z0 + direction * (i + 1) * RUN + direction * 0.05
        slab(x - hx, x + hx, min(ya, yb), max(ya, yb), y0 - sgn * (i + 1) * STEP, depth)
    return z0 + direction * abs(n) * RUN, y0 - sgn * abs(n) * STEP


def stairs_x(zc, hz, x0, y0, direction, n, run, depth=1.6):
    """Floating staircase along x: n steps rising 0.3 m each from (x0, y0) toward `direction` (+1/-1)."""
    for i in range(n):
        xa = x0 + direction * i * run
        xb = x0 + direction * (i + 1) * run + direction * 0.05
        slab(min(xa, xb), max(xa, xb), zc - hz, zc + hz, y0 + (i + 1) * STEP, depth)


class Rng:
    def __init__(self, seed):
        self.s = seed

    def u(self):
        self.s = (self.s * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        return (self.s >> 33) / float(1 << 31)

    def rng(self, a, b):
        return a + (b - a) * self.u()

    def pick(self, seq):
        return seq[int(self.u() * len(seq)) % len(seq)]


rng = Rng(0x49534c)

TORII_POST = (0.4, 2.75, 0.4)  # shinobi_sim::level::TORII_POST_HALF
PYLON = (0.35, 1.8, 0.35)  # shinobi_sim::level::PYLON_HALF
towers = []  # (cx, top, cz, hw) square towers that get a pagoda dais
spawns = []
pickups = []
weapons = []
gates = []  # (axis, cx, cz, top)
keep_clear = []  # (x, z, r): spawn / stair landings the scatter must avoid


def occupied(cx, cz, y0, y1, hx, hz, margin=0.0):
    for _, items in BOXES:
        for (bx, by, bz, bhx, bhy, bhz, rot) in items:
            ex = abs(math.cos(rot)) * bhx + abs(math.sin(rot)) * bhy
            ey = abs(math.sin(rot)) * bhx + abs(math.cos(rot)) * bhy
            if abs(bx - cx) < ex + hx + margin - 0.01 and abs(bz - cz) < bhz + hz + margin - 0.01 and abs(by - (y0 + y1) / 2) < ey + (y1 - y0) / 2 + margin - 0.01:
                return True
    return False


def rock_bottom(cx, cz, top, hx, hz, depth, cones=3):
    """A slab with an inverted rock cone hung under it (wall-run faces on the underside)."""
    slab(cx - hx, cx + hx, cz - hz, cz + hz, top, depth)
    y, w, d = top - depth, 0.72, 5.0
    for i in range(cones):
        ch, cw = hx * w, hz * w
        slab(cx - ch, cx + ch, cz - cw, cz + cw, y, d)
        y -= d
        hx, hz = ch, cw
        d *= 1.2


# =============================================================================================== hub
HUB_TOP = 60.0
section("HEAVEN'S GATE ziggurat (hub, deck y 60)")
slab(-22, 22, -22, 22, HUB_TOP, 16.0)
rock_bottom(0, 0, HUB_TOP - 16.0, 17, 17, 9.0, 3)
# layer 2 (y 66) reached by a 24 degree ramp west -> east inside the base (beside the spoke lane)
slab(-8, 8, -8, 8, 66.0, 6.0)
ramp(-21.5, HUB_TOP, -8.0, 66.0, -10.5, 3.0)
# the shrine (y 74, only by hook / wall-run / the stone spiral)
slab(-4, 4, -4, 4, 74.0, 8.0)
weapons.append(("tag", 3.0, 75.0, 0.0))

# =============================================================================================== east: moon gate
section("MOON GATE island (east, deck y 45): the ring, the eye, the crown")
E_X0, E_X1, E_TOP, E_CX = 56.0, 100.0, 45.0, 78.0
ramp(22.0, HUB_TOP, E_X0, E_TOP, 0.0, 4.0)
slab(E_X0, E_X1, -22, 22, E_TOP, 12.0)
rock_bottom(E_CX, 0, E_TOP - 12.0, 18, 18, 8.0, 3)
R, RY = 17.0, E_TOP + 7.0 + 17.0
N_SEG = 16
for i in range(N_SEG):
    a = i * 2 * math.pi / N_SEG
    px, py = E_CX + R * math.cos(a), RY + R * math.sin(a)
    rot = a + math.pi / 2
    while rot > math.pi / 2 + 1e-9:
        rot -= math.pi
    while rot <= -math.pi / 2 + 1e-9:
        rot += math.pi
    ln = R * math.sin(math.pi / N_SEG) + 0.6
    add(px, py, 0.0, ln, 1.4, 3.0, 0.0 if abs(rot) < 1e-6 else rot)
slab(E_CX - 4, E_CX + 4, -4, 4, RY, 1.5)  # the eye: a floating shrine disc
weapons.append(("tag", E_CX, RY + 1.0, 0.0))
weapons.append(("yumi", E_CX, RY + R + 1.4 + 1.0, 0.0))  # on the crown of the ring
gates.append(("x", 60.5, 0.0, E_TOP))
spawns += [(70.0, 16.0, E_TOP), (88.0, -14.0, E_TOP), (94.0, 14.0, E_TOP)]
pickups.append((66.0, E_TOP + 1.0, -14.0, 25))

# =============================================================================================== west: cascade
section("CASCADE terraces (west, deck y 80): rice terraces, the waterfall wall, the pagoda")
W_TOP, W_X1, W_X0 = 80.0, -67.0, -112.0
ramp(-22.0, HUB_TOP, W_X1, W_TOP, 0.0, 4.0)
slab(W_X0, W_X1, -24, 24, W_TOP, 12.0)
rock_bottom((W_X0 + W_X1) / 2, 0, W_TOP - 12.0, 21, 22, 8.0, 3)
T_TOP = 86.0
stairs_x(-12.0, 5.0, -80.0, W_TOP, -1, 20, 1.0)
slab(W_X0, -100.0, -24, 0, T_TOP, 8.0)  # upper terrace (north half)
block(-113.0, 0.0, 0.5, 22.0, W_TOP - 46.0, W_TOP + 20.0)  # the waterfall wall (46 m below the deck, 20 above)
block(-104.0, -17.0, 3.0, 3.0, T_TOP, T_TOP + 22.0)
towers.append((-104.0, T_TOP + 22.0, -17.0, 3.0))
weapons.append(("tag", -107.0, T_TOP + 1.0, -5.0))
gates.append(("x", -71.0, 0.0, W_TOP))
spawns += [(-80.0, 16.0, W_TOP), (-100.0, 14.0, W_TOP), (-78.0, -18.0, W_TOP), (-106.0, -11.0, T_TOP)]
pickups.append((-90.0, W_TOP + 1.0, 16.0, 25))

# =============================================================================================== north: bamboo
section("BAMBOO GROVE (north, deck y 48): thin stalks to wall-run, some leaning")
z_end, N_TOP = stairs_z(0.0, 4.0, -22.0, HUB_TOP, -1, 40)
N_Z0, N_Z1 = z_end, -112.0
slab(-27, 27, N_Z1, N_Z0, N_TOP, 12.0)
rock_bottom(0, (N_Z0 + N_Z1) / 2, N_TOP - 12.0, 22, 22, 8.0, 3)
gates.append(("z", 0.0, N_Z0 - 4.0, N_TOP))
n_spawns = [(-17.0, N_Z0 - 22.0, N_TOP), (17.0, N_Z0 - 28.0, N_TOP), (-14.0, N_Z0 - 42.0, N_TOP)]
spawns += n_spawns
weapons_n = (0.0, N_TOP + 1.0, N_Z0 - 36.0)
weapons.append(("tag",) + weapons_n)
for gx in range(-24, 25, 6):
    for gz in range(int(N_Z0) - 6, int(N_Z1) + 3, -6):
        if abs(gx) < 6 and gz > N_Z0 - 44:
            continue  # the central lane stays open from the stairs to the middle
        if rng.u() < 0.38:
            continue
        x, z = gx + rng.rng(-1.2, 1.2), gz + rng.rng(-1.2, 1.2)
        if any(abs(x - s[0]) < 4.0 and abs(z - s[1]) < 4.0 for s in n_spawns) or (abs(x - weapons_n[0]) < 4 and abs(z - weapons_n[2]) < 4):
            continue
        h = rng.pick([14.0, 20.0, 26.0, 32.0, 40.0])
        lean = rng.pick([0.0, 0.0, 0.12, -0.12, 0.18, -0.18]) if h <= 32 else 0.0
        if lean == 0.0:
            block(x, z, 0.5, 0.5, N_TOP, N_TOP + h)
        else:  # leans in x; base stays on the deck: centre = base + rotate((0, h/2))
            add(x - (h / 2) * math.sin(lean), N_TOP + (h / 2) * math.cos(lean) - 0.1, z, 0.5, h / 2, 0.5, lean)
pickups.append((12.0, N_TOP + 1.0, N_Z0 - 10.0, 25))

# =============================================================================================== south: bowl
section("DRAGON BOWL (south, deck y 50.1): a sunken duel pit ringed by cover")
z_end, S_TOP = stairs_z(0.0, 4.0, 22.0, HUB_TOP, 1, 33)
S_Z0, S_Z1, S_CZ = z_end, z_end + 52.0, z_end + 26.0
rock_bottom(0, S_CZ, S_TOP - 12.0, 26, 26, 8.0, 3)
PIT = S_TOP - 5.0  # floor of the pit
# the deck is a ring of slabs round an open pit
slab(-32, 32, S_Z0, S_CZ - 9, S_TOP, 12.0)  # north strip
slab(-32, 32, S_CZ + 9, S_Z1, S_TOP, 12.0)  # south strip
slab(-32, -12 - 11.5, S_CZ - 9, S_CZ + 9, S_TOP, 12.0)  # west strip
slab(12 + 11.5, 32, S_CZ - 9, S_CZ + 9, S_TOP, 12.0)  # east strip
slab(-12.5, 12.5, S_CZ - 9, S_CZ + 9, PIT, 4.0)  # pit floor
ramp(-12.0 - 11.5, S_TOP, -12.0, PIT, S_CZ, 9.0)  # west ramp down
ramp(12.0, PIT, 12.0 + 11.5, S_TOP, S_CZ, 9.0)  # east ramp up
# cover on the rim
for dx in (-22.0, 22.0):
    block(dx, S_CZ - 13.0, 5.0, 0.6, S_TOP, S_TOP + 3.5)
    block(dx, S_CZ + 13.0, 5.0, 0.6, S_TOP, S_TOP + 3.5)
for dz in (-16.0, 16.0):
    block(0.0, S_CZ + dz, 3.0, 0.6, S_TOP, S_TOP + 3.5)
# lantern towers at the four far corners
for sx in (-1, 1):
    for sz in (-1, 1):
        block(sx * 28.0, S_CZ + sz * 22.0, 2.5, 2.5, S_TOP, S_TOP + 12.0)
        towers.append((sx * 28.0, S_TOP + 12.0, S_CZ + sz * 22.0, 2.5))
gates.append(("z", 0.0, S_Z0 + 4.0, S_TOP))
spawns += [(-27.0, S_CZ - 4.0, S_TOP), (27.0, S_CZ + 4.0, S_TOP), (0.0, S_Z1 - 4.0, S_TOP)]
weapons.append(("tag", 0.0, PIT + 1.0, S_CZ))
pickups.append((0.0, S_TOP + 1.0, S_Z1 - 12.0, 25))

# =============================================================================================== spire, sanctum, spiral
section("SPIRE and SKY SANCTUM (inverted pagoda island y 140) and the stone spiral")
SP_TOP, SAN_TOP = 140.0, 140.0
block(0.0, 0.0, 2.0, 2.0, 74.0, SAN_TOP - 20.0)
slab(-11, 11, -11, 11, SAN_TOP, 8.0)
slab(-8, 8, -8, 8, SAN_TOP - 8.0, 6.0)
slab(-5, 5, -5, 5, SAN_TOP - 14.0, 6.0)
slab(-3, 3, -3, 3, SAN_TOP - 20.0, 8.0)
for dx in (-8.0, 8.0):
    for dz in (-8.0, 8.0):
        block(dx, dz, 0.8, 0.8, SAN_TOP, SAN_TOP + 9.0)
slab(-9.5, 9.5, -9.5, 9.5, SAN_TOP + 10.0, 1.0)  # pavilion roof
weapons.append(("yumi", 0.0, SAN_TOP + 1.0, 0.0))
# the stone spiral: 4 x 4 m stones, 2.6 m up each, 10 per turn at radius 17
y, a = 69.0, math.radians(200.0)
stones = []
while y <= SAN_TOP - 2.0:
    sx, sz = 17.0 * math.cos(a), 17.0 * math.sin(a)
    slab(sx - 2.0, sx + 2.0, sz - 2.0, sz + 2.0, y, 1.5)
    stones.append((sx, y, sz))
    y += 2.6
    a += 2 * math.pi / 10.0
pickups.append((stones[len(stones) // 2][0], stones[len(stones) // 2][1] + 1.0, stones[len(stones) // 2][2], 25))

# =============================================================================================== shards
section("floating SHARDS (corners: hook / wall-run only) with rock spires")
shards = [(76.0, -78.0, 92.0), (76.0, 76.0, 36.0), (-76.0, 76.0, 102.0), (-76.0, -78.0, 40.0)]
for j, (cx, cz, top) in enumerate(shards):
    rock_bottom(cx, cz, top, 8.0, 6.5, 6.0, 3)
    block(cx - 3.5, cz + 1.0, 1.6, 1.6, top, top + 20.0 + 6.0 * (j % 2))
    block(cx + 3.5, cz - 1.5, 1.4, 1.4, top, top + 12.0)
    slab(cx + 8.0, cx + 13.0, cz - 3.0, cz + 3.0, top - 0.3, 1.0)  # a ledge to land on
    pickups.append((cx, top + 1.0, cz + 4.0, 25))

# =============================================================================================== drifting rocks
section("drifting rock mini-islands (stepping stones, hook anchors, wall-run faces)")
placed, tries = 0, 0
while placed < 28 and tries < 2000:
    tries += 1
    ang = rng.rng(0.0, 2 * math.pi)
    rad = rng.rng(30.0, 108.0)
    x, z = rad * math.cos(ang), rad * math.sin(ang)
    y = rng.rng(30.0, 130.0)
    hx, hz = rng.rng(2.0, 4.5), rng.rng(2.0, 4.5)
    d = rng.rng(2.5, 6.0)
    if abs(x) > 112 or abs(z) > 112:
        continue
    if occupied(x, z, y - d - 4.0, y + 14.0, hx + 6.0, hz + 6.0):
        continue
    slab(x - hx, x + hx, z - hz, z + hz, y, d)
    slab(x - hx * 0.55, x + hx * 0.55, z - hz * 0.55, z + hz * 0.55, y - d, d * 1.6)
    if rng.u() < 0.35:
        block(x, z, 0.9, 0.9, y, y + rng.pick([7.0, 11.0, 15.0]))
    placed += 1

# =============================================================================================== shinobi architecture
section("pagoda daises, torii gates over the spokes, lantern pylons")
for (tx, ttop, tz, thw) in towers:
    if not occupied(tx, tz, ttop + 0.02, ttop + 0.6, thw - 0.5, thw - 0.5):
        add(tx, ttop + 0.15, tz, thw - 0.5, 0.15, thw - 0.5)
        add(tx, ttop + 0.45, tz, thw - 1.2, 0.15, thw - 1.2)
for (axis, cx, cz, top) in gates:
    if axis == "x":
        slab(cx - 1.4, cx + 1.4, cz - 3.9, cz + 3.9, top, 1.0)
        posts = [(cx, cz - 3.1), (cx, cz + 3.1)]
    else:
        slab(cx - 3.9, cx + 3.9, cz - 1.4, cz + 1.4, top, 1.0)
        posts = [(cx - 3.1, cz), (cx + 3.1, cz)]
    for (px, pz) in posts:
        if not occupied(px, pz, top + 0.01, top + 5.5, TORII_POST[0], TORII_POST[2]):
            add(px, top + TORII_POST[1], pz, *TORII_POST)
for i, (sx, sz, top) in enumerate(spawns):
    if i % 3:
        continue
    for sgn in (-1, 1):
        px = sx + sgn * 3.2
        if not occupied(px, sz, top + 0.01, top + 5.5, TORII_POST[0], TORII_POST[2]):
            add(px, top + TORII_POST[1], sz, *TORII_POST)
for (px, pz, top) in [(E_X1 - 1.6, 20.0, E_TOP), (W_X0 + 1.6, 22.0, W_TOP), (-25.0, N_Z1 + 1.6, N_TOP), (30.0, S_Z1 - 1.6, S_TOP),
                      (20.0, 20.0, HUB_TOP), (-20.0, 20.0, HUB_TOP), (20.0, -20.0, HUB_TOP)]:
    if not occupied(px, pz, top + 0.01, top + 3.6, PYLON[0], PYLON[2]):
        add(px, top + PYLON[1], pz, *PYLON)

# hub spawns last so the torii pass above can frame them
spawns = [(16.0, 16.0, HUB_TOP), (-16.0, 16.0, HUB_TOP), (16.0, -16.0, HUB_TOP), (-16.0, -16.0, HUB_TOP)] + spawns
pickups.append((12.0, HUB_TOP + 1.0, 0.0, 25))

# =============================================================================================== write
WEAPON_RESPAWN = 45
lines = [
    "# SHINOBI FFA ISLES - SKYFALL ARCHIPELAGO: a floating-islands arena, ~230 x 230 m, towers to 160 m, no ground (kill plane y=-20).",
    "# GENERATED by scripts/gen_isles_level.py. Edit the script, not this file; tests: crates/shinobi-sim/tests/arena_isles.rs, walkable.rs",
    "# Heaven's Gate ziggurat hub (y 60) with spokes to Moon Gate (E, 45), Cascade (W, 80), Bamboo Grove (N, 48), Dragon Bowl (S, 50);",
    "# the Sky Sanctum (y 140) above a 66 m spire and a stone spiral; four hook-only shards and ~28 drifting rocks in the void.",
    "",
]
n = 0
for title, items in BOXES:
    lines.append("# --- %s ---" % title)
    for (cx, cy, cz, hx, hy, hz, rot) in items:
        row = "box %s %s %s  %s %s %s" % (f(cx), f(cy), f(cz), f(hx), f(hy), f(hz))
        if rot:
            row += " " + f(rot)
        lines.append(row)
        n += 1
    lines.append("")
lines.append("# --- %d spawns (deck level on the hub and the four spoke islands) ---" % len(spawns))
for x, z, top in spawns:
    lines.append("spawn %s %s %s" % (f(x), f(top + 0.81), f(z)))
lines.append("")
lines.append("# --- %d pickups ---" % len(pickups))
for x, y, z, r in pickups:
    lines.append("pickup %s %s %s %s" % (f(x), f(y), f(z), f(r)))
lines.append("")
lines.append("# --- %d power-weapon pedestals (yumi = bow, tag = explosive fuda tags; respawn %d s) ---" % (len(weapons), WEAPON_RESPAWN))
for w in weapons:
    lines.append("%s %s %s %s %s" % (w[0], f(w[1]), f(w[2]), f(w[3]), f(WEAPON_RESPAWN)))

out = sys.argv[1] if len(sys.argv) > 1 else "levels/ffa_isles.lvl"
open(out, "w").write("\n".join(lines) + "\n")
print("wrote %s: %d boxes, %d spawns, %d pickups, %d weapon pedestals" % (out, n, len(spawns), len(pickups), len(weapons)))
