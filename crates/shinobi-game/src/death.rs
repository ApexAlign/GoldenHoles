//! Player death, drawn: when a player's dead flag flips on (remote) or the local death starts (`RenderView::death`),
//! the body bursts into its own colour: a white-hot flash, the body parts fly apart and tumble, a cloud of data
//! voxels drifts up and flickers out (same de-rez language as the printed blocks), and a light column marks the spot.
//! Remote bodies are not drawn while dead (`ninja_draw` returns None), so this is all that is left of them.
//! Cosmetic only; driven by the snapshot flag and `RenderView::death`.

use std::collections::HashMap;

use bevy::prelude::*;
use shinobi_proto::snapshot::FLAG_DEAD;

use crate::net::{Net, ViewRes};
use crate::team::{colour_of, key};

/// Body parts as (centre offset from the capsule centre, full size): head, torso, two arms, two legs.
const CHUNKS: [([f32; 3], [f32; 3]); 6] = [
    ([0.0, 0.62, 0.0], [0.30, 0.30, 0.30]),
    ([0.0, 0.12, 0.0], [0.46, 0.62, 0.26]),
    ([-0.34, 0.12, 0.0], [0.16, 0.56, 0.16]),
    ([0.34, 0.12, 0.0], [0.16, 0.56, 0.16]),
    ([-0.12, -0.5, 0.0], [0.2, 0.72, 0.2]),
    ([0.12, -0.5, 0.0], [0.2, 0.72, 0.2]),
];
const VOXELS: u32 = 64;
const CHUNK_LIFE: f32 = 1.5;
/// Capsule centre to the soles.
const FEET_BELOW: f32 = 0.81;

#[derive(Component)]
struct DeathBit {
    vel: Vec3,
    spin: Vec3,
    age: f32,
    life: f32,
    size: Vec3,
    phase: f32,
    gravity: f32,
    floor_y: f32,
    /// Flash shell / light column: scale follows `shape` instead of the shrink-and-flicker of debris.
    shape: Shape,
}

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Debris,
    /// Expanding shell that peaks early and collapses.
    Flash,
    /// Tall thin column that thins out.
    Column,
}

#[derive(Resource, Default)]
struct DeathViz {
    glow: HashMap<(u32, u8), Handle<StandardMaterial>>,
    cube: Option<Handle<Mesh>>,
    sphere: Option<Handle<Mesh>>,
    /// Per remote id: last known position and whether it was dead on the previous frame.
    seen: HashMap<u16, ([f32; 3], bool)>,
}

pub struct DeathFxPlugin;

impl Plugin for DeathFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DeathViz>().add_systems(Startup, setup).add_systems(Update, (detect_deaths, animate).chain());
    }
}

fn setup(mut viz: ResMut<DeathViz>, mut meshes: ResMut<Assets<Mesh>>) {
    viz.cube = Some(meshes.add(Cuboid::new(1.0, 1.0, 1.0)));
    viz.sphere = Some(meshes.add(Sphere::new(0.5)));
}

fn hash01(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B1) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 65535.0
}

/// Additive HDR material per (quantised colour, gain class: 0 = body glow, 1 = white-hot).
fn glow(viz: &mut DeathViz, mats: &mut Assets<StandardMaterial>, c: [f32; 3], hot: u8) -> Handle<StandardMaterial> {
    viz.glow
        .entry((key(c), hot))
        .or_insert_with(|| {
            let g = if hot == 1 { 5.0 } else { 2.4 };
            let mix = if hot == 1 { 0.55 } else { 0.0 };
            let ch = |v: f32| (v + (1.0 - v) * mix) * g;
            mats.add(StandardMaterial { base_color: Color::linear_rgb(ch(c[0]), ch(c[1]), ch(c[2])), unlit: true, alpha_mode: AlphaMode::Add, ..default() })
        })
        .clone()
}

/// Spawn the whole death burst at `pos` (capsule centre) in colour `c`. `seed` varies the scatter per death.
fn burst(commands: &mut Commands, viz: &mut DeathViz, mats: &mut Assets<StandardMaterial>, pos: [f32; 3], c: [f32; 3], seed: u32) {
    let (Some(cube), Some(sphere)) = (viz.cube.clone(), viz.sphere.clone()) else { return };
    let body = glow(viz, mats, c, 0);
    let hot = glow(viz, mats, c, 1);
    let centre = Vec3::from(pos);
    let floor_y = pos[1] - FEET_BELOW;
    let bit = |vel: Vec3, spin: Vec3, life: f32, size: Vec3, phase: f32, gravity: f32, shape: Shape| DeathBit { vel, spin, age: 0.0, life, size, phase, gravity, floor_y, shape };

    for (i, (off, size)) in CHUNKS.iter().enumerate() {
        let s = seed.wrapping_add(i as u32 * 104_729);
        let out = Vec3::new(hash01(s) - 0.5, 0.0, hash01(s ^ 0x1111) - 0.5).normalize_or_zero();
        let vel = out * (1.6 + 2.2 * hash01(s ^ 0x2222)) + Vec3::Y * (2.6 + 3.2 * hash01(s ^ 0x3333));
        let spin = Vec3::new(hash01(s ^ 0x4444) - 0.5, hash01(s ^ 0x5555) - 0.5, hash01(s ^ 0x6666) - 0.5) * 14.0;
        let size = Vec3::from(*size);
        commands.spawn((
            Mesh3d(cube.clone()),
            MeshMaterial3d(body.clone()),
            Transform::from_translation(centre + Vec3::from(*off)).with_scale(size),
            bit(vel, spin, CHUNK_LIFE * (0.8 + 0.3 * hash01(s ^ 0x7777)), size, hash01(s ^ 0x8888) * 6.28, 7.5, Shape::Debris),
        ));
    }
    for i in 0..VOXELS {
        let s = seed.wrapping_add(7919 * (i + 11));
        let off = Vec3::new(hash01(s) - 0.5, hash01(s ^ 0xAAAA) - 0.5, hash01(s ^ 0xBBBB) - 0.5) * Vec3::new(0.7, 1.5, 0.7);
        let dir = (off * Vec3::new(1.0, 0.4, 1.0)).normalize_or_zero();
        let vel = dir * (1.0 + 2.6 * hash01(s ^ 0xCCCC)) + Vec3::Y * (0.8 + 2.4 * hash01(s ^ 0xDDDD));
        let size = Vec3::splat(0.05 + 0.1 * hash01(s ^ 0xEEEE));
        let mat = if i % 5 == 0 { hot.clone() } else { body.clone() };
        commands.spawn((
            Mesh3d(cube.clone()),
            MeshMaterial3d(mat),
            Transform::from_translation(centre + off).with_scale(size),
            bit(vel, Vec3::ZERO, 0.7 + 0.9 * hash01(s ^ 0xFFFF), size, hash01(s ^ 0x1234) * 6.28, -0.8, Shape::Debris),
        ));
    }
    // flash shell + light column
    commands.spawn((
        Mesh3d(sphere),
        MeshMaterial3d(hot.clone()),
        Transform::from_translation(centre).with_scale(Vec3::splat(0.1)),
        bit(Vec3::ZERO, Vec3::ZERO, 0.32, Vec3::splat(2.6), 0.0, 0.0, Shape::Flash),
    ));
    commands.spawn((
        Mesh3d(cube),
        MeshMaterial3d(hot),
        Transform::from_translation(Vec3::new(centre.x, floor_y + 3.2, centre.z)).with_scale(Vec3::new(0.4, 6.4, 0.4)),
        bit(Vec3::ZERO, Vec3::ZERO, 0.55, Vec3::new(0.4, 6.4, 0.4), 0.0, 0.0, Shape::Column),
    ));
}

fn detect_deaths(mut commands: Commands, net: Res<Net>, view: Res<ViewRes>, mut viz: ResMut<DeathViz>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let v = &view.0;
    let s = &net.0;
    // the local death: one burst on the first view after dying
    if let (Some(d), Some(id)) = (v.death.filter(|d| d.fresh), s.game.net.player_id()) {
        let c = colour_of(&net, id);
        burst(&mut commands, &mut viz, &mut mats, d.pos, c, d.pos[0].to_bits() ^ d.pos[2].to_bits());
    }
    // remote deaths: the dead flag turning on
    let Some(snap) = s.game.net.interp.newest_tick().and_then(|t| s.game.net.interp.get(t)) else { return };
    let mut now: Vec<u16> = Vec::new();
    for (id, smp) in &v.remotes {
        let Some((_, st)) = snap.entities.iter().find(|(e, _)| e == id) else { continue };
        now.push(*id);
        let dead = st.flags & FLAG_DEAD != 0;
        let was = viz.seen.get(id).copied();
        let pos = match was {
            Some((p, false)) if dead => p, // where it stood on the last live frame
            _ => smp.pos,
        };
        if dead && matches!(was, Some((_, false))) {
            let c = colour_of(&net, *id);
            burst(&mut commands, &mut viz, &mut mats, pos, c, (*id as u32).wrapping_mul(2_654_435_761) ^ pos[0].to_bits());
        }
        viz.seen.insert(*id, (if dead { pos } else { smp.pos }, dead));
    }
    viz.seen.retain(|id, _| now.contains(id));
}

fn animate(mut commands: Commands, time: Res<Time>, mut q: Query<(Entity, &mut Transform, &mut DeathBit)>) {
    let dt = time.delta_secs();
    for (e, mut t, mut b) in &mut q {
        b.age += dt;
        if b.age >= b.life {
            commands.entity(e).despawn();
            continue;
        }
        let k = b.age / b.life;
        match b.shape {
            Shape::Flash => {
                // out fast, then collapse
                let s = (k * std::f32::consts::PI).sin().powf(0.6);
                t.scale = b.size * s.max(0.01);
            }
            Shape::Column => {
                let thin = (1.0 - k).powi(2);
                t.scale = Vec3::new(b.size.x * thin, b.size.y, b.size.z * thin).max(Vec3::splat(0.001));
            }
            Shape::Debris => {
                let v = b.vel;
                t.translation += v * dt;
                b.vel.y -= b.gravity * dt;
                if t.translation.y - b.size.y * 0.5 < b.floor_y && b.gravity > 0.0 {
                    t.translation.y = b.floor_y + b.size.y * 0.5;
                    b.vel.y = b.vel.y.abs() * 0.25;
                    b.vel.x *= 0.6;
                    b.vel.z *= 0.6;
                    b.spin *= 0.6;
                }
                let spin = b.spin * dt;
                t.rotation = Quat::from_euler(EulerRot::XYZ, spin.x, spin.y, spin.z) * t.rotation;
                // shrink away, flickering through the last two thirds
                let fade = (1.0 - k).powf(0.8);
                let on = if k > 0.35 && (b.age * 38.0 + b.phase).sin() < -0.35 { 0.0 } else { 1.0 };
                t.scale = b.size * fade * on;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scatter_is_deterministic_and_spread() {
        assert_eq!(hash01(5), hash01(5));
        let v: Vec<f32> = (0..64).map(hash01).collect();
        assert!(v.iter().all(|x| (0.0..=1.0).contains(x)));
        assert!(v.iter().cloned().fold(0.0, f32::max) - v.iter().cloned().fold(1.0, f32::min) > 0.6);
    }

    #[test]
    fn a_body_has_six_parts_inside_the_capsule_height() {
        assert_eq!(CHUNKS.len(), 6);
        for (off, size) in CHUNKS {
            assert!(off[1] - size[1] * 0.5 >= -FEET_BELOW - 0.01 && off[1] + size[1] * 0.5 <= 0.81 + 0.01, "{off:?} {size:?}");
        }
    }
}
