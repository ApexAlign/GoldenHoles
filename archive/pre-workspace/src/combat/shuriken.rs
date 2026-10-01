//! Neon shurikens: physical discs (no hitscan) that mirror-bounce off solids.
//!
//! Discs are stepped with swept ball casts + explicit reflection (`v - 2(v.n)n`) instead of the
//! solver: bounce count and speed are exact and deterministic for server/client. They never enter
//! the shared `PhysicsWorld` body set, so they can't disturb kinematic players/enemies.
use rapier3d::parry::query::ShapeCastOptions;
use rapier3d::prelude::*;

use super::{
    is_enemy,
    weapon::{FireCtx, TickCtx, Trigger, Weapon},
    CombatEvent, DamageKind, GROUP_PROJECTILE,
};
use crate::character::GROUP_TRAIL;

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
}

impl Default for ShurikenCfg {
    fn default() -> Self {
        Self {
            speed: 40.0,
            damage: 25.0,
            max_bounces: 2,
            cooldown: 0.18,
            muzzle: 0.8,
            radius: 0.2,
            max_life: 6.0,
            max_discs: 32,
            recoil_pitch_deg: 1.2,
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
        let dir = ctx.aim.forward();
        let pos = Point::from(ctx.aim.origin + dir * self.cfg.muzzle);
        self.discs.push(Disc { pos, dir, bounces: 0, age: 0.0 });
        ctx.events.push(CombatEvent::ShurikenThrown { pos, dir });
        ctx.events.push(CombatEvent::Recoil { pitch_deg: self.cfg.recoil_pitch_deg, yaw_deg: 0.0 });
        true
    }

    fn tick(&mut self, dt: f32, ctx: &mut TickCtx) {
        let cfg = self.cfg;
        let shape = Ball::new(cfg.radius);
        let filter = QueryFilter::default()
            .exclude_rigid_body(ctx.body)
            .exclude_sensors()
            .groups(InteractionGroups::new(GROUP_PROJECTILE, Group::ALL.difference(GROUP_TRAIL)));

        self.discs.retain_mut(|d| {
            d.age += dt;
            if d.age > cfg.max_life {
                ctx.events.push(CombatEvent::ShurikenGone { pos: d.pos });
                return false;
            }
            let mut remaining = cfg.speed * dt;
            // a few reflections per tick cover tight corners without looping forever
            for _ in 0..4 {
                if remaining <= 1e-5 {
                    break;
                }
                let hit = ctx.world.queries.cast_shape(
                    &ctx.world.bodies,
                    &ctx.world.colliders,
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

                if is_enemy(ctx.world, handle) {
                    let target = ctx.world.colliders.get(handle).map_or(0, |c| c.user_data);
                    ctx.events.push(CombatEvent::Damage {
                        target,
                        collider: handle,
                        amount: cfg.damage,
                        kind: DamageKind::Shuriken,
                        point: d.pos,
                    });
                    ctx.events.push(CombatEvent::ShurikenGone { pos: d.pos });
                    return false;
                }

                d.bounces += 1;
                if d.bounces > cfg.max_bounces {
                    ctx.events.push(CombatEvent::ShurikenGone { pos: d.pos });
                    return false;
                }
                let mut n = h.normal1.into_inner(); // world collider's normal, world space
                if d.dir.dot(&n) > 0.0 {
                    n = -n;
                }
                d.dir = (d.dir - n * (2.0 * d.dir.dot(&n))).normalize();
                d.pos += n * 1e-3; // step off the surface
                ctx.events.push(CombatEvent::ShurikenBounce { pos: d.pos, normal: n });
            }
            true
        });
    }
}
