//! Deterministic neon-obstacle waves. Everything is a pure function of `(seed, wave_index)`, so a server
//! restart with the same seed, or a replay, produces the same waves, and clients need no extra data:
//! one `ObstacleSpawn` descriptor each, then positions come from `ObstacleDesc::pos_at(tick)`.

use shinobi_proto::quant::POS_SCALE;
use shinobi_sim::obstacle::{MotionPath, ObstacleDesc};

/// One wave every 10 s.
pub const WAVE_INTERVAL_TICKS: u32 = 600;
/// Telegraph: the descriptor is broadcast this many ticks before the obstacle starts moving.
pub const LEAD_TICKS: u32 = 60;
pub const OBSTACLE_LIFE_TICKS: u32 = 1800;
const ARENA_HALF_X: f32 = 35.0;
const LANE_HALF_Z: f32 = 30.0;

pub struct ObstacleSpawner {
    seed: u64,
    /// Wave `k` is due at `origin + k * WAVE_INTERVAL_TICKS` (a new map restarts the cadence at its own tick).
    origin: u32,
    next_wave: u32,
    next_id: u16,
}

/// xorshift64*: tiny, portable, integer-only.
struct Rng(u64);

impl Rng {
    fn new(seed: u64, wave: u32) -> Self {
        let mut s = seed ^ (wave as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        if s == 0 {
            s = 0x1234_5678_9abc_def1;
        }
        let mut r = Self(s);
        r.next(); // decorrelate nearby waves
        r
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `lo..=hi`.
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo + 1) as u64) as i32
    }
}

fn fx(m: f32) -> i32 {
    (m * POS_SCALE).round() as i32
}

impl ObstacleSpawner {
    pub fn new(seed: u64) -> Self {
        Self { seed, origin: 0, next_wave: 0, next_id: 1 }
    }

    /// Restart the wave sequence at `tick` (map change); ids keep counting so they never repeat.
    pub fn restart(&mut self, tick: u32, seed: u64) {
        self.seed = seed;
        self.origin = tick;
        self.next_wave = 0;
    }

    /// All obstacles whose wave time is `<= tick` and that have not been emitted yet.
    /// Loops so a stalled frame never skips a wave.
    pub fn poll(&mut self, tick: u32) -> Vec<ObstacleDesc> {
        let mut out = Vec::new();
        while self.origin.saturating_add(self.next_wave.saturating_mul(WAVE_INTERVAL_TICKS)) <= tick {
            self.emit_wave(self.next_wave, &mut out);
            self.next_wave += 1;
        }
        out
    }

    fn emit_wave(&mut self, wave: u32, out: &mut Vec<ObstacleDesc>) {
        let mut rng = Rng::new(self.seed, wave);
        let count = rng.range(3, 5);
        let start = self.origin + wave * WAVE_INTERVAL_TICKS + LEAD_TICKS;
        for _ in 0..count {
            let z = fx(rng.range(-(LANE_HALF_Z as i32), LANE_HALF_Z as i32) as f32);
            let period = rng.range(240, 480) as u16;
            let flip = rng.next() & 1 == 1;
            let (ax, bx) = if flip { (fx(ARENA_HALF_X), fx(-ARENA_HALF_X)) } else { (fx(-ARENA_HALF_X), fx(ARENA_HALF_X)) };
            let y = fx(0.75);
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            out.push(ObstacleDesc {
                id,
                kind: (rng.next() % 3) as u8,
                start_tick: start,
                end_tick: start + OBSTACLE_LIFE_TICKS,
                half: [48, 48, 48], // 0.75 m in 1/64 m
                path: MotionPath::PingPong { a: [ax, y, z], b: [bx, y, z], period },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_per_seed_and_wave_cadence() {
        let (mut a, mut b, mut c) = (ObstacleSpawner::new(42), ObstacleSpawner::new(42), ObstacleSpawner::new(43));
        assert_eq!(a.poll(0), b.poll(0));
        assert_ne!(ObstacleSpawner::new(42).poll(0), c.poll(0));
        assert!(a.poll(WAVE_INTERVAL_TICKS - 1).is_empty());
        let w1 = a.poll(WAVE_INTERVAL_TICKS);
        assert!((3..=5).contains(&w1.len()));
        assert!(w1.iter().all(|d| d.start_tick == WAVE_INTERVAL_TICKS + LEAD_TICKS));
    }

    #[test]
    fn stalled_frame_catches_up_without_skipping_or_duplicating() {
        let mut s = ObstacleSpawner::new(7);
        let mut late = s.poll(3 * WAVE_INTERVAL_TICKS + 5); // waves 0..=3 at once
        let mut inc = ObstacleSpawner::new(7);
        let mut stepwise = Vec::new();
        for t in (0..=3 * WAVE_INTERVAL_TICKS + 5).step_by(7) {
            stepwise.extend(inc.poll(t));
        }
        stepwise.extend(inc.poll(3 * WAVE_INTERVAL_TICKS + 5));
        late.sort_by_key(|d| d.id);
        stepwise.sort_by_key(|d| d.id);
        assert_eq!(late, stepwise);
        let ids: std::collections::HashSet<_> = late.iter().map(|d| d.id).collect();
        assert_eq!(ids.len(), late.len());
    }

    #[test]
    fn restart_rebases_waves_without_a_burst_or_id_reuse() {
        let mut s = ObstacleSpawner::new(5);
        let first = s.poll(0);
        s.restart(100_000, 6);
        assert!(s.poll(99_999).is_empty());
        let w0 = s.poll(100_000); // exactly one wave, not 166 catch-up waves
        assert!((3..=5).contains(&w0.len()));
        assert!(w0.iter().all(|d| d.start_tick == 100_000 + LEAD_TICKS));
        assert!(w0.iter().all(|d| first.iter().all(|f| f.id != d.id)), "ids never repeat across maps");
    }

    #[test]
    fn obstacles_stay_inside_arena() {
        let mut s = ObstacleSpawner::new(99);
        for d in s.poll(10 * WAVE_INTERVAL_TICKS) {
            for t in (d.start_tick..d.end_tick).step_by(31) {
                let p = d.pos_at(t);
                assert!(p[0].abs() <= fx(ARENA_HALF_X) && p[2].abs() <= fx(LANE_HALF_Z));
            }
        }
    }
}
