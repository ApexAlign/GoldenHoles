//! Server-side backward reconciliation. Targets are rewound to what the attacker's screen showed, and the
//! hit test sweeps both bodies across the tick (dash = 30 m/s = 0.5 m/tick would tunnel through point tests).

use super::intent::ViewState;

pub const HISTORY_TICKS: usize = 64;
/// 200 ms at 60 Hz: anything older is clamped, so high-ping players can't shoot around corners forever.
pub const MAX_REWIND_TICKS: u32 = 12;

#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub tick: u32,
    pub pos: [f32; 3],
}

#[derive(Clone)]
pub struct PoseHistory {
    ring: [Option<Pose>; HISTORY_TICKS],
}

impl Default for PoseHistory {
    fn default() -> Self {
        Self { ring: [None; HISTORY_TICKS] }
    }
}

impl PoseHistory {
    pub fn record(&mut self, tick: u32, pos: [f32; 3]) {
        self.ring[tick as usize % HISTORY_TICKS] = Some(Pose { tick, pos });
    }

    pub fn get(&self, tick: u32) -> Option<[f32; 3]> {
        self.ring[tick as usize % HISTORY_TICKS].filter(|p| p.tick == tick).map(|p| p.pos)
    }

    /// Position at `tick + alpha`, alpha in 0..1.
    pub fn sample(&self, tick: u32, alpha: f32) -> Option<[f32; 3]> {
        let a = self.get(tick)?;
        let b = self.get(tick + 1).unwrap_or(a);
        Some([
            a[0] + (b[0] - a[0]) * alpha,
            a[1] + (b[1] - a[1]) * alpha,
            a[2] + (b[2] - a[2]) * alpha,
        ])
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RewindTime {
    pub tick: u32,
    pub alpha: f32,
}

/// Clamp the client-reported view time into the legal rewind window.
pub fn resolve_rewind(server_tick: u32, view: ViewState) -> RewindTime {
    let earliest = server_tick.saturating_sub(MAX_REWIND_TICKS);
    if view.from_tick < earliest {
        RewindTime { tick: earliest, alpha: 0.0 }
    } else if view.from_tick >= server_tick {
        RewindTime { tick: server_tick, alpha: 0.0 }
    } else {
        RewindTime { tick: view.from_tick, alpha: view.alpha as f32 / 256.0 }
    }
}

/// Target motion across the client's dash tick: the rewound position now and one tick later.
pub fn rewound_segment(h: &PoseHistory, rt: RewindTime) -> Option<([f32; 3], [f32; 3])> {
    let p0 = h.sample(rt.tick, rt.alpha)?;
    let p1 = h.sample(rt.tick + 1, rt.alpha).unwrap_or(p0);
    Some((p0, p1))
}

/// Vertical-cylinder body: `half_height` is the full half extent (capsule half-height + radius).
#[derive(Clone, Copy, Debug)]
pub struct Body {
    pub radius: f32,
    pub half_height: f32,
}

/// Swept overlap of two linearly moving cylinders over t in [0,1].
/// `a0 -> a1` attacker (dash / slice reach included in `attacker.radius`), `p0 -> p1` target.
/// Returns the first time of contact, exact for linear motion (no sub-stepping, no tunnelling).
pub fn swept_hit(a0: [f32; 3], a1: [f32; 3], attacker: Body, p0: [f32; 3], p1: [f32; 3], target: Body) -> Option<f32> {
    let d0 = [p0[0] - a0[0], p0[1] - a0[1], p0[2] - a0[2]];
    let v = [
        (p1[0] - p0[0]) - (a1[0] - a0[0]),
        (p1[1] - p0[1]) - (a1[1] - a0[1]),
        (p1[2] - p0[2]) - (a1[2] - a0[2]),
    ];

    // horizontal: |d0.xz + v.xz t| <= R
    let r = attacker.radius + target.radius;
    let (lo, hi) = {
        let a = v[0] * v[0] + v[2] * v[2];
        let b = 2.0 * (d0[0] * v[0] + d0[2] * v[2]);
        let c = d0[0] * d0[0] + d0[2] * d0[2] - r * r;
        if a < 1e-9 {
            if c <= 0.0 { (0.0, 1.0) } else { return None }
        } else {
            let disc = b * b - 4.0 * a * c;
            if disc < 0.0 {
                return None;
            }
            let s = disc.sqrt();
            (((-b - s) / (2.0 * a)).max(0.0), ((-b + s) / (2.0 * a)).min(1.0))
        }
    };
    if lo > hi {
        return None;
    }

    // vertical: |d0.y + v.y t| <= H
    let h = attacker.half_height + target.half_height;
    let (ylo, yhi) = if v[1].abs() < 1e-6 {
        if d0[1].abs() <= h { (0.0, 1.0) } else { return None }
    } else {
        let t1 = (-h - d0[1]) / v[1];
        let t2 = (h - d0[1]) / v[1];
        (t1.min(t2), t1.max(t2))
    };
    let start = lo.max(ylo);
    (start <= hi.min(yhi)).then_some(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Body = Body { radius: 0.1, half_height: 0.8 };

    fn history() -> PoseHistory {
        let mut h = PoseHistory::default();
        // target sprints in -x at 0.5 m/tick
        for t in 0..40u32 {
            h.record(t, [20.0 - 0.5 * t as f32, 0.0, 0.0]);
        }
        h
    }

    #[test]
    fn rewind_window_clamps() {
        let v = |from_tick, alpha| ViewState { from_tick, alpha };
        assert_eq!(resolve_rewind(100, v(95, 128)), RewindTime { tick: 95, alpha: 0.5 });
        assert_eq!(resolve_rewind(100, v(10, 255)), RewindTime { tick: 88, alpha: 0.0 });
        assert_eq!(resolve_rewind(100, v(150, 9)), RewindTime { tick: 100, alpha: 0.0 });
    }

    #[test]
    fn history_lookup_and_expiry() {
        let mut h = history();
        assert_eq!(h.get(10), Some([15.0, 0.0, 0.0]));
        let mid = h.sample(10, 0.5).unwrap();
        assert!((mid[0] - 14.75).abs() < 1e-5);
        h.record(10 + HISTORY_TICKS as u32, [0.0; 3]); // overwrites slot
        assert!(h.get(10).is_none());
    }

    #[test]
    fn swept_test_catches_dash_that_point_sampling_misses() {
        let h = history();
        // view tick 30: target at x=5.0 -> 4.5. Attacker dashes +x from 4.0 -> 4.5...
        let (p0, p1) = rewound_segment(&h, RewindTime { tick: 30, alpha: 0.0 }).unwrap();
        assert_eq!(p0[0], 5.0);
        assert_eq!(p1[0], 4.5);
        let (a0, a1) = ([4.45, 0.0, 0.0], [4.95, 0.0, 0.0]); // swaps sides with the target mid-tick
        // endpoint distances: 0.55 and 0.45 > R = 0.2, so point tests miss
        assert!((p0[0] - a0[0]).abs() > 0.2 && (p1[0] - a1[0]).abs() > 0.2);
        let t = swept_hit(a0, a1, B, p0, p1, B).expect("sweep must hit");
        assert!(t > 0.3 && t < 0.7, "{t}");
    }

    #[test]
    fn swept_test_misses_offset_and_height() {
        let (p0, p1) = ([5.0, 0.0, 0.0], [4.5, 0.0, 0.0]);
        assert!(swept_hit([4.45, 0.0, 1.0], [4.95, 0.0, 1.0], B, p0, p1, B).is_none()); // 1 m to the side
        assert!(swept_hit([4.45, 5.0, 0.0], [4.95, 5.0, 0.0], B, p0, p1, B).is_none()); // above
    }
}
