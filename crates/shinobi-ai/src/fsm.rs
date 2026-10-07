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
/// Bots hook a visible target between these distances now and then (per-tick chance), then swing when close: a
/// hook-strike. The katana reach while hooked / wall-running is `balance.combat.katana.{hook,wall}_reach` + the speed bonus.
const HOOK_MIN: f32 = 16.0;
const HOOK_MAX: f32 = 90.0;
const HOOK_CHANCE: f32 = 0.012;
const HOOK_COOLDOWN_TICKS: u32 = 420;
/// A target this close after a cut gets the next cut of the combo (blade reach 2.4 m + a little).
const COMBO_REACH: f32 = 2.9;
/// Katana moves: the share of combo openers that turn into a heavy cut (hold ~0.83 s, then release), the chance to
/// cut back inside a counter window, the chance and range of a whirl cut right after the shield comes down.
const HEAVY_CHANCE: f32 = 0.2;
const HEAVY_HOLD_TICKS: u32 = 50;
const HEAVY_GAP_TICKS: u32 = 600;
const COUNTER_CHANCE: f32 = 0.5;
const WHIRL_CHANCE: f32 = 0.4;
const WHIRL_RANGE: f32 = 3.0;
/// No shuriken inside this range: point-blank is for the katana.
const POKE_MIN_RANGE: f32 = 3.0;
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
    /// Times the void-edge guard stopped or bent a move, dash or lunge (vertical maps only).
    pub edge_stops: u32,
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
    /// Next tick this bot may try a long hook at its target (hook-strike opportunism, `HOOK_*`).
    hook_ok: u32,
    /// Throwing pause: movement is nearly stopped until this tick.
    pause_until: u32,
    /// Yaw offset (rad) of the dash being wound up: 0 = a clean read, else a deliberate whiff (`skill.slice_miss`).
    slice_off: f32,
    /// Cuts landed so far in the current combo (0..=2) and the shield hold / re-arm ticks.
    combo: u32,
    block_until: u32,
    block_ok: u32,
    /// Heavy cut: hold the katana button until this tick, then release (0 = not charging).
    heavy_until: u32,
    /// Next tick a heavy cut may be started (a charge is a standstill: keep the bots moving).
    heavy_ok: u32,
    /// The shield was up last tick (a whirl cut follows its coming down).
    was_blocking: bool,
    /// Last tick a counter cut was tried (one try per parry window).
    counter_tried: u32,
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
    /// Star variants (docs/stars.md): which throwable this tick's FIRE sends (0 = standard), when each kind may be
    /// thrown again, and the window in which a hurt bot looks down to drop smoke at its own feet.
    star_pick: u8,
    mine_ok: u32,
    flash_ok: u32,
    smoke_ok: u32,
    smoke_aim_until: u32,
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
            hook_ok: 0,
            pause_until: 0,
            slice_off: 0.0,
            combo: 0,
            block_until: 0,
            block_ok: 0,
            heavy_until: 0,
            heavy_ok: 0,
            was_blocking: false,
            counter_tried: 0,
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
            star_pick: 0,
            mine_ok: 0,
            flash_ok: 0,
            smoke_ok: 0,
            smoke_aim_until: 0,
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
            // readable movement: hold a strafe direction for 0.8-1.5 s
            self.strafe_until = tick + 48 + (rng.unit() * 42.0) as u32;
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
        self.star_pick = 0;
        self.star_variants(ctx, p, &mut buttons, &mut rng);
        self.attacks(ctx, p, dir, &mut buttons, &mut rng);

        // ---- 5b. guard: a star flying at me gets a short raised shield now and then (never mid-strike)
        if tick >= self.block_ok && incoming_disc(ctx) && !matches!(self.phase, Phase::Strike { .. } | Phase::Dead) {
            self.block_ok = tick + 150;
            if rng.chance(self.skill.evade_chance * 0.3) {
                self.block_until = tick + 14;
            }
        }
        // 5c. katana moves (docs/melee.md): a parry opens the counter window (drop the shield and cut), and a cut right
        // after the shield comes down is a whirl when someone is close
        let counter_open = ctx
            .world
            .server
            .players
            .iter()
            .find(|q| q.id == self.id)
            .and_then(|q| q.kmoves.counter_at(tick))
            .is_some();
        if counter_open && tick > self.counter_tried + 20 && self.skill.move_scale > 0.0 && rng.chance(COUNTER_CHANCE) {
            self.counter_tried = tick;
            self.block_until = tick;
            self.heavy_until = 0;
            buttons |= Buttons::SLICE;
            self.stats.strikes += 1;
        }
        let blocking = tick < self.block_until && !matches!(self.phase, Phase::Strike { .. });
        if blocking {
            buttons &= !(Buttons::FIRE | Buttons::SLICE);
        } else if self.was_blocking && !matches!(self.phase, Phase::Strike { .. }) && p.target.map_or(false, |t| t.dist <= WHIRL_RANGE && !t.view.spawn_protected) && rng.chance(WHIRL_CHANCE) {
            buttons |= Buttons::SLICE;
            self.stats.strikes += 1;
        }
        self.was_blocking = blocking;

        // ---- 6. aim
        let (yaw, pitch) = self.aim(ctx, p, dir);

        // ---- 6b. void-edge guard (vertical maps: stepping, dashing or lunging off a deck is death)
        if let Some(nav) = ctx.nav.filter(|n| n.is_sky()) {
            let to_target = p.target.map(|t| (unit_or_zero(flat(t.view.pos - ctx.me.pos)), t.dist));
            let (d3, m3) = self.edge_guard(nav, ctx, dir, mag, yaw, to_target, &mut buttons);
            dir = d3;
            mag = m3;
        }

        // ---- 7. long hook at a visible target now and then (normal / hard bots; the strike follows in select_phase)
        let mut zip = false;
        if let Some(t) = p.target {
            // floating maps: a hook or hook-strike launches a bot across the void; the edge guard cannot cover it, so bots take the bridges there
            if self.skill.use_walls
                && !ctx.nav.map_or(false, |n| n.is_sky())
                && me.hook_ready
                && t.los
                && !t.view.spawn_protected
                && (HOOK_MIN..=HOOK_MAX).contains(&t.dist)
                && tick >= self.hook_ok
                && matches!(self.phase, Phase::Advance | Phase::Flank | Phase::Zone)
                && rng.chance(HOOK_CHANCE)
            {
                zip = true;
                self.hook_ok = tick + HOOK_COOLDOWN_TICKS;
            }
        }

        // wall-run bookkeeping (stats only)
        let wr = matches!(me.state, shinobi_sim::character::MoveState::WallRun { .. });
        if wr && !self.was_wallrun {
            self.stats.wall_runs += 1;
        }
        self.was_wallrun = wr;

        // fighting pace ~80% of a full run, and a throw is made standing (nearly) still so shots read as aimed
        let fighting = p.target.map_or(false, |t| t.los && t.dist < 40.0);
        let mut pace = if fighting { 0.8 } else { 1.0 };
        if buttons & Buttons::FIRE != 0 {
            self.pause_until = tick + 9;
        }
        if tick < self.pause_until && !matches!(self.phase, Phase::Strike { .. } | Phase::Evade { .. }) && matches!(self.trav, Traverse::Free) {
            pace *= 0.1;
        }
        let (sx, sz) = stick_for(dir, mag * self.skill.move_scale * pace, yaw);
        out.intent = PlayerIntent {
            tick,
            buttons: Buttons(buttons),
            move_x: q_axis(sx),
            move_z: q_axis(sz),
            aim_yaw: q_yaw(yaw),
            aim_pitch: q_pitch(pitch),
            ultimate: false,
            build: false, build_wall: false, build_ramp: false,
            zip,
            block: blocking,
            mark: false,
            star: if buttons & Buttons::FIRE != 0 { self.star_pick } else { 0 },
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
                        // melee-first: a target still inside blade reach gets the next cut of the combo as soon as the
                        // swing cooldown (~0.42 s = 25 ticks) is over, up to three cuts; otherwise back out
                        // (an off-screen bot only keeps cutting while its attack ticket is still valid: the ticket cap guards the human)
                        let in_reach = t.dist <= COMBO_REACH && t.los && !t.view.spawn_protected && (ctx.has_ticket || !t.needs_ticket());
                        if self.heavy_until > 0 {
                            // heavy cut: keep the button down while the target stays in reach, then let go (the release cuts)
                            if tick >= self.heavy_until || !in_reach {
                                self.heavy_until = 0;
                                self.slice_ok = tick + 36;
                                self.set_phase(Phase::Strike { stage: StrikeStage::Recover }, tick);
                                self.combo = 2;
                            }
                        } else if in_reach && self.combo < 2 && tick == since + 4 && tick >= self.heavy_ok && rng.chance(HEAVY_CHANCE) {
                            self.heavy_until = tick + HEAVY_HOLD_TICKS;
                            self.heavy_ok = tick + HEAVY_GAP_TICKS;
                        } else if in_reach && self.combo < 2 {
                            if tick >= since + 26 {
                                self.combo += 1;
                                self.stats.strikes += 1;
                                self.set_phase(Phase::Strike { stage: StrikeStage::Slice }, tick);
                            }
                        } else if tick >= since + 14 {
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

        // ---- hook-strike / wall slash: hooked or wall-running, swing as soon as the target is inside the longer reach
        let on_wall = matches!(me.state, shinobi_sim::character::MoveState::WallRun { .. });
        if (me.hooked || on_wall) && t.los && !t.view.spawn_protected && tick >= self.slice_ok {
            let kb = &ctx.world.server.balance.combat.katana;
            let reach = STRIKE_RANGE + if me.hooked { kb.hook_reach } else { kb.wall_reach } * 0.7 + me.flat_speed() * 0.12;
            if t.dist <= reach {
                self.stats.strikes += 1;
                self.slice_off = 0.0;
                self.set_phase(Phase::Strike { stage: StrikeStage::Slice }, tick);
                return;
            }
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
                // a bad read is decided up front so the aim has the whole wind-up to slew onto the wrong spot
                self.slice_off = if rng.chance(self.skill.slice_miss) { 1.2 * rng.signed().signum() } else { 0.0 };
                self.combo = 0;
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
        // dash / roll are for dodging a thrown star; being looked at is answered by walking out of the view
        let incoming = incoming_disc(ctx);
        let kind = if self.archetype == Archetype::Light && self.skill.use_walls && wall_side(world, me, dir).is_some() {
            EvadeKind::WallCling
        } else if !incoming {
            EvadeKind::Strafe
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
                    if self.heavy_until > tick {
                        *buttons |= Buttons::SLICE;
                        (to_t, 0.0)
                    } else if t.dist <= COMBO_REACH && self.combo < 2 {
                        // stay on them for the next cut: close the last metre, do not circle off
                        (to_t, if t.dist > 1.4 { 0.5 } else { 0.0 })
                    } else {
                        let side = vector![-to_t.z, 0.0, to_t.x] * self.strafe_sign;
                        (unit_or_zero(side - to_t * 0.4), 1.0)
                    }
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

    // ------------------------------------------------------------------ void-edge guard

    /// Vertical maps only (`NavGrid::is_sky`). While grounded: no dash, roll, katana lunge or jump that would carry the
    /// bot off the deck (the lunge reach is checked along the aim and the move direction), and a walk toward an edge
    /// (reach grows with speed: a runner needs room to stop) bends to the nearest safe heading, or stops. Airborne bots
    /// keep whatever the rest of the brain decided (they cannot be steered back anyway).
    fn edge_guard(&mut self, nav: &crate::nav::NavGrid, ctx: &ThinkCtx, dir: V3, mag: f32, yaw: f32, to_target: Option<(V3, f32)>, buttons: &mut u8) -> (V3, f32) {
        let me = ctx.me;
        let cfg = ctx.world.cfg;
        let feet = me.pos.y - cfg.half_height - cfg.radius;
        if !me.grounded() {
            // A lunge, dash or knockback leaves a fast airborne body with a long carry (air drag is only ~2 m/s^2): when a
            // drop lies inside its stopping distance, hold the stick against the motion (the stick brakes hard in the air,
            // ~20 m/s^2). `feet - 3`: a jump arc is not a drop, only surfaces well below the arc and open void are.
            if matches!(me.state, shinobi_sim::character::MoveState::Airborne) {
                let v = flat(me.vel);
                let speed = v.norm();
                if speed > 4.0 {
                    let vd = v / speed;
                    let stop = speed * speed / (2.0 * (cfg.air_accel + cfg.air_drag)) + 1.5;
                    if nav.edge_ahead(me.pos, vd, stop, feet - 3.0) {
                        self.stats.edge_stops += 1;
                        *buttons &= !(Buttons::DASH | Buttons::ROLL | Buttons::SLICE | Buttons::JUMP_PRESSED | Buttons::JUMP_HELD);
                        return (-vd, 1.0);
                    }
                }
            }
            return (dir, mag);
        }
        let aim_dir = dir_from_yaw(yaw);
        let leaping = *buttons & (Buttons::DASH | Buttons::ROLL | Buttons::SLICE | Buttons::JUMP_PRESSED) != 0;
        // a katana lunge homes onto a locked target and a dash carries far: vet the aim, the move direction AND the line to
        // the target (the lunge may go there, not along the aim), over the longest reach the lunge can have
        let lunge_reach: f32 = 11.0;
        let toward_target = to_target.map_or(false, |(d, dist)| nav.edge_ahead(me.pos, d, lunge_reach.min(dist + 2.0), feet));
        if leaping && (nav.edge_ahead(me.pos, aim_dir, lunge_reach, feet) || toward_target || (mag > 0.05 && nav.edge_ahead(me.pos, dir, lunge_reach, feet))) {
            *buttons &= !(Buttons::DASH | Buttons::ROLL | Buttons::SLICE | Buttons::JUMP_PRESSED | Buttons::JUMP_HELD);
            self.stats.edge_stops += 1;
        }
        // standing beside a drop (a catwalk edge under a shoulder, a knockback, a strafe): lean away from it, and never
        // walk toward it at all
        let push = nav.edge_push(me.pos, 0.9, feet);
        let (mut dir, mut mag) = (dir, mag);
        if push != Vector::zeros() {
            self.stats.edge_stops += 1;
            let toward = dir.dot(&push) < 0.0; // heading at the drop
            dir = unit_or_zero(if toward { flat(dir) + push * 1.6 } else { flat(dir) + push * 0.8 });
            if dir == Vector::zeros() {
                dir = push;
            }
            mag = mag.max(0.5);
        }
        if mag < 0.05 {
            return (dir, mag);
        }
        let reach = 1.8 + 0.3 * me.flat_speed();
        if !nav.edge_ahead(me.pos, dir, reach, feet) {
            return (dir, mag);
        }
        self.stats.edge_stops += 1;
        for k in [0.5f32, -0.5, 1.0, -1.0, 1.6, -1.6, 2.3, -2.3] {
            let alt = rotate_y(dir, k);
            if !nav.edge_ahead(me.pos, alt, reach, feet) {
                return (alt, mag * 0.8);
            }
        }
        (dir, 0.0)
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
                *buttons |= Buttons::JUMP_HELD; // hold: the jump rises higher and glides down (the reach below assumes it)
                return (hdir, 1.0);
            }
        }

        // ---- ledge hop (perch): only when the tactical layer is walking at the box
        if self.phase == Phase::Perch && me.grounded() && mag > 0.1 {
            if let Some(pr) = self.perch.and_then(|i| ctx.perches.get(i)) {
                let feet = me.pos.y - cfg.half_height - cfg.radius;
                let rise = pr.top_center.y - feet;
                if pr.edge_dist(me.pos) <= 2.2 && !pr.contains_xz(me.pos, 0.0) && (0.4..cfg.max_hop_rise(0.3)).contains(&rise) {
                    let hdir = unit_or_zero(flat(pr.top_center - me.pos));
                    self.stats.ledge_hops += 1;
                    *buttons |= Buttons::JUMP_PRESSED | Buttons::JUMP_HELD;
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
                // freest direction, ties broken away from whatever is touching us: pressed flat against a wall's end the
                // body can be wedged even along the wall, and only a step back off the face frees it
                let mut dirs = Vec::with_capacity(8);
                let mut away = Vector::<f32>::zeros();
                for k in 0..8 {
                    let a = k as f32 * std::f32::consts::FRAC_PI_4 + (tick % 7) as f32 * 0.1;
                    let d = vector![a.cos(), 0.0, a.sin()];
                    if world.ray(knee, d, 0.6).is_some() {
                        away -= d;
                    }
                    dirs.push((world.ray(knee, d, 4.0).unwrap_or(4.0), d));
                }
                let top = dirs.iter().map(|d| d.0).fold(0.0f32, f32::max);
                let away = unit_or_zero(away);
                let mut best = (-1.0f32, vector![1.0, 0.0, 0.0]);
                for (room, d) in dirs {
                    if room < top - 0.01 {
                        continue;
                    }
                    let score = d.dot(&away);
                    if score > best.0 {
                        best = (score, d);
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

    /// Now and then a bot throws something other than a plain star: a mine at a target it is closing on, a flash at one that
    /// is looking at it, smoke (dropped at its own feet) when it is hurt. Only with the energy to spare, and never faster
    /// than the per-kind gaps below, so the variants stay seasoning and the plain star stays the bot's poke.
    fn star_variants(&mut self, ctx: &ThinkCtx, p: &Perception, buttons: &mut u8, rng: &mut Rng) {
        let (tick, me) = (ctx.world.tick, ctx.me);
        let Some((energy, _, locked)) = ctx.world.server.players.iter().find(|q| q.id == me.id).and_then(|q| q.combat.shuriken_energy()) else { return };
        let sc = ctx.world.server.balance.combat.star;
        if locked || tick < self.fire_ok || matches!(self.phase, Phase::Strike { .. } | Phase::Dead) {
            return;
        }
        // smoke: hurt or backing off, look down and drop it where we stand
        let hurt = me.hp_frac < 0.45 || self.phase == Phase::Retreat;
        if hurt && energy >= sc.smoke_cost + 0.5 && tick >= self.smoke_ok {
            if tick >= self.smoke_aim_until && rng.chance(0.06) {
                self.smoke_aim_until = tick + 40;
            }
            if tick < self.smoke_aim_until && me.pitch < -0.6 {
                *buttons |= Buttons::FIRE;
                self.star_pick = 3;
                self.smoke_ok = tick + 900;
                self.smoke_aim_until = 0;
                self.stats.shurikens += 1;
                self.fire_ok = tick + self.skill.fire_gap_ticks;
                return;
            }
        } else if !hurt {
            self.smoke_aim_until = 0;
        }
        let Some(t) = p.target else { return };
        if !t.los || t.view.spawn_protected || !matches!(self.phase, Phase::Advance | Phase::Flank | Phase::Zone | Phase::Perch) {
            return;
        }
        let (want_yaw, _) = self.aim_angles(ctx, &t);
        if wrap_pi(want_yaw - me.yaw).abs() >= 0.10 + self.skill.aim_error {
            return;
        }
        let pick = if t.on_screen && (7.0..=16.0).contains(&t.dist) && energy >= sc.flash_cost + 1.0 && tick >= self.flash_ok && rng.chance(0.03) {
            self.flash_ok = tick + 420;
            2
        } else if (8.0..=22.0).contains(&t.dist) && energy >= sc.mine_cost + 1.5 && tick >= self.mine_ok && rng.chance(0.02) {
            self.mine_ok = tick + 600;
            1
        } else {
            return;
        };
        *buttons |= Buttons::FIRE;
        self.star_pick = pick;
        self.stats.shurikens += 1;
        self.fire_ok = tick + self.skill.fire_gap_ticks + (rng.unit() * 5.0) as u32;
    }

    fn attacks(&mut self, ctx: &ThinkCtx, p: &Perception, dir: V3, buttons: &mut u8, rng: &mut Rng) {
        let (tick, me) = (ctx.world.tick, ctx.me);
        if *buttons & Buttons::FIRE != 0 {
            return; // a variant went out this tick
        }
        let Some(t) = p.target else { return };
        let can_throw = matches!(
            self.phase,
            Phase::Advance | Phase::Flank | Phase::Zone | Phase::Perch | Phase::Retreat
        ) || matches!(self.phase, Phase::Evade { kind: EvadeKind::WallCling | EvadeKind::Strafe, .. });
        // stars are the poke: nothing is thrown at point-blank range, where the katana is the weapon
        if !can_throw || !t.los || t.dist > SHURIKEN_RANGE || t.dist < POKE_MIN_RANGE || tick < self.fire_ok || t.view.spawn_protected {
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
        let smoke_down = ctx.world.tick < self.smoke_aim_until;
        let (want_yaw, want_pitch) = match (p.target, self.phase) {
            // dropping smoke at our own feet: keep the heading, look down
            _ if smoke_down => (me.yaw, -0.95),
            // strikes lock on cleanly: the lunge goes where the aim points
            (Some(t), Phase::Strike { .. }) => {
                let d = flat(t.view.pos - me.pos);
                (yaw_of(d) + self.slice_off, 0.0)
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



/// An enemy star within 18 m flying at this bot (closing, roughly on a line through it).
fn incoming_disc(ctx: &ThinkCtx) -> bool {
    let me = ctx.me;
    ctx.world.server.projectiles().into_iter().any(|(owner, d)| {
        if owner == me.id {
            return false;
        }
        let to = me.pos - vector![d.pos.x, d.pos.y, d.pos.z];
        let dist = to.norm();
        dist < 18.0 && dist > 0.5 && to.dot(&d.dir) / dist > 0.97
    })
}
