use rapier3d::na::UnitQuaternion;
use rapier3d::prelude::*;

use super::{
    solid_groups, TARGETS,
    weapon::{FireCtx, Trigger, Weapon},
    CombatEvent, DamageKind, SlashKind, GROUP_MELEE,
};
use crate::character::Cfg;

/// Yaw offsets (degrees, tried left and right) a missed swing re-sweeps before giving up.
const ASSIST_STEPS_DEG: [f32; 3] = [8.0, 16.0, 24.0];

#[derive(Clone, Copy, Debug)]
pub struct KatanaCfg {
    pub damage: f32,
    pub blade_len: f32,
    pub half_width: f32,
    pub half_height: f32,
    pub cooldown: f32,
    /// Horizontal distance the character's dash covers; sizes the hit box.
    pub lunge_distance: f32,
    /// Strength (0..1, times the lock pull) of the hit sweep's bend toward the lock-on target.
    pub magnet: f32,
    /// Damage to each player-built piece inside the blade box.
    pub piece_damage: f32,
    /// Hook-strike (swing while hooked / just after the rope let go): damage multiplier, and blade reach added on top
    /// of the blade length (plus `STRIKE_REACH_PER_MPS` x the speed you arrive with).
    pub hook_damage_mult: f32,
    pub hook_reach: f32,
    /// Wall slash (swing during a wall-run): same two knobs.
    pub wall_damage_mult: f32,
    pub wall_reach: f32,
    /// Rope slash (swing while the rope is taut): enemies within `rope_radius` m of the rope line, up to `rope_reach` m
    /// from the eye, are cut for `rope_damage_mult` (at least the strike multiplier).
    pub rope_damage_mult: f32,
    pub rope_radius: f32,
    pub rope_reach: f32,
    /// Seconds after a swing within which the next swing continues the combo.
    pub combo_window: f32,
    /// Damage multiplier of the third swing of a combo (every swing before it must have connected).
    pub finisher_mult: f32,
    /// Extra blade length (m) of that finishing swing.
    pub finisher_reach: f32,
    /// Target-centred hit cone: a swing also hits any enemy whose centre lies within this half-angle (degrees, flat) of
    /// the aim and within reach + `cone_extra` of the eye, line of sight permitting (0 = off, box only).
    pub cone_deg: f32,
    /// Extra reach (m) of the cone beyond the blade box: generous at the start of a swing.
    pub cone_extra: f32,
    /// Heavy cut (hold after a cut, release): damage multiplier and extra blade length at full charge, and its recovery (s).
    pub heavy_mult: f32,
    pub heavy_reach: f32,
    pub heavy_recover: f32,
    /// Air cut: damage multiplier and the blade box's vertical half-extent (it reaches down at the victim).
    pub air_mult: f32,
    pub air_half_height: f32,
    /// Whirl cut: radius (m) and flat damage per enemy it catches, and its recovery (s).
    pub whirl_radius: f32,
    pub whirl_damage: f32,
    pub whirl_recover: f32,
    /// Counter cut: how far (m) the staggered attacker may be, and how much of the parried hit adds to the damage
    /// (multiplier = 1 + scale x parried / base damage, parried capped at 2 x base).
    pub counter_range: f32,
    pub counter_scale: f32,
    /// Skill combat (see `KatanaBalance`): combo stacks, wall chain / wall-kick, hook / wall-jump air-cut bonuses, cancels.
    pub combo_dmg: f32,
    pub combo_speed: f32,
    pub combo_max: f32,
    pub wall_chain_max: f32,
    pub wall_chain_window: f32,
    pub wallkick_mult: f32,
    pub wallkick_reach: f32,
    pub hook_air_mult: f32,
    pub hook_air_window: f32,
    pub walljump_air_mult: f32,
    pub walljump_air_window: f32,
    pub cancel_cost: f32,
    /// 1 = cancels on, 0 = off.
    pub cancel: f32,
    /// A swing that hits nothing locks the blade out for at least this many seconds (a whiff is punished; 0 = off).
    pub whiff_recover: f32,
}

/// Extra strike reach per m/s of speed at the moment of the swing (a 40 m/s reel reaches 6 m further).
pub const STRIKE_REACH_PER_MPS: f32 = 0.15;

/// A katana swing comes round after this fraction of the dash cooldown: a fast combo of cuts, of which only every
/// third or so carries the dash lunge (the dash has its own, longer cooldown). Derived from the character, like
/// the dash itself, so the client's prediction (`KatanaCfg::for_character`) and the server agree.
pub const SWING_CD_FRAC: f32 = 0.35;
/// Swings in one combo; the last is the finisher.
pub const COMBO_LEN: u32 = 3;

/// Distance `CharacterSystem`'s dash travels: speed = dash_speed * (0.4 + 0.6 f), f: 1 -> 0 over
/// `dash_time`, so the mean speed is 0.7 * dash_speed (30 * 0.18 * 0.7 = 3.8 m by default).
pub fn dash_distance(ch: &Cfg) -> f32 {
    ch.dash_speed * ch.dash_time * 0.7
}

impl KatanaCfg {
    pub fn for_character(ch: &Cfg) -> Self {
        Self {
            damage: 120.0,
            blade_len: 1.6,
            half_width: 1.0,
            half_height: 1.0,
            // Swings are quicker than the dash comes back: a combo of cuts, the lunge on the first.
            cooldown: (ch.dash_cd * SWING_CD_FRAC).max(0.25),
            lunge_distance: dash_distance(ch),
            magnet: 0.8,
            piece_damage: 60.0,
            hook_damage_mult: 1.5,
            hook_reach: 4.0,
            wall_damage_mult: 1.35,
            wall_reach: 3.0,
            rope_damage_mult: 1.6,
            rope_radius: 1.5,
            rope_reach: 25.0,
            combo_window: 1.0,
            finisher_mult: 1.0,
            finisher_reach: 0.0,
            cone_deg: 0.0,
            cone_extra: 0.0,
            heavy_mult: 1.8,
            heavy_reach: 1.2,
            heavy_recover: 0.9,
            air_mult: 1.25,
            air_half_height: 2.2,
            whirl_radius: 3.0,
            whirl_damage: 12.0,
            whirl_recover: 0.6,
            counter_range: 7.0,
            counter_scale: 0.5,
            combo_dmg: 0.0,
            combo_speed: 0.0,
            combo_max: 0.0,
            wall_chain_max: 0.0,
            wall_chain_window: 1.5,
            wallkick_mult: 1.0,
            wallkick_reach: 0.0,
            hook_air_mult: 1.0,
            hook_air_window: 0.0,
            walljump_air_mult: 1.0,
            walljump_air_window: 0.0,
            cancel_cost: 0.0,
            cancel: 0.0,
            whiff_recover: 0.0,
        }
    }
}

/// Enemies inside the swing's cone: the target-centred half of the hit test. The blade box is a hand-sized volume and a
/// moving target slips out of its corners; this is "is the enemy roughly where I swung, and near enough".
fn cone_hits(world: &crate::character::PhysicsWorld, eye: Vector<Real>, dir: Vector<Real>, reach: f32, cone_deg: f32, own: RigidBodyHandle, allies: &[ColliderHandle], lock: Option<ColliderHandle>) -> Vec<ColliderHandle> {
    let cos = cone_deg.to_radians().cos();
    let flat_dir = vector![dir.x, 0.0, dir.z];
    let fd = flat_dir.norm();
    if fd < 1e-4 {
        return Vec::new();
    }
    let flat_dir = flat_dir / fd;
    let mut out = Vec::new();
    for (h, c) in world.colliders.iter() {
        if !c.collision_groups().memberships.intersects(TARGETS) || c.parent() == Some(own) || allies.contains(&h) {
            continue;
        }
        let centre = *c.translation();
        let to = centre - eye;
        let flat = vector![to.x, 0.0, to.z];
        let dist = flat.norm();
        // a locked target is forgiven a wider angle (the player is plainly trying to hit it)
        let wide = if lock == Some(h) { (cos * 0.82).max(-1.0) } else { cos };
        if dist > reach || dist < 1e-4 || flat.dot(&flat_dir) / dist < wide || to.y.abs() > 1.6 {
            continue;
        }
        if crate::spawn::blocked(world, [eye.x, eye.y, eye.z], [centre.x, centre.y, centre.z]) {
            continue;
        }
        out.push(h);
    }
    out
}

/// The movement-relevant part of a katana swing, for client prediction and replay. Mirrors `Katana::fire`
/// minus everything that needs the world (wall truncation of the blade, hit boxes): advance the cooldown by
/// `dt`, and if `pressed` and ready, consume it and (when the dash is ready, or `strike` says a hook-strike / wall
/// slash launch applies) turn the tick's input into a dash / strike launch along `dir`. Returns `Some(lunged)` when a
/// swing happened.
///
/// Known divergence: `Katana::fire` cuts in place when a target is already inside blade reach (it must not dash
/// through them); that needs the enemy set, which the client does not have, so it is corrected by reconciliation.
/// `tests::predict_slice_matches_the_real_katana` keeps the two in step.
pub fn predict_slice(
    cooldown: f32,
    cd_left: &mut f32,
    dt: f32,
    pressed: bool,
    dash_ready: bool,
    strike: Option<crate::character::StrikeKind>,
    dir: Vector<Real>,
    input: &mut crate::character::PlayerInput,
) -> Option<bool> {
    *cd_left = (*cd_left - dt).max(0.0);
    if pressed && *cd_left <= 0.0 {
        let lunge = dash_ready || strike.is_some();
        if strike.is_some() {
            input.strike = true;
            input.move_dir = dir;
        } else if dash_ready {
            input.dash_pressed = true;
            input.move_dir = dir;
        }
        *cd_left = cooldown;
        Some(lunge)
    } else {
        None
    }
}

/// Enemies whose centre is within `radius` of the rope segment `eye -> anchor` (cut to `reach` m from the eye).
fn rope_line_hits(world: &crate::character::PhysicsWorld, eye: Vector<Real>, anchor: Option<Vector<Real>>, radius: f32, reach: f32, own: RigidBodyHandle, allies: &[ColliderHandle]) -> Vec<ColliderHandle> {
    let Some(anchor) = anchor else { return Vec::new() };
    let full = anchor - eye;
    let len = full.norm();
    if len < 0.5 || radius <= 0.0 || reach <= 0.0 || !len.is_finite() {
        return Vec::new();
    }
    let dir = full / len;
    let seg = len.min(reach);
    let mut out = Vec::new();
    for (h, c) in world.colliders.iter() {
        if !c.collision_groups().memberships.intersects(TARGETS) || c.parent() == Some(own) || allies.contains(&h) {
            continue;
        }
        let rel = *c.translation() - eye;
        let t = rel.dot(&dir).clamp(0.0, seg);
        if (rel - dir * t).norm() <= radius {
            out.push(h);
        }
    }
    out
}

/// Seconds the button must stay down after a cut before releasing it throws a heavy cut, and the hold that is a full charge.
/// Code constants (not Balance): the client's own `Katana` runs the same gate on the same buttons, so the press / hold /
/// release timeline (and with it the cooldown mirror) must be identical on both sides without sharing a `Balance`.
pub const HEAVY_MIN_S: f32 = 0.5;
pub const HEAVY_FULL_S: f32 = 0.9;
/// A hold shorter than this is a click, not a charge (the HUD ring stays away).
pub const CHARGE_SHOW_S: f32 = 0.15;

/// The move one katana trigger pull turns into (decided in `Katana::fire`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Light,
    /// Charged 0..1 by the length of the hold.
    Heavy(f32),
    Air,
    Whirl,
    /// The staggered attacker's hitbox, the damage that was parried.
    Counter(ColliderHandle, f32),
}

pub struct Katana {
    pub cfg: KatanaCfg,
    flip: bool,
    /// Consecutive swings that connected inside the combo window (0, 1, 2 -> the next one is the finisher).
    combo: u32,
    /// `now` of the last swing (the combo window runs from it).
    last_swing: f32,
    /// The button was down last tick, and for how long it has been (seconds): the press / hold / release gate.
    prev: bool,
    held: f32,
    /// A heavy cut released this tick: its charge (0..1), consumed by `fire`.
    heavy: Option<f32>,
    /// Recovery of the swing that just happened (`Weapon::cooldown_after`).
    recover: f32,
    /// Skill combo counter: cuts in a row that connected (reset by a whiff, a hit taken, or `combo_window` of idling).
    streak: u32,
    /// Seconds since the last swing (the counter's idle timeout).
    since_swing: f32,
    /// Connected wall-run slashes in a row, and `now` of the last one: at `wall_chain_max` the next is the wall-kick.
    wall_chain: u32,
    last_wall: f32,
}

impl Katana {
    pub fn new(cfg: KatanaCfg) -> Self {
        Self { cfg, flip: false, combo: 0, last_swing: f32::NEG_INFINITY, prev: false, held: 0.0, heavy: None, recover: cfg.cooldown, streak: 0, since_swing: 99.0, wall_chain: 0, last_wall: f32::NEG_INFINITY }
    }
}

impl Weapon for Katana {
    fn name(&self) -> &'static str {
        "Cyber-Katana"
    }
    fn apply_balance(&mut self, b: &crate::balance::Balance, ch: &Cfg) {
        let mut k = KatanaCfg::for_character(ch);
        b.apply_katana(&mut k);
        self.cfg = k;
    }
    fn trigger(&self) -> Trigger {
        Trigger::Secondary
    }
    fn cooldown(&self) -> f32 {
        self.cfg.cooldown
    }
    fn cooldown_after(&self) -> f32 {
        self.recover
    }

    /// Press = a cut right now (as ever). Keep holding after it and the blade charges; releasing after `HEAVY_MIN_S`
    /// throws the heavy cut. A held button no longer auto-repeats: chain cuts by clicking.
    fn gate(&mut self, pressed: bool, dt: f32, ready: bool) -> bool {
        let edge = pressed && !self.prev;
        let release = !pressed && self.prev;
        if edge {
            self.held = 0.0;
        } else if pressed {
            self.held += dt;
        }
        let held = self.held;
        self.prev = pressed;
        if !pressed {
            self.held = 0.0;
        }
        if edge && ready {
            return true;
        }
        if release && held >= HEAVY_MIN_S && ready {
            self.heavy = Some(((held - HEAVY_MIN_S) / (HEAVY_FULL_S - HEAVY_MIN_S)).clamp(0.0, 1.0));
            return true;
        }
        false
    }

    fn charge(&self) -> f32 {
        if self.prev && self.held >= CHARGE_SHOW_S {
            (self.held / HEAVY_FULL_S).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    fn combo_count(&self) -> u32 {
        self.streak
    }

    fn on_hurt(&mut self) {
        self.streak = 0;
        self.combo = 0;
        self.wall_chain = 0;
    }

    fn cancel_cost(&self) -> f32 {
        if self.cfg.cancel > 0.5 { self.cfg.cancel_cost } else { -1.0 }
    }

    fn tick(&mut self, dt: f32, _ctx: &mut super::weapon::TickCtx) {
        self.since_swing += dt;
        if self.since_swing > self.cfg.combo_window {
            self.streak = 0;
        }
    }

    fn reset(&mut self) {
        self.streak = 0;
        self.wall_chain = 0;
        self.combo = 0;
        self.last_swing = f32::NEG_INFINITY;
        self.prev = false;
        self.held = 0.0;
        self.heavy = None;
    }

    fn fire(&mut self, ctx: &mut FireCtx) -> bool {
        let mut c = self.cfg;
        let origin = ctx.aim.origin;
        let has_strike = ctx.ch.strike_kind().is_some();

        // ---- which move is this? heavy (released charge) > counter (just parried) > whirl (sliding / shield just down)
        // > air cut (airborne, no rope or wall) > the plain cut
        let counter = ctx.moves.counter.and_then(|(h, parried)| {
            let t = *ctx.world.colliders.get(h)?.translation();
            let to = t - origin;
            let flat = (to.x * to.x + to.z * to.z).sqrt();
            (flat <= c.counter_range && !crate::spawn::blocked(ctx.world, [origin.x, origin.y, origin.z], [t.x, t.y, t.z])).then_some((h, parried))
        });
        let mode = if let Some(ch) = self.heavy.take() {
            Mode::Heavy(ch)
        } else if let Some((h, parried)) = counter {
            Mode::Counter(h, parried)
        } else if ctx.ch.sliding() || ctx.moves.just_unblocked {
            Mode::Whirl
        } else if !has_strike && matches!(ctx.ch.state, crate::character::MoveState::Airborne) {
            Mode::Air
        } else {
            Mode::Light
        };
        let kind = match mode {
            Mode::Light => SlashKind::Light,
            Mode::Heavy(_) => SlashKind::Heavy,
            Mode::Air => SlashKind::Air,
            Mode::Whirl => SlashKind::Whirl,
            Mode::Counter(..) => SlashKind::Counter,
        };
        let chains = matches!(mode, Mode::Light | Mode::Air);

        // combo: every swing that connected within the window brings the finisher closer; it hits harder and reaches further
        if ctx.now - self.last_swing > c.combo_window {
            self.combo = 0;
            self.streak = 0;
        }
        if ctx.now - self.last_wall > c.wall_chain_window {
            self.wall_chain = 0;
        }
        // wall-run slashes chain on their own (`wall_chain`) into a wall-kick; they do not feed the plain finisher
        let wall_slash = mode == Mode::Light && ctx.ch.strike_kind() == Some(crate::character::StrikeKind::Wall);
        let wallkick = wall_slash && c.wall_chain_max >= 1.0 && self.wall_chain as f32 >= c.wall_chain_max;
        let finisher = chains && !wall_slash && self.combo + 1 >= COMBO_LEN;
        if finisher {
            c.damage *= c.finisher_mult;
            c.blade_len += c.finisher_reach;
        }
        if wallkick {
            c.blade_len += c.wallkick_reach;
        }
        self.recover = c.cooldown;
        let mut move_mult = 1.0f32;
        match mode {
            Mode::Heavy(ch) => {
                move_mult = 1.0 + (c.heavy_mult - 1.0) * ch;
                c.blade_len += c.heavy_reach * ch;
                self.recover = c.heavy_recover;
            }
            Mode::Air => {
                move_mult = c.air_mult;
                c.half_height = c.air_half_height;
                // hook -> air-slash, wall-jump -> air-slash: the move you came out of hands the cut a bonus
                let hook = if ctx.ch.hook_age <= c.hook_air_window { c.hook_air_mult } else { 1.0 };
                let wj = if ctx.ch.walljump_age <= c.walljump_air_window { c.walljump_air_mult } else { 1.0 };
                move_mult *= hook.max(wj);
            }
            Mode::Whirl => self.recover = c.whirl_recover,
            Mode::Counter(_, parried) => move_mult = 1.0 + c.counter_scale * (parried / c.damage.max(1.0)).clamp(0.0, 2.0),
            Mode::Light => {}
        }
        if wallkick {
            move_mult *= c.wallkick_mult;
        }
        // combo counter: every connected cut in the streak quickens the recovery; the damage bonus starts after the
        // third (the finisher): a combo that carries on keeps climbing
        let stacks = self.streak.min(c.combo_max.max(0.0) as u32) as f32;
        move_mult *= 1.0 + c.combo_dmg * (stacks - 2.0).max(0.0);
        self.recover = (self.recover * (1.0 - c.combo_speed * stacks)).max(0.12);
        let mut dir = ctx.aim.flat_forward();

        // Walls stop both lunge and blade: truncate the longest possible reach at the first solid.
        let solid = QueryFilter::default()
            .exclude_rigid_body(ctx.ch.body)
            .exclude_sensors()
            .groups(InteractionGroups::new(GROUP_MELEE, solid_groups()));
        // hook-strike / wall slash: the launch carries the blade further and hits harder (docs/traversal_melee.md); only the
        // plain cut launches
        let strike = if mode == Mode::Light { ctx.ch.strike_kind() } else { None };
        let (strike_dmg, strike_reach) = match strike {
            Some(crate::character::StrikeKind::Hook) => (c.hook_damage_mult, c.hook_reach),
            Some(crate::character::StrikeKind::Wall) => (c.wall_damage_mult, c.wall_reach),
            None => (1.0, 0.0),
        };
        let strike_reach = if strike.is_some() { strike_reach + STRIKE_REACH_PER_MPS * ctx.ch.vel.norm().min(60.0) } else { 0.0 };
        // heavy, whirl and counter cuts are made in place (no dash): the lunge is for the plain and the air cut
        let may_lunge = matches!(mode, Mode::Light | Mode::Air);
        let max_reach = c.blade_len + if strike.is_some() { strike_reach } else if may_lunge { c.lunge_distance } else { 0.0 };

        // Short-lived hit boxes: query-only, they exist for exactly this call.
        let not_ally = |h: ColliderHandle, _: &Collider| !ctx.allies.contains(&h);
        let filter = QueryFilter::default()
            .exclude_rigid_body(ctx.ch.body)
            .predicate(&not_ally)
            .groups(InteractionGroups::new(GROUP_MELEE, TARGETS));

        let mut lunge = false;
        let mut reach;
        let mut hits: Vec<ColliderHandle>;
        let (mut ev_reach, mut ev_half) = (0.0f32, c.half_width);
        let mut wall_piece: Option<[i32; 3]> = None;
        if matches!(mode, Mode::Whirl | Mode::Counter(..)) {
            // no blade box: whirl hits everyone close all round, a counter cut goes straight for the staggered attacker
            match mode {
                Mode::Whirl => {
                    reach = c.whirl_radius;
                    hits = cone_hits(ctx.world, origin, dir, reach, 180.0, ctx.ch.body, ctx.allies, None);
                    ev_half = c.whirl_radius;
                }
                Mode::Counter(h, _) => {
                    let t = ctx.world.colliders.get(h).map(|col| *col.translation()).unwrap_or(origin);
                    let to = vector![t.x - origin.x, 0.0, t.z - origin.z];
                    let n = to.norm();
                    if n > 1e-4 {
                        dir = to / n;
                    }
                    reach = n + 0.5;
                    hits = vec![h];
                    ev_half = c.half_width;
                }
                _ => unreachable!(),
            }
            ev_reach = reach;
        } else {
            let ray = Ray::new(Point::from(origin), dir);
            let wall = ctx
                .world
                .queries
                .cast_ray(&ctx.world.bodies, &ctx.world.colliders, &ray, max_reach, true, solid);
            // the solid that truncates the blade may itself be a printed piece: it is struck by the swing
            wall_piece = wall.and_then(|(h, _)| ctx.world.colliders.get(h)).and_then(|col| crate::blocks::piece_cell(col.user_data));
            let wall = wall.map(|(_, toi)| toi.max(0.3));
            let full = wall.unwrap_or(max_reach).min(max_reach);
            let short = c.blade_len.min(full);

            let boxhits_at = |dir: Vector<Real>, reach: f32| -> Vec<ColliderHandle> {
                let rot = UnitQuaternion::face_towards(&dir, &Vector::y());
                let shape = Cuboid::new(vector![c.half_width, c.half_height, reach * 0.5]);
                let pos = Isometry::from_parts((origin + dir * (reach * 0.5)).into(), rot);
                let mut hits = Vec::new();
                ctx.world.queries.intersections_with_shape(&ctx.world.bodies, &ctx.world.colliders, &pos, &shape, filter, |h| {
                    hits.push(h);
                    true
                });
                hits
            };
            // Melee aim assist: a swing that misses dead-ahead re-sweeps a few degrees to either side and takes the
            // first sweep that connects. Hit boxes only: the lunge keeps the aimed direction (client prediction).
            // magnetism: with a lock, sweep first along the aim bent toward the target (horizontally: the blade is flat)
            let magnet_dir = ctx.lock.map(|h| {
                let flat = vector![h.dir.x, 0.0, h.dir.z];
                let n = flat.norm();
                if n > 1e-4 { super::aim_assist::bend(dir, flat / n, c.magnet * h.pull) } else { dir }
            });
            let boxhits = |reach: f32| -> Vec<ColliderHandle> {
                if let Some(m) = magnet_dir {
                    let h = boxhits_at(m, reach);
                    if !h.is_empty() {
                        return h;
                    }
                }
                let straight = boxhits_at(dir, reach);
                if !straight.is_empty() {
                    return straight;
                }
                for deg in ASSIST_STEPS_DEG {
                    for sign in [1.0f32, -1.0] {
                        let r = UnitQuaternion::from_axis_angle(&Vector::y_axis(), (deg * sign).to_radians());
                        let h = boxhits_at(r * dir, reach);
                        if !h.is_empty() {
                            return h;
                        }
                    }
                }
                straight
            };

            // Lunge = the character's own dash, aimed along the slash. If the dash is still on cooldown the
            // slash still lands (blade reach only, no lunge) rather than eating the input. If someone is already
            // inside blade reach we cut in place: dashing would carry you through them and out the other side.
            lunge = may_lunge && (strike.is_some() || ctx.ch.dash_ready());
            reach = if lunge { full } else { short };
            hits = boxhits(reach);
            if lunge {
                let near = boxhits(short);
                if !near.is_empty() {
                    lunge = false;
                    reach = short;
                    hits = near;
                }
            }
            if lunge {
                if strike.is_some() {
                    ctx.input.strike = true;
                } else {
                    ctx.input.dash_pressed = true;
                }
                ctx.input.move_dir = dir;
            }

            // target-centred cone: anything roughly where the swing points and near enough is hit, whatever the box did
            ev_reach = reach;
            if c.cone_deg > 0.0 {
                ev_reach = reach + c.cone_extra;
                for h in cone_hits(ctx.world, origin, dir, ev_reach, c.cone_deg, ctx.ch.body, ctx.allies, ctx.lock.map(|l| l.collider)) {
                    if !hits.contains(&h) {
                        hits.push(h);
                    }
                }
                // the lag-compensated re-test in pvp.rs sweeps a cylinder of this half-width: match the cone at full reach
                ev_half = ev_half.max((ev_reach * c.cone_deg.to_radians().tan()).min(2.2));
            }
        }

        // player-built pieces in the blade box take damage too (the server decides if they de-rez)
        {
            // whirl: everything within the whirl radius all round; every other cut: the blade box
            let whirl = matches!(mode, Mode::Whirl);
            let pos = if whirl {
                Isometry::translation(origin.x, origin.y, origin.z)
            } else {
                Isometry::from_parts((origin + dir * (reach * 0.5)).into(), UnitQuaternion::face_towards(&dir, &Vector::y()))
            };
            let shape: Box<dyn Shape> = if whirl {
                Box::new(Ball::new(c.whirl_radius))
            } else {
                Box::new(Cuboid::new(vector![c.half_width, c.half_height, reach * 0.5]))
            };
            let shape = shape.as_ref();
            let piece_filter = QueryFilter::default().groups(InteractionGroups::new(GROUP_MELEE, super::GROUP_WORLD));
            let mut cells: Vec<[i32; 3]> = wall_piece.into_iter().collect();
            ctx.world.queries.intersections_with_shape(&ctx.world.bodies, &ctx.world.colliders, &pos, shape, piece_filter, |h| {
                if let Some(cell) = ctx.world.colliders.get(h).and_then(|col| crate::blocks::piece_cell(col.user_data)) {
                    cells.push(cell);
                }
                true
            });
            cells.sort();
            cells.dedup();
            for cell in cells {
                ctx.events.push(CombatEvent::PieceHit { cell, amount: c.piece_damage, kind: DamageKind::Slash });
            }
        }

        self.last_swing = ctx.now;
        self.since_swing = 0.0;
        self.combo = if hits.is_empty() || finisher || !chains || wall_slash { 0 } else { self.combo + 1 };

        // what one connecting blow is worth
        let amount = if matches!(mode, Mode::Whirl) { c.whirl_damage } else { c.damage * strike_dmg * move_mult };

        if hits.is_empty() {
            ctx.events.push(CombatEvent::SlashMiss { origin: Point::from(origin), dir });
        }
        // rope slash: a taut rope is a blade too (docs/traversal_melee.md)
        let rope_hits = rope_line_hits(ctx.world, origin, ctx.ch.grapple.map(|g| g.anchor), c.rope_radius, c.rope_reach, ctx.ch.body, ctx.allies);
        for h in &rope_hits {
            if !hits.contains(h) {
                hits.push(*h);
            }
        }
        // skill counters: a cut that found nothing (or only the rope) breaks the streak and the wall chain
        let connected = !hits.is_empty();
        self.streak = if connected { self.streak + 1 } else { 0 };
        // commit: a whiffed cut cannot be followed up at once (a cancel into dash / hook / wall jump still frees it)
        if !connected {
            self.recover = self.recover.max(c.whiff_recover);
        }
        if wall_slash && connected && !wallkick {
            self.wall_chain += 1;
            self.last_wall = ctx.now;
        } else {
            self.wall_chain = 0;
        }
        for h in &hits {
            let Some(col) = ctx.world.colliders.get(*h) else { continue };
            let mult = if rope_hits.contains(h) { strike_dmg.max(c.rope_damage_mult) } else { strike_dmg };
            ctx.events.push(CombatEvent::SlashHit { target: col.user_data, point: Point::from(*col.translation()), killed: false });
            ctx.events.push(CombatEvent::Damage {
                target: col.user_data,
                collider: *h,
                amount: if matches!(mode, Mode::Whirl) { amount } else { c.damage * mult * move_mult }, // rope slash bonus rides in `mult`
                kind: DamageKind::Slash,
                point: Point::from(*col.translation()),
            });
        }

        self.flip = !self.flip;
        ctx.events.push(CombatEvent::Slash {
            flip: self.flip,
            dir,
            lunge,
            hits: hits.len() as u32,
            origin: Point::from(origin),
            reach: ev_reach,
            half_width: ev_half,
            half_height: c.half_height,
            damage: amount,
            kind,
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::{CharacterSystem, PhysicsWorld, PlayerInput};
    use crate::combat::{Aim, CombatInput, CombatSystem};

    const DT: f32 = 1.0 / 60.0;

    /// With no targets around, `predict_slice` must decide exactly what the real `Katana::fire` does: same
    /// input mutation, same cooldown timeline, same lunge/no-lunge, including while the dash is on cooldown.
    #[test]
    fn predict_slice_matches_the_real_katana() {
        let cfg = Cfg::default();
        let mut w = PhysicsWorld {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            queries: QueryPipeline::new(),
        };
        w.colliders.insert(
            ColliderBuilder::cuboid(100.0, 0.5, 100.0)
                .translation(vector![0.0, -0.5, 0.0])
                .collision_groups(InteractionGroups::new(crate::combat::GROUP_WORLD, Group::ALL)) // not a target
                .build(),
        );
        let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![0.0, 0.81, 0.0]).build());
        w.colliders.insert_with_parent(
            ColliderBuilder::capsule_y(cfg.half_height, cfg.radius)
                .collision_groups(InteractionGroups::new(crate::combat::GROUP_PLAYER, Group::ALL))
                .build(),
            body,
            &mut w.bodies,
        );
        let mut ch = CharacterSystem::new(body, cfg);
        let mut combat = CombatSystem::ninja(&cfg);
        let k = KatanaCfg::for_character(&cfg);
        let mut cd = 0.0f32;
        let mut now = 0.0;
        let mut swings = (0, 0);
        for t in 0..2000u32 {
            // slice taps at irregular intervals, dash taps that sometimes steal the dash cooldown first
            let slice = t % 25 == 3 || t % 91 == 50;
            let dash = t % 100 == 76 || t % 53 == 20;
            let yaw = t as f32 * 0.05;
            w.queries.update(&w.colliders);

            let aim = Aim { origin: vector![0.0, 0.81, 0.0], yaw, pitch: 0.0 };
            let mut real = PlayerInput { dash_pressed: dash, ..Default::default() };
            let mut ev = Vec::new();
            combat.update(DT, now, &CombatInput::new(false, slice), &aim, &w, &ch, &mut real, &mut ev);
            let real_slash = ev.iter().find_map(|e| match e {
                CombatEvent::Slash { lunge, .. } => Some(*lunge),
                _ => None,
            });

            let mut pred = PlayerInput { dash_pressed: dash, ..Default::default() };
            let got = predict_slice(k.cooldown, &mut cd, DT, slice, ch.dash_ready(), ch.strike_kind(), aim.flat_forward(), &mut pred);

            assert_eq!(got, real_slash, "tick {t}: swing/lunge decision");
            assert_eq!((pred.dash_pressed, pred.move_dir), (real.dash_pressed, real.move_dir), "tick {t}: input");
            assert!((cd - combat.slice_cooldown_left()).abs() < 1e-6, "tick {t}: cooldown {cd} vs {}", combat.slice_cooldown_left());
            if let Some(l) = got {
                swings.0 += 1;
                swings.1 += l as u32;
            }
            ch.set_slice_cooldown(cd);
            crate::movement::step_player(&mut ch, &real, &mut w, now, DT);
            now += DT;
        }
        assert!(swings.0 >= 10 && swings.1 >= 5 && swings.1 < swings.0, "covers lunges and cooldown-blocked swings: {swings:?}");
    }

    /// Drives the demo katana against a standing target: returns `(time, damage)` of every hit while the slice button
    /// is clicked as fast as it will go (a press every other tick; a held button no longer auto-repeats) for `secs`; the
    /// target is out of reach during `away` (a whiff).
    fn hold_slice(secs: f32, away: Option<(f32, f32)>) -> Vec<(f32, f32)> {
        let cfg = Cfg::default();
        let mut w = PhysicsWorld {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            queries: QueryPipeline::new(),
        };
        let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![0.0, 0.81, 0.0]).build());
        let target = |z: f32| {
            ColliderBuilder::cuboid(0.4, 0.9, 0.4)
                .translation(vector![0.0, 0.81, z])
                .collision_groups(InteractionGroups::new(crate::combat::GROUP_ENEMY, Group::ALL))
                .user_data(7)
                .build()
        };
        let tgt = w.colliders.insert(target(-1.6));
        let mut ch = CharacterSystem::new(body, cfg);
        ch.state = crate::character::MoveState::Grounded; // a floating test body would be airborne: every cut an air cut
        let mut combat = CombatSystem::ninja(&cfg);
        combat.apply_balance(&crate::balance::Balance::demo(), &cfg);
        let aim = Aim { origin: vector![0.0, 0.81, 0.0], yaw: 0.0, pitch: 0.0 };
        let mut hits = Vec::new();
        let (mut tgt, mut gone) = (tgt, false);
        let mut now = 0.0f32;
        let mut step = 0u32;
        while now < secs {
            let want_gone = away.map_or(false, |(a, b)| now >= a && now < b);
            if want_gone && !gone {
                w.colliders.remove(tgt, &mut w.islands, &mut w.bodies, false);
                gone = true;
            } else if !want_gone && gone {
                tgt = w.colliders.insert(target(-1.6));
                gone = false;
            }
            w.queries.update(&w.colliders);
            let mut input = PlayerInput::default();
            let mut ev = Vec::new();
            combat.update(DT, now, &CombatInput::new(false, step % 2 == 0), &aim, &w, &ch, &mut input, &mut ev);
            step += 1;
            for e in &ev {
                if let CombatEvent::Damage { amount, kind: DamageKind::Slash, .. } = e {
                    hits.push((now, *amount));
                }
            }
            now += DT;
        }
        hits
    }

    /// The demo combo: 20, 20, then a 30 finisher, then it starts over; swings 0.42 s apart. A whiff resets it.
    #[test]
    fn the_third_connected_cut_is_the_finisher_and_a_whiff_resets_the_combo() {
        let hits = hold_slice(2.4, None);
        let dmg: Vec<f32> = hits.iter().map(|h| h.1).collect();
        assert!(dmg.len() >= 5, "clicking keeps cutting: {hits:?}");
        // the combo counter adds 5 % per cut once the combo carries past the finisher: 20, 20, 30, then 20 +5 %, +10 %
        let want = [20.0f32, 20.0, 30.0, 20.0 * 1.05, 20.0 * 1.10];
        assert!(dmg[..5].iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-3), "combo: two cuts, a finisher, the counter keeps climbing: {dmg:?}");
        let gaps: Vec<f32> = hits.windows(2).map(|w| w[1].0 - w[0].0).collect();
        assert!(gaps.iter().all(|g| (0.30..0.47).contains(g)), "swings come round every ~0.42 s, quicker as the counter climbs: {gaps:?}");
        // five cuts = 20+20+30+20+20 = 110 >= 100 hp: a kill ~1.7 s after the first cut
        let ttk = hits[4].0 - hits[0].0;
        eprintln!("katana-only kill: 5 cuts in {ttk:.2} s");
        assert!((1.4..2.0).contains(&ttk), "katana-only time to kill {ttk}");

        // the target steps out of reach for one swing after the second cut (a whiff) and comes back: the combo starts over
        let hits = hold_slice(2.4, Some((0.6, 1.1)));
        let dmg: Vec<f32> = hits.iter().map(|h| h.1).collect();
        assert!(dmg[..3] == [20.0, 20.0, 20.0], "the whiff took the finisher (and the counter) away: {hits:?}");
    }

    /// A demo-balance swordsman at the origin facing a standing target 1.6 m ahead, driven swing by swing.
    struct Rig {
        w: PhysicsWorld,
        ch: CharacterSystem,
        combat: CombatSystem,
        now: f32,
    }

    impl Rig {
        fn new() -> Self {
            let cfg = Cfg::default();
            let mut w = PhysicsWorld { bodies: RigidBodySet::new(), colliders: ColliderSet::new(), islands: IslandManager::new(), queries: QueryPipeline::new() };
            let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![0.0, 0.81, 0.0]).build());
            w.colliders.insert(
                ColliderBuilder::cuboid(0.4, 0.9, 0.4)
                    .translation(vector![0.0, 0.81, -1.6])
                    .collision_groups(InteractionGroups::new(crate::combat::GROUP_ENEMY, Group::ALL))
                    .user_data(7)
                    .build(),
            );
            let mut ch = CharacterSystem::new(body, cfg);
            ch.state = crate::character::MoveState::Grounded;
            let mut combat = CombatSystem::ninja(&cfg);
            combat.apply_balance(&crate::balance::Balance::demo(), &cfg);
            Self { w, ch, combat, now: 0.0 }
        }

        /// Advance `secs` with no input.
        fn idle(&mut self, secs: f32) {
            let mut t = 0.0;
            while t < secs {
                self.tick(false, PlayerInput::default());
                t += DT;
            }
        }

        fn tick(&mut self, slice: bool, mut input: PlayerInput) -> Vec<CombatEvent> {
            self.w.queries.update(&self.w.colliders);
            let aim = Aim { origin: vector![0.0, 0.81, 0.0], yaw: 0.0, pitch: 0.0 };
            let mut ev = Vec::new();
            self.combat.update(DT, self.now, &CombatInput::new(false, slice), &aim, &self.w, &self.ch, &mut input, &mut ev);
            self.now += DT;
            ev
        }

        /// Release, click, and report the damage of the cut that came out (None = whiffed or not ready).
        fn cut(&mut self) -> Option<f32> {
            self.tick(false, PlayerInput::default());
            let ev = self.tick(true, PlayerInput::default());
            ev.iter().find_map(|e| match e {
                CombatEvent::Slash { hits, damage, .. } if *hits > 0 => Some(*damage),
                _ => None,
            })
        }

        /// Wait out the recovery, then cut.
        fn cut_when_ready(&mut self) -> Option<f32> {
            while self.combat.slice_cooldown_left() > 0.0 {
                self.tick(false, PlayerInput::default());
            }
            self.cut()
        }
    }

    fn wall() -> crate::character::MoveState {
        crate::character::MoveState::WallRun { normal: vector![0.0, 0.0, 1.0], tangent: vector![1.0, 0.0, 0.0], time_left: 1.0 }
    }

    #[test]
    fn the_combo_counter_adds_damage_and_speed_and_a_hit_taken_resets_it() {
        let mut r = Rig::new();
        let first = r.cut().unwrap();
        assert_eq!(first, 20.0, "no stacks yet");
        assert_eq!(r.combat.combo_count(), 1);
        let cd1 = r.combat.slice_cooldown_left();
        let second = r.cut_when_ready().unwrap();
        assert_eq!(second, 20.0, "a pair of cuts adds no damage yet");
        // the recovery shortens with the stacks: 4 % per stack off the 0.42 s swing
        assert!(r.combat.slice_cooldown_left() < cd1 - 0.01, "quicker: {} vs {cd1}", r.combat.slice_cooldown_left());
        let third = r.cut_when_ready().unwrap();
        assert_eq!(third, 30.0, "the third cut is the finisher");
        let fourth = r.cut_when_ready().unwrap();
        assert!((fourth - 21.0).abs() < 1e-3, "carrying on past the finisher: +5 %: {fourth}");
        assert_eq!(r.combat.combo_count(), 4);
        // taking a hit resets it
        r.combat.on_hurt();
        assert_eq!(r.combat.combo_count(), 0);
        let after = r.cut_when_ready().unwrap();
        assert_eq!(after, 20.0, "back to the base cut");
        // idling out the window resets it too
        r.idle(1.2);
        assert_eq!(r.combat.combo_count(), 0);
    }

    #[test]
    fn a_whiff_resets_the_combo_counter() {
        let mut r = Rig::new();
        r.cut().unwrap();
        r.cut_when_ready().unwrap();
        assert_eq!(r.combat.combo_count(), 2);
        let tgt = r.w.colliders.iter().find(|(_, c)| c.user_data == 7).map(|(h, _)| h).unwrap();
        r.w.colliders.remove(tgt, &mut r.w.islands, &mut r.w.bodies, false);
        assert!(r.cut_when_ready().is_none(), "nothing to hit");
        assert_eq!(r.combat.combo_count(), 0);
    }

    #[test]
    fn a_whiffed_cut_commits_you_longer_than_a_connected_one() {
        let mut hit = Rig::new();
        hit.cut().unwrap();
        hit.cut_when_ready().unwrap();
        hit.cut_when_ready().unwrap();
        let after_hit = hit.combat.slice_cooldown_left();
        let mut miss = Rig::new();
        let tgt = miss.w.colliders.iter().find(|(_, c)| c.user_data == 7).map(|(h, _)| h).unwrap();
        miss.w.colliders.remove(tgt, &mut miss.w.islands, &mut miss.w.bodies, false);
        assert!(miss.cut().is_none());
        let after_miss = miss.combat.slice_cooldown_left();
        eprintln!("recovery after a connected streak {after_hit:.2} s, after a whiff {after_miss:.2} s");
        assert!(after_miss >= 0.44, "a whiff locks the blade out ~0.45 s: {after_miss}");
        assert!(after_hit < after_miss, "a connected streak recovers quicker than a whiff");
        // a cancel into a dash still frees the blade (it costs energy)
        miss.tick(false, PlayerInput { dash_pressed: true, ..Default::default() });
        assert!(miss.combat.slice_cooldown_left() < 0.05, "dash cancels the whiff recovery too");
    }

    #[test]
    fn three_wall_run_slashes_chain_into_a_wall_kick_finisher() {
        let mut r = Rig::new();
        r.ch.state = wall();
        let d: Vec<f32> = (0..5).map(|_| r.cut_when_ready().unwrap()).collect();
        let slash = |stacks: f32| 20.0 * 1.35 * (1.0 + 0.05 * (stacks - 2.0).max(0.0));
        for (i, want) in [slash(0.0), slash(1.0), slash(2.0)].iter().enumerate() {
            assert!((d[i] - want).abs() < 0.05, "wall slash {i}: {} vs {want}", d[i]);
        }
        let kick = 20.0 * 1.35 * 1.6 * 1.05;
        assert!((d[3] - kick).abs() < 0.05, "the fourth wall cut is the wall-kick: {} vs {kick}", d[3]);
        assert!(d[3] > d[2] * 1.5);
        // the chain starts over after the kick
        assert!((d[4] - slash(4.0)).abs() < 0.05, "fifth: {}", d[4]);
    }

    #[test]
    fn a_broken_wall_chain_loses_the_wall_kick() {
        let mut r = Rig::new();
        r.ch.state = wall();
        r.cut_when_ready().unwrap();
        r.cut_when_ready().unwrap();
        // a plain cut off the wall in between breaks the chain
        r.ch.state = crate::character::MoveState::Grounded;
        r.cut_when_ready().unwrap();
        r.ch.state = wall();
        let d = r.cut_when_ready().unwrap();
        assert!(d < 20.0 * 1.35 * 1.4, "just a wall slash: {d}");
    }

    #[test]
    fn hook_and_wall_jump_into_an_air_cut_hit_harder() {
        let air = |hook: f32, wj: f32| {
            let mut r = Rig::new();
            r.ch.state = crate::character::MoveState::Airborne;
            r.ch.hook_age = hook;
            r.ch.walljump_age = wj;
            r.cut().unwrap()
        };
        let base = air(9.0, 9.0);
        assert!((base - 25.0).abs() < 1e-3, "plain air cut: {base}");
        assert!((air(0.5, 9.0) - 25.0 * 1.3).abs() < 1e-3, "hook -> air-slash");
        assert!((air(9.0, 0.5) - 25.0 * 1.25).abs() < 1e-3, "wall-jump -> air-slash");
        assert!((air(1.5, 9.0) - base).abs() < 1e-3, "the hook window is over");
    }

    #[test]
    fn a_dash_hook_or_wall_jump_cancels_the_recovery_for_a_little_energy() {
        let energy = |r: &Rig| r.combat.shuriken_energy().map(|e| e.0);
        // dash
        let mut r = Rig::new();
        r.cut().unwrap();
        let e0 = energy(&r);
        assert!(r.combat.slice_cooldown_left() > 0.2);
        r.tick(false, PlayerInput::default());
        assert!(r.combat.slice_cooldown_left() > 0.2, "no cancel without an input");
        r.tick(false, PlayerInput { dash_pressed: true, ..Default::default() });
        assert_eq!(r.combat.slice_cooldown_left(), 0.0, "dash cancels the cut");
        if let (Some(a), Some(b)) = (e0, energy(&r)) {
            assert!(b < a + 0.1 - 0.3, "and costs energy: {a} -> {b}");
        }
        // hook
        let mut r = Rig::new();
        r.cut().unwrap();
        r.tick(false, PlayerInput { grapple_aim: Some(vector![0.0, 0.3, -1.0]), ..Default::default() });
        assert_eq!(r.combat.slice_cooldown_left(), 0.0, "hook cancels the cut");
        // wall jump needs a wall
        let mut r = Rig::new();
        r.cut().unwrap();
        r.tick(false, PlayerInput { jump_pressed: true, ..Default::default() });
        assert!(r.combat.slice_cooldown_left() > 0.2, "a jump on the ground is no cancel");
        r.ch.state = wall();
        r.tick(false, PlayerInput { jump_pressed: true, ..Default::default() });
        assert_eq!(r.combat.slice_cooldown_left(), 0.0, "wall jump cancels the cut");
        // an empty pool refuses the cancel
        let mut r = Rig::new();
        r.cut().unwrap();
        if let Some(d) = r.combat.loadout.discs_mut() {
            d.clear();
        }
        let spent = r.combat.shuriken_energy().is_some();
        if spent {
            for _ in 0..30 {
                r.combat.loadout.cancel_slice(); // burn the pool: every call needs a recovery to cut, so re-arm it
                r.combat.set_slice_cooldown_left(0.4);
            }
            r.combat.set_slice_cooldown_left(0.4);
            let (e, _, _) = r.combat.shuriken_energy().unwrap();
            assert!(e < 0.4, "pool drained to {e}");
            r.tick(false, PlayerInput { dash_pressed: true, ..Default::default() });
            assert!(r.combat.slice_cooldown_left() > 0.2, "cannot pay: no cancel");
        }
    }
}
