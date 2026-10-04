//! Display clock: which point on the simulation timeline a rendered frame should show.
//!
//! The fixed-step loop advances the sim by the (noisy) measured frame delta, and drawing at exactly that time makes every
//! frame's displacement follow the delta noise: a 16.7 ms frame is followed by a 14 ms one, the player sees the motion
//! speed wobble even though presentation (vsync) is perfectly regular. `DisplayClock` replaces the raw "now" with a
//! smoothed one: it advances by a filtered frame delta and is gently pulled toward `sim_now - LAG` so it never drifts, never
//! shows the future (the sim has not computed it) and never lags more than `MAX_LAG` ticks (a hitch catches up smoothly
//! instead of jumping). Steady-state lag behind the sim is `LAG` ticks (~8 ms at 60 Hz): 0.5 tick of extra latency traded
//! for a motion with no delta noise in it.
//!
//! All times are in TICKS (1 tick = 1/60 s). `raw = true` bypasses the filter (the displayed time is `sim_now`).

/// Steady-state distance behind the sim, ticks.
pub const LAG: f64 = 0.5;
/// Hard cap on the distance behind the sim, ticks.
pub const MAX_LAG: f64 = 4.0;
/// Smoothing of the frame delta (0..1; higher follows the raw delta faster).
const EMA: f64 = 0.08;
/// Pull toward the target time per frame (0..1).
const PULL: f64 = 0.05;

#[derive(Clone, Copy, Debug)]
pub struct DisplayClock {
    t: f64,
    dt: f64,
    started: bool,
    pub raw: bool,
}

impl Default for DisplayClock {
    fn default() -> Self {
        Self { t: 0.0, dt: 1.0, started: false, raw: false }
    }
}

impl DisplayClock {
    pub fn raw() -> Self {
        Self { raw: true, ..Default::default() }
    }

    /// `dt_ticks`: this frame's measured delta in ticks; `sim_now`: the newest simulated time (tick + overstep fraction).
    /// Returns the time to draw.
    pub fn advance(&mut self, dt_ticks: f64, sim_now: f64) -> f64 {
        if self.raw {
            return sim_now;
        }
        if !self.started || sim_now < self.t - MAX_LAG - 1.0 {
            // first frame, or the sim clock jumped back (reconnect): start over
            self.t = sim_now - LAG;
            self.dt = dt_ticks.clamp(0.25, 4.0);
            self.started = true;
            return self.t.min(sim_now);
        }
        let dt = dt_ticks.clamp(0.0, 8.0);
        // a hitch (one very long frame) is real time that really passed: let it through the filter gradually
        self.dt += (dt - self.dt) * EMA;
        let target = sim_now - LAG;
        let stepped = self.t + self.dt;
        self.t = stepped + (target - stepped) * PULL;
        self.t = self.t.clamp(sim_now - MAX_LAG, sim_now);
        self.t
    }

    /// How far behind the sim the last returned time was, ticks (for the PERF line / tests).
    pub fn lag(&self, sim_now: f64) -> f64 {
        sim_now - self.t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the clock with frame deltas; `sim_now` advances by the raw deltas (the fixed-step accumulator).
    fn run(dts: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let mut c = DisplayClock::default();
        let (mut sim, mut out, mut lag) = (10.0f64, Vec::new(), Vec::new());
        for dt in dts {
            sim += dt;
            out.push(c.advance(*dt, sim));
            lag.push(c.lag(sim));
        }
        (out, lag)
    }

    #[test]
    fn steady_frames_settle_at_the_configured_lag_and_advance_evenly() {
        let (out, lag) = run(&vec![1.0; 300]);
        assert!((lag[299] - LAG).abs() < 0.02, "lag {}", lag[299]);
        let steps: Vec<f64> = out.windows(2).skip(100).map(|w| w[1] - w[0]).collect();
        assert!(steps.iter().all(|s| (s - 1.0).abs() < 0.01));
    }

    #[test]
    fn delta_noise_is_filtered_out_of_the_displayed_motion() {
        let mut rng = 7u64;
        let dts: Vec<f64> = (0..600)
            .map(|_| {
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                1.0 + (((rng >> 33) as f64) / (1u64 << 31) as f64 - 0.5) * 0.36 // +-18 % = +-3 ms at 60 Hz
            })
            .collect();
        let (out, lag) = run(&dts);
        let spread = |v: &[f64]| v.iter().cloned().fold(0.0, f64::max) - v.iter().cloned().fold(f64::MAX, f64::min);
        let shown: Vec<f64> = out.windows(2).skip(100).map(|w| w[1] - w[0]).collect();
        let raw_spread = spread(&dts[100..]);
        assert!(spread(&shown) < raw_spread * 0.55, "displayed step spread {} vs raw {}", spread(&shown), raw_spread);
        assert!(lag.iter().skip(100).all(|l| *l >= 0.0 && *l <= MAX_LAG), "lag stays in [0, {MAX_LAG}]");
    }

    #[test]
    fn a_hitch_never_jumps_the_picture_and_never_shows_the_future() {
        let mut dts = vec![1.0; 120];
        dts.push(4.0); // 66 ms hitch
        dts.extend(vec![1.0; 120]);
        let (out, lag) = run(&dts);
        let steps: Vec<f64> = out.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(steps.iter().skip(60).all(|s| *s < 1.8), "worst displayed step {}", steps.iter().skip(60).cloned().fold(0.0, f64::max));
        assert!(lag.iter().all(|l| *l >= 0.0), "never ahead of the sim");
        assert!((lag[lag.len() - 1] - LAG).abs() < 0.1, "recovers to the steady lag, got {}", lag[lag.len() - 1]);
    }

    #[test]
    fn raw_mode_is_the_sim_clock() {
        let mut c = DisplayClock::raw();
        assert_eq!(c.advance(1.3, 42.5), 42.5);
    }

    #[test]
    fn a_sim_clock_that_jumps_back_restarts_the_filter() {
        let mut c = DisplayClock::default();
        c.advance(1.0, 1000.0);
        let t = c.advance(1.0, 5.0);
        assert!(t <= 5.0 && t >= 5.0 - MAX_LAG);
    }
}
