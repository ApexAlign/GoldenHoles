#!/usr/bin/env python3
"""Generates the free-for-all maps: levels/ffa.lvl (HALF=112, the demo default) and levels/ffa_xl.lvl (HALF=168).

    python3 scripts/gen_ffa_level.py            # writes both
    python3 scripts/gen_ffa_level.py 112 out.lvl

Layered Tron grid: ground, stepped tower tops (9.6 m) joined by sky bridges, and a few 20 m spires. A central hub, four
districts (two kinds, point-symmetric by 180 degrees: wall-run canyons + ramp ziggurats, and tower clusters), long
boulevards (wall-run lanes) from the hub to the void edges, and 24 enclosed spawn alcoves that cannot see each other.
Edges are open void (kill plane y = -20) behind a 1 m lip with gaps. No ceiling.
Cfg limits used: single jump 1.6 m, jump + double ~2.8 m, wall-run 11 m/s for 1.5 s, ramps 20 deg, grapple 40 m, wall gap 5.6 m.
Positions scale with HALF/112; sizes do not (so the XL map is the same buildings with more distance, plus an outer ring).
"""
import math, os, sys

R20 = 0.35
RAMP_RUN = 1.7498 * math.cos(R20)   # horizontal half-length of a 1.2 m ramp
GAP = 5.6


def f(v):
    s = ("%.4f" % v).rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


class Level:
    def __init__(self, half):
        self.half = half
        self.s = half / 112.0
        self.parts = []      # (title, [box])
        self.spawns = []
        self.pickups = []

    # ---- primitives (all in metres, absolute) ----
    @staticmethod
    def box(cx, cy, cz, hx, hy, hz, rot=0.0):
        return ((cx, cy, cz), (hx, hy, hz), rot)

    def P(self, v):
        return v * self.s

    def section(self, title, boxes, mx=False, mz=False, rot180=False):
        """Boxes authored for one place. mx / mz mirror across x=0 / z=0 (rolled boxes flip sign for x); rot180 adds the
        180-degree-rotated copy (x -> -x, z -> -z)."""
        out = []

        def add(b):
            if b not in out:
                out.append(b)

        def flipx(b):
            (cx, cy, cz), h, r = b
            return ((-cx, cy, cz), h, -r)

        def flipz(b):
            (cx, cy, cz), h, r = b
            return ((cx, cy, -cz), h, r)

        for b in boxes:
            vs = [b]
            if mx:
                vs += [flipx(x) for x in vs]
            if mz:
                vs += [flipz(x) for x in vs]
            if rot180:
                vs += [flipz(flipx(x)) for x in vs]
            for v in vs:
                add(v)
        self.parts.append((title, out))

    def wall_x(self, x0, x1, z, h, t=0.5):
        return self.box((x0 + x1) / 2, h / 2, z, (x1 - x0) / 2, h / 2, t)

    def wall_z(self, x, z0, z1, h, t=0.5):
        return self.box(x, h / 2, (z0 + z1) / 2, t, h / 2, (z1 - z0) / 2)

    def pillar(self, x, z, h, r=0.6):
        return self.box(x, h / 2, z, r, h / 2, r)

    def slab(self, x, z, hx, hz, top, thick=0.4):
        return self.box(x, top - thick, z, hx, thick, hz)

    def block(self, x, z, hx, hz, top, base=0.0):
        return self.box(x, (top + base) / 2, z, hx, (top - base) / 2, hz)

    def ramp_x(self, edge_x, z, hz, up_toward_plus):
        """A 20 degree ramp whose high end (1.2 m) meets a block face at x = edge_x, rising toward +x if
        up_toward_plus (the block lies on its +x side)."""
        if up_toward_plus:
            return self.box(edge_x - RAMP_RUN, 0.4591, z, 1.7498, 0.15, hz, R20)
        return self.box(edge_x + RAMP_RUN, 0.4591, z, 1.7498, 0.15, hz, -R20)

    def ziggurat(self, cx, cz, halves, tops=None, ramps=True, ramp_hz=3):
        """Stacked blocks, tops 1.2, 2.4, 3.6 ... (single-jump steps); returns boxes. Ramp on the -x and +x sides."""
        bs = []
        for i, h in enumerate(halves):
            top = 1.2 * (i + 1)
            bs.append(self.block(cx, cz, h, h, top, 0.0 if i == 0 else 1.2 * i))
        if ramps:
            bs.append(self.ramp_x(cx - halves[0], cz, ramp_hz, True))
            bs.append(self.ramp_x(cx + halves[0], cz, ramp_hz, False))
        return bs

    # ---- spawn alcove ----
    def alcove(self, px, pz, face):
        """A 6 x 6 enclosure (5 m walls) around spawn (px, pz) with a 2.4 m door in the side `face` (0 +x, 1 +z, 2 -x, 3 -z)
        and a baffle wall 2.5 m outside it, so there is no straight sight line out of any alcove."""
        a, t, h = 3.2, 0.4, 5.0
        bs = []
        sides = {
            0: lambda door: [self.wall_z(px + a, pz - a, pz + a, h, t)],
            1: lambda door: [self.wall_x(px - a, px + a, pz + a, h, t)],
            2: lambda door: [self.wall_z(px - a, pz - a, pz + a, h, t)],
            3: lambda door: [self.wall_x(px - a, px + a, pz - a, h, t)],
        }
        d = 1.2  # half door
        for side in range(4):
            if side != face:
                bs += sides[side](False)
                continue
            if side == 0:
                bs += [self.wall_z(px + a, pz - a, pz - d, h, t), self.wall_z(px + a, pz + d, pz + a, h, t)]
            elif side == 2:
                bs += [self.wall_z(px - a, pz - a, pz - d, h, t), self.wall_z(px - a, pz + d, pz + a, h, t)]
            elif side == 1:
                bs += [self.wall_x(px - a, px - d, pz + a, h, t), self.wall_x(px + d, px + a, pz + a, h, t)]
            else:
                bs += [self.wall_x(px - a, px - d, pz - a, h, t), self.wall_x(px + d, px + a, pz - a, h, t)]
        gap = 2.6
        w = 3.4
        if face == 0:
            bs.append(self.wall_z(px + a + gap, pz - w, pz + w, h, t))
        elif face == 2:
            bs.append(self.wall_z(px - a - gap, pz - w, pz + w, h, t))
        elif face == 1:
            bs.append(self.wall_x(px - w, px + w, pz + a + gap, h, t))
        else:
            bs.append(self.wall_x(px - w, px + w, pz - a - gap, h, t))
        return bs


def build(half):
    L = Level(half)
    P = L.P
    H = half

    # ---- ground, void lip ----
    L.section("ground: one big floor, open void beyond it (kill plane y = -20)", [L.box(0, -0.5, 0, H, 0.5, H)])
    lip = []
    gaps = [-0.62 * H, -0.2 * H, 0.2 * H, 0.62 * H]   # centres of the 12 m void gaps on every side (kept symmetric)
    for side in "NS":
        z = -H + 0.25 if side == "N" else H - 0.25
        edges = [-H] + sum([[g - 6, g + 6] for g in gaps], []) + [H]
        for i in range(0, len(edges), 2):
            x0, x1 = edges[i], edges[i + 1]
            lip.append(L.box((x0 + x1) / 2, 0.5, z, (x1 - x0) / 2, 0.5, 0.25))
    for side in "WE":
        x = -H + 0.25 if side == "W" else H - 0.25
        edges = [-H] + sum([[g - 6, g + 6] for g in gaps], []) + [H]
        for i in range(0, len(edges), 2):
            z0, z1 = edges[i], edges[i + 1]
            lip.append(L.box(x, 0.5, (z0 + z1) / 2, 0.25, 0.5, (z1 - z0) / 2))
    L.section("void lip: 1 m jumpable lip with 12 m open gaps (fall in = kill plane)", lip)

    # ---- hub (mirrored on both axes): ziggurat spire, ramps east/west, gate arches, cover ----
    hub = [
        L.block(0, 0, 13, 13, 1.2), L.block(0, 0, 9, 9, 2.4, 1.2), L.block(0, 0, 5.5, 5.5, 3.6, 2.4),
        L.block(0, 0, 2.5, 2.5, 14.4, 3.6),
        L.ramp_x(13, 0, 3, False), L.ramp_x(-13, 0, 3, True),
    ]
    L.section("hub spire: tiers 1.2 / 2.4 / 3.6 and a 14.4 m pylon (grapple perch), ramps west + east", hub)
    cover = []
    for x, z in [(22, 22), (30, 12), (12, 30), (36, 36)]:
        cover.append(L.pillar(x, z, 3.0))
    for x, z, hx, hz in [(24, 6, 0.4, 4), (6, 24, 4, 0.4), (30, 26, 3, 0.4)]:
        cover.append(L.box(x, 0.5, z, hx, 0.5, hz))
    L.section("hub cover", cover, mx=True, mz=True)
    arches = []
    for x in (P(34),):
        arches += [L.pillar(x, 9, 10, 0.8), L.pillar(x, -9, 10, 0.8), L.slab(x, 0, 1.2, 10.5, 10.8, 0.4)]
    arch_all = list(arches)
    arch_z = [L.pillar(9, P(34), 10, 0.8), L.pillar(-9, P(34), 10, 0.8), L.slab(0, P(34), 10.5, 1.2, 10.8, 0.4)]
    L.section("hub gate arches (grapple anchors) on the axes", arch_all + arch_z, mx=True, mz=True)

    # ---- boulevards: long wall-run lanes along the axes, 8 m walls, to the void ----
    c = GAP / 2 + 0.5
    b0, b1 = P(46), P(100)
    bl = [L.wall_x(b0, b1, c, 8), L.wall_x(b0, b1, -c, 8), L.wall_z(c, b0, b1, 8), L.wall_z(-c, b0, b1, 8)]
    L.section("boulevards: wall-run lanes (5.6 m between faces, 8 m walls) from the hub gates to the void", bl, mx=True, mz=True)

    # ---- district A (180-degree pair): wall-run canyons + ziggurat + cover ----
    ax, az = P(66), P(66)
    can = []
    cz_, cx_ = az - 20, ax + 24            # canyon centre lines: along x at z = cz_, along z at x = cx_
    # canyon along x (x 38..100), split where the z canyon crosses it; canyon along z (z 26..70), split at the x canyon
    for z in (cz_ - c, cz_ + c):
        can += [L.wall_x(ax - 28, cx_ - c - 0.5, z, 9), L.wall_x(cx_ + c + 0.5, ax + 34, z, 9)]
    for x in (cx_ - c, cx_ + c):
        can += [L.wall_z(x, az - 40, cz_ - c - 0.5, 9), L.wall_z(x, cz_ + c + 0.5, az + 4, 9)]
    can += L.ziggurat(ax - 8, az + 18, [10, 7.5, 5, 3])
    can += [L.pillar(ax - 22, az + 4, 4), L.pillar(ax + 8, az + 20, 4), L.pillar(ax + 14, az + 34, 4)]
    can += [L.box(ax - 20, 0.5, az + 34, 4, 0.5, 0.4), L.box(ax + 6, 0.5, az - 6, 0.4, 0.5, 4)]
    L.section("district A (NE + SW): crossing wall-run canyons, a 4-tier ramp ziggurat, cover", can, rot180=True)

    # ---- district B (180-degree pair): tower cluster with stepped bases, sky bridges ----
    bx, bz = P(66), -P(66)
    twr = []
    tops = {}
    pts = [(-16, -16), (16, -16), (-16, 16), (16, 16)]
    for i, (dx, dz) in enumerate(pts):
        x, z = bx + dx, bz + dz
        twr += L.ziggurat(x, z, [6, 4.2, 2.4], ramps=False)     # stepped base to 3.6
        twr.append(L.block(x, z, 1.6, 1.6, 8.6, 3.6))           # shaft
        twr.append(L.slab(x, z, 5.5, 5.5, 9.6, 0.5))            # 11 x 11 platform, top 9.6
        twr.append(L.ramp_x(x - 6, z, 3, True))
        tops[i] = (x, z)
    # centre tower: tall spire with a high platform
    twr += L.ziggurat(bx, bz, [7, 5.2, 3.4, 2.2], ramps=False)
    twr.append(L.block(bx, bz, 1.8, 1.8, 19.4, 4.8))
    twr.append(L.slab(bx, bz, 5, 5, 20.0, 0.6))
    # sky bridges between the four corner towers at 9.6 m and from two towers to the centre spire at 9.6 -> ground hop
    for (i, j) in [(0, 1), (2, 3), (0, 2), (1, 3)]:
        (x0, z0), (x1, z1) = tops[i], tops[j]
        if x0 == x1:
            twr.append(L.slab(x0, (z0 + z1) / 2, 2.0, abs(z1 - z0) / 2 - 5.2, 9.6, 0.3))
        else:
            twr.append(L.slab((x0 + x1) / 2, z0, abs(x1 - x0) / 2 - 5.2, 2.0, 9.6, 0.3))
    twr += [L.pillar(bx - 34, bz + 6, 4), L.pillar(bx + 8, bz + 32, 4), L.box(bx - 30, 0.5, bz - 20, 4, 0.5, 0.4)]
    L.section("district B (SE + NW): five towers (stepped bases to 3.6, platforms at 9.6, a 20 m spire), sky bridges", twr, rot180=True)

    # ---- sky layer: hanging anchor slabs over the boulevards and hub (grapple highways, 40 m range) ----
    sky = []
    for x in (P(60), P(90)):
        sky.append(L.slab(x, 0, 2.5, 1.2, 15.0, 0.4))
        sky.append(L.slab(0, x, 1.2, 2.5, 15.0, 0.4))
    L.section("sky layer: anchor slabs above the boulevards (rope highways)", sky, mx=True, mz=True)

    # ---- outer ring for big maps ----
    if half > 150:
        outer = []
        for k in range(-3, 4):
            outer.append(L.pillar(P(100), P(30) * k, 6 + (k % 2) * 4, 1.0))
        L.section("outer ring (XL map): pillar rows with perches", outer, mx=True, mz=True)

    # ---- spawn alcoves: 24 pads, 8 at r=38 (hub ring), 8 at r=P(78)+, 8 at r=P(100) ----
    pads = []
    def ring(r, angles):
        for a in angles:
            x, z = r * math.cos(math.radians(a)), r * math.sin(math.radians(a))
            pads.append((round(x, 1), round(z, 1)))
    ring(P(46), [22.5 + 45 * k for k in range(8)])
    ring(P(78), [15, 75, 105, 165, 195, 255, 285, 345])
    ring(P(100), [8, 82, 98, 172, 188, 262, 278, 352])
    al = []
    for (x, z) in pads:
        # the door faces tangentially away from the origin axis so the baffle blocks the straight line out
        face = 1 if abs(x) >= abs(z) else 0
        face = (face + (2 if (x * z) < 0 else 0)) % 4
        al += L.alcove(x, z, face)
        L.spawns.append((x, 0.81, z))
    L.section("spawn alcoves: enclosed 6 x 6 pads with a baffled door, 24 spread over the map", al)

    # ---- pickups: contested spots ----
    pk = []
    pk += [(0, 15.4, 0, 30)]                                   # hub pylon top
    for (x, z) in [(P(34), 0), (-P(34), 0), (0, P(34)), (0, -P(34))]:
        pk.append((x, 12.0, z, 25))                            # gate arch lintels
    pk += [(P(66) + 16, 10.6, -P(66) + 16, 25), (-P(66) - 16, 10.6, P(66) - 16, 25)]   # corner tower platforms (district B)
    pk += [(P(66) - 8, 5.8, P(66) + 18, 25), (-P(66) + 8, 5.8, -P(66) - 18, 25)]   # ziggurat tops
    pk += [(P(66) + 24, 1.0, P(66) - 20, 20), (-P(66) - 24, 1.0, -P(66) + 20, 20)]  # canyon crossings (x = ax+24, z = az-20)
    pk += [(P(66), 21.0, -P(66), 40), (-P(66), 21.0, P(66), 40)]  # centre spires (rare)
    for (x, z) in [(P(75), 0), (-P(75), 0), (0, P(75)), (0, -P(75))]:
        pk.append((x, 1.0, z, 20))                             # boulevard mids
    L.pickups = pk
    return L


def write(L, path):
    lines = [
        "# SHINOBI FFA - %d x %d m free-for-all arena (open void edges, kill plane y=-20). 24 enclosed spawn alcoves." % (2 * L.half, 2 * L.half),
        "# GENERATED by scripts/gen_ffa_level.py (HALF=%d). Edit the script, not this file; tests: crates/shinobi-sim/tests/arena_ffa.rs" % L.half,
        "# x right, y up. Layers: ground, stepped towers (tops 9.6 m) with sky bridges, 14-20 m spires, anchor slabs at 15-20 m.",
        "",
    ]
    for title, bs in L.parts:
        lines.append("# --- %s ---" % title)
        for c, h, r in bs:
            lines.append("box %s %s %s  %s %s %s%s" % (f(c[0]), f(c[1]), f(c[2]), f(h[0]), f(h[1]), f(h[2]), (" " + f(r)) if r else ""))
        lines.append("")
    lines.append("# --- %d spawns (no mutual sight lines) ---" % len(L.spawns))
    for x, y, z in L.spawns:
        lines.append("spawn %s %s %s" % (f(x), f(y), f(z)))
    lines.append("")
    lines.append("# --- %d pickups at contested spots ---" % len(L.pickups))
    for x, y, z, s in L.pickups:
        lines.append("pickup %s %s %s %d" % (f(x), f(y), f(z), s))
    open(path, "w").write("\n".join(lines) + "\n")


def main():
    root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "levels")
    if len(sys.argv) == 3:
        write(build(float(sys.argv[1])), sys.argv[2])
        return
    write(build(112.0), os.path.join(root, "ffa.lvl"))
    write(build(168.0), os.path.join(root, "ffa_xl.lvl"))


main()
