//! World FX of the star variants (docs/stars.md): the mine's glyph ring on its surface, the smoke volume, the flash and
//! mine-burst shells. Fed by `RenderView::star_objs` (landed objects) and `NetEvent::StarFx` (one-shot bursts). Purely
//! cosmetic; the sim never reads any of it.
//!
//! * MINE: a flat glyph (two rings, four ticks, a dot) lying on the surface in its owner's colour. Faint for everyone;
//!   once armed it pulses, and for enemies within `NEAR_ENEMY` metres it burns bright (you can see it if you are close).
//! * SMOKE: a ball of overlapping translucent puffs, `smoke_radius` wide, drifting slowly and thinning out with its life.
//! * FLASH: a white shell that pops out to the blast radius in a quarter second.
//! * MINE BURST: an orange shell plus sparks (through `ImpactQueue`).

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use shinobi_sim::combat::stars::{StarCfg, StarKind};
use shinobi_sim::events::{NetEvent, STAR_FX_FLASH_BURST, STAR_FX_BARREL_BURST};

use crate::impacts::{Impact, ImpactKind, ImpactQueue};
use crate::net::{Net, ViewRes};
use crate::team::colour_of;

/// Enemy-eyes distance (m) inside which an armed mine's glyph burns bright.
pub const NEAR_ENEMY: f32 = 12.0;
const GLYPH_POOL: usize = 16;
const GLYPH_RADIUS: f32 = 0.95;
const SMOKE_POOL: usize = 6;
const PUFFS: usize = 8;

#[derive(Resource)]
struct Look {
    glyphs: Vec<(Entity, Handle<StandardMaterial>)>,
    puffs: Vec<Vec<(Entity, Handle<StandardMaterial>)>>,
    sphere: Handle<Mesh>,
}

#[derive(Component)]
struct Shell {
    age: f32,
    life: f32,
    size: f32,
    col: [f32; 3],
}

pub struct StarFxPlugin;

impl Plugin for StarFxPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup.after(crate::play::setup)).add_systems(Update, (update_objects, spawn_bursts, animate_shells).chain());
    }
}

/// Mine glyph in the XZ plane (normal +Y), unit outer radius, greyscale HDR vertex colours (the material tints it).
pub fn glyph_mesh() -> Mesh {
    let (mut pos, mut col, mut idx): (Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<u32>) = (Vec::new(), Vec::new(), Vec::new());
    let pt = |a: f32, r: f32| [r * a.cos(), 0.0, r * a.sin()];
    let mut ring = |r0: f32, r1: f32, n: usize, gain: f32, gap: bool| {
        for i in 0..n {
            if gap && i % 8 == 0 {
                continue; // a broken ring reads as a glyph, not a plate
            }
            let (a0, a1) = (i as f32 / n as f32 * std::f32::consts::TAU, (i + 1) as f32 / n as f32 * std::f32::consts::TAU);
            let b = pos.len() as u32;
            pos.extend([pt(a0, r0), pt(a0, r1), pt(a1, r1), pt(a1, r0)]);
            col.extend([[gain, gain, gain, 1.0]; 4]);
            idx.extend([b, b + 1, b + 2, b, b + 2, b + 3]);
        }
    };
    ring(0.88, 1.0, 48, 3.0, true);
    ring(0.5, 0.56, 40, 2.0, true);
    // four ticks pointing in from the outer ring
    for k in 0..4 {
        let a = k as f32 * std::f32::consts::FRAC_PI_2 + std::f32::consts::FRAC_PI_4;
        let b = pos.len() as u32;
        pos.extend([pt(a - 0.07, 0.6), pt(a - 0.05, 0.86), pt(a + 0.05, 0.86), pt(a + 0.07, 0.6)]);
        col.extend([[4.0, 4.0, 4.0, 1.0]; 4]);
        idx.extend([b, b + 1, b + 2, b, b + 2, b + 3]);
    }
    // centre dot
    let b = pos.len() as u32;
    pos.push([0.0; 3]);
    col.push([6.0, 6.0, 6.0, 1.0]);
    for i in 0..12 {
        pos.push(pt(i as f32 / 12.0 * std::f32::consts::TAU, 0.16));
        col.push([6.0, 6.0, 6.0, 1.0]);
    }
    for i in 0..12u32 {
        idx.extend([b, b + 1 + i, b + 1 + (i + 1) % 12]);
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
        .with_inserted_indices(Indices::U32(idx))
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let glyph = meshes.add(glyph_mesh());
    let glyphs = (0..GLYPH_POOL)
        .map(|_| {
            let m = mats.add(StandardMaterial { base_color: Color::BLACK, unlit: true, alpha_mode: AlphaMode::Add, double_sided: true, cull_mode: None, ..default() });
            let e = commands.spawn((Mesh3d(glyph.clone()), MeshMaterial3d(m.clone()), Transform::default(), Visibility::Hidden, NoFrustumCulling)).id();
            (e, m)
        })
        .collect();
    let sphere = meshes.add(Sphere::new(1.0).mesh().ico(3).unwrap());
    let puffs = (0..SMOKE_POOL)
        .map(|_| {
            let m = mats.add(StandardMaterial {
                base_color: Color::linear_rgba(0.5, 0.58, 0.7, 0.0),
                unlit: true,
                alpha_mode: AlphaMode::Blend,
                double_sided: true,
                cull_mode: None,
                ..default()
            });
            (0..PUFFS)
                .map(|_| {
                    let e = commands.spawn((Mesh3d(sphere.clone()), MeshMaterial3d(m.clone()), Transform::default(), Visibility::Hidden, NoFrustumCulling)).id();
                    (e, m.clone())
                })
                .collect()
        })
        .collect();
    commands.insert_resource(Look { glyphs, puffs, sphere });
}

fn hash01(mut x: u32) -> f32 {
    x = x.wrapping_mul(0x9e37_79b1) ^ (x >> 15);
    x = x.wrapping_mul(0x85eb_ca6b) ^ (x >> 13);
    (x & 0xffff) as f32 / 65536.0
}

/// Glyph brightness 0..~1.6 for a mine: faint for everyone, a slow pulse once armed, and bright to an enemy close by.
pub fn glyph_gain(armed: bool, enemy_dist: Option<f32>, t: f32, life_left: f32) -> f32 {
    let base = if armed { 0.16 + 0.06 * (t * 5.0).sin().abs() } else { 0.10 * (0.5 + 0.5 * (t * 14.0).sin().abs()) };
    let near = enemy_dist.filter(|d| armed && *d < NEAR_ENEMY).map_or(0.0, |d| 1.3 * (1.0 - d / NEAR_ENEMY) + 0.25);
    // the last seconds of a mine's life flicker out
    let fade = if life_left < 0.12 { life_left / 0.12 } else { 1.0 };
    (base + near) * fade
}

fn update_objects(time: Res<Time>, view: Res<ViewRes>, net: Res<Net>, look: Option<Res<Look>>, mut mats: ResMut<Assets<StandardMaterial>>, mut q: Query<(&mut Transform, &mut Visibility)>) {
    let Some(look) = look else { return };
    let v = &view.0;
    let t = time.elapsed_secs();
    let me = net.0.own_id().unwrap_or(0);
    let eye = Vec3::from(v.eye);
    // ---- mines ----
    let mines: Vec<_> = v.star_objs.iter().filter(|o| o.kind == StarKind::Barrel).collect();
    for (i, (e, m)) in look.glyphs.iter().enumerate() {
        let Ok((mut tr, mut vis)) = q.get_mut(*e) else { continue };
        let Some(o) = mines.get(i) else {
            *vis = Visibility::Hidden;
            continue;
        };
        let n = Vec3::from(o.normal).normalize_or(Vec3::Y);
        *tr = Transform::from_translation(Vec3::from(o.pos) + n * 0.03).with_rotation(Quat::from_rotation_arc(Vec3::Y, n)).with_scale(Vec3::splat(GLYPH_RADIUS));
        let enemy_dist = (o.owner != me).then(|| eye.distance(Vec3::from(o.pos)));
        let gain = glyph_gain(o.armed, enemy_dist, t + i as f32, o.life_left);
        let c = colour_of(&net, o.owner);
        if let Some(mut mat) = mats.get_mut(m) {
            mat.base_color = Color::linear_rgb(c[0] * gain, c[1] * gain, c[2] * gain);
        }
        *vis = Visibility::Visible;
    }
    // ---- smoke ----
    let clouds: Vec<_> = v.star_objs.iter().filter(|o| o.kind == StarKind::Canister).collect();
    let radius = StarCfg::default().smoke_radius;
    for (ci, puffs) in look.puffs.iter().enumerate() {
        let cloud = clouds.get(ci);
        for (pi, (e, m)) in puffs.iter().enumerate() {
            let Ok((mut tr, mut vis)) = q.get_mut(*e) else { continue };
            let Some(o) = cloud else {
                *vis = Visibility::Hidden;
                continue;
            };
            let centre = Vec3::from(o.pos) + Vec3::from(o.normal);
            let seed = (ci * 31 + pi) as u32 * 7919;
            let dir = Vec3::new(hash01(seed) - 0.5, (hash01(seed ^ 1) - 0.5) * 0.7, hash01(seed ^ 2) - 0.5).normalize_or(Vec3::X);
            // puffs sit inside the cloud and slowly swirl; they swell in over the first half second
            let swirl = Quat::from_rotation_y(t * (0.25 + 0.2 * hash01(seed ^ 3)));
            let off = swirl * dir * radius * (0.18 + 0.5 * hash01(seed ^ 4));
            let size = radius * (0.42 + 0.22 * hash01(seed ^ 5)) * (1.0 + 0.05 * (t * 1.3 + pi as f32).sin());
            *tr = Transform::from_translation(centre + off).with_scale(Vec3::splat(size));
            let fade = (o.life_left / 0.15).min(1.0); // thins out over the last ~0.75 s of 5
            if let Some(mut mat) = mats.get_mut(m) {
                mat.base_color = Color::linear_rgba(0.5, 0.58, 0.7, 0.2 * fade);
            }
            *vis = Visibility::Visible;
        }
    }
}

fn spawn_bursts(mut commands: Commands, look: Option<Res<Look>>, view: Res<ViewRes>, net: Res<Net>, mut mats: ResMut<Assets<StandardMaterial>>, mut impacts: ResMut<ImpactQueue>) {
    let Some(look) = look else { return };
    let sc = StarCfg::default();
    for e in &view.0.events {
        let NetEvent::StarFx { what, owner, pos, .. } = e else { continue };
        let p = Vec3::from(shinobi_proto::quant::dq_pos3(*pos));
        let (colour, size, life, gain) = match *what {
            STAR_FX_FLASH_BURST => ([1.0, 1.0, 1.0], sc.flash_radius * 0.5, 0.28, 6.0),
            STAR_FX_BARREL_BURST => ([1.0, 0.45, 0.08], sc.mine_burst_radius, 0.35, 4.0),
            _ => continue,
        };
        let m = mats.add(StandardMaterial {
            base_color: Color::linear_rgb(colour[0] * gain, colour[1] * gain, colour[2] * gain),
            unlit: true,
            alpha_mode: AlphaMode::Add,
            double_sided: true,
            cull_mode: None,
            ..default()
        });
        commands.spawn((Mesh3d(look.sphere.clone()), MeshMaterial3d(m), Transform::from_translation(p).with_scale(Vec3::splat(0.05)), Shell { age: 0.0, life, size, col: [colour[0] * gain, colour[1] * gain, colour[2] * gain] }));
        let spark = if *what == STAR_FX_BARREL_BURST { colour } else { colour_of(&net, *owner) };
        impacts.0.push(Impact { pos: p, color: spark, kind: ImpactKind::Hit { kill: true } });
    }
}

fn animate_shells(mut commands: Commands, time: Res<Time>, mut mats: ResMut<Assets<StandardMaterial>>, mut q: Query<(Entity, &mut Transform, &mut Shell, &MeshMaterial3d<StandardMaterial>)>) {
    let dt = time.delta_secs();
    for (e, mut t, mut s, m) in &mut q {
        s.age += dt;
        if s.age >= s.life {
            mats.remove(&m.0);
            commands.entity(e).despawn();
            continue;
        }
        let u = s.age / s.life;
        // pops out fast (ease-out), fading as it goes
        let r = 1.0 - (1.0 - u) * (1.0 - u);
        t.scale = Vec3::splat(s.size * r.max(0.02));
        if let Some(mut mat) = mats.get_mut(&m.0) {
            let a = (1.0 - u).powi(2);
            mat.base_color = Color::linear_rgb(s.col[0] * a, s.col[1] * a, s.col[2] * a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mine_glyph_is_faint_until_an_enemy_is_close_and_it_is_armed() {
        let far = glyph_gain(true, Some(40.0), 0.3, 1.0);
        let near = glyph_gain(true, Some(3.0), 0.3, 1.0);
        let unarmed_near = glyph_gain(false, Some(3.0), 0.3, 1.0);
        let own = glyph_gain(true, None, 0.3, 1.0);
        assert!(far < 0.3 && own < 0.3, "faint for everyone: {far} {own}");
        assert!(near > 4.0 * far, "bright to a near enemy: {near} vs {far}");
        assert!(unarmed_near < 0.15, "an unarmed mine does not light up: {unarmed_near}");
        assert!(glyph_gain(true, Some(3.0), 0.3, 0.02) < near * 0.3, "flickers out at the end of its life");
    }

    #[test]
    fn glyph_mesh_is_flat_and_unit_sized() {
        let m = glyph_mesh();
        let pos = m.attribute(Mesh::ATTRIBUTE_POSITION).unwrap().as_float3().unwrap();
        assert!(pos.iter().all(|p| p[1] == 0.0 && (p[0] * p[0] + p[2] * p[2]).sqrt() <= 1.0001));
    }
}
