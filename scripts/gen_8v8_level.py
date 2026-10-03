#!/usr/bin/env python3
"""Generates levels/8v8.lvl. Edit the layout here, rerun, and `cargo test -p shinobi-sim --test arena_8v8`.

Map: 112 x 112 m, mirror-symmetric across z = 0 (Cyan base at -z, Orange at +z) and across x = 0 where marked.
Everything is authored for the Cyan half (z <= 0) and mirrored; boxes centred on z = 0 are emitted once.
Cfg limits used: single jump apex 1.6 m, jump + double jump ~2.8 m, autostep 0.35 m, wall-run 11 m/s for 1.5 s,
ramps 20 deg (0.35 rad), grapple range 40 m, wall lane gap 5.6 m (proven in arena.lvl).
"""
import math, os

HALF = 56.0      # playable half extent
CEIL = 22.0
out_boxes = []   # (comment, [boxes])
spawns = []
pickups = []

def f(v):
    s = ("%.4f" % v).rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s

def box(c, h, rot=0.0):
    return (tuple(c), tuple(h), rot)

def zmir(b):
    c, h, r = b
    return (c[0], c[1], -c[2]), h, r

def xmir(b):
    c, h, r = b
    return (-c[0], c[1], c[2]), h, -r

def section(title, boxes, mirror_z=True, mirror_x=False):
    """boxes are authored for z <= 0 (x >= 0 if mirror_x); mirrors are added automatically."""
    allb = []
    for b in boxes:
        variants = [b] + ([xmir(b)] if mirror_x else [])
        for v in variants:
            allb.append(v)
            if mirror_z and abs(v[0][2]) > 1e-6:
                allb.append(zmir(v))
    out_boxes.append((title, allb))

# --- shell ---
section("shell: floor, ceiling (wall-jump zigzags must not climb out), perimeter", [
    box((0, -0.5, 0), (HALF, 0.5, HALF)),
    box((0, CEIL + 0.5, 0), (HALF + 1, 0.5, HALF + 1)),
    box((-HALF - 0.5, CEIL / 2, 0), (0.5, CEIL / 2, HALF + 1)),
    box((HALF + 0.5, CEIL / 2, 0), (0.5, CEIL / 2, HALF + 1)),
    box((0, CEIL / 2, -HALF - 0.5), (HALF + 1, CEIL / 2, 0.5)),
], mirror_z=True)

# --- base: front wall (no sight line to the other base), cover pillars, low walls ---
section("bases: 10 m front wall z=+-42 blocks every line across the map; open at the sides toward the flank lanes", [
    box((0, 5, -42), (22, 5, 0.6)),
    box((-12, 1.5, -45), (0.6, 1.5, 0.6)),
    box((12, 1.5, -45), (0.6, 1.5, 0.6)),
    box((0, 0.5, -49.5), (3, 0.5, 0.4)),
    box((-20, 0.5, -52), (0.4, 0.5, 3)),
    box((20, 0.5, -52), (0.4, 0.5, 3)),
])

# --- yard cover between base and centre: pillars (3 m) and low walls (1 m) for shuriken corners ---
section("yard cover", [
    box((-8, 1.5, -30), (0.6, 1.5, 0.6)),
    box((8, 1.5, -30), (0.6, 1.5, 0.6)),
    box((-18, 1.5, -24), (0.6, 1.5, 0.6)),
    box((18, 1.5, -24), (0.6, 1.5, 0.6)),
    box((0, 0.5, -35), (5, 0.5, 0.4)),
    box((-13, 0.5, -17), (3, 0.5, 0.4)),
    box((13, 0.5, -17), (3, 0.5, 0.4)),
    box((-34, 0.5, -36), (0.4, 0.5, 4)),
    box((34, 0.5, -36), (0.4, 0.5, 4)),
])

# --- three wall-run lanes along z: west and east flank lanes (32 m) and the central spine (24 m) ---
section("wall-run lanes: west/east, 5.6 m between faces, 6 m tall, run along z (self-symmetric about z=0)", [
    box((26.8, 3, 0), (0.5, 3, 16)),
    box((33.4, 3, 0), (0.5, 3, 16)),
], mirror_z=False, mirror_x=True)
section("central spine lane: 5.8 m between faces, 5 m tall", [
    box((3.4, 2.5, 0), (0.5, 2.5, 12)),
    box((-3.4, 2.5, 0), (0.5, 2.5, 12)),
], mirror_z=False)

# --- contested centre: two towers (tiers 1.2 / 2.4 / 3.6, then a 6.6 m pillar only the grapple reaches) ---
RAMP = 0.35
section("centre towers at x=+-16: tier tops 1.2, 2.4, 3.6; pillar top 6.6 (grapple); 20 deg ramps on the spine side", [
    box((16, 0.6, 0), (7, 0.6, 7)),
    box((16, 1.8, 0), (4.5, 0.6, 4.5)),
    box((16, 3.0, 0), (2.5, 0.6, 2.5)),
    box((16, 5.1, 0), (1.5, 1.5, 1.5)),
    box((16 - 7 - 1.7498 * math.cos(RAMP), 0.4591, 0), (1.7498, 0.15, 3), RAMP),
], mirror_z=False, mirror_x=True)

# --- flanks: shelves with pickups, ramps from the lane side, grapple anchors (hanging slabs + 14 m pylons) ---
section("flank shelves x=+-50: tier tops 1.2 / 2.4 / 3.6 (pickups on top); lane-side ramps", [
    box((50, 0.6, 0), (4.5, 0.6, 30)),
    box((52, 1.8, 0), (2.5, 0.6, 24)),
    box((53.5, 3.0, 0), (1.3, 0.6, 18)),
    box((45.5 - 1.7498 * math.cos(RAMP), 0.4591, -12), (1.7498, 0.15, 3), RAMP),
], mirror_z=True, mirror_x=True)
section("grapple anchors: hanging slabs at y=13 over the flank gap, 14 m pylons; every face is open sky above the lane", [
    box((40, 13, -30), (3, 0.4, 1.2)),
    box((40, 13, -10), (3, 0.4, 1.2)),
    box((40, 7, -22), (1.5, 7, 1.5)),
    box((22, 12, -22), (2.5, 0.4, 1.2)),
    box((30, 11, -24), (2.5, 0.4, 1.2)),   # over each lane mouth: swing out of the lane along its centre line
], mirror_z=True, mirror_x=True)

# --- spawns: 2 rows of 4 behind the front wall, mirrored for Orange ---
CY = [(-15, -53), (-5, -53), (5, -53), (15, -53), (-18, -48), (-6, -48), (6, -48), (18, -48)]
for x, z in CY:
    spawns.append((x, 0.81, z, 0))
for x, z in CY:
    spawns.append((x, 0.81, -z, 1))

# --- pickups: shelf tops (reward), lane mids, tower pillar tops (grapple-only) ---
for sx in (-1, 1):
    for sz in (-1, 1):
        pickups.append((sx * 53.5, 4.6, sz * 14, 20))
    pickups.append((sx * 30.1, 1.0, 0, 20))
    pickups.append((sx * 16, 7.6, 0, 30))

def main():
    lines = [
        "# SHINOBI 8v8 - 112 x 112 m, Cyan base at -z, Orange base at +z, mirror-symmetric across z=0 and x=0 flanks.",
        "# GENERATED by scripts/gen_8v8_level.py, pinned by crates/shinobi-sim/tests/arena_8v8.rs. Edit the script, not this file.",
        "# x right, y up. `spawn x y z team`: team 0 = Cyan, 1 = Orange. 8 spawns per team, capsule centre 0.81 above the floor.",
        "",
    ]
    for title, bs in out_boxes:
        lines.append("# --- %s ---" % title)
        for c, h, r in bs:
            lines.append("box %s %s %s  %s %s %s%s" % (f(c[0]), f(c[1]), f(c[2]), f(h[0]), f(h[1]), f(h[2]), (" " + f(r)) if r else ""))
        lines.append("")
    lines.append("# --- spawns: 8 per team in two rows behind the 10 m front wall (z=+-42), no sight line to the other base ---")
    for x, y, z, t in spawns:
        lines.append("spawn %s %s %s %d" % (f(x), f(y), f(z), t))
    lines.append("")
    lines.append("# --- 8 pickups: shelf tops (4), lane mids (2), tower pillar tops reachable only by grapple (2), respawn 20-30 s ---")
    for x, y, z, s in pickups:
        lines.append("pickup %s %s %s %d" % (f(x), f(y), f(z), s))
    open(os.path.join(os.path.dirname(__file__), "..", "levels", "8v8.lvl"), "w").write("\n".join(lines) + "\n")

main()
