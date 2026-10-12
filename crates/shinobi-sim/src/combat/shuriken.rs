//! Neon shurikens: physical discs (no hitscan) that mirror-bounce off solids.
//!
//! Discs are stepped with swept ball casts + explicit reflection (`v - 2(v.n)n`) instead of the
//! solver: bounce count and speed are exact and deterministic for server/client. They never enter
//! the shared `PhysicsWorld` body set, so they can't disturb kinematic players/enemies.
use rapier3d::parry::query::ShapeCastOptions;
use rapier3d::prelude::*;

use crate::character::PhysicsWorld;

use super::stars::{StarCfg, StarKind};
use super::{
    passes_projectile, projectile_groups, TARGETS,
    weapon::{FireCtx, TickCtx, Trigger, Weapon},
    CombatEvent, DamageKind, GROUP_PROJECTILE,
};

#[derive(Clone, Copy, Debug)]
pub struct ShurikenCfg {
    pub speed: f32,
    pub damage: f32,
    /// Solid hits survived; the disc dies on hit number `max_bounces + 1`.
    pub max_bounces: u8,
    pub cooldown: f32,
    pub muzzle: f32,
    pub radius: f32,
    pub max_life: f32,
    pub max_discs: usize,
    pub recoil_pitch_deg: f32,
    /// Aim assist: a throw whose aim passes within this many degrees of a visible target is bent toward it
    /// (0 = off). Server and client prediction run the same code, so the shot you see is the shot that lands.
    pub assist_cone_deg: f32,
    /// How far (0..=1) the throw is pulled from the crosshair toward the target's centre.
    pub assist_strength: f32,
    /// Fraction of the lead (target velocity x flight time) a locked throw aims ahead by (0 = off, 1 = full lead).
    pub assist_lead: f32,
    pub assist_range: f32,
    /// A locked throw curves toward its target at up to this many rad/s until it bounces (0 = straight flight).
    pub homing_turn: f32,
    /// Fraction of its speed a disc keeps after each bounce (1 = no loss).
    pub bounce_retention: f32,
    /// Damage a bounce does to a player-built piece it strikes (Balance `combat.shuriken.piece_damage`).
    pub piece_damage: f32,
    /// Star energy: a pool of `energy_max` charges (0 = no pool, unlimited), one star costs `energy_cost`, a charge comes
    /// back every `energy_regen_s` seconds. Emptying the pool locks throwing until `energy_resume` charges are back, so
    /// stars can be fired in measured volleys but not spammed across the map.
    pub energy_max: f32,
    pub energy_cost: f32,
    pub energy_regen_s: f32,
    pub energy_resume: f32,
    /// Charges a MARK star (hold to charge) costs: it does no damage and marks its target for the katana.
    pub mark_cost: f32,
    /// The barrel / canister / flash variants (`combat.star.*`).
    pub star: StarCfg,
    /// Collider groups this disc damages when it touches them.
    pub targets: Group,
    /// Collider groups it can touch at all (everything else is ignored).
    pub mask: Group,
}

impl Default for ShurikenCfg {
    fn default() -> Self {
        Self {
            speed: 40.0,
            damage: 25.0,
            max_bounces: 2,
            cooldown: 0.18,
            muzzle: 0.8,
            radius: 0.6, // generous: a star that grazes the body counts (hit reach = hitbox 0.3 + 0.6)
            max_life: 6.0,
            max_discs: 32,
            recoil_pitch_deg: 1.2,
            assist_cone_deg: 10.0,
            assist_strength: 0.95,
            assist_lead: 1.0,
            assist_range: 80.0,
            homing_turn: 5.0,
            bounce_retention: 0.9,
            piece_damage: 12.0,
            energy_max: 0.0,
            energy_cost: 1.0,
            energy_regen_s: 0.45,
            energy_resume: 2.0,
            mark_cost: 2.0,
            star: StarCfg::default(),
            targets: TARGETS,
            mask: projectile_groups(),
        }
    }
}

/// Radius of a canister's flight ball: small, so it lands on the surface it was thrown at.
const STICK_RADIUS: f32 = 0.3;
/// Radius of a barrel's flight ball.
const BARREL_RADIUS: f32 = 0.45;
/// A landed explosive that nobody detonated (client prediction, an unwatched copy) is dropped this long after its fuse.
pub const FUSE_FAILSAFE_S: f32 = 0.6;

#[derive(Clone, Copy, Debug, Default)]
pub struct Disc {
    pub pos: Point<Real>,
    /// Flight direction (for a barrel: its velocity direction, it arcs).
    pub dir: Vector<Real>,
    pub bounces: u8,
    pub age: f32,
    /// Multiplier on `cfg.speed` (bounces bleed speed, see `bounce_retention`).
    pub speed_scale: f32,
    /// Target hitbox a locked throw curves toward until its first bounce.
    pub homing: Option<ColliderHandle>,
    /// A mark star: no damage, it marks the player it strikes (`melee.rs`).
    pub mark: bool,
    /// Which throwable this is (`Standard` unless thrown with a star variant selected).
    pub kind: StarKind,
    /// Landed (a barrel at rest, a stuck canister): it no longer flies, `stick_age` counts the fuse from the landing.
    pub stuck: bool,
    pub stick_age: f32,
    /// Surface normal at the landing (glyph orientation).
    pub normal: Vector<Real>,
    /// Unique per launcher: lets the replicator keep a stable entity id for a stuck object.
    pub uid: u16,
    /// A flying barrel's velocity (m/s); zero for everything else.
    pub vel: Vector<Real>,
    /// A canister stuck to a player: that player's hitbox, and where on them it sits (relative to their position, set by
    /// the server's `star_pass` on the first tick).
    pub attach: Option<ColliderHandle>,
    pub attach_off: Option<Vector<Real>>,
}

impl Disc {
    /// A barrel at rest or a stuck canister: its fuse is burning.
    pub fn armed(&self, _cfg: &StarCfg) -> bool {
        self.kind.explosive() && self.stuck
    }

    /// Has a landed explosive's fuse run out (the server detonates it)?
    pub fn fuse_done(&self, cfg: &StarCfg) -> bool {
        self.kind.explosive() && self.stuck && self.stick_age >= cfg.fuse(self.kind)
    }

    /// Fraction of a landed explosive's fuse left, 1 = just landed (0 for flying discs).
    pub fn life_left(&self, cfg: &StarCfg) -> f32 {
        let fuse = cfg.fuse(self.kind);
        if !self.stuck || fuse <= 0.0 {
            return 0.0;
        }
        (1.0 - self.stick_age / fuse).clamp(0.0, 1.0)
    }
}

pub struct ShurikenLauncher {
    pub cfg: ShurikenCfg,
    discs: Vec<Disc>,
    next_uid: u16,
    /// Star energy in charges (only meaningful with `cfg.energy_max > 0`) and the empty-pool lockout.
    energy: f32,
    locked: bool,
}

impl Default for ShurikenLauncher {
    fn default() -> Self {
        Self::new(ShurikenCfg::default())
    }
}

impl ShurikenLauncher {
    pub fn new(cfg: ShurikenCfg) -> Self {
        Self { cfg, discs: Vec::new(), next_uid: 0, energy: cfg.energy_max, locked: false }
    }

    /// (energy, max, locked) when a pool is in force.
    pub fn energy_state(&self) -> Option<(f32, f32, bool)> {
        (self.cfg.energy_max > 0.0).then_some((self.energy, self.cfg.energy_max, self.locked))
    }

    /// For the renderer (spin the mesh by `age`).
    pub fn discs(&self) -> &[Disc] {
        &self.discs
    }

    /// The server's world pass removes the explosives it detonated.
    pub fn discs_mut(&mut self) -> &mut Vec<Disc> {
        &mut self.discs
    }

    /// Client prediction: a landed barrel / canister is the server's to track from here on (it can detonate or be shot);
    /// the predicted copy is only kept for `grace` seconds so the object does not blink out before the snapshot
    /// brings the authoritative one.
    pub fn drop_landed_after(&mut self, grace: f32) {
        self.discs.retain(|d| !(d.stuck && d.stick_age > grace));
    }
}

impl Weapon for ShurikenLauncher {
    fn name(&self) -> &'static str {
        "Neon Shuriken"
    }
    fn trigger(&self) -> Trigger {
        Trigger::Primary
    }
    fn cooldown(&self) -> f32 {
        self.cfg.cooldown
    }

    fn energy(&self) -> Option<(f32, f32, bool)> {
        self.energy_state()
    }

    fn fire(&mut self, ctx: &mut FireCtx) -> bool {
        if self.discs.len() >= self.cfg.max_discs {
            return false;
        }
        let kind = if ctx.mark { StarKind::Standard } else { ctx.star };
        // star energy: a star (or a mark star) must be affordable and the pool must not be locked out
        if self.cfg.energy_max > 0.0 {
            let cost = if ctx.mark { self.cfg.mark_cost } else { self.cfg.star.cost(kind, self.cfg.energy_cost) };
            if self.locked || self.energy + 1e-4 < cost {
                return false; // refused: the cooldown is not consumed either
            }
            self.energy -= cost;
            if self.energy + 1e-4 < self.cfg.energy_cost {
                self.locked = true; // cannot afford another star: locked until `energy_resume` charges are back
            }
        }
        // the variants fly where the crosshair is (no aim assist, no homing); a barrel then arcs under gravity
        let plain = kind != StarKind::Standard;
        let dir = if plain { ctx.aim.forward() } else { assisted_dir(&self.cfg, ctx) };
        let pos = Point::from(ctx.aim.origin + dir * self.cfg.muzzle);
        let homing = ctx.lock.filter(|h| !plain && h.pull >= 0.5 && self.cfg.homing_turn > 0.0 && self.cfg.assist_strength > 0.0).map(|h| h.collider);
        let vel = if kind == StarKind::Barrel { dir * self.cfg.star.barrel_speed } else { Vector::zeros() };
        let uid = self.next_uid;
        self.next_uid = self.next_uid.wrapping_add(1);
        self.discs.push(Disc { pos, dir, bounces: 0, age: 0.0, speed_scale: 1.0, homing, mark: ctx.mark, kind, stuck: false, stick_age: 0.0, normal: Vector::y(), uid, vel, attach: None, attach_off: None });
        ctx.events.push(CombatEvent::ShurikenThrown { pos, dir, kind });
        ctx.events.push(CombatEvent::Recoil { pitch_deg: self.cfg.recoil_pitch_deg, yaw_deg: 0.0 });
        true
    }

    fn apply_balance(&mut self, b: &crate::balance::Balance, _ch: &crate::character::Cfg) {
        self.cfg = b.shuriken_cfg();
        self.energy = self.cfg.energy_max;
        self.locked = false;
    }

    fn active(&self) -> bool {
        !self.discs.is_empty()
    }

    fn projectiles(&self, out: &mut Vec<([f32; 3], [f32; 3])>) {
        // landed barrels / canisters are world objects (`stars`), not flying discs
        out.extend(self.discs.iter().filter(|d| !d.stuck).map(|d| ([d.pos.x, d.pos.y, d.pos.z], [d.dir.x, d.dir.y, d.dir.z])));
    }

    fn spend_energy(&mut self, amount: f32) -> bool {
        if self.cfg.energy_max <= 0.0 {
            return true;
        }
        if self.locked || self.energy + 1e-4 < amount {
            return false;
        }
        self.energy -= amount;
        true
    }

    fn discs_mut(&mut self) -> Option<&mut Vec<Disc>> {
        Some(&mut self.discs)
    }

    fn reset(&mut self) {
        self.energy = self.cfg.energy_max; // a new life starts with a full pool
        self.locked = false;
        self.discs.clear();
    }

    fn collect_discs(&self, out: &mut Vec<Disc>) {
        out.extend_from_slice(&self.discs);
    }

    fn tick(&mut self, dt: f32, ctx: &mut TickCtx) {
        let cfg = self.cfg;
        if cfg.energy_max > 0.0 {
            self.energy = (self.energy + dt / cfg.energy_regen_s.max(1e-3)).min(cfg.energy_max);
            if self.locked && self.energy + 1e-4 >= cfg.energy_resume.min(cfg.energy_max) {
                self.locked = false;
            }
        }
        self.discs.retain_mut(|d| {
            if d.kind != StarKind::Standard {
                return step_variant(ctx.world, d, &cfg, dt, Some(ctx.body), ctx.allies, ctx.events);
            }
            match advance_disc_skipping(ctx.world, d, &cfg, dt, Some(ctx.body), ctx.allies, ctx.events) {
            DiscStep::Flying => true,
            DiscStep::Hit { collider, point } => {
                let target = ctx.world.colliders.get(collider).map_or(0, |c| c.user_data);
                if d.bounces > 0 {
                    ctx.events.push(CombatEvent::RicochetHit { collider });
                }
                if d.mark {
                    ctx.events.push(CombatEvent::MarkHit { target, collider });
                } else {
                    ctx.events.push(CombatEvent::Damage { target, collider, amount: cfg.damage, kind: DamageKind::Shuriken, point });
                }
                ctx.events.push(CombatEvent::ShurikenGone { pos: point });
                false
            }
            DiscStep::Gone => false,
            }
        });
    }
}

/// One tick of a barrel / flash / canister star. Returns false when it is gone. Flash and canister fly straight on a swept
/// ball cast: a flash bursts on the first solid or player (`StarFlash`) or when its fuse runs out; a canister sticks to the
/// first solid or player (`StarStuck`). A barrel arcs under gravity, bounces off solids (restitution, friction) until it
/// comes to rest (`StarStuck`), and goes off at once when it strikes a player. A landed explosive only ages (the server's
/// `star_pass` detonates it when the fuse is out).
fn step_variant(
    world: &PhysicsWorld,
    d: &mut Disc,
    cfg: &ShurikenCfg,
    dt: f32,
    exclude: Option<RigidBodyHandle>,
    skip: &[ColliderHandle],
    events: &mut Vec<CombatEvent>,
) -> bool {
    d.age += dt;
    let sc = &cfg.star;
    if d.stuck {
        d.stick_age += dt;
        if d.stick_age > sc.fuse(d.kind) + FUSE_FAILSAFE_S {
            events.push(CombatEvent::StarGone { kind: d.kind, pos: d.pos });
            return false;
        }
        return true;
    }
    if d.age > cfg.max_life {
        return false;
    }
    if d.kind == StarKind::Flash && d.age >= sc.flash_fuse_s {
        events.push(CombatEvent::StarFlash { pos: d.pos });
        return false;
    }
    // every variant bursts / lands on a player's hitbox as well as on geometry (allies and the thrower are skipped)
    let radius = match d.kind {
        StarKind::Flash => cfg.radius,
        StarKind::Barrel => BARREL_RADIUS,
        _ => STICK_RADIUS,
    };
    let pass = |h: ColliderHandle, c: &Collider| passes_projectile(h, c) && !skip.contains(&h);
    let mut filter = QueryFilter::default().predicate(&pass).groups(InteractionGroups::new(GROUP_PROJECTILE, cfg.mask));
    if let Some(b) = exclude {
        filter = filter.exclude_rigid_body(b);
    }
    let barrel = d.kind == StarKind::Barrel;
    if barrel {
        d.vel.y -= sc.barrel_gravity * dt;
        let sp = d.vel.norm();
        if sp > 1e-3 && sp.is_finite() {
            d.dir = d.vel / sp;
        }
    }
    let dist = if barrel { d.vel.norm() * dt } else { sc.speed(d.kind, cfg.speed) * dt };
    let hit = world.queries.cast_shape(
        &world.bodies,
        &world.colliders,
        &Isometry::from(d.pos.coords),
        &d.dir,
        &Ball::new(radius),
        ShapeCastOptions { max_time_of_impact: dist, target_distance: 0.0, stop_at_penetration: true, compute_impact_geometry_on_penetration: true },
        filter,
    );
    let Some((handle, h)) = hit else {
        d.pos += d.dir * dist;
        if !d.pos.coords.iter().all(|c| c.is_finite()) {
            return false;
        }
        return true;
    };
    d.pos += d.dir * h.time_of_impact;
    let mut n = h.normal1.into_inner();
    if !(n.iter().all(|c| c.is_finite()) && n.norm() > 0.5) {
        n = -d.dir;
    }
    if d.dir.dot(&n) > 0.0 {
        n = -n;
    }
    n = n.normalize();
    if !d.pos.coords.iter().all(|c| c.is_finite()) {
        return false; // never feed NaN to the query pipeline
    }
    let on_target = world.colliders.get(handle).map_or(false, |c| c.collision_groups().memberships.intersects(cfg.targets));
    match d.kind {
        StarKind::Flash => {
            // burst just off the surface so nothing about the wall itself matters
            let at = if on_target { d.pos } else { d.pos + n * 0.1 };
            events.push(CombatEvent::StarFlash { pos: at });
            false
        }
        StarKind::Barrel if !on_target => {
            let vn = d.vel.dot(&n); // negative: into the surface
            let v_t = d.vel - n * vn;
            // came to rest: nearly no speed into a floor-ish surface
            if n.y > 0.6 && -vn < 3.0 {
                d.vel = Vector::zeros();
                d.stuck = true;
                d.stick_age = 0.0;
                d.normal = n;
                events.push(CombatEvent::StarStuck { kind: d.kind, pos: d.pos, normal: n });
                return true;
            }
            d.vel = v_t * sc.barrel_grip - n * (vn * sc.barrel_bounce);
            let sp = d.vel.norm();
            if sp > 1e-3 && sp.is_finite() {
                d.dir = d.vel / sp;
            }
            d.bounces = d.bounces.saturating_add(1);
            d.pos += n * 1e-3;
            events.push(CombatEvent::ShurikenBounce { pos: d.pos, normal: n });
            true
        }
        _ => {
            // a canister meeting a surface rests on it itself (the ball's centre met it a radius off); a canister on a player
            // or a barrel that struck one stays where it touched, and a barrel's fuse is spent at once (the server bursts it)
            if !on_target {
                d.pos += n * (0.02 - radius);
            } else if d.kind == StarKind::Canister {
                d.attach = Some(handle);
            }
            d.vel = Vector::zeros();
            d.stuck = true;
            d.stick_age = if barrel { sc.barrel_fuse_s } else { 0.0 };
            d.normal = n;
            events.push(CombatEvent::StarStuck { kind: d.kind, pos: d.pos, normal: n });
            true
        }
    }
}

/// The crosshair direction, bent toward the lock-on target by `assist_strength` scaled with the lock's pull (weak
/// while acquiring, full at LOCK). Never reaches past the cone, so it only forgives near-misses.
fn assisted_dir(cfg: &ShurikenCfg, ctx: &FireCtx) -> Vector<Real> {
    let fwd = ctx.aim.forward();
    let Some(hint) = ctx.lock else { return fwd };
    if cfg.assist_cone_deg <= 0.0 || cfg.assist_strength <= 0.0 {
        return fwd;
    }
    // already on target (or about to hit a wall first): leave the shot alone, so headshots stay headshots
    let pass = |h: ColliderHandle, c: &Collider| passes_projectile(h, c) && !ctx.allies.contains(&h);
    let filter = QueryFilter::default().predicate(&pass).groups(InteractionGroups::new(GROUP_PROJECTILE, cfg.mask)).exclude_rigid_body(ctx.ch.body);
    let ray = Ray::new(Point::from(ctx.aim.origin), fwd);
    let first = ctx.world.queries.cast_ray(&ctx.world.bodies, &ctx.world.colliders, &ray, cfg.assist_range, true, filter);
    let moving = cfg.assist_lead > 0.0 && hint.vel.norm() > 0.5;
    if !moving && first.map_or(false, |(h, _)| ctx.world.colliders.get(h).map_or(false, |c| c.collision_groups().memberships.intersects(cfg.targets))) {
        return fwd;
    }
    // a moving target is led: aim where it will be when the disc arrives (two refinements of the flight time)
    let eye = Point::from(ctx.aim.origin);
    let lead_dir = |lead: f32| {
        let mut p = hint.centre;
        for _ in 0..2 {
            let t = (p - eye).norm() / cfg.speed.max(1.0);
            p = hint.centre + hint.vel * t * lead;
        }
        (p - eye).normalize()
    };
    let aim_at = if moving { lead_dir(cfg.assist_lead.min(1.0)) } else { hint.dir };
    super::aim_assist::bend(fwd, aim_at, cfg.assist_strength * hint.pull)
}

pub enum DiscStep {
    Flying,
    /// Struck something in `cfg.targets` (disc is spent; caller decides the damage).
    Hit { collider: ColliderHandle, point: Point<Real> },
    /// Expired or out of bounces (`ShurikenGone` already pushed).
    Gone,
}

/// Advance one disc by `dt`: swept ball casts with explicit mirror reflection. Used by player shurikens and
/// enemy throwers alike; `cfg.targets` / `cfg.mask` choose who it can hurt and what it ignores.
/// Pushes `ShurikenBounce` / `ShurikenGone` (expiry, spent bounces). A `Hit` is returned, not pushed.
pub fn advance_disc(
    world: &PhysicsWorld,
    d: &mut Disc,
    cfg: &ShurikenCfg,
    dt: f32,
    exclude: Option<RigidBodyHandle>,
    events: &mut Vec<CombatEvent>,
) -> DiscStep {
    advance_disc_skipping(world, d, cfg, dt, exclude, &[], events)
}

/// `advance_disc` that flies straight through the `skip` colliders (teammates' hitboxes): no hit, no bounce.
pub fn advance_disc_skipping(
    world: &PhysicsWorld,
    d: &mut Disc,
    cfg: &ShurikenCfg,
    dt: f32,
    exclude: Option<RigidBodyHandle>,
    skip: &[ColliderHandle],
    events: &mut Vec<CombatEvent>,
) -> DiscStep {
    let shape = Ball::new(cfg.radius);
    let pass = |h: ColliderHandle, c: &Collider| passes_projectile(h, c) && !skip.contains(&h);
    let mut filter = QueryFilter::default().predicate(&pass).groups(InteractionGroups::new(GROUP_PROJECTILE, cfg.mask));
    if let Some(b) = exclude {
        filter = filter.exclude_rigid_body(b);
    }
    d.age += dt;
    if d.age > cfg.max_life {
        events.push(CombatEvent::ShurikenGone { pos: d.pos });
        return DiscStep::Gone;
    }
    // a locked throw curves toward its target (pursuit, turn rate capped), until it bounces or the target is behind it
    if let (Some(h), 0) = (d.homing, d.bounces) {
        match world.colliders.get(h) {
            Some(c) if cfg.homing_turn > 0.0 => {
                let to = c.position().translation.vector - d.pos.coords;
                let dist = to.norm();
                if dist > 1.0 {
                    let want = to / dist;
                    let ang = d.dir.dot(&want).clamp(-1.0, 1.0).acos();
                    if ang > 1.0 {
                        d.homing = None; // it flew past: stop chasing
                    } else if ang > 1e-4 {
                        let f = ((cfg.homing_turn * dt).min(ang)) / ang;
                        let v = d.dir * (1.0 - f) + want * f;
                        if v.norm() > 1e-4 {
                            d.dir = v.normalize();
                        }
                    }
                }
            }
            _ => d.homing = None,
        }
    }
    let mut remaining = cfg.speed * d.speed_scale * dt;
    // a few reflections per tick cover tight corners without looping forever
    for _ in 0..4 {
        if remaining <= 1e-5 {
            break;
        }
        let hit = world.queries.cast_shape(
            &world.bodies,
            &world.colliders,
            &Isometry::from(d.pos.coords),
            &d.dir,
            &shape,
            ShapeCastOptions {
                max_time_of_impact: remaining,
                target_distance: 0.0,
                stop_at_penetration: true,
                compute_impact_geometry_on_penetration: true,
            },
            filter,
        );
        let Some((handle, h)) = hit else {
            d.pos += d.dir * remaining;
            break;
        };
        d.pos += d.dir * h.time_of_impact;
        remaining -= h.time_of_impact;

        if world.colliders.get(handle).map_or(false, |c| c.collision_groups().memberships.intersects(cfg.targets)) {
            return DiscStep::Hit { collider: handle, point: d.pos };
        }

        if let Some(cell) = world.colliders.get(handle).and_then(|c| crate::blocks::piece_cell(c.user_data)) {
            events.push(CombatEvent::PieceHit { cell, amount: cfg.piece_damage, kind: DamageKind::Shuriken });
        }
        d.bounces += 1;
        if d.bounces > cfg.max_bounces {
            events.push(CombatEvent::ShurikenGone { pos: d.pos });
            return DiscStep::Gone;
        }
        let mut n = h.normal1.into_inner(); // world collider's normal, world space
        // a disc spawned inside geometry reports a degenerate normal: just bounce it straight back
        if !(n.iter().all(|c| c.is_finite()) && n.norm() > 0.5) {
            n = -d.dir;
        }
        if d.dir.dot(&n) > 0.0 {
            n = -n;
        }
        d.dir = d.dir - n * (2.0 * d.dir.dot(&n));
        let len = d.dir.norm();
        if !(len.is_finite() && len > 0.5) || !d.pos.coords.iter().all(|c| c.is_finite()) {
            events.push(CombatEvent::ShurikenGone { pos: d.pos });
            return DiscStep::Gone; // never feed NaN to the query pipeline
        }
        d.dir /= len;
        d.speed_scale *= cfg.bounce_retention;
        d.homing = None;
        d.pos += n * 1e-3; // step off the surface
        events.push(CombatEvent::ShurikenBounce { pos: d.pos, normal: n });
    }
    DiscStep::Flying
}
