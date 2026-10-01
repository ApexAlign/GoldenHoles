use rapier3d::na::UnitQuaternion;
use rapier3d::prelude::*;

use super::{
    solid_groups,
    weapon::{FireCtx, Trigger, Weapon},
    CombatEvent, DamageKind, GROUP_MELEE,
};
use crate::character::{Cfg, GROUP_ENEMY};

#[derive(Clone, Copy, Debug)]
pub struct KatanaCfg {
    pub damage: f32,
    pub blade_len: f32,
    pub half_width: f32,
    pub half_height: f32,
    pub cooldown: f32,
    /// Horizontal distance the character's dash covers; sizes the hit box.
    pub lunge_distance: f32,
}

/// Distance `CharacterSystem`'s dash travels: speed = dash_speed * (0.4 + 0.6 f), f: 1 -> 0 over
/// `dash_time`, so the mean speed is 0.7 * dash_speed (30 * 0.18 * 0.7 = 3.8 m by default).
pub fn dash_distance(ch: &Cfg) -> f32 {
    ch.dash_speed * ch.dash_time * 0.7
}

impl KatanaCfg {
    pub fn for_character(ch: &Cfg) -> Self {
        Self {
            damage: 120.0,
            blade_len: 1.2,
            half_width: 1.0,
            half_height: 1.0,
            // Matching the dash cooldown keeps every slice paired with a lunge.
            cooldown: ch.dash_cd,
            lunge_distance: dash_distance(ch),
        }
    }
}

pub struct Katana {
    pub cfg: KatanaCfg,
    flip: bool,
}

impl Katana {
    pub fn new(cfg: KatanaCfg) -> Self {
        Self { cfg, flip: false }
    }
}

impl Weapon for Katana {
    fn name(&self) -> &'static str {
        "Cyber-Katana"
    }
    fn trigger(&self) -> Trigger {
        Trigger::Secondary
    }
    fn cooldown(&self) -> f32 {
        self.cfg.cooldown
    }

    fn fire(&mut self, ctx: &mut FireCtx) -> bool {
        let c = self.cfg;
        let dir = ctx.aim.flat_forward();
        let origin = ctx.aim.origin;

        // Lunge = the character's own dash, aimed along the slash. Policy when the dash is still
        // on cooldown: the slash still lands (short reach, no lunge) rather than eating the input.
        let lunge = ctx.ch.dash_ready();
        if lunge {
            ctx.input.dash_pressed = true;
            ctx.input.move_dir = dir;
        }
        let mut reach = c.blade_len + if lunge { c.lunge_distance } else { 0.0 };

        // Walls stop both lunge and blade: truncate the box at the first solid.
        let solid = QueryFilter::default()
            .exclude_rigid_body(ctx.ch.body)
            .exclude_sensors()
            .groups(InteractionGroups::new(GROUP_MELEE, solid_groups()));
        let ray = Ray::new(Point::from(origin), dir);
        if let Some(toi) =
            ctx.world.queries.cast_ray(&ctx.world.bodies, &ctx.world.colliders, &ray, reach, true, solid)
        {
            reach = toi.1.max(0.3);
        }

        // Short-lived hit box: query-only, exists for exactly this call.
        let shape = Cuboid::new(vector![c.half_width, c.half_height, reach * 0.5]);
        let rot = UnitQuaternion::face_towards(&dir, &Vector::y()); // local +Z -> dir
        let pos = Isometry::from_parts((origin + dir * (reach * 0.5)).into(), rot);
        let filter = QueryFilter::default()
            .exclude_rigid_body(ctx.ch.body)
            .groups(InteractionGroups::new(GROUP_MELEE, GROUP_ENEMY));

        let mut hits: Vec<ColliderHandle> = Vec::new();
        ctx.world.queries.intersections_with_shape(
            &ctx.world.bodies,
            &ctx.world.colliders,
            &pos,
            &shape,
            filter,
            |h| {
                hits.push(h);
                true
            },
        );

        for h in &hits {
            let Some(col) = ctx.world.colliders.get(*h) else { continue };
            ctx.events.push(CombatEvent::Damage {
                target: col.user_data,
                collider: *h,
                amount: c.damage,
                kind: DamageKind::Slash,
                point: Point::from(*col.translation()),
            });
        }

        self.flip = !self.flip;
        ctx.events.push(CombatEvent::Slash { flip: self.flip, dir, lunge, hits: hits.len() as u32 });
        true
    }
}
