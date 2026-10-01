use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

use super::{
    fx::{Fx, Spin},
    layers,
    weapon::{FireCtx, Weapon},
    DamageEvent, DamageKind, Enemy,
};

pub const RADIUS: f32 = 0.25;

#[derive(Component)]
pub struct Shuriken {
    pub damage: f32,
    pub speed: f32,
    pub bounces: u8,
    pub max_bounces: u8,
}

pub struct ShurikenLauncher {
    pub speed: f32,
    pub damage: f32,
    /// World hits survived; disc dies on hit number `max_bounces + 1`.
    pub max_bounces: u8,
    pub cooldown: f32,
    pub muzzle: f32,
    pub max_flight_secs: f32,
}

impl Default for ShurikenLauncher {
    fn default() -> Self {
        Self { speed: 40.0, damage: 25.0, max_bounces: 2, cooldown: 0.18, muzzle: 0.8, max_flight_secs: 6.0 }
    }
}

impl Weapon for ShurikenLauncher {
    fn name(&self) -> &'static str {
        "Neon Shuriken"
    }
    fn cooldown(&self) -> f32 {
        self.cooldown
    }

    fn fire(&mut self, ctx: &mut FireCtx) -> bool {
        let dir = *ctx.aim;
        ctx.commands
            .spawn((
                Transform::from_translation(ctx.origin + dir * self.muzzle),
                Visibility::default(),
                Shuriken { damage: self.damage, speed: self.speed, bounces: 0, max_bounces: self.max_bounces },
                Fx::new(self.max_flight_secs, false),
                // --- physics: real rigid body, no hitscan ---
                RigidBody::Dynamic,
                // Ball (not a thin cylinder): reflects cleanly off any surface
                // regardless of disc spin. The disc is purely the visual child.
                Collider::ball(RADIUS),
                Velocity::linear(dir * self.speed),
                GravityScale(0.0),
                Damping { linear_damping: 0.0, angular_damping: 0.0 },
                LockedAxes::ROTATION_LOCKED,
                Ccd::enabled(), // 40 m/s would tunnel through thin walls otherwise
                // Perfect mirror bounce: restitution 1, zero friction, `Max`/`Min`
                // combine rules so the *disc's* material wins over the wall's.
                Restitution { coefficient: 1.0, combine_rule: CoefficientCombineRule::Max },
                Friction { coefficient: 0.0, combine_rule: CoefficientCombineRule::Min },
                ColliderMassProperties::Mass(0.05),
                ActiveEvents::COLLISION_EVENTS,
                CollisionGroups::new(layers::PROJECTILE, layers::WORLD | layers::ENEMY),
            ))
            .with_children(|p| {
                p.spawn((
                    Mesh3d(ctx.assets.shuriken_mesh.clone()),
                    MeshMaterial3d(ctx.assets.shuriken_mat.clone()),
                    Transform::default(),
                    Spin(40.0),
                ));
            });
        true
    }
}

/// Enemy hit → damage + despawn. Anything else = bounce (Rapier already
/// reflected the velocity); despawn once the bounce budget is spent.
pub fn shuriken_collisions(
    mut commands: Commands,
    mut events: EventReader<CollisionEvent>,
    mut shurikens: Query<&mut Shuriken>,
    enemies: Query<(), With<Enemy>>,
    mut damage: EventWriter<DamageEvent>,
) {
    for ev in events.read() {
        let CollisionEvent::Started(a, b, _) = ev else { continue };
        for (s, other) in [(*a, *b), (*b, *a)] {
            let Ok(mut sh) = shurikens.get_mut(s) else { continue };
            let spent = if enemies.contains(other) {
                damage.send(DamageEvent { target: other, amount: sh.damage, kind: DamageKind::Shuriken });
                true
            } else {
                sh.bounces += 1;
                sh.bounces > sh.max_bounces
            };
            if spent {
                if let Some(e) = commands.get_entity(s) {
                    e.despawn_recursive();
                }
            }
            break;
        }
    }
}

/// Solver leaves tiny energy error per bounce; pin speed so discs stay lethal.
pub fn lock_speed(mut q: Query<(&mut Velocity, &Shuriken)>) {
    for (mut v, s) in &mut q {
        if v.linvel.length_squared() > 1e-4 {
            v.linvel = v.linvel.normalize() * s.speed;
        }
    }
}
