//! Steering helpers. There is no navmesh: bots steer with whiskers (Rapier rays) over primitive level geometry,
//! (Placeable wall blocks, once the sim has them, are ordinary solids to these rays.)

use std::f32::consts::{PI, TAU};

use shinobi_sim::combat::GROUP_WORLD;
use shinobi_sim::rapier3d::prelude::*;
use shinobi_sim::tick::Server;

use crate::view::{PlayerView, WorldView, V3};

/// Horizontal part of a vector.
pub fn flat(v: V3) -> V3 {
    vector![v.x, 0.0, v.z]
}

pub fn unit_or_zero(v: V3) -> V3 {
    let n = v.norm();
    if n < 1e-5 { Vector::zeros() } else { v / n }
}

/// Wrap an angle to (-pi, pi].
pub fn wrap_pi(a: f32) -> f32 {
    let w = a.rem_euclid(TAU);
    if w > PI { w - TAU } else { w }
}

/// Forward vector for a yaw (0 faces -Z, positive turns toward -X). Same as `Aim::flat_forward`.
pub fn dir_from_yaw(yaw: f32) -> V3 {
    vector![-yaw.sin(), 0.0, -yaw.cos()]
}

pub fn right_of_yaw(yaw: f32) -> V3 {
    vector![yaw.cos(), 0.0, -yaw.sin()]
}

/// Yaw that faces `dir` (flat).
pub fn yaw_of(dir: V3) -> f32 {
    (-dir.x).atan2(-dir.z)
}

/// World direction -> view-frame stick (strafe, forward), each in -1..1, scaled by `mag`.
pub fn stick_for(dir: V3, mag: f32, yaw: f32) -> (f32, f32) {
    let d = unit_or_zero(flat(dir)) * mag.clamp(0.0, 1.0);
    (d.dot(&right_of_yaw(yaw)), d.dot(&dir_from_yaw(yaw)))
}

pub fn rotate_y(v: V3, a: f32) -> V3 {
    let (s, c) = a.sin_cos();
    vector![v.x * c + v.z * s, v.y, -v.x * s + v.z * c]
}

/// A flat-topped box a bot can hop onto (single + double jump). Derived from level colliders.
#[derive(Clone, Copy, Debug)]
pub struct Perch {
    /// Box centre x/z, y = top face.
    pub top_center: V3,
    pub half_xz: [f32; 2],
}

impl Perch {
    pub fn contains_xz(&self, p: V3, pad: f32) -> bool {
        (p.x - self.top_center.x).abs() <= self.half_xz[0] + pad && (p.z - self.top_center.z).abs() <= self.half_xz[1] + pad
    }

    /// Horizontal distance from `p` to the box outline (0 inside).
    pub fn edge_dist(&self, p: V3) -> f32 {
        let dx = ((p.x - self.top_center.x).abs() - self.half_xz[0]).max(0.0);
        let dz = ((p.z - self.top_center.z).abs() - self.half_xz[1]).max(0.0);
        (dx * dx + dz * dz).sqrt()
    }
}

/// Scan static level cuboids for hop-able tops. Deterministic: collider arena order, then sorted.
pub fn find_perches(server: &Server, min_top: f32, max_top: f32) -> Vec<Perch> {
    let mut out = Vec::new();
    for (_, c) in server.world.colliders.iter() {
        if c.is_sensor() || !c.collision_groups().memberships.intersects(GROUP_WORLD) {
            continue;
        }
        let Some(cu) = c.shape().as_cuboid() else { continue };
        let pos = c.position();
        if pos.rotation.angle().abs() > 0.05 {
            continue; // ramps and yawed boxes are not flat tops
        }
        let h = cu.half_extents;
        let top = pos.translation.y + h.y;
        if top < min_top || top > max_top || h.x < 0.6 || h.z < 0.6 || h.x > 60.0 || h.z > 60.0 {
            continue;
        }
        out.push(Perch { top_center: vector![pos.translation.x, top, pos.translation.z], half_xz: [h.x, h.z] });
    }
    out.sort_by(|a, b| {
        (a.top_center.x, a.top_center.z, a.top_center.y)
            .partial_cmp(&(b.top_center.x, b.top_center.z, b.top_center.y))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// Walkable ground-layer grid for path planning on big maps. Built once per level from the static colliders:
/// a cell is walkable when there is ground under it and a body-sized capsule fits there; neighbours connect when
/// the ground height difference is small enough to walk (steps, ramps). Pieces printed later are not in it: the
/// whiskers (`steer_static`) handle those locally. Deterministic (fixed scan order, ordered tie-breaks).
pub struct NavGrid {
    x0: f32,
    z0: f32,
    cell: f32,
    nx: usize,
    nz: usize,
    /// Ground height per cell, `NaN` = not walkable.
    y: Vec<f32>,
    /// Ground height at the midpoint of each cell's east / south edge (`NaN` = none): tells a smooth ramp from a
    /// sheer step between two cells of different height.
    ye: Vec<f32>,
    ys: Vec<f32>,
    /// Per cell: bit k set when `DIRS[k]` is a walkable move (precomputed once, so A* never re-derives it).
    adj: Vec<u8>,
    /// Per cell: walkable with a blocked cell on one of its four sides (path cost penalty).
    hug: Vec<bool>,
}

/// `(searches, nodes expanded)` on this thread since the previous call; resets the counters.
pub fn take_plan_stats() -> (u32, u64) {
    STATS.with(|st| st.replace((0, 0)))
}

/// Result of `NavGrid::path_search`.
pub struct PathResult {
    pub waypoints: Vec<[f32; 2]>,
    /// The goal was reached (else the waypoints lead to the closest cell the search got to).
    pub complete: bool,
    /// Nodes expanded (cost indicator).
    pub expanded: usize,
    /// Distance (m) from the end of the route to the goal (0 when complete).
    pub end_gap: f32,
}

/// Neighbour offsets; bit k of `NavGrid::adj[cell]` says whether the move `DIRS[k]` is walkable.
const DIRS: [(i32, i32); 8] = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];

/// Reused A* buffers (one set per thread, regrown to the largest grid seen): searching allocates nothing.
struct Scratch {
    g: Vec<f32>,
    came: Vec<u32>,
    stamp: Vec<u32>,
    gen: u32,
}

impl Scratch {
    fn reset(&mut self, n: usize) {
        if self.g.len() < n {
            self.g.resize(n, 0.0);
            self.came.resize(n, u32::MAX);
            self.stamp.resize(n, 0);
        }
        self.gen = self.gen.wrapping_add(1);
        if self.gen == 0 {
            self.stamp.iter_mut().for_each(|s| *s = 0);
            self.gen = 1;
        }
    }
}

thread_local! {
    /// (A* searches, nodes expanded) on this thread since the last `take_plan_stats`: lets a test assert the per-tick
    /// planning budget instead of timing it.
    static STATS: std::cell::Cell<(u32, u64)> = const { std::cell::Cell::new((0, 0)) };
    static SCRATCH: std::cell::RefCell<Scratch> = const { std::cell::RefCell::new(Scratch { g: Vec::new(), came: Vec::new(), stamp: Vec::new(), gen: 0 }) };
}

/// Largest ground-height difference between neighbouring cells a walking bot crosses on a smooth ramp.
const NAV_STEP: f32 = 0.9;
/// A height difference the character steps over without a ramp (its autostep is 0.35 m).
const NAV_AUTOSTEP: f32 = 0.3;
/// How far the ground at the edge midpoint may sit from the average of the two cells and still count as a ramp.
const NAV_RAMP_TOL: f32 = 0.12;
/// Highest ground a path may use: tiers a bot can walk onto, not roofs only reachable by hopping.
const NAV_MAX_Y: f32 = 4.0;

impl NavGrid {
    /// Scan the level inside `[-half, half]` in x and z at `cell` metres.
    pub fn build(server: &Server, half: f32, cell: f32) -> NavGrid {
        let n = ((half * 2.0) / cell).ceil() as usize;
        let (x0, z0) = (-half, -half);
        let cfg = *server.cfg();
        let world = &server.world;
        // the server's own pipeline may not have seen the level yet (a host built before the first tick)
        let mut queries = QueryPipeline::new();
        queries.update(&world.colliders);
        let solids = QueryFilter::default().exclude_sensors().groups(InteractionGroups::new(Group::ALL, GROUP_WORLD));
        let body = Capsule::new_y(cfg.half_height, cfg.radius * 0.9);
        let mut y = vec![f32::NAN; n * n];
        let (mut ye, mut ys) = (vec![f32::NAN; n * n], vec![f32::NAN; n * n]);
        let ground = |x: f32, z: f32| -> f32 {
            let ray = Ray::new(Point::new(x, NAV_MAX_Y + 1.0, z), vector![0.0, -1.0, 0.0]);
            match queries.cast_ray(&world.bodies, &world.colliders, &ray, NAV_MAX_Y + 25.0, true, solids) {
                Some((_, toi)) if NAV_MAX_Y + 1.0 - toi <= NAV_MAX_Y => NAV_MAX_Y + 1.0 - toi,
                _ => f32::NAN,
            }
        };
        for iz in 0..n {
            for ix in 0..n {
                let (cx, cz) = (x0 + (ix as f32 + 0.5) * cell, z0 + (iz as f32 + 0.5) * cell);
                // ground: highest solid top below y = NAV_MAX_Y + 1 (so towers' undersides do not count as floor)
                let ray = Ray::new(Point::new(cx, NAV_MAX_Y + 1.0, cz), vector![0.0, -1.0, 0.0]);
                let Some((_, toi)) = queries.cast_ray(&world.bodies, &world.colliders, &ray, NAV_MAX_Y + 25.0, true, solids) else { continue };
                let gy = NAV_MAX_Y + 1.0 - toi;
                if gy > NAV_MAX_Y {
                    continue;
                }
                // a capsule standing there must not overlap any solid
                let centre = Isometry::translation(cx, gy + cfg.half_height + cfg.radius + 0.05, cz);
                let mut blocked = false;
                queries.intersections_with_shape(&world.bodies, &world.colliders, &centre, &body, solids, |_| {
                    blocked = true;
                    false
                });
                if !blocked {
                    y[iz * n + ix] = gy;
                    ye[iz * n + ix] = ground(cx + cell * 0.5, cz);
                    ys[iz * n + ix] = ground(cx, cz + cell * 0.5);
                }
            }
        }
        let mut grid = NavGrid { x0, z0, cell, nx: n, nz: n, y, ye, ys, adj: vec![0; n * n], hug: vec![false; n * n] };
        for iz in 0..n as i32 {
            for ix in 0..n as i32 {
                if grid.ground(ix, iz).is_none() {
                    continue;
                }
                let i = iz as usize * n + ix as usize;
                grid.hug[i] = grid.hugs_wall((ix, iz));
                let mut mask = 0u8;
                for (k, (dx, dz)) in DIRS.iter().enumerate() {
                    if grid.passable((ix, iz), (ix + dx, iz + dz)) {
                        mask |= 1 << k;
                    }
                }
                grid.adj[i] = mask;
            }
        }
        grid
    }

    pub fn walkable_count(&self) -> usize {
        self.y.iter().filter(|v| !v.is_nan()).count()
    }

    fn idx(&self, ix: i32, iz: i32) -> Option<usize> {
        (ix >= 0 && iz >= 0 && (ix as usize) < self.nx && (iz as usize) < self.nz).then(|| iz as usize * self.nx + ix as usize)
    }

    fn cell_of(&self, x: f32, z: f32) -> (i32, i32) {
        (((x - self.x0) / self.cell).floor() as i32, ((z - self.z0) / self.cell).floor() as i32)
    }

    fn center(&self, ix: i32, iz: i32) -> (f32, f32) {
        (self.x0 + (ix as f32 + 0.5) * self.cell, self.z0 + (iz as f32 + 0.5) * self.cell)
    }

    fn ground(&self, ix: i32, iz: i32) -> Option<f32> {
        self.idx(ix, iz).map(|i| self.y[i]).filter(|v| !v.is_nan())
    }

    /// The nearest walkable cell to `(x, z)` within a few rings (a bot standing in a doorway can map to a
    /// neighbour); `None` if the area is solid.
    fn snap(&self, x: f32, z: f32) -> Option<(i32, i32)> {
        let (cx, cz) = self.cell_of(x, z);
        for r in 0..=4i32 {
            let mut best: Option<(f32, (i32, i32))> = None;
            for dz in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dz.abs()) != r || self.ground(cx + dx, cz + dz).is_none() {
                        continue;
                    }
                    let (px, pz) = self.center(cx + dx, cz + dz);
                    let d = (px - x).powi(2) + (pz - z).powi(2);
                    if best.map_or(true, |(bd, _)| d < bd) {
                        best = Some((d, (cx + dx, cz + dz)));
                    }
                }
            }
            if let Some((_, c)) = best {
                return Some(c);
            }
        }
        None
    }

    /// Orthogonal neighbours `a` -> `b` (ground heights `ya`, `yb`): a small step the character climbs, or a smooth
    /// slope (the ground at the shared edge's midpoint lies between the two, not at one cell's level).
    fn edge_ok(&self, a: (i32, i32), b: (i32, i32), ya: f32, yb: f32) -> bool {
        let d = (ya - yb).abs();
        if d <= NAV_AUTOSTEP {
            return true;
        }
        if d > NAV_STEP {
            return false;
        }
        // the edge midpoint is stored on the west/north cell of the pair (east / south edge)
        let (lo, along_x) = if a.1 == b.1 { (if a.0 < b.0 { a } else { b }, true) } else { (if a.1 < b.1 { a } else { b }, false) };
        let Some(i) = self.idx(lo.0, lo.1) else { return false };
        let em = if along_x { self.ye[i] } else { self.ys[i] };
        !em.is_nan() && (em - (ya + yb) * 0.5).abs() <= NAV_RAMP_TOL
    }

    /// A walkable cell with a blocked cell on one of its four sides.
    fn hugs_wall(&self, c: (i32, i32)) -> bool {
        [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|(dx, dz)| self.ground(c.0 + dx, c.1 + dz).is_none())
    }

    fn passable(&self, a: (i32, i32), b: (i32, i32)) -> bool {
        match (self.ground(a.0, a.1), self.ground(b.0, b.1)) {
            (Some(ya), Some(yb)) => {
                if a.0 == b.0 || a.1 == b.1 {
                    return self.edge_ok(a, b, ya, yb);
                }
                // a diagonal is two L-shaped orthogonal routes: both must be walkable, leg by leg (a corner of a
                // raised block would otherwise be cut diagonally from the ground onto its top)
                let (c1, c2) = ((a.0, b.1), (b.0, a.1));
                let (Some(y1), Some(y2)) = (self.ground(c1.0, c1.1), self.ground(c2.0, c2.1)) else { return false };
                self.edge_ok(a, c1, ya, y1) && self.edge_ok(c1, b, y1, yb) && self.edge_ok(a, c2, ya, y2) && self.edge_ok(c2, b, y2, yb)
            }
            _ => false,
        }
    }

    /// Waypoints (world x, z) from `from` to `to`, string-pulled; `None` when either end is not on the grid or
    /// no path exists. `max_nodes` bounds the search. See `path_search` for the partial-path variant.
    pub fn path(&self, from: [f32; 2], to: [f32; 2], max_nodes: usize) -> Option<Vec<[f32; 2]>> {
        let r = self.path_search(from, to, max_nodes)?;
        r.complete.then_some(r.waypoints)
    }

    /// A* from `from` to `to` over flat, reused buffers (no per-call allocation or hashing). When the node budget
    /// runs out or the goal is unreachable the result is the best partial route (to the visited cell closest to
    /// the goal) with `complete == false`, so a bot still moves the right way. `None` only when an end is off the
    /// grid. Deterministic: ties break on node index.
    pub fn path_search(&self, from: [f32; 2], to: [f32; 2], max_nodes: usize) -> Option<PathResult> {
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;
        let (s, g) = (self.snap(from[0], from[1])?, self.snap(to[0], to[1])?);
        if s == g {
            return Some(PathResult { waypoints: vec![to], complete: true, expanded: 0, end_gap: 0.0 });
        }
        let nx = self.nx as i32;
        let idx = |c: (i32, i32)| (c.1 * nx + c.0) as usize;
        let h = |c: (i32, i32)| (((c.0 - g.0).pow(2) + (c.1 - g.1).pow(2)) as f32).sqrt();
        let key = |f: f32| (f * 1000.0) as u32; // millicells: integer heap order, deterministic
        STATS.with(|st| st.set((st.get().0 + 1, st.get().1)));
        SCRATCH.with(|sc| {
            let sc = &mut *sc.borrow_mut();
            sc.reset(self.nx * self.nz);
            let gen = sc.gen;
            let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
            let si = idx(s);
            sc.g[si] = 0.0;
            sc.stamp[si] = gen;
            sc.came[si] = u32::MAX;
            heap.push(Reverse((key(h(s)), si as u32)));
            let mut best = (h(s), si);
            let mut expanded = 0usize;
            let mut goal_hit = false;
            while let Some(Reverse((_, ci))) = heap.pop() {
                let ci = ci as usize;
                let c = ((ci % self.nx) as i32, (ci / self.nx) as i32);
                if ci == idx(g) {
                    goal_hit = true;
                    best = (0.0, ci);
                    break;
                }
                expanded += 1;
                if expanded > max_nodes {
                    break;
                }
                let hc = h(c);
                if hc < best.0 {
                    best = (hc, ci);
                }
                let gc = sc.g[ci];
                let mask = self.adj[ci];
                for (k, (dx, dz)) in DIRS.iter().enumerate() {
                    if mask & (1 << k) == 0 {
                        continue;
                    }
                    let n = (c.0 + dx, c.1 + dz);
                    let ni = idx(n);
                    // hugging a wall costs extra: routes keep a body width from corners where the map allows it
                    let hug = if self.hug[ni] { 0.8 } else { 0.0 };
                    let ng = gc + if *dx != 0 && *dz != 0 { std::f32::consts::SQRT_2 } else { 1.0 } + hug;
                    if sc.stamp[ni] != gen || ng < sc.g[ni] - 1e-4 {
                        sc.stamp[ni] = gen;
                        sc.g[ni] = ng;
                        sc.came[ni] = ci as u32;
                        heap.push(Reverse((key(ng + h(n)), ni as u32)));
                    }
                }
            }
            STATS.with(|st| st.set((st.get().0, st.get().1 + expanded as u64)));
            let end = best.1;
            if end == si && !goal_hit {
                return Some(PathResult { waypoints: Vec::new(), complete: false, expanded, end_gap: h(s) * self.cell });
            }
            let mut cells = vec![((end % self.nx) as i32, (end / self.nx) as i32)];
            let mut cur = end;
            while sc.came[cur] != u32::MAX {
                cur = sc.came[cur] as usize;
                cells.push(((cur % self.nx) as i32, (cur / self.nx) as i32));
            }
            cells.reverse();
            let tail = if goal_hit { to } else { let (x, z) = self.center(cells.last().unwrap().0, cells.last().unwrap().1); [x, z] };
            Some(PathResult { waypoints: self.pull(&cells, tail), complete: goal_hit, expanded, end_gap: best.0 * self.cell })
        })
    }

    /// Straight walkable run between two cells (supercover-ish: sample every half cell).
    fn line_clear(&self, a: (i32, i32), b: (i32, i32)) -> bool {
        let (ax, az) = self.center(a.0, a.1);
        let (bx, bz) = self.center(b.0, b.1);
        let len = ((bx - ax).powi(2) + (bz - az).powi(2)).sqrt();
        let steps = (len / (self.cell * 0.5)).ceil().max(1.0) as i32;
        let mut prev = a;
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let c = self.cell_of(ax + (bx - ax) * t, az + (bz - az) * t);
            if c != prev && !self.passable(prev, c) {
                // a corner touch (diagonal hop between two cells) is allowed only if both sides are walkable
                return false;
            }
            prev = c;
        }
        true
    }

    /// True if a straight walk from `a` to `b` (world x, z) stays on walkable ground the whole way. Positions off the
    /// grid count as not walkable.
    pub fn straight_walkable(&self, a: [f32; 2], b: [f32; 2]) -> bool {
        let (Some(ca), Some(cb)) = (self.snap(a[0], a[1]), self.snap(b[0], b[1])) else { return false };
        // walk the real segment (not cell centre to cell centre), so a wall between two nearby bots counts
        let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        let steps = (len / (self.cell * 0.5)).ceil().max(1.0) as i32;
        let mut prev = self.cell_of(a[0], a[1]);
        if self.ground(prev.0, prev.1).is_none() {
            prev = ca;
        }
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let c = self.cell_of(a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t);
            if c != prev && !self.passable(prev, c) {
                return false;
            }
            prev = c;
        }
        let _ = cb;
        true
    }

    /// String pulling: keep only the cells where the straight line to the farther cell breaks.
    fn pull(&self, cells: &[(i32, i32)], goal: [f32; 2]) -> Vec<[f32; 2]> {
        let mut out = Vec::new();
        let mut i = 0;
        while i + 1 < cells.len() {
            let mut j = cells.len() - 1;
            while j > i + 1 && !self.line_clear(cells[i], cells[j]) {
                j -= 1;
            }
            let (px, pz) = self.center(cells[j].0, cells[j].1);
            out.push([px, pz]);
            i = j;
        }
        out.push(goal);
        out
    }
}

/// Whisker steering over static geometry: if the way ahead is blocked, bend toward the freer side.
pub fn steer_static(world: &WorldView, me: &PlayerView, dir: V3) -> V3 {
    let d = unit_or_zero(flat(dir));
    if d == Vector::zeros() {
        return d;
    }
    let knee = vector![me.pos.x, me.pos.y - 0.3, me.pos.z];
    const LEN: f32 = 2.0;
    let ahead = world.ray(knee, d, LEN);
    if ahead.is_none() {
        return d;
    }
    let side = |a: f32| world.ray(knee, rotate_y(d, a), LEN).unwrap_or(LEN + 1.0);
    let (l, r) = (side(0.9), side(-0.9));
    let (l2, r2) = (side(1.6), side(-1.6));
    // prefer the side with more room; ties go left (deterministic)
    let (ls, rs) = (l + l2 * 0.5, r + r2 * 0.5);
    let a = if ls >= rs { 0.9 + 0.3 * (1.0 - ahead.unwrap() / LEN) } else { -0.9 - 0.3 * (1.0 - ahead.unwrap() / LEN) };
    rotate_y(d, a)
}

/// Which side (+1 = right of `heading`, -1 = left) has level geometry within wall-run reach, if any.
pub fn wall_side(world: &WorldView, me: &PlayerView, heading: V3) -> Option<f32> {
    let h = unit_or_zero(flat(heading));
    if h == Vector::zeros() {
        return None;
    }
    let right = vector![-h.z, 0.0, h.x].normalize();
    let reach = world.cfg.radius + world.cfg.wall_probe;
    let mut best: Option<(f32, f32)> = None;
    for s in [1.0f32, -1.0] {
        let hit = [-world.cfg.half_height, 0.0, world.cfg.half_height].iter().all(|dy| {
            let o = vector![me.pos.x, me.pos.y + dy, me.pos.z];
            world.ray(o, right * s, reach).is_some()
        });
        if hit {
            let d = world.ray(me.pos, right * s, reach).unwrap_or(reach);
            if best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, s));
            }
        }
    }
    best.map(|b| b.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stick_roundtrips_world_direction() {
        for yaw in [0.0f32, 0.7, 2.0, -1.3] {
            for ang in [0.0f32, 1.0, -2.0, 3.0] {
                let want = dir_from_yaw(ang);
                let (x, z) = stick_for(want, 1.0, yaw);
                // what the server does in PlayerIntent::world_move
                let (s, c) = yaw.sin_cos();
                let got = vector![z * -s + x * c, 0.0, z * -c + x * -s];
                assert!((got - want).norm() < 1e-4, "yaw {yaw} ang {ang}: {got:?} vs {want:?}");
            }
        }
    }

    #[test]
    fn yaw_of_inverts_dir_from_yaw() {
        for y in [0.0f32, 1.0, -2.5, 3.0] {
            assert!(wrap_pi(yaw_of(dir_from_yaw(y)) - y).abs() < 1e-4);
        }
    }
}
