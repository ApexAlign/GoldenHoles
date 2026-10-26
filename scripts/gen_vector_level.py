#!/usr/bin/env python3
"""Generates levels/ffa_vector.lvl: VECTOR VECTOR, the Phase 3 blockout plus its feature layer.

    python3 scripts/gen_vector_level.py [out.lvl]

A 100 x 100 m ring arena over a dark void, Y-up only. Three concentric horizontal `ring` platforms (tops y = 0 / 8 /
16, centre-line radius 30 / 24 / 18 m, 2.5 m wide) joined by four long radial ramps that cross over the centre at
different heights and offsets (the box format only rolls about Z, so every ramp runs along X). Twelve floating neon
cubes are cover and grapple anchors; eight mirror prism pillars stand in the gaps between the rings (stars already
ricochet off any face with a bounce budget); four refraction panels are rim walls on the outer ring. The central
singularity pulses every 60 s for 10 s (first at 50 s): a zero-damage `push` hazard that shoves anyone inside it outward every 0.5 s.
Ten spawns sit on the outer ring, four data cubes on the inner ring.

Prism / refraction runtime (split arrows) is not built yet: `prism` and `refraction_panel` zones mark the volumes.
Tests: crates/shinobi-sim/tests/arena_vector.rs
"""
import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lvl_lib import Level  # noqa: E402

RINGS = [(30.0, 0.0, 48), (24.0, 8.0, 40), (18.0, 16.0, 32)]   # (centre-line radius, top y, segments)
RING_W, RING_T = 2.5, 0.5
RAMP_HALF_W, RAMP_T = 1.25, 0.4

L = Level("SHINOBI FFA VECTOR - VECTOR VECTOR")


def ring_x(r, z):
    """x of the ring centre line at offset z (positive branch)."""
    return math.sqrt(r * r - z * z)


def ramp(x_from, y_from, x_to, y_to, z):
    """A ramp whose TOP surface runs (x_from, y_from) -> (x_to, y_to) at offset z (rot_z only)."""
    if x_to < x_from:
        x_from, y_from, x_to, y_to = x_to, y_to, x_from, y_from
    angle = math.atan2(y_to - y_from, x_to - x_from)
    length = math.hypot(x_to - x_from, y_to - y_from)
    cx = (x_from + x_to) / 2 + RAMP_T * math.sin(angle)
    cy = (y_from + y_to) / 2 - RAMP_T * math.cos(angle)
    L.perm(cx, cy, z, length / 2, RAMP_T, RAMP_HALF_W, angle)


# ------------------------------------------------------------------------------------------------------------------ ramps
L.section("structural: four radial ramps crossing over the centre (outer -> middle, middle -> inner)")
(r0, y0, _), (r1, y1, _), (r2, y2, _) = RINGS
ramp(ring_x(r0, 3.0), y0, -ring_x(r1, 3.0), y1, 3.0)        # outer east -> middle west
ramp(-ring_x(r0, 9.0), y0, ring_x(r1, 9.0), y1, 9.0)        # outer west -> middle east
ramp(ring_x(r1, -3.0), y1, -ring_x(r2, -3.0), y2, -3.0)     # middle east -> inner west
ramp(-ring_x(r1, -9.0), y1, ring_x(r2, -9.0), y2, -9.0)     # middle west -> inner east

# ------------------------------------------------------------------------------------------------------------------ cubes
L.section("structural: twelve floating neon cubes (cover and grapple anchors)")
for i in range(12):
    a = math.radians(15.0 + 30.0 * i)
    r = 12.0 if i % 2 == 0 else 36.0
    y = (20.0, 24.0, 12.0)[i % 3]
    L.perm(r * math.cos(a), y, r * math.sin(a), 1.0, 1.0, 1.0)

L.section("structural: eight mirror prism pillars between the rings")
PRISMS = []
for i in range(8):
    a = math.radians(45.0 * i + 22.5)
    r, base, top = (27.0, -2.0, 8.0) if i % 2 == 0 else (21.0, 6.0, 14.0)
    x, z = r * math.cos(a), r * math.sin(a)
    L.perm(x, (base + top) / 2, z, 1.0, (top - base) / 2, 1.0)
    PRISMS.append((x, (base + top) / 2, z, (top - base) / 2))

L.section("structural: four refraction panels, rim walls on the outer ring (8 x 6 m)")
PANELS = []
for k in range(4):
    a = math.radians(90.0 * k + 45.0)
    # axis-aligned panels sit where the rim is closest to diagonal: offset along x or z alternately
    rim = r0 + RING_W / 2 + 0.6
    x, z = rim * math.cos(a), rim * math.sin(a)
    if k % 2 == 0:
        L.perm(x, 3.0, z, 0.15, 3.0, 4.0)
        PANELS.append((x, 3.0, z, 0.15, 3.0, 4.0))
    else:
        L.perm(x, 3.0, z, 4.0, 3.0, 0.15)
        PANELS.append((x, 3.0, z, 4.0, 3.0, 0.15))

# ------------------------------------------------------------------------------------------------------------------ spawns / pickups
for i in range(10):
    a = math.radians(36.0 * i)
    L.spawn(r0 * math.cos(a), y0 + 0.81, r0 * math.sin(a))
for i in range(4):
    a = math.radians(90.0 * i + 45.0)
    L.pickup(r2 * math.cos(a), y2 + 1.0, r2 * math.sin(a), 25)

# ------------------------------------------------------------------------------------------------------------------ features
for r, top, seg in RINGS:
    L.ring(0.0, top - RING_T / 2, 0.0, r, RING_W, RING_T, seg, "vector_ring")

# Singularity: a 60 s cycle, 10 s pulse, 3 s telegraph; first pulse at 50 s. Zero damage, an outward shove every 0.5 s
# for anyone inside the inner 30 m (the middle and inner rings).
L.hazard(0.0, 10.0, 0.0, 30.0, 12.0, 30.0, 3600, 180, 0, "singularity",
         active_ticks=600, phase_ticks=3000, push=14, pulse_every=30)

for x, y, z, hy in PRISMS:
    L.zone(x, y, z, 1.0, hy, 1.0, "prism")
for x, y, z, hx, hy, hz in PANELS:
    L.zone(x, y, z, hx, hy, hz, "refraction_panel")

out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "levels", "ffa_vector.lvl")
L.write(out, header=[
    "GENERATED by scripts/gen_vector_level.py. Edit the script, not this file; tests: crates/shinobi-sim/tests/arena_vector.rs",
    "VECTOR VECTOR, Phase 3 blockout. Y-up. Three rings (tops y = 0 / 8 / 16, r 30 / 24 / 18, 2.5 m wide), four crossing",
    "radial ramps, 12 floating cubes, 8 prism pillars, 4 refraction rim panels, a 60 s singularity push pulse;",
    "10 spawns on the outer ring, 4 data cubes on the inner ring.",
])
