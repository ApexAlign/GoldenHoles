//! Neon shurikens: physical discs (no hitscan) that mirror-bounce off solids.
//!
//! Discs are stepped with swept ball casts + explicit reflection (`v - 2(v.n)n`) instead of the
//! solver: bounce count and speed are exact and deterministic for server/client. They never enter
//! the shared `PhysicsWorld` body set, so they can't disturb kinematic players/enemies.
use rapier3d::parry::query::ShapeCastOptions;
use rapier3d::prelude::*;

use crate::character::PhysicsWorld;

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
    pub assist_range: f32,
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
            radius: 0.35,
            max_life: 6.0,
            max_discs: 32,
            recoil_pitch_deg: 1.2,
            assist_cone_deg: 6.0,
            assist_strength: 0.75,
            assist_range: 60.0,
            targets: TARGETS,
            mask: projectile_groups(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Disc {
    pub pos: Point<Real>,
    pub dir: Vector<Real>,
    pub bounces: u8,
    pub age: f32,
}

#[derive(Default)]
pub struct ShurikenLauncher {
    pub cfg: ShurikenCfg,
    discs: Vec<Disc>,
}

impl ShurikenLauncher {
    pub fn new(cfg: ShurikenCfg) -> Self {
        Self { cfg, discs: Vec::new() }
    }

    /// For the renderer (spin the mesh by `age`).
    pub fn discs(&self) -> &[Disc] {
        &self.discs
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

    fn fire(&mut self, ctx: &mut FireCtx) -> bool {
        if self.discs.len() >= self.cfg.max_discs {
            return false;
        }
        let dir = assisted_dir(&self.cfg, ctx);
        let pos = Point::from(ctx.aim.origin + dir * self.cfg.muzzle);
        self.discs.push(Disc { pos, dir, bounces: 0, age: 0.0 });
        ctx.events.push(CombatEvent::ShurikenThrown { pos, dir });
        ctx.events.push(CombatEvent::Recoil { pitch_deg: self.cfg.recoil_pitch_deg, yaw_deg: 0.0 });
        true
    }

    fn apply_balance(&mut self, b: &crate::balance::Balance, _ch: &crate::character::Cfg) {
        self.cfg = b.shuriken_cfg();
    }

    fn active(&self) -> bool {
        !self.discs.is_empty()
    }

    fn projectiles(&self, out: &mut Vec<([f32; 3], [f32; 3])>) {
        out.extend(self.discs.iter().map(|d| ([d.pos.x, d.pos.y, d.pos.z], [d.dir.x, d.dir.y, d.dir.z])));
    }

    fn reset(&mut self) {
        self.discs.clear();
    }

    fn tick(&mut self, dt: f32, ctx: &mut TickCtx) {
        let cfg = self.cfg;
        self.discs.retain_mut(|d| match advance_disc(ctx.world, d, &cfg, dt, Some(ctx.body), ctx.events) {
            DiscStep::Flying => true,
            DiscStep::Hit { collider, point } => {
                let target = ctx.world.colliders.get(collider).map_or(0, |c| c.user_data);
                ctx.events.push(CombatEvent::Damage { target, collider, amount: cfg.damage, kind: DamageKind::Shuriken, point });
                ctx.events.push(CombatEvent::ShurikenGone { pos: point });
                false
            }
            DiscStep::Gone => false,
        });
    }
}

/// The crosshair direction, bent toward the best target inside the assist cone (smallest angle off the
/// crosshair, line of sight required). Never reaches past the cone, so it only forgives near-misses.
fn assisted_dir(cfg: &ShurikenCfg, ctx: &FireCtx) -> Vector<Real> {
    let fwd = ctx.aim.forward();
    if cfg.assist_cone_deg <= 0.0 || cfg.assist_strength <= 0.0 {
        return fwd;
    }
    let eye = ctx.aim.origin;
    // already on target (or about to hit a wall first): leave the shot alone, so headshots stay headshots
    let filter = QueryFilter::default().predicate(&passes_projectile).groups(InteractionGroups::new(GROUP_PROJECTILE, cfg.mask)).exclude_rigid_body(ctx.ch.body);
    let ray = Ray::new(Point::from(eye), fwd);
    let first = ctx.world.queries.cast_ray(&ctx.world.bodies, &ctx.world.colliders, &ray, cfg.assist_range, true, filter);
    if first.map_or(false, |(h, _)| ctx.world.colliders.get(h).map_or(false, |c| c.collision_groups().memberships.intersects(cfg.targets))) {
        return fwd;
    }
    let min_cos = cfg.assist_cone_deg.to_radians().cos();
    let mut best: Option<(f32, Vector<Real>)> = None;
    for (_, c) in ctx.world.colliders.iter() {
        if !c.collision_groups().memberships.intersects(cfg.targets) || c.parent() == Some(ctx.ch.body) {
            continue;
        }
        let centre = c.position().translation.vector;
        let to = centre - eye;
        let dist = to.norm();
        if !(0.5..=cfg.assist_range).contains(&dist) {
            continue;
        }
        let to = to / dist;
        let cos = to.dot(&fwd);
        if cos < min_cos || best.map_or(false, |(b, _)| cos <= b) {
            continue;
        }
        if crate::spawn::blocked(ctx.world, [eye.x, eye.y, eye.z], [centre.x, centre.y, centre.z]) {
            continue;
        }
        best = Some((cos, to));
    }
    match best {
        Some((_, to)) => {
            let v = fwd * (1.0 - cfg.assist_strength) + to * cfg.assist_strength;
            let n = v.norm();
            if n > 1e-4 { v / n } else { fwd }
        }
        None => fwd,
    }
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
    let shape = Ball::new(cfg.radius);
    let mut filter = QueryFilter::default().predicate(&passes_projectile).groups(InteractionGroups::new(GROUP_PROJECTILE, cfg.mask));
    if let Some(b) = exclude {
        filter = filter.exclude_rigid_body(b);
    }
    d.age += dt;
    if d.age > cfg.max_life {
        events.push(CombatEvent::ShurikenGone { pos: d.pos });
        return DiscStep::Gone;
    }
    let mut remaining = cfg.speed * dt;
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
        d.pos += n * 1e-3; // step off the surface
        events.push(CombatEvent::ShurikenBounce { pos: d.pos, normal: n });
    }
    DiscStep::Flying
}
