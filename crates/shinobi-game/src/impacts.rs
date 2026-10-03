//! Impact FX: muzzle flash at a throw, sparks + a bright flash where a shuriken ends, bounces off, or lands a hit.
//! Producers push `Impact`s onto `ImpactQueue` (discs.rs for disc lifecycle, `queue_hits` for server hit messages);
//! this module turns them into short-lived HDR sparks (stretched cubes that shrink out) and an expanding flash
//! sphere so bloom makes a hit unmistakable. Purely cosmetic.

use bevy::prelude::*;

use crate::net::ViewRes;
use crate::team::{key, LocalColour};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ImpactKind {
    /// Disc leaves the hand.
    Muzzle,
    /// Disc ended or bounced: small cyan puff.
    Gone,
    /// Damage dealt: big flash + sparks (`kill` makes it bigger).
    Hit { kill: bool },
}

#[derive(Clone, Copy, Debug)]
pub struct Impact {
    pub pos: Vec3,
    /// Colour of the effect (the shooter's player / team colour, unit scale).
    pub color: [f32; 3],
    pub kind: ImpactKind,
}

#[derive(Resource, Default)]
pub struct ImpactQueue(pub Vec<Impact>);

#[derive(Component)]
struct Spark {
    vel: Vec3,
    age: f32,
    life: f32,
    size: f32,
}

#[derive(Component)]
struct Flash {
    age: f32,
    life: f32,
    size: f32,
}

#[derive(Resource)]
struct Look {
    cube: Handle<Mesh>,
    sphere: Handle<Mesh>,
    white: Handle<StandardMaterial>,
    /// Spark materials per (quantised colour, gain class): a handful of colours, so this stays tiny.
    sparks: std::collections::HashMap<(u32, bool), Handle<StandardMaterial>>,
}

pub struct ImpactPlugin;

impl Plugin for ImpactPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ImpactQueue>().add_systems(Startup, setup).add_systems(Update, (queue_hits, spawn_impacts, animate).chain());
    }
}

fn mat(mats: &mut Assets<StandardMaterial>, c: [f32; 3], gain: f32) -> Handle<StandardMaterial> {
    mats.add(StandardMaterial {
        base_color: Color::linear_rgb(c[0] * gain, c[1] * gain, c[2] * gain),
        unlit: true,
        alpha_mode: AlphaMode::Add,
        ..default()
    })
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    commands.insert_resource(Look {
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        sphere: meshes.add(Sphere::new(0.5)),
        white: mat(&mut mats, [1.0; 3], 10.0),
        sparks: Default::default(),
    });
}

/// Server-confirmed hits dealt by us: flash on the victim (hit marker in the world, not just on the HUD).
fn queue_hits(view: Res<ViewRes>, local: Res<LocalColour>, mut q: ResMut<ImpactQueue>) {
    for h in view.0.hits.iter().filter(|h| !h.taken) {
        if let Some((_, s)) = view.0.remotes.iter().find(|(id, _)| *id == h.other) {
            q.0.push(Impact { pos: Vec3::from(s.pos), color: local.0, kind: ImpactKind::Hit { kill: h.kill } });
        }
    }
}

/// Cheap deterministic [0,1) from an integer (no rand dependency, stable across runs).
fn hash01(mut x: u32) -> f32 {
    x = x.wrapping_mul(0x9e37_79b1) ^ (x >> 15);
    x = x.wrapping_mul(0x85eb_ca6b) ^ (x >> 13);
    (x & 0xffff) as f32 / 65536.0
}

fn dir_from(seed: u32) -> Vec3 {
    let (a, b) = (hash01(seed) * std::f32::consts::TAU, hash01(seed ^ 0xabcd) * 2.0 - 1.0);
    let r = (1.0 - b * b).sqrt();
    Vec3::new(r * a.cos(), b, r * a.sin())
}

fn spawn_impacts(
    mut commands: Commands,
    mut look: ResMut<Look>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut q: ResMut<ImpactQueue>,
    time: Res<Time>,
) {
    let base = (time.elapsed_secs() * 1000.0) as u32;
    for (n, im) in q.0.drain(..).enumerate() {
        let seed = base.wrapping_add(n as u32 * 977);
        // (spark count, speed, spark life, spark size, flash size, flash life, spark mat)
        let mut team_mat = |c: [f32; 3], hit: bool| {
            look.sparks.entry((key(c), hit)).or_insert_with(|| mat(&mut mats, c, if hit { 8.0 } else { 6.0 })).clone()
        };
        let (count, speed, life, size, flash, flife, m) = match im.kind {
            ImpactKind::Muzzle => (6, 4.0, 0.12, 0.05, 0.35, 0.08, team_mat(im.color, false)),
            ImpactKind::Gone => (8, 5.0, 0.2, 0.05, 0.3, 0.1, team_mat(im.color, false)),
            ImpactKind::Hit { kill: false } => (18, 9.0, 0.3, 0.07, 1.0, 0.15, team_mat(im.color, true)),
            ImpactKind::Hit { kill: true } => (36, 12.0, 0.5, 0.09, 2.2, 0.25, team_mat(im.color, true)),
        };
        for i in 0..count {
            let s = seed.wrapping_add(i * 31);
            let d = dir_from(s);
            let v = d * speed * (0.5 + hash01(s ^ 0x55));
            commands.spawn((
                Mesh3d(look.cube.clone()),
                MeshMaterial3d(m.clone()),
                Transform::from_translation(im.pos).looking_to(d, Vec3::Y).with_scale(Vec3::new(size, size, size * 4.0)),
                Spark { vel: v, age: 0.0, life: life * (0.6 + 0.4 * hash01(s ^ 0x77)), size },
            ));
        }
        commands.spawn((
            Mesh3d(look.sphere.clone()),
            MeshMaterial3d(look.white.clone()),
            Transform::from_translation(im.pos).with_scale(Vec3::splat(0.01)),
            Flash { age: 0.0, life: flife, size: flash },
        ));
    }
}

fn animate(
    mut commands: Commands,
    time: Res<Time>,
    mut sparks: Query<(Entity, &mut Transform, &mut Spark), Without<Flash>>,
    mut flashes: Query<(Entity, &mut Transform, &mut Flash), Without<Spark>>,
) {
    let dt = time.delta_secs();
    for (e, mut t, mut s) in &mut sparks {
        s.age += dt;
        if s.age >= s.life {
            commands.entity(e).despawn();
            continue;
        }
        s.vel *= 1.0 - (4.0 * dt).min(1.0); // air drag
        t.translation += s.vel * dt;
        let k = 1.0 - s.age / s.life;
        t.scale = Vec3::new(s.size * k, s.size * k, s.size * 4.0 * k);
    }
    for (e, mut t, mut f) in &mut flashes {
        f.age += dt;
        if f.age >= f.life {
            commands.entity(e).despawn();
            continue;
        }
        let u = f.age / f.life;
        // pop out fast, then collapse: reads as a flash, not a ball
        t.scale = Vec3::splat(f.size * (u * (1.0 - u) * 4.0).max(0.02));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic_and_in_range() {
        for i in 0..1000 {
            let h = hash01(i);
            assert!((0.0..1.0).contains(&h));
            assert_eq!(h, hash01(i));
        }
    }

    #[test]
    fn spark_dirs_are_unit_and_spread() {
        let dirs: Vec<Vec3> = (0..64).map(dir_from).collect();
        assert!(dirs.iter().all(|d| (d.length() - 1.0).abs() < 1e-4));
        let sum: Vec3 = dirs.iter().copied().sum();
        assert!(sum.length() < 40.0, "directions should not all point one way");
    }
}
