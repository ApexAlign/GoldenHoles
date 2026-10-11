"""Shared level builder for the (almost) totally destructible maps. See docs/destruction_design.md, docs/destructible.md.

    from lvl_lib import Level            # scripts/ is on sys.path when run as `python3 scripts/gen_x.py`
    L = Level("TITLE", hp_scale=1.0)
    L.section("tower")
    L.bricks(-10, 10, 0, 40, -10, 10, mat="concrete", size=(4, 4, 4))   # fills the box with a running-bond brick grid
    L.floor(-30, 30, -30, 30, top=60, thick=2, mat="steel")             # a walkable deck made of plates
    L.perm(0, 59, 0, 5, 1, 5)                                          # a permanent box (never breaks)
    L.spawn(0, 61, 0); L.pickup(5, 62, 0); L.weapon("yumi", 0, 63, 0)
    L.write("levels/ffa_x.lvl", header=[...])

Every `bricks` / `floor` / `column` box becomes a `dbox` (destructible, hit points = material hp per m^3 x volume x hp_scale).
Bricks rest on each other exactly (same face heights), so `shinobi_sim::breaks` support rules make walls and towers collapse
when their base goes: a brick falls when everything under it is gone. Stagger (running bond) makes walls arch over holes.

Budgets (tests and the renderer): <= 1800 boxes per level, >= 70 % of the volume destructible, bricks 2..6 m on a side.
"""
import math
import sys

# hit points per m^3 (a 4x4x4 m = 64 m^3 brick: glass 384, wood 1152, brick 1920, concrete 3520, steel 7040). Tuned with the
# Balance `destruct.*` keys at runtime; these are the BASE numbers. katana 60 / star 7 / tag blast 250 at the centre.
MATERIALS = {"glass": 0.6, "wood": 1.8, "brick": 3.0, "concrete": 5.5, "steel": 11.0}
# a "brick" is small: hp per box is volume-based but floored so tiny boxes are not paper
MIN_HP = 20.0


def fnum(v):
    s = ("%.4f" % v).rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


class Level:
    def __init__(self, title, hp_scale=1.0):
        self.title = title
        self.hp_scale = hp_scale
        self.sections = []  # (title, [(cx, cy, cz, hx, hy, hz, rot, hp)])
        self.cur = None
        self.spawns, self.pickups, self.weapons = [], [], []
        self.section("main")

    def section(self, title):
        self.cur = []
        self.sections.append((title, self.cur))

    # ------------------------------------------------------------------------------------------------ raw boxes
    def perm(self, cx, cy, cz, hx, hy, hz, rot=0.0):
        """A permanent `box` (centre + half extents)."""
        self.cur.append((cx, cy, cz, hx, hy, hz, rot, 0.0))

    def dbox(self, cx, cy, cz, hx, hy, hz, hp, rot=0.0):
        self.cur.append((cx, cy, cz, hx, hy, hz, rot, hp))

    def hp_for(self, hx, hy, hz, mat):
        vol = 8.0 * hx * hy * hz
        return max(MIN_HP, MATERIALS[mat] * vol) * self.hp_scale

    def block(self, x0, x1, y0, y1, z0, z1, mat="brick", dest=True):
        """One box from corner coordinates; destructible with the material's hp (or permanent)."""
        x0, x1, y0, y1, z0, z1 = min(x0, x1), max(x0, x1), min(y0, y1), max(y0, y1), min(z0, z1), max(z0, z1)
        c = ((x0 + x1) / 2, (y0 + y1) / 2, (z0 + z1) / 2)
        h = ((x1 - x0) / 2, (y1 - y0) / 2, (z1 - z0) / 2)
        if dest:
            self.dbox(*c, *h, self.hp_for(*h, mat))
        else:
            self.perm(*c, *h)

    # ------------------------------------------------------------------------------------------------ brick fills
    def bricks(self, x0, x1, y0, y1, z0, z1, mat="brick", size=(4.0, 3.0, 4.0), bond=True):
        """Fill the region with a grid of destructible bricks, layer by layer from y0 up. `size` = (x, y, z) of a brick; with
        `bond` every other layer is shifted half a brick in x and z (a running bond: neighbours share load, arches form).
        Edge bricks are clipped to the region. Returns the number of boxes."""
        sx, sy, sz = size
        n = 0
        ly = 0
        y = y0
        while y < y1 - 1e-6:
            ya, yb = y, min(y + sy, y1)
            off_x = (sx / 2.0 if (bond and ly % 2) else 0.0)
            off_z = (sz / 2.0 if (bond and ly % 2) else 0.0)
            xs = _cuts(x0, x1, sx, off_x)
            zs = _cuts(z0, z1, sz, off_z)
            for (xa, xb) in xs:
                for (za, zb) in zs:
                    if xb - xa < 0.4 or zb - za < 0.4:
                        continue
                    self.block(xa, xb, ya, yb, za, zb, mat)
                    n += 1
            y = yb
            ly += 1
        return n

    def floor(self, x0, x1, z0, z1, top, thick=2.0, mat="concrete", size=(6.0, 6.0), dest=True):
        """A walkable deck of plates (top face at `top`, `thick` deep). Plates are `size` (x, z) metres, all flush."""
        xs, zs = _cuts(x0, x1, size[0], 0.0), _cuts(z0, z1, size[1], 0.0)
        n = 0
        for (xa, xb) in xs:
            for (za, zb) in zs:
                if xb - xa < 0.4 or zb - za < 0.4:
                    continue
                self.block(xa, xb, top - thick, top, za, zb, mat, dest)
                n += 1
        return n

    def column(self, cx, cz, half, y0, y1, mat="concrete", seg=4.0):
        """A square column stacked from `seg` m segments (falls from the foot up when its base breaks)."""
        n = max(1, int(math.ceil((y1 - y0) / seg - 1e-6)))
        h = (y1 - y0) / n
        for i in range(n):
            self.block(cx - half, cx + half, y0 + i * h, y0 + (i + 1) * h, cz - half, cz + half, mat)
        return n

    def wall_x(self, x0, x1, y0, y1, z, thick=2.0, mat="brick", size=(4.0, 3.0)):
        """A wall along x at depth z, `thick` thick, built of bricks."""
        return self.bricks(x0, x1, y0, y1, z - thick / 2, z + thick / 2, mat, (size[0], size[1], thick), bond=True)

    def wall_z(self, z0, z1, y0, y1, x, thick=2.0, mat="brick", size=(4.0, 3.0)):
        return self.bricks(x - thick / 2, x + thick / 2, y0, y1, z0, z1, mat, (thick, size[1], size[0]), bond=True)

    def beam(self, x0, x1, y, z, thick=1.5, mat="steel", width=None):
        """A thin horizontal girder along x (a hook anchor / tightrope): one destructible box."""
        w = width or thick
        self.block(x0, x1, y - thick / 2, y + thick / 2, z - w / 2, z + w / 2, mat)

    # ------------------------------------------------------------------------------------------------ entities
    def spawn(self, x, y, z):
        """y = capsule centre (deck top + 0.81)."""
        self.spawns.append((x, y, z))

    def pickup(self, x, y, z, respawn=25):
        self.pickups.append((x, y, z, respawn))

    def weapon(self, kind, x, y, z, respawn=45):
        self.weapons.append((kind, x, y, z, respawn))

    # ------------------------------------------------------------------------------------------------ output
    def stats(self):
        n = nd = 0
        vol = vold = 0.0
        for _, items in self.sections:
            for (cx, cy, cz, hx, hy, hz, rot, hp) in items:
                v = 8 * hx * hy * hz
                n += 1
                vol += v
                if hp:
                    nd += 1
                    vold += v
        return n, nd, (vold / vol if vol else 0.0)

    def make_pedestals_permanent(self):
        """A box that holds a pickup / weapon / spawn must never break: make every box in a column under one permanent."""
        pts = [(p[0], p[1], p[2]) for p in self.pickups] + [(w[1], w[2], w[3]) for w in self.weapons] + [(s[0], s[1] - 0.81 + 1.0, s[2]) for s in self.spawns]
        for _, items in self.sections:
            for k, (cx, cy, cz, hx, hy, hz, rot, hp) in enumerate(items):
                if not hp:
                    continue
                for (px, py, pz) in pts:
                    if abs(px - cx) <= hx + 0.6 and abs(pz - cz) <= hz + 0.6 and py - 1.0 >= cy - hy - 0.5 and py - 1.0 <= cy + hy + 0.5:
                        items[k] = (cx, cy, cz, hx, hy, hz, rot, 0.0)
                        break

    def write(self, path, header=()):
        self.make_pedestals_permanent()
        lines = ["# " + self.title] + ["# " + h for h in header] + [""]
        n = 0
        for title, items in self.sections:
            if not items:
                continue
            lines.append("# --- %s ---" % title)
            for (cx, cy, cz, hx, hy, hz, rot, hp) in items:
                if hp:
                    lines.append("dbox %s %s %s  %s %s %s  %s %s" % (fnum(cx), fnum(cy), fnum(cz), fnum(hx), fnum(hy), fnum(hz), fnum(rot), fnum(hp)))
                else:
                    row = "box %s %s %s  %s %s %s" % (fnum(cx), fnum(cy), fnum(cz), fnum(hx), fnum(hy), fnum(hz))
                    if rot:
                        row += " " + fnum(rot)
                    lines.append(row)
                n += 1
            lines.append("")
        lines.append("# --- %d spawns ---" % len(self.spawns))
        for x, y, z in self.spawns:
            lines.append("spawn %s %s %s" % (fnum(x), fnum(y), fnum(z)))
        lines.append("")
        lines.append("# --- %d pickups ---" % len(self.pickups))
        for x, y, z, r in self.pickups:
            lines.append("pickup %s %s %s %s" % (fnum(x), fnum(y), fnum(z), fnum(r)))
        lines.append("")
        lines.append("# --- %d power-weapon pedestals ---" % len(self.weapons))
        for kind, x, y, z, r in self.weapons:
            lines.append("%s %s %s %s %s" % (kind, fnum(x), fnum(y), fnum(z), fnum(r)))
        open(path, "w").write("\n".join(lines) + "\n")
        total, dest, frac = self.stats()
        print("wrote %s: %d boxes (%d destructible, %.0f%% of volume), %d spawns, %d pickups, %d weapon pedestals" % (path, total, dest, 100 * frac, len(self.spawns), len(self.pickups), len(self.weapons)))
        if total > 1800:
            print("WARNING: %d boxes is over the 1800 budget" % total, file=sys.stderr)
        return total


def _cuts(a, b, step, off):
    """Cut [a, b] into segments of `step` starting at a + off (the first segment is shorter), clipped to the range."""
    out = []
    x = a
    first = a + off if off > 1e-9 else a + step
    if first > b - 1e-6:
        first = b
    out.append((a, min(first, b)))
    x = out[-1][1]
    while x < b - 1e-6:
        nx = min(x + step, b)
        out.append((x, nx))
        x = nx
    return out
