//! Server health metrics: per-window tick rate, sim cost per step, traffic. Printed every `interval`
//! as one greppable line so soak tests and ops can parse it.

use std::time::{Duration, Instant};

use shinobi_proto::TICK_HZ;

/// A step costing more than half its 16.6 ms budget is an overrun.
pub const OVERRUN_STEP_MS: f64 = 0.5 * 1000.0 / TICK_HZ as f64;

pub struct Stats {
    interval: Duration,
    window_start: Instant,
    start_tick: Option<u32>,
    steps: u64,
    step_ms_sum: f64,
    step_ms_max: f64,
    repl_ms_max: f64,
    overruns: u32,
    rx: u64,
    tx: u64,
    rate_dropped: u64,
    pub kicks: u32,
    pub timeouts: u32,
    /// Contained panics (see `supervise`).
    pub panics: u32,
    last_overrun_log: Option<Instant>,
}

impl Stats {
    pub fn new(now: Instant, interval: Duration) -> Self {
        Self {
            interval,
            window_start: now,
            start_tick: None,
            steps: 0,
            step_ms_sum: 0.0,
            step_ms_max: 0.0,
            repl_ms_max: 0.0,
            overruns: 0,
            rx: 0,
            tx: 0,
            rate_dropped: 0,
            kicks: 0,
            timeouts: 0,
            panics: 0,
            last_overrun_log: None,
        }
    }

    pub fn on_rx(&mut self) {
        self.rx += 1;
    }

    pub fn on_tx(&mut self) {
        self.tx += 1;
    }

    pub fn on_rate_drop(&mut self) {
        self.rate_dropped += 1;
    }

    /// One `Server::frame` that ran `ticks` steps costing `sim` in total, plus `repl` for replication.
    /// Returns a log line if this frame overran (at most one per second, so a bad stretch is not a log flood).
    pub fn on_frame(&mut self, now: Instant, ticks: u32, sim: Duration, repl: Duration) -> Option<String> {
        self.repl_ms_max = self.repl_ms_max.max(repl.as_secs_f64() * 1e3);
        if ticks == 0 {
            return None;
        }
        let per_step = sim.as_secs_f64() * 1e3 / ticks as f64;
        self.steps += ticks as u64;
        self.step_ms_sum += sim.as_secs_f64() * 1e3;
        self.step_ms_max = self.step_ms_max.max(per_step);
        if per_step <= OVERRUN_STEP_MS {
            return None;
        }
        self.overruns += 1;
        if self.last_overrun_log.map_or(true, |t| now - t >= Duration::from_secs(1)) {
            self.last_overrun_log = Some(now);
            return Some(format!(
                "overrun: {per_step:.2} ms/step over {ticks} step(s) (budget {:.1} ms, warn at {OVERRUN_STEP_MS:.1})",
                1000.0 / TICK_HZ as f64
            ));
        }
        None
    }

    /// Emits the window line when `interval` has elapsed, then starts a new window.
    pub fn maybe_report(&mut self, now: Instant, server_tick: u32, peers: usize) -> Option<String> {
        let start_tick = *self.start_tick.get_or_insert(server_tick);
        let elapsed = (now - self.window_start).as_secs_f64();
        if now - self.window_start < self.interval {
            return None;
        }
        let ticks = server_tick.wrapping_sub(start_tick);
        let line = format!(
            "stats: tick={server_tick} hz={:.2} steps={} step_avg_ms={:.3} step_max_ms={:.3} repl_max_ms={:.3} \
             overruns={} peers={peers} rx_pps={:.0} tx_pps={:.0} rate_dropped={} kicks={} timeouts={} panics={}",
            ticks as f64 / elapsed,
            self.steps,
            if self.steps > 0 { self.step_ms_sum / self.steps as f64 } else { 0.0 },
            self.step_ms_max,
            self.repl_ms_max,
            self.overruns,
            self.rx as f64 / elapsed,
            self.tx as f64 / elapsed,
            self.rate_dropped,
            self.kicks,
            self.timeouts,
            self.panics,
        );
        *self = Self { last_overrun_log: self.last_overrun_log, start_tick: Some(server_tick), ..Self::new(now, self.interval) };
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_report_computes_rate_and_resets() {
        let t0 = Instant::now();
        let mut s = Stats::new(t0, Duration::from_secs(10));
        assert!(s.maybe_report(t0, 100, 2).is_none()); // anchors the window at tick 100
        for _ in 0..5 {
            s.on_rx();
        }
        s.on_frame(t0, 2, Duration::from_millis(2), Duration::from_millis(1));
        assert!(s.maybe_report(t0 + Duration::from_secs(9), 640, 2).is_none());
        let line = s.maybe_report(t0 + Duration::from_secs(10), 700, 3).unwrap();
        assert!(line.contains("hz=60.00"), "{line}");
        assert!(line.contains("peers=3") && line.contains("step_max_ms=1.000") && line.contains("overruns=0"), "{line}");
        // next window starts from tick 700
        let line = s.maybe_report(t0 + Duration::from_secs(20), 1300, 3).unwrap();
        assert!(line.contains("hz=60.00") && line.contains("steps=0"), "{line}");
    }

    #[test]
    fn overrun_detected_and_log_is_rate_limited() {
        let t0 = Instant::now();
        let mut s = Stats::new(t0, Duration::from_secs(10));
        // 2 steps costing 20 ms total = 10 ms/step > 8.3 ms
        assert!(s.on_frame(t0, 2, Duration::from_millis(20), Duration::ZERO).is_some());
        assert!(s.on_frame(t0 + Duration::from_millis(100), 1, Duration::from_millis(12), Duration::ZERO).is_none());
        assert!(s.on_frame(t0 + Duration::from_millis(1100), 1, Duration::from_millis(12), Duration::ZERO).is_some());
        // healthy frame never logs
        assert!(s.on_frame(t0, 1, Duration::from_millis(3), Duration::ZERO).is_none());
        let line = s.maybe_report(t0 + Duration::from_secs(10), 10, 0).unwrap();
        assert!(line.contains("overruns=3"), "{line}");
    }
}
