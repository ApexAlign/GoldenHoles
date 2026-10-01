//! Client-side entity interpolation with adaptive delay (jitter-aware) and capped extrapolation.

use std::collections::VecDeque;

use shinobi_proto::intent::ViewState;
use shinobi_proto::quant::{dq_pos3, dq_vel, dq_yaw};
use shinobi_proto::snapshot::{EntityId, Snapshot};
use shinobi_proto::{SNAPSHOT_DIVISOR, TICK_DT};

/// Extrapolate at most 100 ms past the newest snapshot, then hold.
pub const MAX_EXTRAP_TICKS: f64 = 6.0;
const MAX_SNAPSHOTS: usize = 32;

/// EWMA of one-way transit variation, in ticks.
#[derive(Default)]
pub struct JitterEstimator {
    mean: Option<f64>,
    dev: f64,
}

impl JitterEstimator {
    /// `local_tick`: the client's estimate of the current server tick when the datagram arrived (fractional).
    pub fn on_arrival(&mut self, snap_tick: u32, local_tick: f64) {
        let transit = local_tick - snap_tick as f64;
        match &mut self.mean {
            None => self.mean = Some(transit),
            Some(m) => {
                let err = transit - *m;
                *m += err * 0.0625;
                self.dev += (err.abs() - self.dev) * 0.125;
            }
        }
    }

    /// How far behind the newest data to render. Floor = one snapshot interval + a tick of slack.
    pub fn delay_ticks(&self) -> f64 {
        let floor = SNAPSHOT_DIVISOR as f64 + 1.0;
        (floor + 3.0 * self.dev).clamp(floor, 12.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub pos: [f32; 3],
    pub yaw: f32,
    pub extrapolated: bool,
}

#[derive(Default)]
pub struct InterpBuffer {
    snaps: VecDeque<Snapshot>,
    pub jitter: JitterEstimator,
}

impl InterpBuffer {
    pub fn push(&mut self, s: Snapshot, local_tick: f64) {
        if self.snaps.back().map_or(false, |b| b.tick >= s.tick) {
            return; // reordered / duplicate
        }
        self.jitter.on_arrival(s.tick, local_tick);
        self.snaps.push_back(s);
        while self.snaps.len() > MAX_SNAPSHOTS {
            self.snaps.pop_front();
        }
    }

    pub fn get(&self, tick: u32) -> Option<&Snapshot> {
        self.snaps.iter().find(|s| s.tick == tick)
    }

    pub fn newest_tick(&self) -> Option<u32> {
        self.snaps.back().map(|s| s.tick)
    }

    /// Render tick for a given estimate of the current server tick.
    pub fn render_tick(&self, server_tick_estimate: f64) -> f64 {
        server_tick_estimate - self.jitter.delay_ticks()
    }

    /// Send this with the next input packet so the server rewinds to what was on screen.
    pub fn view_state(render_tick: f64) -> ViewState {
        let t = render_tick.max(0.0);
        ViewState { from_tick: t.floor() as u32, alpha: ((t.fract() * 256.0) as u32).min(255) as u8 }
    }

    pub fn sample(&self, id: EntityId, render_tick: f64) -> Option<Sample> {
        let mut prev = None;
        for s in self.snaps.iter().filter(|s| s.get(id).is_some()) {
            if s.tick as f64 > render_tick {
                let a = match prev {
                    Some(a) => a,
                    None => {
                        // render time is before everything we have: hold the oldest state
                        let e = s.get(id)?;
                        return Some(Sample { pos: dq_pos3(e.pos), yaw: dq_yaw(e.yaw), extrapolated: false });
                    }
                };
                return Some(lerp(a, s, id, render_tick));
            }
            prev = Some(s);
        }
        // beyond the newest snapshot: extrapolate on velocity, capped
        let s: &Snapshot = prev?;
        let e = s.get(id)?;
        let dt = (render_tick - s.tick as f64).clamp(0.0, MAX_EXTRAP_TICKS) as f32 * TICK_DT;
        let p = dq_pos3(e.pos);
        Some(Sample {
            pos: [
                p[0] + dq_vel(e.vel[0]) * dt,
                p[1] + dq_vel(e.vel[1]) * dt,
                p[2] + dq_vel(e.vel[2]) * dt,
            ],
            yaw: dq_yaw(e.yaw),
            extrapolated: dt > 0.0,
        })
    }
}

fn lerp(a: &Snapshot, b: &Snapshot, id: EntityId, rt: f64) -> Sample {
    let (ea, eb) = (a.get(id).unwrap(), b.get(id).unwrap());
    let f = ((rt - a.tick as f64) / (b.tick - a.tick) as f64).clamp(0.0, 1.0) as f32;
    let (pa, pb) = (dq_pos3(ea.pos), dq_pos3(eb.pos));
    let pos = [pa[0] + (pb[0] - pa[0]) * f, pa[1] + (pb[1] - pa[1]) * f, pa[2] + (pb[2] - pa[2]) * f];
    // shortest-arc yaw
    let dy = eb.yaw.wrapping_sub(ea.yaw) as i16 as f32 / 65536.0 * std::f32::consts::TAU;
    Sample { pos, yaw: dq_yaw(ea.yaw) + dy * f, extrapolated: false }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shinobi_proto::snapshot::{EntityState, MoveKind};

    fn snap(tick: u32, x: i32, vx: i16) -> Snapshot {
        let st = EntityState { pos: [x, 0, 0], vel: [vx, 0, 0], yaw: 0, kind: MoveKind::Grounded, flags: 0 };
        Snapshot { tick, entities: vec![(1, st)] }
    }

    #[test]
    fn interpolates_and_extrapolates_with_cap() {
        let mut b = InterpBuffer::default();
        b.push(snap(10, 0, 0), 12.0);
        b.push(snap(12, 512, 0), 14.0);
        let s = b.sample(1, 11.0).unwrap();
        assert!((s.pos[0] - 0.5).abs() < 1e-5 && !s.extrapolated);
        // 9 m/s = 576 u
        let mut c = InterpBuffer::default();
        c.push(snap(10, 0, 576), 12.0);
        let e = c.sample(1, 10.0 + 100.0).unwrap();
        assert!(e.extrapolated);
        assert!((e.pos[0] - 9.0 * MAX_EXTRAP_TICKS as f32 * TICK_DT).abs() < 1e-3);
    }

    #[test]
    fn jitter_grows_delay() {
        let mut calm = JitterEstimator::default();
        let mut rough = JitterEstimator::default();
        for i in 0..200u32 {
            calm.on_arrival(i * 2, (i * 2) as f64 + 3.0);
            rough.on_arrival(i * 2, (i * 2) as f64 + 3.0 + if i % 2 == 0 { 0.0 } else { 5.0 });
        }
        assert!(rough.delay_ticks() > calm.delay_ticks() + 2.0);
    }

    #[test]
    fn view_state_packs_fraction() {
        let v = InterpBuffer::view_state(100.5);
        assert_eq!((v.from_tick, v.alpha), (100, 128));
    }
}
