//! Environmental events (docs/destruction_design.md): QUAKES and LIGHTNING. Server-driven, only while a match is live,
//! telegraphed so they are dramatic and fair, replicated as `NetEvent::EnvFx`.
//!
//! * QUAKE: a rumble warning, then a few seconds of shaking. Every brick takes damage every tick (weak, barely supported ones
//!   far more), so towers and overhangs come down in chains; grounded players are shoved around. Camera shake / dust is the
//!   client's job.
//! * LIGHTNING: a ring appears where a bolt WILL land (usually under whoever has been standing still: campers are targeted),
//!   `bolt_warn_s` later it strikes there: a huge blast on the bricks, heavy damage and a launch for players in the radius.
//!   Move and you live. Bolts come in volleys.
//!
//! Everything is off in `Balance::demo()` (tests and the shipped duel numbers stay as they are); `balance/storm.cfg` turns it on
//! intensely (`PRESET=storm`, the demo default).

use crate::pvp::{Cause, PvpHit, NO_ATTACKER};
use crate::tick::Server;

/// Obstacle id a lightning hit reports as (`HazardSource::Obstacle`), like the void (65535) and the splat (65534).
pub const LIGHTNING_SOURCE_ID: u16 = u16::MAX - 2;
pub const LIGHTNING: crate::pvp::HazardSource = crate::pvp::HazardSource::Obstacle(LIGHTNING_SOURCE_ID);

pub const HZ: u32 = 60;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvCfg {
    pub quake: bool,
    pub lightning: bool,
    /// Gap between quakes (s, drawn uniformly), warning before the first shake (s), shaking time (s).
    pub quake_min_s: f32,
    pub quake_max_s: f32,
    pub quake_warn_s: f32,
    pub quake_s: f32,
    /// Share of a brick's hit points removed per second of shaking (jittered per brick; weakly supported bricks take 2.5x).
    pub quake_dmg_frac_per_s: f32,
    /// Horizontal shove (m/s) a grounded player gets every half second of shaking.
    pub quake_shove: f32,
    pub bolt_min_s: f32,
    pub bolt_max_s: f32,
    pub bolt_warn_s: f32,
    pub bolt_radius: f32,
    pub bolt_player_dmg: f32,
    /// Damage on bricks at the centre of a strike (before `destruct.blast_mult`'s share: applied raw).
    pub bolt_brick_dmg: f32,
    /// Most bolts in one volley.
    pub bolt_volley: u32,
    /// Launch speed (m/s) at the centre of a strike.
    pub bolt_knock: f32,
}

impl Default for EnvCfg {
    fn default() -> Self {
        Self {
            quake: false,
            lightning: false,
            quake_min_s: 45.0,
            quake_max_s: 75.0,
            quake_warn_s: 2.5,
            quake_s: 4.0,
            quake_dmg_frac_per_s: 0.12,
            quake_shove: 2.5,
            bolt_min_s: 6.0,
            bolt_max_s: 11.0,
            bolt_warn_s: 2.0,
            bolt_radius: 7.0,
            bolt_player_dmg: 80.0,
            bolt_brick_dmg: 900.0,
            bolt_volley: 3,
            bolt_knock: 16.0,
        }
    }
}

impl EnvCfg {
    pub fn from_balance(b: &crate::balance::EnvBalance) -> Self {
        Self {
            quake: b.quake > 0.5,
            lightning: b.lightning > 0.5,
            quake_min_s: b.quake_min_s,
            quake_max_s: b.quake_max_s.max(b.quake_min_s),
            quake_warn_s: b.quake_warn_s,
            quake_s: b.quake_s,
            quake_dmg_frac_per_s: b.quake_dmg_frac_per_s,
            quake_shove: b.quake_shove,
            bolt_min_s: b.bolt_min_s,
            bolt_max_s: b.bolt_max_s.max(b.bolt_min_s),
            bolt_warn_s: b.bolt_warn_s,
            bolt_radius: b.bolt_radius,
            bolt_player_dmg: b.bolt_player_dmg,
            bolt_brick_dmg: b.bolt_brick_dmg,
            bolt_volley: b.bolt_volley.max(1),
            bolt_knock: b.bolt_knock,
        }
    }
}

/// What the server tells clients about, one per `NetEvent::EnvFx`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EnvEvent {
    QuakeWarn { tick: u32, warn_ticks: u32 },
    QuakeStart { tick: u32, dur_ticks: u32 },
    QuakeEnd { tick: u32 },
    BoltMark { tick: u32, id: u8, pos: [f32; 3], warn_ticks: u32 },
    BoltStrike { tick: u32, id: u8, pos: [f32; 3] },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum QuakePhase {
    Idle,
    Warn { start_at: u32 },
    Active { until: u32 },
}

#[derive(Clone, Copy, Debug)]
struct Bolt {
    id: u8,
    pos: [f32; 3],
    strike_at: u32,
}

pub struct Env {
    pub cfg: EnvCfg,
    rng: u64,
    quake: QuakePhase,
    /// Tick the next quake / bolt volley is scheduled for (0 = not scheduled yet).
    next_quake: u32,
    next_bolt: u32,
    bolts: Vec<Bolt>,
    next_id: u8,
}

impl Default for Env {
    fn default() -> Self {
        Self { cfg: EnvCfg::default(), rng: 0x9E37_79B9_7F4A_7C15, quake: QuakePhase::Idle, next_quake: 0, next_bolt: 0, bolts: Vec::new(), next_id: 0 }
    }
}

impl Env {
    fn u(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.u()
    }

    /// New match / map: nothing pending, timers reschedule on the next live tick.
    pub fn reset(&mut self) {
        self.quake = QuakePhase::Idle;
        self.next_quake = 0;
        self.next_bolt = 0;
        self.bolts.clear();
    }

    pub fn quake_active(&self) -> bool {
        matches!(self.quake, QuakePhase::Active { .. })
    }

    /// Late joiner catch-up: the quake in progress as `(remaining ticks, warning?)` and the marked bolts `(id, pos, ticks left)`.
    pub fn catch_up(&self, tick: u32) -> (Option<(u32, bool)>, Vec<(u8, [f32; 3], u32)>) {
        let q = match self.quake {
            QuakePhase::Idle => None,
            QuakePhase::Warn { start_at } => Some((start_at.saturating_sub(tick), true)),
            QuakePhase::Active { until } => Some((until.saturating_sub(tick), false)),
        };
        (q, self.bolts.iter().map(|b| (b.id, b.pos, b.strike_at.saturating_sub(tick))).collect())
    }
}

impl Server {
    /// Once per tick, after the live-phase check. `hits` collects the lightning damage for `pvp::apply_hits`.
    pub(crate) fn env_step(&mut self, tick: u32, hits: &mut Vec<PvpHit>) {
        let cfg = self.env.cfg;
        if !(cfg.quake || cfg.lightning) {
            return;
        }
        if self.match_state.kind() != crate::match_state::PhaseKind::Live || self.match_state.frozen() {
            self.env.reset();
            return;
        }
        // ------------------------------------------------------------------------------------------ quake
        if cfg.quake {
            if self.env.next_quake == 0 {
                let d = self.env.range(cfg.quake_min_s, cfg.quake_max_s);
                self.env.next_quake = tick + (d * HZ as f32) as u32;
            }
            match self.env.quake {
                QuakePhase::Idle if tick >= self.env.next_quake => {
                    let warn = (cfg.quake_warn_s * HZ as f32) as u32;
                    self.env.quake = QuakePhase::Warn { start_at: tick + warn };
                    self.env_events.push(EnvEvent::QuakeWarn { tick, warn_ticks: warn });
                }
                QuakePhase::Warn { start_at } if tick >= start_at => {
                    let dur = (cfg.quake_s * HZ as f32) as u32;
                    self.env.quake = QuakePhase::Active { until: tick + dur };
                    self.env_events.push(EnvEvent::QuakeStart { tick, dur_ticks: dur });
                }
                QuakePhase::Active { until } => {
                    self.quake_tick(tick, &cfg);
                    if tick >= until {
                        self.env.quake = QuakePhase::Idle;
                        let d = self.env.range(cfg.quake_min_s, cfg.quake_max_s);
                        self.env.next_quake = tick + (d * HZ as f32) as u32;
                        self.env_events.push(EnvEvent::QuakeEnd { tick });
                        self.breaks.flush_cracks(tick, 300, &mut self.break_events);
                    }
                }
                _ => {}
            }
        }
        // ------------------------------------------------------------------------------------------ lightning
        if cfg.lightning {
            if self.env.next_bolt == 0 {
                let d = self.env.range(cfg.bolt_min_s, cfg.bolt_max_s);
                self.env.next_bolt = tick + (d * HZ as f32) as u32;
            }
            if tick >= self.env.next_bolt {
                let n = 1 + (self.env.u() < 0.45) as u32 + (self.env.u() < 0.2) as u32;
                for k in 0..n.min(cfg.bolt_volley) {
                    if let Some(pos) = self.pick_bolt_target() {
                        let warn = (cfg.bolt_warn_s * HZ as f32) as u32 + k * 24; // volley: staggered strikes
                        let id = self.env.next_id;
                        self.env.next_id = self.env.next_id.wrapping_add(1);
                        self.env.bolts.push(Bolt { id, pos, strike_at: tick + warn });
                        self.env_events.push(EnvEvent::BoltMark { tick, id, pos, warn_ticks: warn });
                    }
                }
                let d = self.env.range(cfg.bolt_min_s, cfg.bolt_max_s);
                self.env.next_bolt = tick + (d * HZ as f32) as u32;
            }
            let due: Vec<Bolt> = self.env.bolts.iter().copied().filter(|b| tick >= b.strike_at).collect();
            if !due.is_empty() {
                self.env.bolts.retain(|b| tick < b.strike_at);
                for b in due {
                    self.bolt_strike(tick, b, &cfg, hits);
                }
            }
        }
    }

    /// Target: usually the live player who has moved least lately (campers pay), sometimes a random one, else a random brick.
    fn pick_bolt_target(&mut self) -> Option<[f32; 3]> {
        let live: Vec<usize> = (0..self.players.len()).filter(|&i| self.players[i].vitals.alive() && !self.players[i].vitals.spawn_protected(self.tick)).collect();
        if live.is_empty() {
            return None;
        }
        let i = if self.env.u() < 0.65 {
            *live
                .iter()
                .min_by(|&&a, &&b| {
                    let sa = self.players[a].ch.vel.norm();
                    let sb = self.players[b].ch.vel.norm();
                    sa.total_cmp(&sb)
                })
                .unwrap()
        } else {
            live[(self.env.u() * live.len() as f32) as usize % live.len()]
        };
        Some(self.players[i].pos(&self.world))
    }

    /// Quake, one tick: bricks take damage (weak ones more), grounded players get shoved twice a second.
    fn quake_tick(&mut self, tick: u32, cfg: &EnvCfg) {
        let frac = cfg.quake_dmg_frac_per_s / HZ as f32;
        if self.breaks.quake(&mut self.world, tick, frac, &mut self.break_events) {
            self.world.queries.update(&self.world.colliders);
        }
        if tick % 30 == 0 && cfg.quake_shove > 0.0 {
            for i in 0..self.players.len() {
                if !self.players[i].vitals.alive() || !matches!(self.players[i].ch.state, crate::character::MoveState::Grounded) {
                    continue;
                }
                let a = self.env.u() * std::f32::consts::TAU;
                let v = &mut self.players[i].ch.vel;
                v.x += a.cos() * cfg.quake_shove;
                v.z += a.sin() * cfg.quake_shove;
            }
        }
    }

    fn bolt_strike(&mut self, tick: u32, b: Bolt, cfg: &EnvCfg, hits: &mut Vec<PvpHit>) {
        self.env_events.push(EnvEvent::BoltStrike { tick, id: b.id, pos: b.pos });
        let raw = cfg.bolt_brick_dmg / self.breaks.cfg.blast_mult.max(1e-3);
        let r = cfg.bolt_radius;
        if self.breaks.blast(&mut self.world, b.pos, r, |d| raw * (1.0 - 0.5 * (d / r).min(1.0)), tick, &mut self.break_events) {
            self.world.queries.update(&self.world.colliders);
        }
        for j in 0..self.players.len() {
            let p = &self.players[j];
            if !p.vitals.alive() || p.vitals.spawn_protected(tick) {
                continue;
            }
            let pp = p.pos(&self.world);
            let d = ((pp[0] - b.pos[0]).powi(2) + (pp[1] - b.pos[1]).powi(2) + (pp[2] - b.pos[2]).powi(2)).sqrt();
            if d > r {
                continue;
            }
            let k = 1.0 - 0.6 * (d / r).min(1.0);
            hits.push(PvpHit { attacker: NO_ATTACKER, victim: j, amount: cfg.bolt_player_dmg * k, cause: Cause::Hazard(LIGHTNING), head: false, stealth: false });
            // launched away from the strike, and up
            let (dx, dz) = (pp[0] - b.pos[0], pp[2] - b.pos[2]);
            let dl = (dx * dx + dz * dz).sqrt().max(1e-3);
            let v = &mut self.players[j].ch.vel;
            *v = crate::rapier3d::prelude::Vector::new(dx / dl * cfg.bolt_knock * k, 7.0 + 5.0 * k, dz / dl * cfg.bolt_knock * k);
            if matches!(self.players[j].ch.state, crate::character::MoveState::Grounded) {
                self.players[j].ch.state = crate::character::MoveState::Airborne;
            }
        }
    }
}
