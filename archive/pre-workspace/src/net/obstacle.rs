//! Moving neon obstacles. Motion is an integer function of the server tick, so the only network cost is
//! one reliable spawn descriptor (and a re-send with a new `start_tick` when the server changes behaviour).
//! Server collision and client rendering evaluate `pos_at(tick)` and are bit-identical.

use std::collections::HashMap;

pub type ObstacleId = u16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MotionPath {
    /// `origin + vel * (tick - start)`, vel in 1/512 m per tick.
    Linear { origin: [i32; 3], vel: [i32; 3] },
    /// Back and forth between `a` and `b`, one leg takes `period` ticks.
    PingPong { a: [i32; 3], b: [i32; 3], period: u16 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ObstacleDesc {
    pub id: ObstacleId,
    /// Visual / gameplay archetype, client-side lookup.
    pub kind: u8,
    pub start_tick: u32,
    /// 0 = until despawn event.
    pub end_tick: u32,
    /// Half extents, 1/64 m.
    pub half: [u16; 3],
    pub path: MotionPath,
}

impl ObstacleDesc {
    /// Exact fixed-point position (1/512 m) at an integer tick.
    pub fn pos_at(&self, tick: u32) -> [i32; 3] {
        let dt = tick.saturating_sub(self.start_tick) as i64;
        match self.path {
            MotionPath::Linear { origin, vel } => {
                let f = |o: i32, v: i32| (o as i64 + v as i64 * dt) as i32;
                [f(origin[0], vel[0]), f(origin[1], vel[1]), f(origin[2], vel[2])]
            }
            MotionPath::PingPong { a, b, period } => {
                let p = period.max(1) as i64;
                let ph = dt % (2 * p);
                let t = if ph < p { ph } else { 2 * p - ph };
                let f = |a: i32, b: i32| (a as i64 + (b as i64 - a as i64) * t / p) as i32;
                [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
            }
        }
    }

    /// Render position at a fractional render tick (interpolates between exact tick positions).
    pub fn pos_at_f(&self, tick: f64) -> [f32; 3] {
        let t0 = tick.floor().max(0.0) as u32;
        let a = self.pos_at(t0);
        let b = self.pos_at(t0 + 1);
        let f = (tick - t0 as f64).clamp(0.0, 1.0) as f32;
        let l = |i: usize| (a[i] as f32 + (b[i] - a[i]) as f32 * f) / super::quant::POS_SCALE;
        [l(0), l(1), l(2)]
    }

    pub fn is_active(&self, tick: u32) -> bool {
        tick >= self.start_tick && (self.end_tick == 0 || tick < self.end_tick)
    }
}

#[derive(Default)]
pub struct ObstacleStore {
    map: HashMap<ObstacleId, ObstacleDesc>,
}

impl ObstacleStore {
    /// Spawn, or retarget an existing obstacle (server-side behaviour change).
    pub fn apply_spawn(&mut self, d: ObstacleDesc) {
        self.map.insert(d.id, d);
    }

    pub fn apply_despawn(&mut self, id: ObstacleId, tick: u32) {
        if let Some(d) = self.map.get_mut(&id) {
            d.end_tick = if d.end_tick == 0 { tick } else { d.end_tick.min(tick) };
        }
    }

    pub fn active_at(&self, tick: u32) -> impl Iterator<Item = &ObstacleDesc> + '_ {
        self.map.values().filter(move |d| d.is_active(tick))
    }

    pub fn prune(&mut self, tick: u32) {
        self.map.retain(|_, d| d.end_tick == 0 || tick < d.end_tick.saturating_add(120));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pingpong_is_periodic_and_exact() {
        let d = ObstacleDesc {
            id: 1,
            kind: 0,
            start_tick: 10,
            end_tick: 0,
            half: [64; 3],
            path: MotionPath::PingPong { a: [0, 0, 0], b: [5120, 0, 0], period: 100 },
        };
        assert_eq!(d.pos_at(10), [0, 0, 0]);
        assert_eq!(d.pos_at(60), [2560, 0, 0]);
        assert_eq!(d.pos_at(110), [5120, 0, 0]);
        assert_eq!(d.pos_at(210), [0, 0, 0]);
        assert_eq!(d.pos_at(5 + 10 + 200), d.pos_at(5 + 10));
    }

    #[test]
    fn linear_and_lifetime() {
        let d = ObstacleDesc {
            id: 2,
            kind: 1,
            start_tick: 0,
            end_tick: 30,
            half: [32; 3],
            path: MotionPath::Linear { origin: [0, 100, 0], vel: [77, 0, -3] },
        };
        assert_eq!(d.pos_at(10), [770, 100, -30]);
        assert!(d.is_active(29) && !d.is_active(30));
        let mid = d.pos_at_f(10.5);
        assert!((mid[0] - 770.0 / 512.0 - 38.5 / 512.0).abs() < 1e-4);
    }
}
