"""Prop layer for destructible levels: crates, barrels, statues, lantern posts, banner poles.

Library (each emits small `dbox`es onto a `lvl_lib.Level`, base resting exactly on y so the support rules hold):

    from lvl_props import crate, crate_stack, barrel, statue, lantern, banner
    crate(L, x, y, z)            # y = the surface the prop stands on

CLI (applies the layer to ANY existing level; never edits the input):

    python3 scripts/lvl_props.py levels/ffa_split.lvl levels/ffa_split_props.lvl [--seed N] [--max-boxes 1800]

It reads the .lvl, finds walkable box tops (axis-aligned, >= 4x4 m, nothing above them), and scatters props (seeded, so the
output is deterministic) that never sit near a spawn / pickup / weapon pedestal, never overhang an edge, never intrude on
another box or a ramp. Hit points: one katana hit (piece_damage 60) smashes a small prop, 2-3 a statue.
"""
import math
import random
import sys

sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from lvl_lib import Level, fnum  # noqa: E402

CRATE_HP, BARREL_HP, POST_HP, LAMP_HP, POLE_HP, CLOTH_HP = 40.0, 30.0, 45.0, 20.0, 40.0, 25.0
FIG_BODY_HP, FIG_HEAD_HP = 100.0, 50.0  # a statue: torso 2 hits, head 1


def crate(L, x, y, z, s=0.5, rot=0.0):
    """One crate (half side `s`), base on y. 1 box."""
    L.dbox(x, y + s, z, s, s, s, CRATE_HP, rot)
    return 2 * s


def crate_stack(L, x, y, z, n=2):
    """A stack of n crates (0.5..0.6 m half side), each resting exactly on the one below. n boxes."""
    top = y
    for i in range(n):
        s = 0.6 if i == 0 else 0.5
        top += crate(L, x, top, z, s)
    return top


def barrel(L, x, y, z):
    """A barrel as an upright box (0.35 x 0.5 half extents). 1 box."""
    L.dbox(x, y + 0.5, z, 0.35, 0.5, 0.35, BARREL_HP)


def statue(L, x, y, z):
    """Permanent pedestal + destructible torso + head. 3 boxes. Returns top y."""
    L.perm(x, y + 0.3, z, 0.6, 0.3, 0.6)
    y += 0.6
    L.dbox(x, y + 0.9, z, 0.35, 0.9, 0.25, FIG_BODY_HP)
    y += 1.8
    L.dbox(x, y + 0.25, z, 0.25, 0.25, 0.25, FIG_HEAD_HP)
    return y + 0.5


def lantern(L, x, y, z):
    """Lantern post (thin pole) with a lamp box on top. 2 boxes."""
    L.dbox(x, y + 1.25, z, 0.12, 1.25, 0.12, POST_HP)
    L.dbox(x, y + 2.5 + 0.2, z, 0.25, 0.2, 0.25, LAMP_HP)


def banner(L, x, y, z, along_x=True):
    """Banner pole with a hanging cloth slab. 2 boxes."""
    L.dbox(x, y + 1.75, z, 0.1, 1.75, 0.1, POLE_HP)
    if along_x:
        L.dbox(x + 0.1 + 0.45, y + 2.7, z, 0.45, 0.7, 0.04, CLOTH_HP)
    else:
        L.dbox(x, y + 2.7, z + 0.1 + 0.45, 0.04, 0.7, 0.45, CLOTH_HP)


# (builder, boxes, footprint half extent, weight)
KINDS = [
    ("crate", lambda L, x, y, z, r: crate(L, x, y, z, rot=0.0), 1, 0.6, 5),
    ("stack2", lambda L, x, y, z, r: crate_stack(L, x, y, z, 2), 2, 0.7, 3),
    ("stack3", lambda L, x, y, z, r: crate_stack(L, x, y, z, 3), 3, 0.7, 1),
    ("barrel", lambda L, x, y, z, r: barrel(L, x, y, z), 1, 0.4, 4),
    ("statue", lambda L, x, y, z, r: statue(L, x, y, z), 3, 0.7, 1),
    ("lantern", lambda L, x, y, z, r: lantern(L, x, y, z), 2, 0.3, 3),
    ("banner", lambda L, x, y, z, r: banner(L, x, y, z, r.random() < 0.5), 2, 1.0, 2),
]
HEIGHT = 3.6  # clear height each prop needs above its surface


def parse(text):
    """-> (boxes [(cx,cy,cz,hx,hy,hz,rot)], points [(x,y,z)] of spawns / pickups / weapons, n_boxes)."""
    boxes, pts = [], []
    for raw in text.splitlines():
        t = raw.split("#")[0].split()
        if not t:
            continue
        v = t[0]
        f = [float(a) for a in t[1:]]
        if v in ("box", "dbox"):
            rot = f[6] if len(f) > 6 else 0.0
            boxes.append((f[0], f[1], f[2], f[3], f[4], f[5], rot))
        elif v in ("spawn", "pickup", "yumi", "tag"):
            pts.append((f[0], f[1], f[2]))
    return boxes, pts


def candidate_tops(boxes, min_size=4.0):
    """Walkable tops: flat boxes at least min_size on both horizontal sides. Clear air above is checked per prop in `place`."""
    return [b for b in boxes if not b[6] and 2 * b[3] >= min_size and 2 * b[5] >= min_size]


def place(boxes, pts, seed=7, max_boxes=1800, max_items=300, margin=0.9):
    rng = random.Random(seed)
    tops = candidate_tops(boxes)
    budget = max(0, max_boxes - len(boxes))
    L = Level("props")
    placed = []  # (x, y, z, r)
    used = 0
    items = 0
    names = [k for k in KINDS]
    weights = [k[4] for k in KINDS]
    cand = []
    for b in tops:
        cx, cy, cz, hx, hy, hz, _ = b
        n = int(min(12, (4 * hx * hz) / 45.0 + 1))
        for _ in range(n):
            cand.append((b, rng.random()))
    rng.shuffle(cand)
    for b, _ in cand:
        if used >= budget or items >= max_items:
            break
        cx, cy, cz, hx, hy, hz, _ = b
        kind = rng.choices(names, weights)[0]
        name, build, nbox, r, _w = kind
        if used + nbox > budget:
            continue
        lo = margin + r
        if hx - lo <= 0 or hz - lo <= 0:
            continue
        # half the time line the prop along an edge (cover along walkways), else anywhere
        if rng.random() < 0.5:
            side = rng.choice("nsew")
            x = cx + rng.uniform(-(hx - lo), hx - lo)
            z = cz + rng.uniform(-(hz - lo), hz - lo)
            if side == "n":
                z = cz - (hz - lo)
            elif side == "s":
                z = cz + (hz - lo)
            elif side == "w":
                x = cx - (hx - lo)
            else:
                x = cx + (hx - lo)
        else:
            x = cx + rng.uniform(-(hx - lo), hx - lo)
            z = cz + rng.uniform(-(hz - lo), hz - lo)
        y = cy + hy
        if any(math.hypot(x - px, z - pz) < 3.0 and abs(py - y) < 6.0 for px, py, pz in pts):
            continue
        if any(abs(x - qx) < r + qr + 0.4 and abs(z - qz) < r + qr + 0.4 and abs(y - qy) < 0.1 for qx, qy, qz, qr in placed):
            continue
        # no other box (or ramp, widened) may intrude on the prop's volume
        bad = False
        for o in boxes:
            if o is b:
                continue
            ocx, ocy, ocz, ohx, ohy, ohz, orot = o
            pad = (max(ohx, ohy) * 1.5 + 1.0) if orot else 0.0
            if abs(ocx - x) < ohx + r + pad and abs(ocz - z) < ohz + r + pad and ocy + ohy > y + 0.02 and ocy - ohy < y + HEIGHT:
                bad = True
                break
        if bad:
            continue
        build(L, x, y, z, rng)
        placed.append((x, y, z, r))
        used += nbox
        items += 1
    return L, items


def format_boxes(L):
    out = []
    for _, sec in L.sections:
        for (cx, cy, cz, hx, hy, hz, rot, hp) in sec:
            if hp:
                out.append("dbox %s %s %s  %s %s %s  %s %s" % tuple(fnum(v) for v in (cx, cy, cz, hx, hy, hz, rot, hp)))
            else:
                out.append("box %s %s %s  %s %s %s" % tuple(fnum(v) for v in (cx, cy, cz, hx, hy, hz)))
    return out


def apply(text, seed=7, max_boxes=1800):
    boxes, pts = parse(text)
    L, items = place(boxes, pts, seed, max_boxes)
    lines = format_boxes(L)
    out = text.rstrip("\n") + "\n\n# --- props (scripts/lvl_props.py, seed %d): %d items, %d boxes ---\n" % (seed, items, len(lines))
    return out + "\n".join(lines) + "\n", items, len(lines), len(boxes)


def main(argv):
    args = [a for a in argv if not a.startswith("--")]
    opt = {a.split("=")[0]: a.split("=")[1] for a in argv if a.startswith("--") and "=" in a}
    if len(args) != 2:
        print(__doc__)
        return 2
    seed = int(opt.get("--seed", 7))
    cap = int(opt.get("--max-boxes", 1800))
    text, items, nprop, nold = apply(open(args[0]).read(), seed, cap)
    open(args[1], "w").write(text)
    print("wrote %s: %d props (%d boxes) on %d existing = %d boxes (cap %d)" % (args[1], items, nprop, nold, nold + nprop, cap))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
