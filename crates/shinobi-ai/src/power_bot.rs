//! Bots and the power weapons (docs/powerups.md). Everything here reads only what a client knows: the pedestals it was told
//! about, its own weapon, the arrows and tags in the snapshot, and the poses of the other players.
//!
//! * PICKUPS: a bot with empty hands that has a free moment and a pedestal (or a dropped weapon) within `LOOK_RANGE` walks
//!   to it, if it can get there on foot. Only some bots fancy a given pedestal (a per-bot, per-pedestal roll), so a map
//!   does not turn into a queue.
//! * YUMI: at a visible target 12 to 110 m away it draws (the draw ring, the slowed walk and the faint laser line are all
//!   there for the target to see), aims with lead and drop, and looses when the draw is full and the aim has settled.
//! * TAGS: thrown at a target 5 to 18 m away, more eagerly when other enemies stand near it (a cluster); once the hands
//!   are empty the bot sets them off when a foe is inside the blast, or lets the fuse run.
//! * DEFENCE: a bot that sees an arrow coming sometimes raises its shield (a fresh one parries it back), and a bot with a tag
//!   stuck on it swings at it.

use shinobi_proto::intent::Buttons;
use shinobi_sim::power::{PowerKind, YumiCfg};
use shinobi_sim::power_pass::{Arrow, Tag};
use shinobi_sim::rapier3d::prelude::*;

use crate::fsm::{Perception, Phase, TargetInfo, ThinkCtx};
use crate::rng::Rng;
use crate::view::V3;

/// Pedestals this close (m, flat) are considered, and bots give up on one after this many ticks.
pub const LOOK_RANGE: f32 = 30.0;
const GIVE_UP_TICKS: u32 = 60 * 14;
const YUMI_MIN: f32 = 12.0;
const YUMI_MAX: f32 = 110.0;
const TAG_MIN: f32 = 5.0;
const TAG_MAX: f32 = 18.0;
/// Share of bots that go for a given pedestal.
const FANCY_PERCENT: u64 = 55;

/// What the power layer wants this tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct PowerOut {
    /// Held draw button (yumi).
    pub draw: bool,
    /// A throw / detonate press this tick (tags).
    pub fire: bool,
    /// Walk toward this point instead of the tactical move (a pedestal).
    pub goal: Option<V3>,
    /// Look here instead of at the target: (yaw, pitch).
    pub aim: Option<(f32, f32)>,
    /// A shield for an arrow in flight.
    pub block: bool,
    /// Swing at a tag stuck to us.
    pub cut: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PowerBrain {
    /// The pedestal being walked to: id and the tick to give up.
    goal: Option<(u32, u32)>,
    next_look: u32,
    /// Ticks the bow has been fully drawn without a good shot.
    held_full: u32,
    /// Pedestals ruled out (not reachable) until a tick.
    skip: Option<(u32, u32)>,
    fire_ok: u32,
    cut_ok: u32,
    /// A tag throw decided: keep looking at the target until this tick, throw when the look has caught up.
    tag_aim_until: u32,
}

fn fancy(seed: u64, id: u16, pedestal: u32) -> bool {
    let mut x = seed ^ ((id as u64) << 20) ^ (pedestal as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 31;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 29;
    x % 100 < FANCY_PERCENT
}

/// Pitch that puts a projectile of speed `v` under gravity `g` on a point `x` m away horizontally and `y` m up (the flat
/// trajectory), or `None` when it cannot get there.
pub fn ballistic_pitch(x: f32, y: f32, v: f32, g: f32) -> Option<f32> {
    if x < 0.5 {
        return Some(y.atan2(x.max(0.01)));
    }
    if g <= 0.0 {
        return Some(y.atan2(x));
    }
    let disc = v.powi(4) - g * (g * x * x + 2.0 * y * v * v);
    if disc < 0.0 {
        return None;
    }
    Some(((v * v - disc.sqrt()) / (g * x)).atan())
}

fn flat_dist(a: V3, b: V3) -> f32 {
    ((a.x - b.x).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

impl PowerBrain {
    /// An enemy arrow within 20 m (~13 ticks out: a shield raised now parries it) closing on `me`.
    fn arrow_coming(arrows: &[Arrow], me: V3, id: u16, team_of: impl Fn(u16) -> u8, my_team: u8) -> bool {
        arrows.iter().any(|a| {
            if a.owner == id || (my_team != shinobi_sim::tick::NO_TEAM && team_of(a.owner) == my_team) {
                return false;
            }
            let to = me - vector![a.pos[0], a.pos[1], a.pos[2]];
            let dist = to.norm();
            let v = vector![a.vel[0], a.vel[1], a.vel[2]];
            dist < 20.0 && dist > 1.0 && to.dot(&v) / (dist * v.norm().max(1e-3)) > 0.985
        })
    }

    pub fn think(&mut self, ctx: &ThinkCtx, p: &Perception, rng: &mut Rng, seed: u64, phase: Phase, skill_err: f32, aim_noise: (f32, f32)) -> PowerOut {
        let (tick, me, world) = (ctx.world.tick, ctx.me, ctx.world);
        let server = world.server;
        let mut out = PowerOut::default();
        let Some(mine) = server.players.iter().find(|q| q.id == me.id) else { return out };
        let committed = matches!(phase, Phase::Strike { .. } | Phase::Evade { .. } | Phase::Dead);
        let pcfg = server.balance.combat.power;

        // ---- defence: an arrow on its way, a tag on my back
        let team_of = |id: u16| server.players.iter().find(|q| q.id == id).map_or(shinobi_sim::tick::NO_TEAM, |q| q.team);
        if !committed && Self::arrow_coming(&server.arrows, me.pos, me.id, team_of, me.team) {
            out.block = true;
        }
        if server.tags.iter().any(|t: &Tag| t.on.map_or(false, |(v, _)| v == me.id) && t.owner != me.id) && tick >= self.cut_ok && !committed {
            self.cut_ok = tick + 14;
            out.cut = true;
        }

        match mine.power.kind {
            PowerKind::Yumi => {
                self.goal = None;
                self.yumi(ctx, p, mine.power.ammo, mine.power.drawing, mine.power.full(&pcfg), &pcfg.yumi, committed, skill_err, aim_noise, &mut out);
            }
            PowerKind::Tag => {
                self.goal = None;
                self.tags(ctx, p, mine.power.ammo, mine.id, rng, committed, tick, &pcfg.tag, aim_noise, &mut out);
            }
            PowerKind::None => {
                self.held_full = 0;
                self.pickup(ctx, p, rng, seed, committed, &mut out);
            }
        }
        out
    }

    fn yumi(&mut self, ctx: &ThinkCtx, p: &Perception, ammo: u8, drawing: bool, full: bool, yc: &YumiCfg, committed: bool, skill_err: f32, noise: (f32, f32), out: &mut PowerOut) {
        let (tick, me) = (ctx.world.tick, ctx.me);
        let Some(t) = p.target.filter(|t| t.los && !t.view.spawn_protected && (YUMI_MIN..=YUMI_MAX).contains(&t.dist)) else {
            self.held_full = 0;
            return;
        };
        if committed || ammo == 0 || tick < self.fire_ok {
            self.held_full = 0;
            return;
        }
        // lead and drop: the point to look at so the arrow arrives where the target will be
        let flight = (t.dist / yc.speed).min(1.4);
        // (flat lead only: a grounded target's small downward velocity is the floor holding it up, not a fall)
        let aim_pt = t.view.pos + vector![t.view.vel.x, if t.view.grounded() { 0.0 } else { t.view.vel.y * 0.5 }, t.view.vel.z] * flight;
        let d = aim_pt - me.eye();
        let h = (d.x * d.x + d.z * d.z).sqrt();
        let Some(pitch) = ballistic_pitch(h, d.y, yc.speed, yc.gravity) else { return };
        let yaw = (-d.x).atan2(-d.z) + noise.0 * 0.2;
        let pitch = pitch + noise.1 * 0.2;
        out.aim = Some((yaw, pitch));
        out.draw = true;
        let wrap = |a: f32| (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        let err = wrap(yaw - me.yaw).abs().max((pitch - me.pitch).abs());
        let settled = err < 0.006 + skill_err * 0.1;
        if drawing && full {
            self.held_full += 1;
            if settled {
                out.draw = false; // release: the arrow leaves on this tick
                self.held_full = 0;
                self.fire_ok = tick + 20;
            } else if self.held_full > 75 {
                out.draw = false; // could not settle: let the draw go
                self.held_full = 0;
                self.fire_ok = tick + 40;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn tags(&mut self, ctx: &ThinkCtx, p: &Perception, ammo: u8, my_id: u16, rng: &mut Rng, committed: bool, tick: u32, tc: &shinobi_sim::power::TagCfg, noise: (f32, f32), out: &mut PowerOut) {
        let (me, server) = (ctx.me, ctx.world.server);
        let mine: Vec<&Tag> = server.tags.iter().filter(|t| t.owner == my_id).collect();
        if ammo == 0 {
            // hands empty: set them off when a foe is inside a blast, otherwise the fuse does it
            let armed: Vec<&&Tag> = mine.iter().filter(|t| t.armed(tc)).collect();
            let foe_near = server.players.iter().any(|q| {
                q.id != my_id && q.vitals.alive() && !shinobi_sim::tick::allied(me.team, q.team) && armed.iter().any(|t| {
                    let c = q.pos(&server.world);
                    flat_dist(vector![c[0], c[1], c[2]], vector![t.pos[0], t.pos[1], t.pos[2]]) < tc.radius * 0.6
                })
            });
            let me_safe = armed.iter().all(|t| flat_dist(me.pos, vector![t.pos[0], t.pos[1], t.pos[2]]) > tc.radius * 0.8);
            if foe_near && me_safe && tick >= self.fire_ok {
                out.fire = true;
                self.fire_ok = tick + 30;
            }
            return;
        }
        let Some(t) = p.target.filter(|t| t.los && !t.view.spawn_protected && (TAG_MIN..=TAG_MAX).contains(&t.dist)) else { return };
        if committed || tick < self.fire_ok || mine.len() as u32 >= tc.max_live {
            return;
        }
        // enemies bunched around the target make the throw worth it
        let cluster = server.players.iter().filter(|q| q.id != my_id && q.id != t.view.id && q.vitals.alive() && !shinobi_sim::tick::allied(me.team, q.team)).filter(|q| {
            let c = q.pos(&server.world);
            flat_dist(vector![c[0], c[1], c[2]], t.view.pos) < tc.radius * 0.8
        });
        let bunched = cluster.count() > 0;
        if tick >= self.tag_aim_until {
            let chance = if bunched { 0.2 } else { 0.06 };
            if !rng.chance(chance) {
                return;
            }
            self.tag_aim_until = tick + 25; // decided: take aim for up to 25 ticks
        }
        // aim at the target's middle with the tag's drop and a little lead
        let flight = (t.dist / tc.throw_speed).min(1.2);
        let aim_pt = t.view.pos + vector![t.view.vel.x, if t.view.grounded() { 0.0 } else { t.view.vel.y * 0.5 }, t.view.vel.z] * flight * 0.8;
        let d = aim_pt - me.eye();
        let h = (d.x * d.x + d.z * d.z).sqrt();
        let Some(pitch) = ballistic_pitch(h, d.y, tc.throw_speed, tc.gravity) else { return };
        let yaw = (-d.x).atan2(-d.z) + noise.0;
        out.aim = Some((yaw, pitch + noise.1 * 0.5));
        // the press goes out once the look has caught up (the next ticks of the same decision keep the aim)
        let wrap = |a: f32| (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        if wrap(yaw - me.yaw).abs() < 0.06 && (pitch - me.pitch).abs() < 0.06 {
            out.fire = true;
            self.fire_ok = tick + 70;
            self.tag_aim_until = 0;
        }
    }

    fn pickup(&mut self, ctx: &ThinkCtx, p: &Perception, rng: &mut Rng, seed: u64, committed: bool, out: &mut PowerOut) {
        let (tick, me, world) = (ctx.world.tick, ctx.me, ctx.world);
        let server = world.server;
        // in a fight: forget it
        let fighting = p.target.map_or(false, |t| t.los && t.dist < 14.0);
        if committed || fighting || me.hp_frac < 0.35 {
            return;
        }
        if let Some((id, give_up)) = self.goal {
            // still there? walk on; gone or too long: drop it
            let spot = server.pickups.active_weapons().find(|w| w.0 == id).map(|w| vector![w.1[0], w.1[1], w.1[2]]);
            match spot {
                Some(pos) if tick < give_up => {
                    out.goal = Some(pos);
                    return;
                }
                _ => {
                    self.skip = Some((id, tick + 600));
                    self.goal = None;
                }
            }
        }
        if tick < self.next_look {
            return;
        }
        self.next_look = tick + 45 + (rng.unit() * 30.0) as u32;
        let skipped = |id: u32| self.skip.map_or(false, |(s, until)| s == id && tick < until);
        let best = server
            .pickups
            .active_weapons()
            .filter(|w| w.2 != PowerKind::None && !skipped(w.0))
            .map(|w| (w, flat_dist(me.pos, vector![w.1[0], w.1[1], w.1[2]]), (w.1[1] - me.pos.y).abs()))
            .filter(|(w, d, dy)| *d < LOOK_RANGE && *dy < 2.5 && fancy(seed, me.id, w.0))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let Some((w, dist, _)) = best else { return };
        let pos = vector![w.1[0], w.1[1], w.1[2]];
        // reachable on foot? big maps have a path grid; on open ground a clear line is enough
        let reachable = match ctx.nav {
            Some(nav) => nav.path_search([me.pos.x, me.pos.z], [pos.x, pos.z], 12_000).map_or(false, |r| r.complete || r.end_gap < 2.0),
            None => world.los(me.eye(), pos + vector![0.0, 0.6, 0.0]) || dist < 6.0,
        };
        if !reachable {
            self.skip = Some((w.0, tick + 900));
            return;
        }
        self.goal = Some((w.0, tick + GIVE_UP_TICKS));
        out.goal = Some(pos);
    }
}

/// Buttons a power decision adds: a throw / detonate press, or a katana swing at a tag on our back.
pub fn buttons_of(o: &PowerOut) -> u8 {
    (if o.fire { Buttons::FIRE } else { 0 }) | (if o.cut { Buttons::SLICE } else { 0 })
}

pub fn target_pos(t: &TargetInfo) -> V3 {
    t.view.pos
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ballistic_pitch_lifts_for_drop_and_gives_up_out_of_range() {
        let flat = ballistic_pitch(50.0, 0.0, 90.0, 5.0).unwrap();
        assert!(flat > 0.0 && flat < 0.02, "a slow arc over 50 m needs a hair of lift: {flat}");
        let far = ballistic_pitch(100.0, 0.0, 90.0, 5.0).unwrap();
        assert!(far > flat, "more lift for more distance");
        // a tag (24 m/s, g 9) 18 m away needs a clear arc
        let tag = ballistic_pitch(18.0, 0.0, 24.0, 9.0).unwrap();
        assert!(tag > 0.1, "{tag}");
        assert!(ballistic_pitch(200.0, 0.0, 24.0, 9.0).is_none(), "out of range");
        assert_eq!(ballistic_pitch(0.1, 2.0, 24.0, 9.0).map(|p| p > 1.0), Some(true), "straight up close in");
    }

    #[test]
    fn about_half_the_bots_fancy_a_pedestal_and_the_choice_is_stable() {
        let n = (0..400u16).filter(|id| fancy(7, *id, 3)).count();
        assert!((140..300).contains(&n), "{n} of 400");
        assert_eq!(fancy(7, 5, 3), fancy(7, 5, 3));
    }
}
