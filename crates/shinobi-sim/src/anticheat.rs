//! Anti-cheat delta ceilings. The server already derives position from clamped intents, so these are a
//! second wall: they catch sim exploits (stacked lunges, knockback bugs) and hostile aim/clock input.
//!
//! * `max_step`      - per-tick displacement ceiling; breach -> snap back to the pre-step pose + strike.
//! * `max_turn`      - per-tick aim slew ceiling; excess is clamped (server aim != claimed aim) + strike.
//! * `future ticks`  - `IntentQueue::push_packet` rejects them; callers pass the count to `Guard::strike_n`.
//!
//! Strikes decay while the player behaves; `kick_at` strikes -> `Verdict::Kick`.

use std::f32::consts::{FRAC_PI_2, TAU};

use crate::character::Cfg;

#[derive(Clone, Copy, Debug)]
pub struct AntiCheatCfg {
    /// Max legal displacement in one tick, metres.
    pub max_step: f32,
    /// Max aim yaw/pitch change in one tick, radians.
    pub max_turn: f32,
    pub kick_at: u32,
    /// One strike forgiven per this many clean ticks.
    pub decay_ticks: u32,
}

impl AntiCheatCfg {
    /// Ceiling = fastest legal mover (dash/roll/wall-run/terminal fall) * dt, plus `slack` (fraction) for
    /// depenetration and step-up, so honest play never trips it.
    pub fn for_character(cfg: &Cfg, dt: f32, slack: f32) -> Self {
        let fastest = [cfg.dash_speed, cfg.roll_speed, cfg.wall_speed, cfg.run_speed, cfg.terminal.abs(), cfg.hookstrike_speed, cfg.wallslash_speed]
            .into_iter()
            .fold(0.0, f32::max);
        Self { max_step: fastest * dt * (1.0 + slack), max_turn: FRAC_PI_2, kick_at: 8, decay_ticks: 600 }
    }
}

impl AntiCheatCfg {
    /// The server's standard ceilings (25% slack). `Server::new` uses the same numbers; use this when a
    /// caller outside `shinobi-sim` needs a cfg for `Guard::strike_n`.
    ///
    /// The aim slew ceiling is off (`PI` = any turn is legal): a big flick, or the mouse motion a hitching client
    /// accumulated and delivers in one tick, is indistinguishable from a snap and is no cheat signal (an aimbot turns
    /// exactly as fast); striking it only kicked honest players. Position ceilings and flood limits still apply.
    pub fn standard(cfg: &Cfg) -> Self {
        Self { max_turn: std::f32::consts::PI, ..Self::for_character(cfg, shinobi_proto::TICK_DT, 0.25) }
    }
}

/// Token-bucket packet limiter driven by caller-supplied time (no clock inside the sim).
/// `burst` absorbs cellular bunching (a stalled radio delivers a second of packets at once);
/// `rate` is the sustained cap. `allow == false` means drop the datagram and strike the sender.
#[derive(Clone, Copy, Debug)]
pub struct RateLimiter {
    rate: f64,
    burst: f64,
    tokens: f64,
    last: f64,
}

impl RateLimiter {
    pub fn new(rate_per_sec: f64, burst: f64) -> Self {
        Self { rate: rate_per_sec, burst, tokens: burst, last: 0.0 }
    }

    pub fn allow(&mut self, now_secs: f64) -> bool {
        let dt = (now_secs - self.last).max(0.0);
        self.last = now_secs.max(self.last);
        self.tokens = (self.tokens + dt * self.rate).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Verdict {
    Ok,
    /// Displacement ceiling breached: put the body back at `to`, zero velocity.
    SnapBack { to: [f32; 3] },
    Kick,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Guard {
    pub strikes: u32,
    clean_ticks: u32,
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

impl Guard {
    pub fn strike_n(&mut self, n: u32, cfg: &AntiCheatCfg) -> Verdict {
        if n == 0 {
            return Verdict::Ok;
        }
        self.strikes += n;
        self.clean_ticks = 0;
        if self.strikes >= cfg.kick_at { Verdict::Kick } else { Verdict::Ok }
    }

    /// Call once per tick after the body moved. `allowance` = extra metres legitimately granted this tick
    /// (e.g. a server-side teleport pad); 0 normally.
    pub fn check_step(&mut self, before: [f32; 3], after: [f32; 3], allowance: f32, cfg: &AntiCheatCfg) -> Verdict {
        if dist(before, after) <= cfg.max_step + allowance {
            self.clean_ticks += 1;
            if self.clean_ticks >= cfg.decay_ticks {
                self.clean_ticks = 0;
                self.strikes = self.strikes.saturating_sub(1);
            }
            return Verdict::Ok;
        }
        match self.strike_n(1, cfg) {
            Verdict::Kick => Verdict::Kick,
            _ => Verdict::SnapBack { to: before },
        }
    }

    /// Clamp a claimed yaw (radians) to at most `max_turn` from `prev`; returns (yaw to use, strike verdict).
    pub fn limit_yaw(&mut self, prev: f32, claimed: f32, cfg: &AntiCheatCfg) -> (f32, Verdict) {
        let d = (claimed - prev + std::f32::consts::PI).rem_euclid(TAU) - std::f32::consts::PI;
        if d.abs() <= cfg.max_turn {
            return (claimed, Verdict::Ok);
        }
        let v = self.strike_n(1, cfg);
        ((prev + cfg.max_turn * d.signum()).rem_euclid(TAU), v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AntiCheatCfg {
        AntiCheatCfg::for_character(&Cfg::default(), 1.0 / 60.0, 0.25)
    }

    #[test]
    fn dash_speed_is_legal_teleport_is_not() {
        let c = cfg();
        let mut g = Guard::default();
        let dash = Cfg::default().dash_speed / 60.0;
        assert_eq!(g.check_step([0.0; 3], [dash, 0.0, 0.0], 0.0, &c), Verdict::Ok);
        assert_eq!(g.check_step([0.0; 3], [50.0, 0.0, 0.0], 0.0, &c), Verdict::SnapBack { to: [0.0; 3] });
        assert_eq!(g.strikes, 1);
    }

    #[test]
    fn repeated_breaches_kick_and_clean_play_decays() {
        let c = cfg();
        let mut g = Guard::default();
        for _ in 0..c.kick_at - 1 {
            assert!(matches!(g.check_step([0.0; 3], [99.0, 0.0, 0.0], 0.0, &c), Verdict::SnapBack { .. }));
        }
        assert_eq!(g.check_step([0.0; 3], [99.0, 0.0, 0.0], 0.0, &c), Verdict::Kick);
        let mut g = Guard::default();
        g.strike_n(3, &c);
        for _ in 0..c.decay_ticks {
            g.check_step([0.0; 3], [0.0; 3], 0.0, &c);
        }
        assert_eq!(g.strikes, 2);
    }

    #[test]
    fn yaw_slew_is_clamped_across_the_wrap() {
        let c = cfg();
        let mut g = Guard::default();
        let (y, v) = g.limit_yaw(0.1, TAU - 0.1, &c); // tiny turn through the wrap: legal
        assert_eq!((y, v), (TAU - 0.1, Verdict::Ok));
        let (y, _) = g.limit_yaw(0.0, std::f32::consts::PI - 0.01, &c); // near-180 flick: clamped
        assert!((y - c.max_turn).abs() < 1e-5);
        assert_eq!(g.strikes, 1);
    }
}

#[cfg(test)]
mod rate_tests {
    use super::*;

    #[test]
    fn burst_then_sustained_cap() {
        let mut r = RateLimiter::new(100.0, 50.0);
        // a radio stall delivers 50 packets at t=0: all allowed, the 51st is not
        assert_eq!((0..60).filter(|_| r.allow(0.0)).count(), 50);
        // honest 60 Hz afterwards is always within the cap
        let mut ok = 0;
        for i in 1..=120 {
            ok += r.allow(i as f64 / 60.0) as u32;
        }
        assert_eq!(ok, 120);
        // a 1000 pkt/s flood gets ~rate through
        let mut r = RateLimiter::new(100.0, 50.0);
        let passed = (0..2000).filter(|i| r.allow(*i as f64 / 1000.0)).count();
        assert!((240..=260).contains(&passed), "{passed}"); // 2 s * 100 + 50 burst
    }
}
