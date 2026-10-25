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
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import runwalls  # noqa: E402

# debugging aid: SKIP=boulevards,canyon,towers,sky,hub,cover,alcoves drops those sections (AI-friendliness bisecting)
SKIP = set(filter(None, os.environ.get('SKIP', '').split(',')))

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
        if any(k in title for k in SKIP):
            return
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
        """A U-shaped bay (5 m walls: back + two sides, 6 m deep, 7 m wide) around spawn (px, pz), open toward `face`
        (0 +x, 1 +z, 2 -x, 3 -z). Open on purpose: the AI steers rather than path-finds and got stuck in baffled rooms."""
        w, d, t, h = 3.6, 3.2, 0.4, 5.0
        bs = []
        if face in (0, 2):
            sx = 1 if face == 0 else -1          # direction the bay opens
            bs.append(self.wall_z(px - sx * d, pz - w, pz + w, h, t))                # back
            bs.append(self.wall_x(min(px - sx * d, px + sx * 4.5), max(px - sx * d, px + sx * 4.5), pz - w, h, t))  # sides
            bs.append(self.wall_x(min(px - sx * d, px + sx * 4.5), max(px - sx * d, px + sx * 4.5), pz + w, h, t))
        else:
            sz = 1 if face == 1 else -1
            bs.append(self.wall_x(px - w, px + w, pz - sz * d, h, t))
            bs.append(self.wall_z(px - w, min(pz - sz * d, pz + sz * 4.5), max(pz - sz * d, pz + sz * 4.5), h, t))
            bs.append(self.wall_z(px + w, min(pz - sz * d, pz + sz * 4.5), max(pz - sz * d, pz + sz * 4.5), h, t))
        return bs


def _seg_hits_box(p0, p1, box):
    """Does the segment p0 -> p1 pass through the (possibly z-rolled) box? Slab test in the box frame."""
    (cx, cy, cz), (hx, hy, hz), rot = box
    c, sn = math.cos(rot), math.sin(rot)

    def local(p):
        dx, dy = p[0] - cx, p[1] - cy
        return (dx * c + dy * sn, -dx * sn + dy * c, p[2] - cz)

    a, b = local(p0), local(p1)
    t0, t1 = 0.0, 1.0
    for i, h in enumerate((hx, hy, hz)):
        d = b[i] - a[i]
        if abs(d) < 1e-9:
            if abs(a[i]) > h:
                return False
            continue
        ta, tb = (-h - a[i]) / d, (h - a[i]) / d
        if ta > tb:
            ta, tb = tb, ta
        t0, t1 = max(t0, ta), min(t1, tb)
        if t0 > t1:
            return False
    return True


def _visible(boxes, a, b):
    """Sight between two spawn pads (capsule centre y 0.81): eye of a to feet / hip / eye of b; any clear ray = visible."""
    eye = (a[0], 0.81 + 0.6, a[2])
    for ty in (0.1, 0.81, 0.81 + 0.6):
        tgt = (b[0], ty, b[2])
        if not any(_seg_hits_box(eye, tgt, bx) for bx in boxes):
            return True
    return False


def find_spawns(boxes, half, n):
    import random
    rnd = random.Random(7)
    solid = [b for b in boxes if b[0][1] + b[1][1] > 0.05]          # everything above the floor
    cands = []
    step = 5.0
    x = -half + 14
    while x <= half - 14:
        z = -half + 14
        while z <= half - 14:
            p = (x + rnd.uniform(-2, 2), 0.81, z + rnd.uniform(-2, 2))
            ok = True
            for (cx, cy, cz), (hx, hy, hz), rot in solid:
                m = 2.5 if rot == 0 else 6.0       # clear of walls (so the AI is not wedged), well clear of ramps
                if abs(p[0] - cx) <= hx + m and abs(p[2] - cz) <= hz + m and cy - hy < 2.5:
                    ok = False
                    break
            if ok:
                cands.append(p)
            z += step
        x += step
    rnd.shuffle(cands)
    # bounding boxes in xz for a cheap reject before the slab test
    bb = [(cx - (abs(hx * math.cos(r)) + abs(hy * math.sin(r))), cx + (abs(hx * math.cos(r)) + abs(hy * math.sin(r))), cz - hz, cz + hz, bx)
          for bx in solid for ((cx, cy, cz), (hx, hy, hz), r) in [bx]]

    def vis(a, b):
        lo_x, hi_x, lo_z, hi_z = min(a[0], b[0]), max(a[0], b[0]), min(a[2], b[2]), max(a[2], b[2])
        near = [e[4] for e in bb if e[1] >= lo_x and e[0] <= hi_x and e[3] >= lo_z and e[2] <= hi_z]
        return _visible(near, a, b) or _visible(near, b, a)

    chosen = []
    pool = cands[:700]
    while len(chosen) < n and pool:
        best, best_score = None, -1.0
        for c in pool:
            dmin = min([math.hypot(c[0] - o[0], c[2] - o[2]) for o in chosen] or [1e9])
            if dmin > best_score:
                best, best_score = c, dmin
        chosen.append(best)
        pool = [c for c in pool if c is not best and math.hypot(c[0] - best[0], c[2] - best[2]) > 14.0 and not vis(c, best)]
    if len(chosen) < n:
        print("warning: only %d spawns found (wanted %d)" % (len(chosen), n))
    return [(round(c[0], 1), 0.81, round(c[2], 1)) for c in chosen]


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

    # ---- monolith field: free-standing 8 m grid walls scattered on the open ground. They break the long sight lines
    # (so pads can hide from each other without enclosures the AI cannot leave) and give shurikens corners to bounce off.
    import random
    rnd = random.Random(11)
    placed = [b for _, bs in L.parts for b in bs if b[0][1] + b[1][1] > 0.05 and b[1][0] < 60 and b[1][2] < 60]
    mono = []
    tries = 0
    target_n = int(70 * (half / 112.0) ** 2)
    while len(mono) < target_n and tries < 20000:
        tries += 1
        x, z = rnd.uniform(-half + 16, half - 16), rnd.uniform(-half + 16, half - 16)
        along_x = rnd.random() < 0.5
        hx, hz = (7.0, 0.5) if along_x else (0.5, 7.0)
        # keep clear of everything placed (lanes, towers, hub) and of the other monoliths
        if any(abs(x - cx) <= hx + 5 + ahx and abs(z - cz) <= hz + 5 + ahz and cy - ahy < 12 for (cx, cy, cz), (ahx, ahy, ahz), r in placed + mono):
            continue
        if abs(x) < 20 and abs(z) < 20:
            continue                                 # the hub plaza stays open
        mono.append(((x, 4.0, z), (hx, 4.0, hz), 0.0))
    L.section("monolith field: free-standing 8 m walls on the open ground", mono)

    # ---- spawns: found, not placed. Open ground pads with no sight line to each other (checked against every box), far
    # apart. (Enclosed alcoves made the AI stall: it steers, it does not path-find.) ----
    all_boxes = [b for _, bs in L.parts for b in bs]
    L.spawns = find_spawns(all_boxes, half, 28)

    # ---- run walls: long tall wall-run panels, each with a hook anchor past its end (scripts/runwalls.py) ----
    rw = runwalls.boxes('ffa' if half < 140 else 'ffa_xl')
    L.section('run walls: 20 m x 7 m wall-run panels, a hook anchor 8 m past each end and 2 m above', [L.box(cx, cy, cz, hx, hy, hz) for _k, cx, cy, cz, hx, hy, hz in rw])

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
    import lvl_env
    lvl_env.main([os.path.join(root, "ffa.lvl"), os.path.join(root, "ffa_xl.lvl")])


main()
