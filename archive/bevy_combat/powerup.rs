use std::collections::HashSet;

use bevy::prelude::*;
use bevy_rapier3d::prelude::*;

use super::{layers, CombatAssets, DamageEvent, DamageKind, Enemy, Player};

pub const SLOTS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerupKind {
    GridBeam,
}

#[derive(Clone, Copy)]
pub struct Charge {
    pub kind: PowerupKind,
    /// Seconds left before the charge decays unused.
    pub remaining: f32,
}

#[derive(Component, Default)]
pub struct Inventory {
    pub slots: [Option<Charge>; SLOTS],
}

impl Inventory {
    pub fn push(&mut self, c: Charge) -> bool {
        match self.slots.iter_mut().find(|s| s.is_none()) {
            Some(s) => {
                *s = Some(c);
                true
            }
            None => false,
        }
    }
}

/// Glowing pickup. Sensor collider → overlaps, never blocks.
#[derive(Component)]
pub struct DataCube {
    pub kind: PowerupKind,
    pub charge_secs: f32,
}

impl DataCube {
    pub fn spawn(commands: &mut Commands, assets: &CombatAssets, pos: Vec3, kind: PowerupKind) {
        commands.spawn((
            Mesh3d(assets.cube_mesh.clone()),
            MeshMaterial3d(assets.cube_mat.clone()),
            Transform::from_translation(pos),
            DataCube { kind, charge_secs: 20.0 },
            RigidBody::Fixed,
            Collider::cuboid(0.4, 0.4, 0.4),
            Sensor,
            ActiveEvents::COLLISION_EVENTS,
            layers::pickup_groups(),
        ));
    }
}

pub fn pickup_cubes(
    mut commands: Commands,
    mut events: EventReader<CollisionEvent>,
    cubes: Query<&DataCube>,
    mut players: Query<&mut Inventory, With<Player>>,
) {
    for ev in events.read() {
        let CollisionEvent::Started(a, b, _) = ev else { continue };
        for (c, p) in [(*a, *b), (*b, *a)] {
            let (Ok(cube), Ok(mut inv)) = (cubes.get(c), players.get_mut(p)) else { continue };
            if inv.push(Charge { kind: cube.kind, remaining: cube.charge_secs }) {
                if let Some(e) = commands.get_entity(c) {
                    e.despawn_recursive();
                }
            }
            break;
        }
    }
}

pub fn expire_charges(time: Res<Time>, mut inv: Query<&mut Inventory>) {
    for mut inv in &mut inv {
        for slot in &mut inv.slots {
            if let Some(c) = slot {
                c.remaining -= time.delta_secs();
                if c.remaining <= 0.0 {
                    *slot = None;
                }
            }
        }
    }
}

#[derive(Component)]
pub struct GridBeamBurst {
    pub origin: Vec3,
    pub radius: f32,
    pub speed: f32,
    pub max_radius: f32,
    pub damage: f32,
    hit: HashSet<Entity>,
}

#[derive(Component)]
pub enum BeamPart {
    Ring,
    X,
    Z,
}

/// `R` burns the first charge in the inventory.
pub fn activate_ultimate(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    assets: Option<Res<CombatAssets>>,
    mut players: Query<(&GlobalTransform, &mut Inventory), With<Player>>,
) {
    let Some(assets) = assets else { return };
    if !keys.just_pressed(KeyCode::KeyR) {
        return;
    }
    for (gtf, mut inv) in &mut players {
        let Some(slot) = inv.slots.iter_mut().find(|s| s.is_some()) else { continue };
        let Some(Charge { kind: PowerupKind::GridBeam, .. }) = slot.take() else { continue };
        let origin = gtf.translation();
        commands
            .spawn((
                Transform::from_translation(origin),
                Visibility::default(),
                GridBeamBurst { origin, radius: 0.0, speed: 60.0, max_radius: 120.0, damage: 9999.0, hit: HashSet::new() },
            ))
            .with_children(|p| {
                p.spawn((
                    Mesh3d(assets.ring_mesh.clone()),
                    MeshMaterial3d(assets.beam_mat.clone()),
                    Transform::from_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)),
                    BeamPart::Ring,
                ));
                p.spawn((Mesh3d(assets.beam_mesh_x.clone()), MeshMaterial3d(assets.beam_mat.clone()), Transform::default(), BeamPart::X));
                p.spawn((Mesh3d(assets.beam_mesh_z.clone()), MeshMaterial3d(assets.beam_mat.clone()), Transform::default(), BeamPart::Z));
            });
    }
}

/// Expanding ring + cross-hair grid beams; everything the wavefront crosses
/// takes `damage` once. Clears the map at `max_radius`.
pub fn grid_beam_burst(
    mut commands: Commands,
    time: Res<Time>,
    mut bursts: Query<(Entity, &mut GridBeamBurst, &Children)>,
    mut parts: Query<(&BeamPart, &mut Transform)>,
    enemies: Query<(Entity, &GlobalTransform), With<Enemy>>,
    mut damage: EventWriter<DamageEvent>,
) {
    for (e, mut burst, children) in &mut bursts {
        burst.radius += burst.speed * time.delta_secs();
        let r = burst.radius;

        for (en, gtf) in &enemies {
            let d = gtf.translation() - burst.origin;
            if Vec2::new(d.x, d.z).length() <= r && burst.hit.insert(en) {
                damage.send(DamageEvent { target: en, amount: burst.damage, kind: DamageKind::GridBeam });
            }
        }

        for &c in children.iter() {
            if let Ok((part, mut tf)) = parts.get_mut(c) {
                tf.scale = match part {
                    BeamPart::Ring => Vec3::new(r, r, 1.0), // annulus is in local XY before the flat rotation
                    BeamPart::X => Vec3::new(r, 1.0, 1.0),
                    BeamPart::Z => Vec3::new(1.0, 1.0, r),
                };
            }
        }

        if r >= burst.max_radius {
            commands.entity(e).despawn_recursive();
        }
    }
}
