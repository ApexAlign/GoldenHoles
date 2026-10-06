#!/usr/bin/env python3
"""Generates levels/ffa_sky.lvl: the vertical free-for-all arena of floating buildings (124 x 124 m, 92 m tall).

    python3 scripts/gen_sky_level.py [out.lvl]

Design: no ground at all, only void below y = -20 (the kill plane). Seven floating structures share one roof height
(y = 56) so the roofs are fighting decks you can walk between over six narrow bridges; everything else is vertical:
  - centre tower (20 x 20 m) with a 36 m spire (the grapple / wall-run column);
  - two slabs at x = +-44: 6 m thick, 28 m long, 48 m tall faces, made for wall-running up and down;
  - four corner towers (18 x 18 m) at (+-50, +-50): NE / SW carry a stepped crown, NW / SE a low penthouse;
  - ten mid-air ledges stuck to the faces (hook targets, wall-run landing pads, ambush spots).
Gaps between structures are 27 m (bridged) to 40+ m (grapple / wall-run only). A 0.9 m lip runs round every roof edge
except at the bridge mouths and one open "launch notch" per roof, so a roof is a fight, not a cliff you trip off.
Everything is axis aligned and rectangular: big flat faces, no clutter, readable from far away.
Cfg limits used: single jump 1.6 m, jump + double ~2.8 m (crown steps are 2.4 m), grapple 40 m (the 10x traversal
agent makes the long gaps cheap).
Tests: crates/shinobi-sim/tests/arena_sky.rs, crates/shinobi-ai/tests/stuck.rs (no_bot_stays_stuck_on_ffa_sky)
"""
import sys

ROOF = 56.0  # every walkable deck
LIP_H = 0.9
LIP_T = 0.2  # half thickness
BRIDGE_W = 2.0  # half width
BOX = []  # (title, [(cx, cy, cz, hx, hy, hz)])


def f(v):
    s = ("%.4f" % v).rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def block(title, items):
    BOX.append((title, items))


def tower(cx, cz, hx, hz, base, top=ROOF):
    """A solid building from `base` up to `top`."""
    return (cx, (base + top) / 2, cz, hx, (top - base) / 2, hz)


def slab_top(cx, cz, hx, hz, y_top, thick):
    return (cx, y_top - thick, cz, hx, thick, hz)


def lip_segments(cx, cz, hx, hz, gaps):
    """Roof-edge lips. `gaps` = [(side, lo, hi)] with side in 'n','s','e','w' ('n' = +z, 'e' = +x) and lo..hi the open
    interval along the edge in world coordinates (x for n/s, z for e/w)."""
    out = []
    y = ROOF + LIP_H / 2
    hh = LIP_H / 2

    def cut(lo, hi, opens):
        segs = [(lo, hi)]
        for (a, b) in opens:
            nxt = []
            for (s0, s1) in segs:
                if b <= s0 or a >= s1:
                    nxt.append((s0, s1))
                    continue
                if a > s0:
                    nxt.append((s0, a))
                if b < s1:
                    nxt.append((b, s1))
            segs = nxt
        return [(s0, s1) for (s0, s1) in segs if s1 - s0 > 0.5]

    for side in "nsew":
        opens = [(lo, hi) for (sd, lo, hi) in gaps if sd == side]
        if side in "ns":
            z = cz + (hz - LIP_T if side == "n" else -(hz - LIP_T))
            for (s0, s1) in cut(cx - hx, cx + hx, opens):
                out.append(((s0 + s1) / 2, y, z, (s1 - s0) / 2, hh, LIP_T))
        else:
            x = cx + (hx - LIP_T if side == "e" else -(hx - LIP_T))
            for (s0, s1) in cut(cz - hz, cz + hz, opens):
                out.append((x, y, (s0 + s1) / 2, LIP_T, hh, (s1 - s0) / 2))
    return out


# --------------------------------------------------------------------------------------------- structures
C = (0.0, 0.0, 10.0, 10.0, 12.0)  # cx, cz, hx, hz, base
SLABS = [(-44.0, 0.0, 3.0, 14.0, 8.0), (44.0, 0.0, 3.0, 14.0, 8.0)]
CORNERS = [  # (cx, cz, hx, hz, base, kind)
    (50.0, 50.0, 9.0, 9.0, 14.0, "crown"),
    (-50.0, -50.0, 9.0, 9.0, 14.0, "crown"),
    (-50.0, 50.0, 9.0, 9.0, 14.0, "penthouse"),
    (50.0, -50.0, 9.0, 9.0, 14.0, "penthouse"),
]

block("centre tower", [tower(*C)])
block("slabs (wall-run faces)", [tower(*s) for s in SLABS])
block("corner towers", [tower(cx, cz, hx, hz, base) for (cx, cz, hx, hz, base, _) in CORNERS])

# bridges: 4 m wide, 0.5 m thick, top at ROOF. (x0, z0) -> (x1, z1) along one axis.
BRIDGES = [
    ("x", 0.0, 0.0, 10.0, 41.0),    # centre -> east slab
    ("x", 0.0, 0.0, -41.0, -10.0),  # west slab -> centre
    ("z", 44.0, 14.0, 41.0, None),  # east slab -> NE corner (z from 14 to 41 at x = 44)
    ("z", 44.0, -41.0, -14.0, None),  # SE corner -> east slab
    ("z", -44.0, 14.0, 41.0, None),  # west slab -> NW corner
    ("z", -44.0, -41.0, -14.0, None),  # SW corner -> west slab
]
bridges = []
for b in BRIDGES:
    if b[0] == "x":
        _, z, _, lo, hi = b
        bridges.append(((lo + hi) / 2, ROOF - 0.25, z, (hi - lo) / 2, 0.25, BRIDGE_W))
    else:
        _, x, lo, hi, _ = b
        bridges.append((x, ROOF - 0.25, (lo + hi) / 2, BRIDGE_W, 0.25, (hi - lo) / 2))
block("bridges (the easy route between decks)", bridges)

# roofs: crowns, spire, penthouses
roofs = []
roofs.append((0.0, ROOF + 18.0, 0.0, 2.5, 18.0, 2.5))  # spire: 56 -> 92
roofs.append((0.0, ROOF + 0.6, 0.0, 6.0, 0.6, 6.0))  # spire plinth, top 57.2 (a 1.2 m step: walkable)
for (cx, cz, hx, hz, base, kind) in CORNERS:
    if kind == "crown":
        roofs.append((cx, ROOF + 1.2, cz, 5.0, 1.2, 5.0))  # top 58.4
        roofs.append((cx, ROOF + 3.6, cz, 2.5, 1.2, 2.5))  # top 60.8
    else:
        roofs.append((cx, ROOF + 1.2, cz, 5.5, 1.2, 3.0))  # top 58.4
block("roof crowns, penthouses and the spire", roofs)

# lips with open mouths where bridges land and one launch notch per roof
lips = []
lips += lip_segments(C[0], C[1], C[2], C[3], [("e", -3, 3), ("w", -3, 3), ("n", -4, 4)])
for (cx, cz, hx, hz, base) in SLABS:
    sgn = 1 if cx > 0 else -1
    # bridges land on the z ends (corner side) and the inner x face (centre side)
    gaps = [("n", cx - 3, cx + 3), ("s", cx - 3, cx + 3), ("e" if sgn < 0 else "w", -3, 3), ("w" if sgn < 0 else "e", -4, 4)]
    lips += lip_segments(cx, cz, hx, hz, gaps)
for (cx, cz, hx, hz, base, kind) in CORNERS:
    # bridge mouth faces the slab: slab z end -> corner south/north face at x = 44 (+-)
    face = "s" if cz > 0 else "n"
    gaps = [(face, 44.0 * (1 if cx > 0 else -1) - 3, 44.0 * (1 if cx > 0 else -1) + 3)]
    # launch notch on the outer x face
    gaps.append(("e" if cx > 0 else "w", cz - 3, cz + 3))
    lips += lip_segments(cx, cz, hx, hz, gaps)
block("roof lips (0.9 m, open at bridge mouths and launch notches)", lips)

# mid-air ledges: 8 x 8 m, 0.8 m thick, stuck to a face (hook targets, wall-run pads)
ledges = []


def ledge(x, y, z, hx=4.0, hz=4.0):
    ledges.append((x, y - 0.4, z, hx, 0.4, hz))


for (cx, cz, hx, hz, base, kind) in CORNERS:  # facing the centre
    sx, sz = (-1 if cx > 0 else 1), (-1 if cz > 0 else 1)
    ledge(cx + sx * (hx + 4.0), 38.4, cz)  # on the x face toward the centre
for dx, dz in [(1, 0), (-1, 0), (0, 1), (0, -1)]:
    ledge(dx * 14.0, 30.4, dz * 14.0)  # centre tower, one per side
for (cx, cz, hx, hz, base) in SLABS:
    sgn = 1 if cx > 0 else -1
    ledge(cx + sgn * (hx + 4.0), 24.4, 0.0, 4.0, 6.0)  # outer face
block("mid-air ledges", ledges)

# --------------------------------------------------------------------------------------------- spawns, pickups
spawns = [(C[0] + 7, C[1] - 7), (C[0] - 7, C[1] + 7)]
for (cx, cz, hx, hz, base, kind) in CORNERS:
    sx = 1 if cx > 0 else -1
    sz = 1 if cz > 0 else -1
    spawns.append((cx - sx * 6.5, cz + sz * 6.5))
    spawns.append((cx + sx * 6.5, cz - sz * 6.5))
for (cx, cz, hx, hz, base) in SLABS:
    spawns.append((cx, 8.0 if cx > 0 else -8.0))

pickups = [
    (50, ROOF + 5.8, 50, 25), (-50, ROOF + 5.8, -50, 25),  # crown tops (60.8 + 1)
    (-50 + 13, 39.4, 50, 25),  # NW corner ledge
    (50 - 13, 39.4, -50, 25),  # SE corner ledge
]

lines = [
    "# SHINOBI FFA SKY - 124 x 124 m, 92 m tall: seven floating buildings, no ground (kill plane y=-20).",
    "# GENERATED by scripts/gen_sky_level.py. Edit the script, not this file; tests: crates/shinobi-sim/tests/arena_sky.rs",
    "# x right, y up. Roof decks at y=56 joined by six bridges; wall-run faces 48 m tall; ten mid-air ledges; a centre spire to y=92.",
    "",
]
n = 0
for title, items in BOX:
    lines.append("# --- %s ---" % title)
    for (cx, cy, cz, hx, hy, hz) in items:
        lines.append("box %s %s %s  %s %s %s" % (f(cx), f(cy), f(cz), f(hx), f(hy), f(hz)))
        n += 1
    lines.append("")
lines.append("# --- %d spawns (roof decks, spread so players meet within seconds) ---" % len(spawns))
for x, z in spawns:
    lines.append("spawn %s %s %s" % (f(x), f(ROOF + 0.81), f(z)))
lines.append("")
lines.append("# --- %d pickups ---" % len(pickups))
for x, y, z, r in pickups:
    lines.append("pickup %s %s %s %s" % (f(x), f(y), f(z), f(r)))

out = sys.argv[1] if len(sys.argv) > 1 else "levels/ffa_sky.lvl"
open(out, "w").write("\n".join(lines) + "\n")
print("wrote %s: %d boxes, %d spawns, %d pickups" % (out, n, len(spawns), len(pickups)))
