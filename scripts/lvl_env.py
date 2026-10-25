#!/usr/bin/env python3
"""Environment pass: fill the empty space of a level with melee furniture.

The combat loop is "use the world": knock a victim into a wall (wall-splat), cut them off a ledge, break the cover they
are hiding behind, run the wall they are pinned against. This pass appends (idempotently) a marked section of those
elements to an existing `.lvl`; it never edits what is already there and never moves spawns / pickups / pedestals.

    python3 scripts/lvl_env.py levels/ffa_small.lvl [...]            # rewrite in place (the section is replaced)
    python3 scripts/lvl_env.py levels/ffa.lvl --dry-run              # report only
    python3 scripts/lvl_env.py levels/8v8.lvl --clear                # drop the section again

What it places, on flat, open, permanent surfaces (top <= 12 m), with >= 3.7 m of clearance round everything that is
already there and >= 3.5 m from every spawn / pickup / pedestal:

* break-cover  (dbox): 3.5 m tall block, 2.6-3.4 m square. One katana combo smashes it: cover that dies.
* splat-wall   (box) : full-height 0.7 m thick panel, 4-8 m long. The wall you launch people into; runnable.
* splat-alley  (box) : two facing panels 3.5-4.3 m apart: a knock in any direction ends in one of them.
* monolith     (box) : 6-9 m tall pillar. Sight breaker, grapple anchor, splat surface.

Nothing is waist high without a way up, so `unwalkable_obstacles` stays clean (panels are >= WALL_RISE). Objects are axis
aligned and never overlap one another (the pass tracks what it placed). Deterministic: same input, same output.
"""
import math
import random
import sys

MARK = "# === ENVIRONMENT PASS (scripts/lvl_env.py; regenerating from the generator drops it: run this again) ==="
HEADER = "# Melee furniture: break-cover, splat-walls, splat-alleys, monoliths. Clearance >= 3.7 m, spawns/pickups >= 3.5 m."

MARGIN = 3.7       # clearance between new geometry and existing geometry (run-wall zones need 3.5 m)
SPAWN_CLEAR = 3.5  # clearance from spawn / pickup / pedestal points
EDGE_KEEP = 6.0    # keep this far inside the playable bounds
MAX_TOP = 12.0     # only dress floors / low decks (never roofs high in the sky)
MIN_SURFACE = 8.0  # a surface must be at least this wide both ways

# (name, kind, needed height above the surface, needed free footprint half x, half z) - used for candidate filtering
ELEMS = {
    "cover": ("dbox", 3.5, (1.5, 1.8)),
    "splat": ("box", 5.0, (3.4, 3.4)),
    "alley": ("box", 5.0, (4.6, 4.6)),
    "monolith": ("box", 9.5, (2.0, 2.0)),
}


def fnum(v):
    s = ("%.4f" % v).rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def parse(text):
    """-> (boxes [(cx,cy,cz,hx,hy,hz,rot,hp)], points [(x,y,z)], raw_lines)."""
    boxes, pts = [], []
    for raw in text.splitlines():
        t = raw.split("#")[0].split()
        if not t:
            continue
        v, f = t[0], [float(a) for a in t[1:]]
        if v in ("box", "dbox"):
            rot = f[6] if len(f) > 6 else 0.0
            hp = f[7] if v == "dbox" and len(f) > 7 else 0.0
            boxes.append((f[0], f[1], f[2], f[3], f[4], f[5], rot, hp))
        elif v in ("spawn", "pickup", "yumi", "tag"):
            pts.append((f[0], f[1], f[2]))
    return boxes, pts


def aabb(b):
    """Horizontal AABB of a box (rotated ones are conservatively boxed)."""
    cx, cy, cz, hx, hy, hz, rot, _hp = b
    c, s = abs(math.cos(rot)), abs(math.sin(rot))
    return (cx - (hx * c + hz * s), cx + (hx * c + hz * s), cz - (hx * s + hz * c), cz + (hx * s + hz * c))


def overlap(a, b, m):
    """AABBs (x0,x1,z0,z1) overlap with margin m."""
    return not (a[1] + m <= b[0] or b[1] + m <= a[0] or a[3] + m <= b[2] or b[3] + m <= a[2])


def point_near_rect(p, r, d):
    dx = max(r[0] - p[0], 0.0, p[0] - r[1])
    dz = max(r[2] - p[1], 0.0, p[1] - r[3])
    return dx * dx + dz * dz < d * d


def tops(boxes):
    """Flat surfaces players stand on: axis aligned, wide enough, low enough, not destructible floors."""
    out = []
    for b in boxes:
        cx, cy, cz, hx, hy, hz, rot, hp = b
        top = cy + hy
        if rot != 0.0 or hp > 0.0 or top < -0.5 or top > MAX_TOP:
            continue
        if 2 * hx < MIN_SURFACE or 2 * hz < MIN_SURFACE:
            continue
        # above the ground layer only big decks: the small tiers and ledges are traversal routes, keep them clear
        if top > 0.5 and (2 * hx < 12.0 or 2 * hz < 12.0 or 4 * hx * hz < 400.0):
            continue
        out.append(b)
    return out


def headroom(boxes, r, top, need):
    """True when no existing box hangs into [top, top + need] over the footprint r."""
    for b in boxes:
        x0, x1, z0, z1 = aabb(b)
        if not overlap(r, (x0, x1, z0, z1), 0.0):
            continue
        bot, up = b[1] - b[4], b[1] + b[4]
        if up > top + 0.05 and bot < top + need - 0.05:
            return False
    return True


def build(name, boxes, pts):
    """Pick placements -> [(kind, [boxes])]; each box is (cx,cy,cz,hx,hy,hz,hp)."""
    rng = random.Random(sum(ord(ch) * (i + 7) for i, ch in enumerate(name.rsplit("/", 1)[-1])))
    surfaces = tops(boxes)
    if not surfaces:
        return []
    # playable bounds: the biggest flat surface plus everything already built on it
    ground = max(surfaces, key=lambda b: b[3] * b[5])
    x0, x1, z0, z1 = aabb(ground)
    placed, rects = [], []
    points = [(p[0], p[2]) for p in pts]

    def free(r, need, top):
        if not (x0 + EDGE_KEEP <= r[0] and r[1] <= x1 - EDGE_KEEP and z0 + EDGE_KEEP <= r[2] and r[3] <= z1 - EDGE_KEEP):
            return False
        for b in boxes:
            if b[1] + b[4] <= top + 0.05 or b[1] - b[4] >= top + need - 0.05:
                continue  # the surface we stand on, or something well above us
            if overlap(r, aabb(b), MARGIN):
                return False
        for pr in rects:
            if overlap(r, pr, 1.2):
                return False
        for p in points:
            if point_near_rect(p, r, SPAWN_CLEAR):
                return False
        return headroom(boxes, r, top, need)

    target = int(min(140, max(6, (x1 - x0) * (z1 - z0) / 240.0)))
    tries = 0
    while len(placed) < target and tries < target * 120:
        tries += 1
        s = rng.choice(surfaces)
        sx0, sx1, sz0, sz1 = aabb(s)
        top = s[1] + s[4]
        pick = rng.choices(["cover", "splat", "alley", "monolith"], [46, 28, 14, 12])[0]
        if pick == "cover":
            hx = rng.uniform(1.3, 1.7)
            hz = rng.uniform(1.3, 1.7)
            cx = rng.uniform(sx0 + hx + 1.0, sx1 - hx - 1.0)
            cz = rng.uniform(sz0 + hz + 1.0, sz1 - hz - 1.0)
            r = (cx - hx, cx + hx, cz - hz, cz + hz)
            if free(r, 3.5, top):
                hp = 130.0 + rng.uniform(0, 50)
                placed.append(("break-cover", [(cx, top + 1.75, cz, hx, 1.75, hz, hp)]))
                rects.append(r)
        elif pick == "splat":
            long_x = rng.random() < 0.5
            half_len = rng.uniform(2.0, 4.0)
            hx, hz = (half_len, 0.35) if long_x else (0.35, half_len)
            h = rng.uniform(2.0, 2.4)
            cx = rng.uniform(sx0 + hx + 1.0, sx1 - hx - 1.0)
            cz = rng.uniform(sz0 + hz + 1.0, sz1 - hz - 1.0)
            r = (cx - hx, cx + hx, cz - hz, cz + hz)
            if free(r, 5.0, top):
                placed.append(("splat-wall", [(cx, top + h, cz, hx, h, hz, 0.0)]))
                rects.append(r)
        elif pick == "alley":
            long_x = rng.random() < 0.5
            half_len = rng.uniform(2.5, 4.0)
            sep = rng.uniform(4.2, 5.0)  # centre to centre: 3.5-4.3 m of air between the faces
            h = rng.uniform(2.0, 2.4)
            if long_x:
                hx, hz = half_len, 0.35
                cx = rng.uniform(sx0 + hx + 1.0, sx1 - hx - 1.0)
                cz = rng.uniform(sz0 + sep / 2 + 1.0, sz1 - sep / 2 - 1.0)
                r = (cx - hx, cx + hx, cz - sep / 2 - hz, cz + sep / 2 + hz)
                walls = [(cx, top + h, cz - sep / 2, hx, h, hz, 0.0), (cx, top + h, cz + sep / 2, hx, h, hz, 0.0)]
            else:
                hx, hz = 0.35, half_len
                cx = rng.uniform(sx0 + sep / 2 + 1.0, sx1 - sep / 2 - 1.0)
                cz = rng.uniform(sz0 + hz + 1.0, sz1 - hz - 1.0)
                r = (cx - sep / 2 - hx, cx + sep / 2 + hx, cz - hz, cz + hz)
                walls = [(cx - sep / 2, top + h, cz, hx, h, hz, 0.0), (cx + sep / 2, top + h, cz, hx, h, hz, 0.0)]
            if free(r, 5.0, top):
                placed.append(("splat-alley", walls))
                rects.append(r)
        else:
            hx = rng.uniform(1.1, 1.6)
            hz = rng.uniform(1.1, 1.6)
            h = rng.uniform(3.0, 4.5)
            cx = rng.uniform(sx0 + hx + 1.0, sx1 - hx - 1.0)
            cz = rng.uniform(sz0 + hz + 1.0, sz1 - hz - 1.0)
            r = (cx - hx, cx + hx, cz - hz, cz + hz)
            if free(r, 9.5, top):
                placed.append(("monolith", [(cx, top + h, cz, hx, h, hz, 0.0)]))
                rects.append(r)
    return placed


def dress(text, name):
    cut = text.split(MARK)[0].rstrip("\n")
    boxes, pts = parse(cut)
    placed = build(name, boxes, pts)
    if not placed:
        return cut + "\n", {}, 0
    lines = [cut, "", MARK, HEADER]
    counts = {}
    for kind, items in placed:
        counts[kind] = counts.get(kind, 0) + 1
        lines.append("# --- %s x%d ---" % (kind, counts[kind]))
        for (cx, cy, cz, hx, hy, hz, hp) in items:
            if hp:
                lines.append("dbox %s %s %s  %s %s %s  0 %s" % (fnum(cx), fnum(cy), fnum(cz), fnum(hx), fnum(hy), fnum(hz), fnum(hp)))
            else:
                lines.append("box %s %s %s  %s %s %s" % (fnum(cx), fnum(cy), fnum(cz), fnum(hx), fnum(hy), fnum(hz)))
    return "\n".join(lines) + "\n", counts, sum(len(i) for _, i in placed)


def main(argv):
    dry = "--dry-run" in argv
    clear = "--clear" in argv
    paths = [a for a in argv if not a.startswith("--")]
    for path in paths:
        text = open(path).read()
        if clear:
            if MARK in text:
                open(path, "w").write(text.split(MARK)[0].rstrip("\n") + "\n")
                print("%s: environment section removed" % path)
            continue
        out, counts, n = dress(text, path)
        summary = ", ".join("%s %d" % kv for kv in sorted(counts.items())) or "nothing (no open flat surface)"
        print("%s: +%d boxes (%s)" % (path, n, summary))
        if not dry:
            open(path, "w").write(out)


if __name__ == "__main__":
    # 8v8 (mirror symmetry / pinned grapple lines) and runs (permanent traversal gauntlet) stay untouched
    main(sys.argv[1:] or ["levels/ffa_small.lvl", "levels/arena.lvl", "levels/ffa.lvl", "levels/ffa_xl.lvl", "levels/demo.lvl"])
