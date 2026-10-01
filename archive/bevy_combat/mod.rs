use bevy::prelude::*;

pub mod fx;
pub mod katana;
pub mod powerup;
pub mod shuriken;
pub mod viewmodel;
pub mod weapon;

pub use powerup::{DataCube, Inventory, PowerupKind};
pub use weapon::{Loadout, Weapon};

/// Rapier collision layers. Give EVERY world/enemy/player collider one of the
/// `layers::*()` groups; default (ALL-member) colliders would confuse the
/// melee ray/box filters.
pub mod layers {
    use bevy_rapier3d::prelude::{CollisionGroups, Group};

    pub const WORLD: Group = Group::GROUP_1;
    pub const PLAYER: Group = Group::GROUP_2;
    pub const ENEMY: Group = Group::GROUP_3;
    pub const PROJECTILE: Group = Group::GROUP_4;
    pub const PICKUP: Group = Group::GROUP_5;
    /// Only used as the *membership* of melee query filters.
    pub const MELEE: Group = Group::GROUP_6;

    pub fn world() -> CollisionGroups {
        CollisionGroups::new(WORLD, Group::ALL)
    }
    pub fn enemy() -> CollisionGroups {
        CollisionGroups::new(ENEMY, Group::ALL)
    }
    pub fn pickup_groups() -> CollisionGroups {
        CollisionGroups::new(PICKUP, PLAYER)
    }
    pub fn player() -> CollisionGroups {
        // Player ignores own projectiles; collides with world/enemy, overlaps pickups.
        CollisionGroups::new(PLAYER, WORLD | ENEMY | PICKUP)
    }
}

#[derive(Component)]
pub struct Player;

#[derive(Component)]
pub struct Enemy;

#[derive(Component)]
pub struct Health {
    pub current: f32,
    pub max: f32,
}

impl Health {
    pub fn new(max: f32) -> Self {
        Self { current: max, max }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageKind {
    Shuriken,
    Slash,
    GridBeam,
}

#[derive(Event)]
pub struct DamageEvent {
    pub target: Entity,
    pub amount: f32,
    pub kind: DamageKind,
}

/// Shared meshes/materials so weapons never touch `Assets<_>` at fire time.
/// Add `Bloom` to your camera (HDR) to make the emissive values glow.
#[derive(Resource)]
pub struct CombatAssets {
    pub shuriken_mesh: Handle<Mesh>,
    pub shuriken_mat: Handle<StandardMaterial>,
    pub streak_mesh: Handle<Mesh>,
    pub streak_mat: Handle<StandardMaterial>,
    pub cube_mesh: Handle<Mesh>,
    pub cube_mat: Handle<StandardMaterial>,
    pub ring_mesh: Handle<Mesh>,
    pub beam_mesh_x: Handle<Mesh>,
    pub beam_mesh_z: Handle<Mesh>,
    pub beam_mat: Handle<StandardMaterial>,
    // first-person view model
    pub blade_mesh: Handle<Mesh>,
    pub grip_mesh: Handle<Mesh>,
    pub grip_mat: Handle<StandardMaterial>,
    pub trail_mesh: Handle<Mesh>,
    pub trail_mat: Handle<StandardMaterial>,
}

fn neon(r: f32, g: f32, b: f32) -> StandardMaterial {
    StandardMaterial {
        base_color: Color::linear_rgb(r, g, b),
        emissive: LinearRgba::rgb(r * 10.0, g * 10.0, b * 10.0),
        unlit: true,
        ..default()
    }
}

fn setup_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(CombatAssets {
        shuriken_mesh: meshes.add(Cylinder::new(shuriken::RADIUS, 0.03)),
        shuriken_mat: mats.add(neon(0.0, 0.9, 1.0)),
        streak_mesh: meshes.add(Cuboid::new(0.05, 0.05, 3.0)),
        streak_mat: mats.add(neon(1.0, 0.0, 0.8)),
        cube_mesh: meshes.add(Cuboid::from_length(0.6)),
        cube_mat: mats.add(neon(0.9, 1.0, 0.1)),
        ring_mesh: meshes.add(Annulus::new(0.97, 1.0)),
        beam_mesh_x: meshes.add(Cuboid::new(2.0, 0.06, 0.06)),
        beam_mesh_z: meshes.add(Cuboid::new(0.06, 0.06, 2.0)),
        beam_mat: mats.add(neon(0.2, 1.0, 0.4)),
        blade_mesh: meshes.add(Cuboid::new(0.012, 0.04, 0.95)),
        grip_mesh: meshes.add(Cuboid::new(0.03, 0.03, 0.25)),
        grip_mat: mats.add(StandardMaterial { base_color: Color::linear_rgb(0.02, 0.02, 0.03), unlit: true, ..default() }),
        trail_mesh: meshes.add(Cuboid::new(0.045, 0.045, 1.0)), // unit length on Z; scaled per segment
        trail_mat: mats.add(neon(0.5, 1.0, 1.0)),
    });
}

fn apply_damage(
    mut commands: Commands,
    mut events: EventReader<DamageEvent>,
    mut health: Query<&mut Health>,
) {
    for ev in events.read() {
        let Ok(mut h) = health.get_mut(ev.target) else { continue };
        if h.current <= 0.0 {
            continue; // already dead this frame
        }
        h.current -= ev.amount;
        if h.current <= 0.0 {
            if let Some(e) = commands.get_entity(ev.target) {
                e.despawn_recursive();
            }
        }
    }
}

pub struct CombatPlugin;

impl Plugin for CombatPlugin {
    fn build(&self, app: &mut App) {
        app.add_event::<DamageEvent>()
            .add_event::<viewmodel::SlashEvent>()
            .add_systems(Startup, setup_assets)
            .add_systems(
                Update,
                (
                    weapon::tick_cooldowns,
                    weapon::weapon_input,
                    shuriken::shuriken_collisions,
                    shuriken::lock_speed,
                    powerup::pickup_cubes,
                    powerup::expire_charges,
                    powerup::activate_ultimate,
                    powerup::grid_beam_burst,
                    viewmodel::start_slash,
                    viewmodel::animate_slash,
                    viewmodel::dash_kick,
                    apply_damage,
                    fx::tick_fx,
                )
                    .chain(),
            );
    }
}
