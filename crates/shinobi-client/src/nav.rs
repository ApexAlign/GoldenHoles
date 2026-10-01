//! Coarse navigation over a `LevelDesc`: a 1 m occupancy grid and an A* planner. Used by the demo "visitor" (and
//! usable by bots / tutorials). Pure and deterministic. Boxes taller than `BLOCK_ABOVE` block; the floor, the
//! ceiling and ramps/steps below that height do not. Walls are inflated by `margin` so paths keep clear of them.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use shinobi_sim::level::{LevelBox, LevelDesc};

pub const CELL: f32 = 1.0;
/// A box whose top is above this blocks walking (low walls at 1.0 m are avoided too; the planner is for filming).
pub const BLOCK_ABOVE: f32 = 0.9;
/// Boxes starting above this are ceilings.
const CEILING_FROM: f32 = 6.0;

pub struct NavGrid {
    min: [f32; 2],
    w: usize,
    h: usize,
    blocked: Vec<bool>,
}

fn rot_aabb(b: &LevelBox) -> ([f32; 2], [f32; 2]) {
    // XZ extent of a box rolled about Z only changes X: |hx*cos| + |hy*sin|
    let (s, c) = b.rot_z.sin_cos();
    let hx = b.half[0] * c.abs() + b.half[1] * s.abs();
    ([b.center[0] - hx, b.center[2] - b.half[2]], [b.center[0] + hx, b.center[2] + b.half[2]])
}

impl NavGrid {
    /// `margin`: metres to keep clear of blocking boxes (player radius + slack).
    pub fn from_level(l: &LevelDesc, margin: f32) -> Self {
        let solid: Vec<&LevelBox> = l
            .boxes
            .iter()
            .filter(|b| b.center[1] + b.half[1] > BLOCK_ABOVE && b.center[1] - b.half[1] < CEILING_FROM)
            .collect();
        // walkable area = the footprint of floor-level boxes (top at ~0)
        let floors: Vec<&LevelBox> = l.boxes.iter().filter(|b| (b.center[1] + b.half[1]).abs() < 0.05).collect();
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for b in &floors {
            let (a, z) = rot_aabb(b);
            lo = [lo[0].min(a[0]), lo[1].min(a[1])];
            hi = [hi[0].max(z[0]), hi[1].max(z[1])];
        }
        if floors.is_empty() {
            lo = [-50.0, -50.0];
            hi = [50.0, 50.0];
        }
        let (w, h) = (((hi[0] - lo[0]) / CELL).ceil() as usize + 1, ((hi[1] - lo[1]) / CELL).ceil() as usize + 1);
        let mut g = Self { min: lo, w, h, blocked: vec![false; w * h] };
        for gz in 0..h {
            for gx in 0..w {
                let p = g.center(gx, gz);
                // outside every floor, or inside an inflated solid box
                let on_floor = floors.iter().any(|b| {
                    let (a, z) = rot_aabb(b);
                    p[0] >= a[0] && p[0] <= z[0] && p[1] >= a[1] && p[1] <= z[1]
                });
                let hit = solid.iter().any(|b| {
                    let (a, z) = rot_aabb(b);
                    p[0] >= a[0] - margin && p[0] <= z[0] + margin && p[1] >= a[1] - margin && p[1] <= z[1] + margin
                });
                g.blocked[gz * w + gx] = !on_floor || hit;
            }
        }
        g
    }

    fn center(&self, gx: usize, gz: usize) -> [f32; 2] {
        [self.min[0] + gx as f32 * CELL, self.min[1] + gz as f32 * CELL]
    }

    fn cell(&self, p: [f32; 2]) -> Option<(usize, usize)> {
        let gx = ((p[0] - self.min[0]) / CELL).round();
        let gz = ((p[1] - self.min[1]) / CELL).round();
        (gx >= 0.0 && gz >= 0.0 && (gx as usize) < self.w && (gz as usize) < self.h).then(|| (gx as usize, gz as usize))
    }

    pub fn is_blocked(&self, p: [f32; 2]) -> bool {
        self.cell(p).map_or(true, |(x, z)| self.blocked[z * self.w + x])
    }

    /// Nearest free cell to `p` (search outward), so a goal inside a wall margin still resolves.
    fn nearest_free(&self, p: [f32; 2]) -> Option<(usize, usize)> {
        let (cx, cz) = self.cell(p)?;
        for r in 0..8i32 {
            for dz in -r..=r {
                for dx in -r..=r {
                    if dx.abs().max(dz.abs()) != r {
                        continue;
                    }
                    let (x, z) = (cx as i32 + dx, cz as i32 + dz);
                    if x >= 0 && z >= 0 && (x as usize) < self.w && (z as usize) < self.h && !self.blocked[z as usize * self.w + x as usize] {
                        return Some((x as usize, z as usize));
                    }
                }
            }
        }
        None
    }

    /// Waypoints (cell centres, start excluded, goal last) around obstacles; `None` if no route.
    pub fn path(&self, from: [f32; 2], to: [f32; 2]) -> Option<Vec<[f32; 2]>> {
        let start = self.nearest_free(from)?;
        let goal = self.nearest_free(to)?;
        let idx = |c: (usize, usize)| c.1 * self.w + c.0;
        let mut g = vec![f32::INFINITY; self.w * self.h];
        let mut prev = vec![usize::MAX; self.w * self.h];
        #[derive(PartialEq)]
        struct Node(f32, usize);
        impl Eq for Node {}
        impl Ord for Node {
            fn cmp(&self, o: &Self) -> Ordering {
                o.0.total_cmp(&self.0) // min-heap
            }
        }
        impl PartialOrd for Node {
            fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
                Some(self.cmp(o))
            }
        }
        let hfn = |c: (usize, usize)| ((c.0 as f32 - goal.0 as f32).powi(2) + (c.1 as f32 - goal.1 as f32).powi(2)).sqrt();
        let mut open = BinaryHeap::new();
        g[idx(start)] = 0.0;
        open.push(Node(hfn(start), idx(start)));
        while let Some(Node(_, i)) = open.pop() {
            let c = (i % self.w, i / self.w);
            if c == goal {
                let mut out = vec![self.center(c.0, c.1)];
                let mut j = i;
                while prev[j] != usize::MAX {
                    j = prev[j];
                    out.push(self.center(j % self.w, j / self.w));
                }
                out.pop(); // drop the start cell
                out.reverse();
                if let Some(last) = out.last_mut() {
                    *last = if self.is_blocked(to) { *last } else { to };
                }
                return Some(self.smooth(out, from));
            }
            for (dx, dz) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
                let (x, z) = (c.0 as i32 + dx, c.1 as i32 + dz);
                if x < 0 || z < 0 || x as usize >= self.w || z as usize >= self.h {
                    continue;
                }
                let n = (x as usize, z as usize);
                if self.blocked[idx(n)] {
                    continue;
                }
                // no cutting corners through diagonal gaps
                if dx != 0 && dz != 0 && (self.blocked[idx((c.0, n.1))] || self.blocked[idx((n.0, c.1))]) {
                    continue;
                }
                let cost = g[i] + if dx != 0 && dz != 0 { std::f32::consts::SQRT_2 } else { 1.0 };
                if cost < g[idx(n)] {
                    g[idx(n)] = cost;
                    prev[idx(n)] = i;
                    open.push(Node(cost + hfn(n), idx(n)));
                }
            }
        }
        None
    }

    /// Drop waypoints that a straight, unblocked line can skip (string pulling, coarse).
    fn smooth(&self, pts: Vec<[f32; 2]>, from: [f32; 2]) -> Vec<[f32; 2]> {
        let mut out = Vec::new();
        let mut anchor = from;
        let mut i = 0;
        while i < pts.len() {
            // furthest point visible from the anchor
            let mut j = i;
            for k in i..pts.len() {
                if self.line_free(anchor, pts[k]) {
                    j = k;
                }
            }
            out.push(pts[j]);
            anchor = pts[j];
            i = j + 1;
        }
        out
    }

    fn line_free(&self, a: [f32; 2], b: [f32; 2]) -> bool {
        let d = [b[0] - a[0], b[1] - a[1]];
        let n = ((d[0].abs().max(d[1].abs())) / (CELL * 0.5)).ceil().max(1.0) as usize;
        (0..=n).all(|i| {
            let t = i as f32 / n as f32;
            !self.is_blocked([a[0] + d[0] * t, a[1] + d[1] * t])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arena() -> LevelDesc {
        // the repo's arena, so the planner is tested on the real thing
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../levels/arena.lvl")).unwrap();
        LevelDesc::from_text(&text).unwrap()
    }

    #[test]
    fn open_floor_goes_straight_and_blocked_cells_are_walls() {
        let l = LevelDesc::standard();
        let g = NavGrid::from_level(&l, 0.8);
        let p = g.path([0.0, 0.0], [20.0, 15.0]).unwrap();
        assert_eq!(p.len(), 1, "no obstacles: one waypoint: {p:?}");
        assert_eq!(p[0], [20.0, 15.0]);
    }

    #[test]
    fn path_goes_around_the_ziggurat_not_through_it() {
        let l = arena();
        let g = NavGrid::from_level(&l, 0.8);
        assert!(g.is_blocked([0.0, 0.0]), "tier 1 is solid");
        assert!(g.is_blocked([-22.0, -16.0]) == false, "corridor floor is open");
        assert!(g.is_blocked([-25.3, -16.0]), "corridor wall blocks");
        let p = g.path([-12.0, 0.0], [12.0, 0.0]).unwrap();
        assert!(p.len() >= 2, "needs a detour: {p:?}");
        // every straight segment of the route is clear
        let mut a = [-12.0, 0.0];
        for w in &p {
            assert!(g.line_free(a, *w), "segment {a:?} -> {w:?} crosses a wall");
            a = *w;
        }
    }

    #[test]
    fn corridor_a_mouth_is_reachable_from_every_spawn() {
        let l = arena();
        let g = NavGrid::from_level(&l, 0.8);
        let mouth = [-19.8, 1.5];
        for s in &l.spawns {
            let p = g.path([s[0], s[2]], mouth).unwrap_or_else(|| panic!("no route from spawn {s:?}"));
            assert!(p.len() < 12, "reasonable route from {s:?}: {} waypoints", p.len());
            assert!((p.last().unwrap()[0] - mouth[0]).abs() < 1.0);
        }
    }

    #[test]
    fn unreachable_and_off_map_goals() {
        let l = arena();
        let g = NavGrid::from_level(&l, 0.8);
        assert!(g.path([0.0, 20.0], [500.0, 500.0]).is_none(), "off the map");
        // a goal inside a wall resolves to the nearest free cell instead of failing
        assert!(g.path([0.0, 20.0], [-25.3, -16.0]).is_some());
    }
}
