use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

use super::{
    fx::Fx,
    layers,
    viewmodel::SlashEvent,
    weapon::{FireCtx, Weapon},
    DamageEvent, DamageKind,
};

pub struct Katana {
    /// Forward impulse applied to the (dynamic) player body. Tune with mass.
    pub dash_impulse: f32,
    pub damage: f32,
    /// Slash box length ahead of player: dash distance + blade length.
    pub reach: f32,
    pub half_width: f32,
    pub half_height: f32,
    pub cooldown: f32,
    /// Alternates streak diagonal each swing.
    flip: bool,
}

impl Default for Katana {
    fn default() -> Self {
        Self { dash_impulse: 60.0, damage: 120.0, reach: 5.0, half_width: 1.2, half_height: 1.0, cooldown: 0.6, flip: false }
    }
}

impl Weapon for Katana {
    fn name(&self) -> &'static str {
        "Cyber-Katana"
    }
    fn cooldown(&self) -> f32 {
        self.cooldown
    }

    fn fire(&mut self, ctx: &mut FireCtx) -> bool {
        // Dash along the horizontal aim; keeps you grounded.
        let fwd = *ctx.aim;
        let flat = Vec3::new(fwd.x, 0.0, fwd.z).normalize_or_zero();
        let dir = if flat == Vec3::ZERO { fwd } else { flat };

        // 1. sharp forward impulse (applied by Rapier next step)
        ctx.impulse.impulse += dir * self.dash_impulse;

        // 2. truncate the slash at the first wall so you can't cut through geometry
        let world_only = QueryFilter::new().groups(CollisionGroups::new(layers::MELEE, layers::WORLD));
        let reach = ctx
            .rapier
            .cast_ray(ctx.origin, dir, self.reach, true, world_only)
            .map_or(self.reach, |(_, toi)| toi)
            .max(0.1);

        // 3. instant, short-lived hit box over the dash path (never spawned as
        //    an entity: query-only, so it lives exactly one call)
        let shape = Collider::cuboid(self.half_width, self.half_height, reach * 0.5);
        let center = ctx.origin + dir * (reach * 0.5);
        let rot = Quat::from_rotation_arc(Vec3::NEG_Z, dir); // cuboid long axis = local Z
        let enemy_only = QueryFilter::new()
            .groups(CollisionGroups::new(layers::MELEE, layers::ENEMY))
            .exclude_collider(ctx.owner);

        let mut hits = Vec::new();
        ctx.rapier.intersections_with_shape(center, rot, &shape, enemy_only, |e| {
            hits.push(e);
            true
        });

        // 4. damage + neon streak across each victim's 3D position
        self.flip = !self.flip;
        // first-person: view-model swing + arc trails + FOV dash kick
        ctx.slash.send(SlashEvent { flip: self.flip });
        let right = ctx.rotation * Vec3::X;
        let diag = (right * 0.8 + Vec3::Y * if self.flip { 0.6 } else { -0.6 }).normalize();
        for e in hits {
            ctx.damage.send(DamageEvent { target: e, amount: self.damage, kind: DamageKind::Slash });
            let Ok(gtf) = ctx.enemies.get(e) else { continue };
            ctx.commands.spawn((
                Mesh3d(ctx.assets.streak_mesh.clone()),
                MeshMaterial3d(ctx.assets.streak_mat.clone()),
                // streak mesh long axis is Z → point Z along `diag`
                Transform::from_translation(gtf.translation()).with_rotation(Quat::from_rotation_arc(Vec3::Z, diag)),
                Fx::new(0.2, true),
            ));
        }
        true
    }
}
