//! The bot brain: a two-layer finite state machine, evaluated once per 60 Hz tick.
//!
//! ```text
//!  TACTICAL layer (Phase)                      TRAVERSAL layer (Traverse)  <- owns the legs, can veto the tactical move
//!  ------------------------------              ------------------------------------------------------------
//!  Advance  heavy lane rush / close in          Free   run the tactical move, whisker-steer around geometry
//!  Flank    light: orbit to a side/rear slot
//!  Perch    light: hop onto a box, shoot down   Hop    jump + double-jump onto a ledge
//!  Zone     hold ring radius, strafe, shuriken
//!  Strike   WindUp -> Slice -> Recover (katana dash-slice; needs a ticket when off-screen)
//!  Evade    break the player's view: dash / roll / wall-cling
//!  Retreat  low hp: back off
//!  Dead
//! ```
//!
//! Priority each tick: dead > committed phase (Strike / Evade in progress) > Retreat > Evade (FOV threat) >
//! Strike (in range, ticket or visible) > role movement. The traversal layer owns ledge hops (jump + double jump
//! onto a box) and whisker steering around level geometry.
//!
//! The brain is a pure function of `(WorldView, own memory)`: no clock, no thread RNG (see `rng.rs`), no map
//! iteration order. Its only output is a `PlayerIntent`, pushed through the same `IntentQueue` a real client uses,
//! so anti-cheat, lag-comp, vitals and respawn all apply unchanged.

use std::f32::consts::{FRAC_PI_2, PI};

use shinobi_proto::intent::{Buttons, PlayerIntent};
use shinobi_proto::quant::{q_axis, q_pitch, q_yaw};
use shinobi_proto::TICK_HZ;

use crate::nav::*;
use crate::rng::Rng;
use crate::skill::{Archetype, SkillParams};
use crate::tickets::Request;
use crate::view::{Frustum, PlayerView, ViewAngles, WorldView, V3};
use shinobi_sim::rapier3d::prelude::*;

/// Katana lunge + blade reach is ~6.6 m; start a little inside it.
pub const STRIKE_RANGE: f32 = 6.0;
/// Bots ask for a ticket this far out so it is ready when they arrive.
pub const REQUEST_RANGE: f32 = 13.0;
const SHURIKEN_RANGE: f32 = 34.0;
const EVADE_TICKS: u32 = 16;
const EVADE_COOLDOWN: u32 = 50;
const THREAT_MARGIN: f32 = 0.21; // ~12 deg: react before being fully on screen

#[derive(Clone, Copy, Debug, Default)]
pub struct Assignment {
    pub target: Option<u16>,
    /// Slot angle relative to the target's facing (0 = ahead, pi = behind).
    pub slot_angle: f32,
    pub rank: usize,
}

pub struct ThinkCtx<'a, 'w> {
    pub world: &'a WorldView<'w>,
    pub me: &'a PlayerView,
    pub assign: Assignment,
    pub has_ticket: bool,
    pub perches: &'a [Perch],
    pub frustum: Frustum,
    /// Ground-layer path grid (big maps); `None` until the host has built one.
    pub nav: Option<&'a crate::nav::NavGrid>,
    /// A* plans still allowed this tick (shared by all bots, `PLAN_PER_TICK` per tick): a replan that finds none
    /// left is postponed to a later tick, so a burst of bots needing paths never turns into one long frame.
    pub plan_budget: &'a std::cell::Cell<u32>,
}

#[derive(Clone, Copy, Debug)]
pub struct TargetInfo {
    pub view: PlayerView,
    pub dist: f32,
    pub los: bool,
    /// Where *I* sit in the target's camera.
    pub angles: ViewAngles,
    /// Inside the target's frustum with clear line of sight: the player can see me.
    pub on_screen: bool,
    /// Inside the frustum widened by `THREAT_MARGIN`.
    pub near_screen: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Perception {
    pub target: Option<TargetInfo>,
}

impl TargetInfo {
    /// First-person fairness: only a *human* camera can be blindsided.
    pub fn needs_ticket(&self) -> bool {
        !self.view.is_bot && !self.on_screen
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StrikeStage {
    WindUp,
    Slice,
    Recover,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EvadeKind {
    Dash,
    Roll,
    WallCling,
    Strafe,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Dead,
    Advance,
    Flank,
    Perch,
    Zone,
    Strike { stage: StrikeStage },
    Evade { kind: EvadeKind, dir: V3, until: u32 },
    Retreat,
}

impl Phase {
    pub fn name(&self) -> &'static str {
        match self {
            Phase::Dead => "dead",
            Phase::Advance => "advance",
            Phase::Flank => "flank",
            Phase::Perch => "perch",
            Phase::Zone => "zone",
            Phase::Strike { .. } => "strike",
            Phase::Evade { .. } => "evade",
            Phase::Retreat => "retreat",
        }
    }

    fn committed(&self, tick: u32) -> bool {
        match self {
            Phase::Strike { .. } => true,
            Phase::Evade { until, .. } => tick < *until,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HopKind {
    /// Onto a box top at height `top`.
    Ledge { top: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Traverse {
    Free,
    Hop { kind: HopKind, since: u32, dir: V3, start: V3, double_sent: bool },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BotStats {
    pub evades: u32,
    pub strikes: u32,
    pub shurikens: u32,
    pub wall_runs: u32,
    pub ledge_hops: u32,
    pub ticketless_waits: u32,
    /// Times the wedge detector backed the bot out of geometry.
    pub unsticks: u32,
}

#[derive(Default)]
pub struct ThinkOut {
    pub intent: PlayerIntent,
    pub release_ticket: bool,
}

pub struct BotMem {
    pub id: u16,
    pub archetype: Archetype,
    pub skill: SkillParams,
    /// The tier's own numbers; `skill` is re-derived from these by the host as the pace changes.
    pub base_skill: SkillParams,
    pub seed: u64,
    pub phase: Phase,
    pub phase_since: u32,
    pub trav: Traverse,
    pub stats: BotStats,
    // timers (ticks)
    fire_ok: u32,
    slice_ok: u32,
    evade_ok: u32,
    retreat_block: u32,
    strafe_sign: f32,
    strafe_until: u32,
    aim_noise: (f32, f32),
    threat_since: Option<u32>,
    threat_react: Option<bool>,
    want_since: Option<u32>,
    want_last: u32,
    perch: Option<usize>,
    perch_since: u32,
    was_wallrun: bool,
    top_ticks: u32,
    // path following (NavGrid): waypoints to the target's last known cell, replanned every so often
    path: Vec<[f32; 2]>,
    path_goal: [f32; 2],
    path_at: u32,
    /// Targets no walkable path leads to (on a roof / tower top): `(target id, ignore until tick)`. The director
    /// picks someone else while an entry is live, so the bot keeps moving instead of pushing at the foot of a wall.
    pub unreachable: Vec<(u16, u32)>,
    // wedge detector: position at the last check and the tick it was taken; while `unstick_until` is in the future the
    // bot backs out along the freest direction instead of following its plan
    stuck_check: ([f32; 2], u32),
    unstick_until: u32,
    unstick_dir: V3,
}

impl BotMem {
    pub fn new(id: u16, archetype: Archetype, skill: SkillParams, seed: u64) -> Self {
        Self {
            id,
            archetype,
            skill,
            base_skill: skill,
            seed,
            phase: Phase::Dead,
            phase_since: 0,
            trav: Traverse::Free,
            stats: BotStats::default(),
            fire_ok: 0,
            slice_ok: 0,
            evade_ok: 0,
            retreat_block: 0,
            strafe_sign: if id % 2 == 0 { 1.0 } else { -1.0 },
            strafe_until: 0,
            aim_noise: (0.0, 0.0),
            threat_since: None,
            threat_react: None,
            want_since: None,
            want_last: 0,
            perch: None,
            perch_since: 0,
            was_wallrun: false,
            top_ticks: 0,
            path: Vec::new(),
            path_goal: [0.0; 2],
            path_at: 0,
            unreachable: Vec::new(),
            stuck_check: ([0.0; 2], 0),
            unstick_until: 0,
            unstick_dir: Vector::zeros(),
        }
    }

    fn set_phase(&mut self, p: Phase, tick: u32) {
        if self.phase != p {
            self.phase = p;
            self.phase_since = tick;
        }
    }

    // ------------------------------------------------------------------ perception

    pub fn perceive(&self, ctx: &ThinkCtx) -> Perception {
        let me = ctx.me;
        let target = ctx.assign.target.and_then(|id| ctx.world.player(id)).filter(|t| t.alive).map(|t| {
            let dist = (t.pos - me.pos).norm();
            let los = dist < 80.0 && ctx.world.los(me.eye(), t.eye());
            let angles = ctx.frustum.angles(t.eye(), t.yaw, t.pitch, me.pos);
            let on_screen = los && ctx.frustum.contains(t.eye(), t.yaw, t.pitch, me.pos, 0.0);
            let near_screen = los && ctx.frustum.contains(t.eye(), t.yaw, t.pitch, me.pos, THREAT_MARGIN);
            TargetInfo { view: *t, dist, los, angles, on_screen, near_screen }
        });
        Perception { target }
    }

    fn strike_ready(&self, ctx: &ThinkCtx) -> bool {
        ctx.me.dash_ready && ctx.world.tick >= self.slice_ok
    }

    /// Pass 1 (before the director settles tickets): does this bot want to launch a slice it is not allowed to
    /// start yet because it is off-screen? Mutates only the wait-time bookkeeping.
    pub fn strike_request(&mut self, ctx: &ThinkCtx, p: &Perception) -> Option<Request> {
        let tick = ctx.world.tick;
        let wish = ctx.me.alive
            && matches!(self.phase, Phase::Advance | Phase::Flank | Phase::Zone | Phase::Perch)
            && p.target.map_or(false, |t| t.needs_ticket() && t.dist <= REQUEST_RANGE && !t.view.spawn_protected)
            && self.strike_ready(ctx);
        if wish {
            self.want_since.get_or_insert(tick);
            self.want_last = tick;
        } else if tick > self.want_last + 12 {
            self.want_since = None;
        }
        let t = p.target?;
        wish.then(|| Request { bot: self.id, target: t.view.id, since: self.want_since.unwrap_or(tick), dist: t.dist })
    }

    // ------------------------------------------------------------------ think

    pub fn think(&mut self, ctx: &ThinkCtx, p: &Perception) -> ThinkOut {
        let (tick, me, world) = (ctx.world.tick, ctx.me, ctx.world);
        let mut out = ThinkOut::default();
        out.intent.tick = tick;

        if !me.alive {
            if self.phase != Phase::Dead {
                out.release_ticket = true;
            }
            self.phase = Phase::Dead;
            self.trav = Traverse::Free;
            self.threat_since = None;
            self.threat_react = None;
            self.perch = None;
            // the server still clamps aim slew while dead: hold the current aim, never snap to 0
            out.intent.aim_yaw = q_yaw(me.yaw);
            out.intent.aim_pitch = q_pitch(me.pitch);
            return out;
        }
        if self.phase == Phase::Dead {
            self.set_phase(Phase::Advance, tick);
        }

        let mut rng = Rng::new(self.seed, self.id, tick, 1);
        if tick % 8 == (self.id as u32) % 8 {
            let e = self.skill.aim_error;
            self.aim_noise = (rng.signed() * e, rng.signed() * e * 0.6);
        }
        if tick >= self.strafe_until {
            self.strafe_sign = -self.strafe_sign;
            self.strafe_until = tick + 40 + (rng.unit() * 50.0) as u32;
        }

        // ---- 1. threat tracking: am I inside the human's view long enough to react?
        let threatened = p.target.map_or(false, |t| !t.view.is_bot && t.near_screen && t.dist > 2.5);
        if threatened {
            if self.threat_since.is_none() {
                self.threat_since = Some(tick);
                self.threat_react = None;
            }
            let since = self.threat_since.unwrap();
            if self.threat_react.is_none() && tick >= since + self.skill.reaction_ticks {
                let mut chance = self.skill.evade_chance;
                if self.archetype == Archetype::Heavy {
                    chance *= 0.35; // heavies are the decoy: they hold the lane and soak the view
                }
                self.threat_react = Some(rng.chance(chance));
            }
        } else {
            self.threat_since = None;
            self.threat_react = None;
        }

        // ---- 2. phase selection
        let mut released = false;
        self.select_phase(ctx, p, &mut rng, &mut released);
        out.release_ticket |= released;

        // ---- 3. tactical movement + buttons
        let mut buttons = 0u8;
        let (mut dir, mut mag) = self.tactical_move(ctx, p, &mut buttons);

        // ---- 4. traversal: sweep walls / hop / hold. May rewrite dir, mag and buttons.
        let (d2, m2) = self.traverse(ctx, p, dir, mag, &mut buttons);
        dir = d2;
        mag = m2;

        // ---- 5. attacks (never from inside a hop or hold that would be disturbed)
        self.attacks(ctx, p, dir, &mut buttons, &mut rng);

        // ---- 6. aim
        let (yaw, pitch) = self.aim(ctx, p, dir);

        // wall-run bookkeeping (stats only)
        let wr = matches!(me.state, shinobi_sim::character::MoveState::WallRun { .. });
        if wr && !self.was_wallrun {
            self.stats.wall_runs += 1;
        }
        self.was_wallrun = wr;

        let (sx, sz) = stick_for(dir, mag * self.skill.move_scale, yaw);
        out.intent = PlayerIntent {
            tick,
            buttons: Buttons(buttons),
            move_x: q_axis(sx),
            move_z: q_axis(sz),
            aim_yaw: q_yaw(yaw),
            aim_pitch: q_pitch(pitch),
            ultimate: false,
            build: false, build_wall: false, build_ramp: false,
            zip: false,
        };
        // leaving Strike hands the ticket back
        if released {
            out.release_ticket = true;
        }
        let _ = world;
        out
    }

    // ------------------------------------------------------------------ phase selection

    fn select_phase(&mut self, ctx: &ThinkCtx, p: &Perception, rng: &mut Rng, released: &mut bool) {
        let (tick, me) = (ctx.world.tick, ctx.me);
        let Some(t) = p.target else {
            // nobody to fight: hold position in Zone (the host keeps assigning targets as players spawn)
            if let Phase::Strike { .. } = self.phase {
                *released = true;
            }
            self.set_phase(Phase::Zone, tick);
            return;
        };

        // ---- committed phases run to completion
        match self.phase {
            Phase::Strike { stage } => {
                let since = self.phase_since;
                match stage {
                    StrikeStage::WindUp => {
                        let lost = !t.los || t.dist > STRIKE_RANGE * 1.8 || t.view.spawn_protected;
                        if lost {
                            self.slice_ok = tick + 30;
                            self.set_phase(Phase::Zone, tick);
                            *released = true;
                        } else if tick >= since + self.skill.strike_windup && t.dist <= STRIKE_RANGE && me.dash_ready {
                            self.set_phase(Phase::Strike { stage: StrikeStage::Slice }, tick);
                        }
                    }
                    StrikeStage::Slice => {
                        self.set_phase(Phase::Strike { stage: StrikeStage::Recover }, tick);
                    }
                    StrikeStage::Recover => {
                        if tick >= since + 14 {
                            self.slice_ok = tick + 36;
                            self.set_phase(Phase::Zone, tick);
                            *released = true;
                        }
                    }
                }
                return;
            }
            Phase::Evade { until, .. } if tick < until => return,
            _ => {}
        }

        // ---- retreat (hysteresis: enter below 26 m, leave beyond 32 m or after 4 s; one threshold dithers in place)
        let low = me.hp_frac < 0.3 && tick >= self.retreat_block;
        if self.phase == Phase::Retreat {
            if !low || tick > self.phase_since + 240 || t.dist > 32.0 {
                self.retreat_block = tick + 600;
                self.set_phase(Phase::Advance, tick);
            } else {
                return;
            }
        } else if low && t.dist < 26.0 {
            self.set_phase(Phase::Retreat, tick);
            return;
        }

        // ---- evade: the human can see me and I decided to react
        if self.threat_react == Some(true) && tick >= self.evade_ok && !self.phase.committed(tick) {
            self.start_evade(ctx, t, rng);
            return;
        }

        // ---- strike
        if t.dist <= STRIKE_RANGE && t.los && !t.view.spawn_protected && self.strike_ready(ctx) {
            if !t.needs_ticket() || ctx.has_ticket {
                self.stats.strikes += 1;
                self.set_phase(Phase::Strike { stage: StrikeStage::WindUp }, tick);
                return;
            }
            self.stats.ticketless_waits += 1; // in range, off-screen, no ticket: keep zoning
        }

        // ---- role movement
        // hysteresis: enter Zone inside 0.7 R, only leave beyond 1.0 R (a single threshold dithers in place)
        let zr = self.archetype.zone_radius();
        let limit = if self.phase == Phase::Zone { zr } else { zr * 0.7 };
        let near_with_no_license = t.needs_ticket() && !ctx.has_ticket && t.dist < limit;
        let next = match self.archetype {
            Archetype::Heavy => {
                if near_with_no_license { Phase::Zone } else { Phase::Advance }
            }
            Archetype::Light => {
                if ctx.has_ticket && t.needs_ticket() {
                    Phase::Advance // ticket in hand: close the distance from the blind side
                } else if self.skill.use_walls && self.want_perch(ctx, t) {
                    Phase::Perch
                } else if t.dist > self.archetype.zone_radius() * 0.8 || self.phase == Phase::Flank && t.dist > 6.0 {
                    Phase::Flank
                } else {
                    Phase::Zone
                }
            }
        };
        let next = if next == Phase::Perch && self.perch.is_none() { Phase::Flank } else { next };
        self.set_phase(next, tick);
    }

    fn want_perch(&mut self, ctx: &ThinkCtx, t: TargetInfo) -> bool {
        let tick = ctx.world.tick;
        if let Some(i) = self.perch {
            // hold the high ground for a while, then reposition
            let on_top = ctx.perches.get(i).map_or(false, |pr| pr.contains_xz(ctx.me.pos, -0.2) && ctx.me.grounded());
            self.top_ticks = if on_top { self.top_ticks + 1 } else { 0 };
            if self.top_ticks > 240 {
                self.perch = None;
                self.perch_since = tick;
                self.top_ticks = 0;
                return false;
            }
            // keep a perch for a while, give it up when the target closes in or it times out
            if t.dist < 6.0 || tick > self.perch_since + 420 {
                self.perch = None;
                self.perch_since = tick; // cool down before choosing again
                return false;
            }
            return i < ctx.perches.len();
        }
        if tick < self.perch_since + 120 || ctx.perches.is_empty() || t.dist < 9.0 {
            return false;
        }
        // nearest perch (to me) that is 7..26 m from the target and has a shot at it
        let mut best: Option<(f32, usize)> = None;
        for (i, pr) in ctx.perches.iter().enumerate() {
            let d_t = (flat(pr.top_center) - flat(t.view.pos)).norm();
            if !(7.0..=26.0).contains(&d_t) {
                continue;
            }
            let top = pr.top_center + vector![0.0, 1.4, 0.0];
            if !ctx.world.los(top, t.view.eye()) {
                continue;
            }
            let d_me = (flat(pr.top_center) - flat(ctx.me.pos)).norm();
            if d_me > 28.0 {
                continue;
            }
            // rank offset spreads squad-mates over different perches deterministically
            let score = d_me + ((i + self.id as usize) % 3) as f32 * 2.5;
            if best.map_or(true, |b| score < b.0) {
                best = Some((score, i));
            }
        }
        if let Some((_, i)) = best {
            self.perch = Some(i);
            self.perch_since = tick;
            true
        } else {
            self.perch_since = tick;
            false
        }
    }

    fn start_evade(&mut self, ctx: &ThinkCtx, t: TargetInfo, rng: &mut Rng) {
        let (tick, me, world) = (ctx.world.tick, ctx.me, ctx.world);
        // leave the frustum through its nearest edge: the side the bot is already on
        let side = if t.angles.h >= 0.0 { 1.0 } else { -1.0 };
        let away = unit_or_zero(flat(me.pos - t.view.pos));
        let mut dir = unit_or_zero(right_of_yaw(t.view.yaw) * side + away * 0.35);
        if dir == Vector::zeros() {
            dir = away;
        }
        let kind = if self.archetype == Archetype::Light && self.skill.use_walls && wall_side(world, me, dir).is_some() {
            EvadeKind::WallCling
        } else if self.archetype == Archetype::Light && me.dash_ready {
            EvadeKind::Dash
        } else if me.grounded() {
            EvadeKind::Roll
        } else if me.dash_ready {
            EvadeKind::Dash
        } else {
            EvadeKind::Strafe
        };
        let _ = rng;
        self.stats.evades += 1;
        self.evade_ok = tick + EVADE_TICKS + EVADE_COOLDOWN;
        self.threat_react = None;
        self.set_phase(Phase::Evade { kind, dir, until: tick + EVADE_TICKS }, tick);
    }

    // ------------------------------------------------------------------ tactical movement

    fn slot_point(&self, ctx: &ThinkCtx, t: &TargetInfo, radius: f32) -> V3 {
        t.view.pos + dir_from_yaw(t.view.yaw + ctx.assign.slot_angle) * radius
    }

    fn ring_move(&self, t: &TargetInfo, ctx: &ThinkCtx, radius: f32) -> V3 {
        let radial = unit_or_zero(flat(ctx.me.pos - t.view.pos));
        let err = flat(ctx.me.pos - t.view.pos).norm() - radius;
        let tangent = vector![-radial.z, 0.0, radial.x] * self.strafe_sign;
        tangent * 0.9 - radial * (err * 0.15).clamp(-0.8, 0.8)
    }

    fn tactical_move(&mut self, ctx: &ThinkCtx, p: &Perception, buttons: &mut u8) -> (V3, f32) {
        let (tick, me, world) = (ctx.world.tick, ctx.me, ctx.world);
        let Some(t) = p.target else {
            // nobody to fight: patrol. Standing still is lethal under Tron rules (own newest segment arms in 15 ticks).
            let mut r = Rng::new(self.seed, self.id, tick / 90, 7);
            let heading = r.unit() * std::f32::consts::TAU;
            return (dir_from_yaw(heading), 0.7);
        };
        let to_t = unit_or_zero(flat(t.view.pos - me.pos));
        match self.phase {
            Phase::Dead => (Vector::zeros(), 0.0),
            Phase::Advance => {
                let dest = if ctx.has_ticket || t.dist < 9.0 {
                    t.view.pos
                } else {
                    self.slot_point(ctx, &t, if self.archetype == Archetype::Heavy { 5.0 } else { 7.0 })
                };
                (unit_or_zero(flat(dest - me.pos)), 1.0)
            }
            Phase::Flank => {
                let slot = self.slot_point(ctx, &t, self.archetype.zone_radius());
                let to_slot = flat(slot - me.pos);
                let d = if to_slot.norm() < 2.5 { self.ring_move(&t, ctx, self.archetype.zone_radius()) } else { to_slot };
                let dir = unit_or_zero(d);
                self.wall_run_assist(ctx, dir, buttons);
                (dir, 1.0)
            }
            Phase::Zone => (unit_or_zero(self.ring_move(&t, ctx, self.archetype.zone_radius())), 0.9),
            Phase::Perch => {
                let Some(pr) = self.perch.and_then(|i| ctx.perches.get(i)) else { return (to_t, 1.0) };
                let on_top = pr.contains_xz(me.pos, -0.2) && (me.pos.y - 0.8 - pr.top_center.y).abs() < 0.3;
                if on_top {
                    let d = unit_or_zero(self.ring_move(&t, ctx, (t.dist).max(6.0))) * 0.0; // hold the high ground
                    (d, 0.0)
                } else {
                    let to = flat(pr.top_center - me.pos);
                    (unit_or_zero(to), 1.0)
                }
            }
            Phase::Strike { stage } => match stage {
                StrikeStage::WindUp => (to_t, 1.0),
                StrikeStage::Slice => {
                    *buttons |= Buttons::SLICE;
                    (to_t, 1.0)
                }
                StrikeStage::Recover => {
                    let side = vector![-to_t.z, 0.0, to_t.x] * self.strafe_sign;
                    (unit_or_zero(side - to_t * 0.4), 1.0)
                }
            },
            Phase::Evade { kind, dir, until } => {
                let first = tick == self.phase_since;
                match kind {
                    EvadeKind::Dash => {
                        if first {
                            *buttons |= Buttons::DASH;
                        }
                    }
                    EvadeKind::Roll => {
                        if first {
                            *buttons |= Buttons::ROLL;
                        }
                    }
                    EvadeKind::WallCling => {
                        // wall-run needs to be airborne with cling held, moving along the wall
                        if me.grounded() && first {
                            *buttons |= Buttons::JUMP_PRESSED;
                        }
                        if !me.grounded() {
                            *buttons |= Buttons::WALL_CLING;
                        }
                    }
                    EvadeKind::Strafe => {}
                }
                let _ = until;
                (dir, 1.0)
            }
            Phase::Retreat => {
                let away = -to_t;
                let side = vector![-to_t.z, 0.0, to_t.x] * self.strafe_sign;
                (unit_or_zero(away + side * 0.5), 1.0)
            }
        }
    }

    /// Light bots run along level walls when one is within reach: jump, then hold cling while airborne.
    fn wall_run_assist(&self, ctx: &ThinkCtx, dir: V3, buttons: &mut u8) {
        if !self.skill.use_walls || self.archetype != Archetype::Light {
            return;
        }
        let (me, world) = (ctx.me, ctx.world);
        if wall_side(world, me, dir).is_none() {
            return;
        }
        if me.grounded() {
            if world.tick % 45 == (self.id as u32) % 45 {
                *buttons |= Buttons::JUMP_PRESSED;
            }
        } else if me.flat_speed() >= world.cfg.wall_min_speed {
            *buttons |= Buttons::WALL_CLING;
        }
    }

    // ------------------------------------------------------------------ traversal (hops + steering)

    /// Legs: finish a ledge hop in progress, start one when walking at a perch, otherwise steer around level
    /// geometry with whiskers.
    fn traverse(&mut self, ctx: &ThinkCtx, p: &Perception, dir: V3, mag: f32, buttons: &mut u8) -> (V3, f32) {
        let (tick, me, world) = (ctx.world.tick, ctx.me, ctx.world);
        let cfg = world.cfg;

        // ---- already hopping: the hop owns the legs
        if let Traverse::Hop { kind, since, dir: hdir, start, mut double_sent } = self.trav {
            let elapsed = tick.saturating_sub(since);
            let airborne = !me.grounded();
            if elapsed > 160 || (elapsed >= 3 && !airborne) {
                self.trav = Traverse::Free;
            } else {
                if !double_sent && elapsed >= 2 && airborne && me.vel.y.abs() <= cfg.peak_window - 0.05 {
                    *buttons |= Buttons::JUMP_PRESSED; // edge only
                    double_sent = true;
                    self.trav = Traverse::Hop { kind, since, dir: hdir, start, double_sent };
                }
                *buttons &= !(Buttons::DASH | Buttons::ROLL | Buttons::WALL_CLING | Buttons::SLICE);
                return (hdir, 1.0);
            }
        }

        // ---- ledge hop (perch): only when the tactical layer is walking at the box
        if self.phase == Phase::Perch && me.grounded() && mag > 0.1 {
            if let Some(pr) = self.perch.and_then(|i| ctx.perches.get(i)) {
                let feet = me.pos.y - cfg.half_height - cfg.radius;
                let rise = pr.top_center.y - feet;
                if pr.edge_dist(me.pos) <= 2.2 && !pr.contains_xz(me.pos, 0.0) && (0.4..2.55).contains(&rise) {
                    let hdir = unit_or_zero(flat(pr.top_center - me.pos));
                    self.stats.ledge_hops += 1;
                    *buttons |= Buttons::JUMP_PRESSED;
                    self.trav = Traverse::Hop {
                        kind: HopKind::Ledge { top: pr.top_center.y },
                        since: tick,
                        dir: hdir,
                        start: me.pos,
                        double_sent: false,
                    };
                    return (hdir, 1.0);
                }
            }
        }

        if mag < 0.05 {
            self.stuck_check = ([me.pos.x, me.pos.z], tick);
            return (dir, mag);
        }
        // wedged on geometry the plan cannot see (a body pinned against a wall corner): same place for 1.5 s while
        // trying to move -> back out along the freest direction for a moment, then plan again
        if tick < self.unstick_until {
            return (self.unstick_dir, 1.0);
        }
        if tick.wrapping_sub(self.stuck_check.1) >= 90 {
            let moved = (me.pos.x - self.stuck_check.0[0]).hypot(me.pos.z - self.stuck_check.0[1]);
            self.stuck_check = ([me.pos.x, me.pos.z], tick);
            if moved < 0.4 && me.grounded() && !matches!(self.phase, Phase::Strike { .. }) {
                let knee = vector![me.pos.x, me.pos.y - 0.3, me.pos.z];
                let mut best = (-1.0f32, vector![1.0, 0.0, 0.0]);
                for k in 0..8 {
                    let a = k as f32 * std::f32::consts::FRAC_PI_4 + (tick % 7) as f32 * 0.1;
                    let d = vector![a.cos(), 0.0, a.sin()];
                    let room = world.ray(knee, d, 4.0).unwrap_or(4.0);
                    if room > best.0 {
                        best = (room, d);
                    }
                }
                self.unstick_dir = best.1;
                self.unstick_until = tick + 40;
                self.path.clear();
                self.stats.unsticks += 1;
                return (self.unstick_dir, 1.0);
            }
        }
        // approaching a target that is out of sight, or in sight but not walkable to in a straight line (ledges, low
        // walls, void gaps): follow a planned path instead of walking into the first wall
        // (alcoves with baffled doors, big maps). Retreat / evade / strike keep their own direction.
        let planned = match (ctx.nav, p.target) {
            (Some(nav), Some(t))
                if t.dist > 5.0
                    && matches!(self.phase, Phase::Advance | Phase::Flank | Phase::Zone)
                    && (!t.los || !nav.straight_walkable([me.pos.x, me.pos.z], [t.view.pos.x, t.view.pos.z])) =>
            {
                self.path_dir(world, nav, me.pos, t.view.pos, tick, t.view.id, ctx.plan_budget)
            }
            _ => {
                self.path.clear();
                None
            }
        };
        if let Some(d) = planned {
            // the plan already goes around static geometry (whiskers would shy away from the walls of a narrow door)
            return (d, mag);
        }
        (steer_static(world, me, dir), mag)
    }

    /// Direction along a path to `goal`, replanned when the goal moved to another cell, the plan is old, or the bot
    /// strayed from it. Waypoints are consumed as the bot reaches them.
    fn path_dir(&mut self, world: &WorldView, nav: &crate::nav::NavGrid, from: V3, goal: V3, tick: u32, target: u16, budget: &std::cell::Cell<u32>) -> Option<V3> {
        const REPLAN_TICKS: u32 = 60;
        /// Search cap per plan: a far goal returns the best partial route instead of flooding the map.
        const PLAN_NODES: usize = 12_000;
        const REACH: f32 = 0.8; // a baffled door is ~1 m wide: pass through the corner waypoint, do not cut it
        let goal2 = [goal.x, goal.z];
        let moved = (self.path_goal[0] - goal2[0]).hypot(self.path_goal[1] - goal2[1]) > 6.0;
        if (self.path.is_empty() || moved || tick.wrapping_sub(self.path_at) >= REPLAN_TICKS) && budget.get() > 0 {
            budget.set(budget.get() - 1);
            self.path_at = tick;
            self.path_goal = goal2;
            self.path = match nav.path_search([from.x, from.z], goal2, PLAN_NODES) {
                Some(r) => {
                    // search exhausted (not just capped) and it still ends far from them: no walkable way, so let
                    // the director pick someone else for a while; the partial route still gets the bot closer
                    if !r.complete && r.expanded < PLAN_NODES && r.end_gap > 8.0 {
                        self.unreachable.retain(|(_, until)| *until > tick);
                        self.unreachable.push((target, tick + 360));
                    }
                    r.waypoints
                }
                None => {
                    self.unreachable.retain(|(_, until)| *until > tick);
                    self.unreachable.push((target, tick + 360));
                    Vec::new()
                }
            };
            self.path.reverse(); // pop from the back
        }
        // a waypoint is passed when reached, or when close AND the way to the next one is open for a body (centre
        // and both shoulders): popping a corner early is how bots cut it and pin themselves on the wall's end
        let knee = vector![from.x, from.y - 0.3, from.z];
        while self.path.len() > 1 {
            let w = self.path[self.path.len() - 1];
            let dist = (w[0] - from.x).hypot(w[1] - from.z);
            let open = || {
                let n = self.path[self.path.len() - 2];
                let d = vector![n[0] - from.x, 0.0, n[1] - from.z];
                let len = d.norm();
                if len < 0.1 {
                    return true;
                }
                let u = d / len;
                let side = vector![-u.z, 0.0, u.x] * 0.4;
                let reach = len.min(3.0);
                [Vector::zeros(), side, -side].iter().all(|o| world.ray(knee + o, u, reach).is_none())
            };
            if dist < 0.35 || (dist < REACH && open()) {
                self.path.pop();
            } else {
                break;
            }
        }
        let w = self.path.last()?;
        let d = vector![w[0] - from.x, 0.0, w[1] - from.z];
        (d.norm() > 0.05).then(|| d / d.norm())
    }

    // ------------------------------------------------------------------ attacks & aim

    fn attacks(&mut self, ctx: &ThinkCtx, p: &Perception, dir: V3, buttons: &mut u8, rng: &mut Rng) {
        let (tick, me) = (ctx.world.tick, ctx.me);
        let Some(t) = p.target else { return };
        let can_throw = matches!(
            self.phase,
            Phase::Advance | Phase::Flank | Phase::Zone | Phase::Perch | Phase::Retreat
        ) || matches!(self.phase, Phase::Evade { kind: EvadeKind::WallCling | EvadeKind::Strafe, .. });
        if !can_throw || !t.los || t.dist > SHURIKEN_RANGE || tick < self.fire_ok || t.view.spawn_protected {
            return;
        }
        let (want_yaw, _) = self.aim_angles(ctx, &t);
        let err = wrap_pi(want_yaw - me.yaw).abs();
        if err < 0.10 + self.skill.aim_error {
            *buttons |= Buttons::FIRE;
            self.stats.shurikens += 1;
            self.fire_ok = tick + self.skill.fire_gap_ticks + (rng.unit() * 5.0) as u32;
        }
        let _ = dir;
    }

    fn aim_angles(&self, ctx: &ThinkCtx, t: &TargetInfo) -> (f32, f32) {
        let me = ctx.me;
        // lead the shot a little (Easy bots do not lead)
        // projectile speed comes from the live balance, so retuning shurikens retunes the bots' lead
        let speed = ctx.world.server.balance.combat.shuriken.speed.max(1.0);
        let lead_t = if self.skill.aim_error < 0.1 { (t.dist / speed).min(0.6) } else { 0.0 };
        let aim_pt = t.view.pos + t.view.vel * lead_t;
        let d = aim_pt - me.eye();
        let h = (d.x * d.x + d.z * d.z).sqrt();
        let yaw = yaw_of(vector![d.x, 0.0, d.z]) + self.aim_noise.0;
        let pitch = d.y.atan2(h.max(1e-3)) + self.aim_noise.1;
        (yaw, pitch.clamp(-1.2, 1.2))
    }

    fn aim(&mut self, ctx: &ThinkCtx, p: &Perception, dir: V3) -> (f32, f32) {
        let me = ctx.me;
        let (want_yaw, want_pitch) = match (p.target, self.phase) {
            // strikes lock on cleanly: the lunge goes where the aim points
            (Some(t), Phase::Strike { .. }) => {
                let d = flat(t.view.pos - me.pos);
                (yaw_of(d), 0.0)
            }
            (Some(t), _) => self.aim_angles(ctx, &t),
            (None, _) => {
                if flat(dir).norm() > 0.1 { (yaw_of(dir), 0.0) } else { (me.yaw, 0.0) }
            }
        };
        let cap = self.skill.turn_per_tick.min(FRAC_PI_2 * 0.5);
        let dy = wrap_pi(want_yaw - me.yaw).clamp(-cap, cap);
        let dp = (want_pitch - me.pitch).clamp(-cap, cap);
        (me.yaw + dy, (me.pitch + dp).clamp(-PI * 0.45, PI * 0.45))
    }
}

/// Short helper used by tests and the host.
pub fn describe(m: &BotMem) -> String {
    format!("bot {} {:?} {} trav={:?}", m.id, m.archetype, m.phase.name(), m.trav)
}


