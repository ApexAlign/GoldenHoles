//! World-space shurikens in flight: big spinning tron discs (glowing ring, hot blade tips, bright hub), drawn on the
//! world camera so bloom picks them up. Fed by `RenderView::discs` (the local player's predicted throws).

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use shinobi_client::hud::Palette;
use shinobi_sim::combat::shuriken::ShurikenCfg;

use crate::net::ViewRes;

const POOL: usize = 32;
const SPIN: f32 = 28.0; // rad/s

#[derive(Resource)]
pub struct DiscPool(Vec<Entity>);

pub struct DiscPlugin;

impl Plugin for DiscPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn.after(crate::play::setup)).add_systems(Update, update);
    }
}

/// Flat disc in the XZ plane (normal +Y), unit outer radius, HDR vertex colours.
fn disc_mesh(pal: &Palette) -> Mesh {
    let (n, ring_in) = (48usize, 0.72f32);
    let hdr = |c: shinobi_client::hud::Rgba, g: f32| [c.0 * g, c.1 * g, c.2 * g, 1.0];
    let (ring, tip, hub) = (hdr(pal.cyan, 3.0), hdr(pal.orange, 5.0), hdr(pal.cyan, 6.0));
    let mut pos: Vec<[f32; 3]> = Vec::new();
    let mut col: Vec<[f32; 4]> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    let quad = |pos: &mut Vec<[f32; 3]>, col: &mut Vec<[f32; 4]>, idx: &mut Vec<u32>, p: [[f32; 3]; 4], c: [f32; 4]| {
        let b = pos.len() as u32;
        pos.extend(p);
        col.extend([c; 4]);
        idx.extend([b, b + 1, b + 2, b, b + 2, b + 3]);
    };
    let pt = |a: f32, r: f32| [r * a.cos(), 0.0, r * a.sin()];
    for i in 0..n {
        let (a0, a1) = (i as f32 / n as f32 * std::f32::consts::TAU, (i + 1) as f32 / n as f32 * std::f32::consts::TAU);
        quad(&mut pos, &mut col, &mut idx, [pt(a0, ring_in), pt(a0, 0.8), pt(a1, 0.8), pt(a1, ring_in)], ring);
    }
    // four blade tips sticking out past the ring
    for k in 0..4 {
        let a = k as f32 / 4.0 * std::f32::consts::TAU;
        let (l, r) = (a - 0.22, a + 0.22);
        let b = pos.len() as u32;
        pos.extend([pt(l, 0.78), pt(a + 0.08, 1.0), pt(r, 0.78)]);
        col.extend([tip; 3]);
        idx.extend([b, b + 1, b + 2]);
    }
    // hub
    let b = pos.len() as u32;
    pos.push([0.0; 3]);
    col.push(hub);
    for i in 0..16 {
        pos.push(pt(i as f32 / 16.0 * std::f32::consts::TAU, 0.3));
        col.push(hub);
    }
    for i in 0..16u32 {
        idx.extend([b, b + 1 + i, b + 1 + (i + 1) % 16]);
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
        .with_inserted_indices(Indices::U32(idx))
}

fn spawn(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let mesh = meshes.add(disc_mesh(&Palette::TRON));
    let mat = mats.add(StandardMaterial { base_color: Color::WHITE, unlit: true, double_sided: true, cull_mode: None, ..default() });
    let pool = (0..POOL)
        .map(|_| commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone()), Transform::default(), Visibility::Hidden, NoFrustumCulling)).id())
        .collect();
    commands.insert_resource(DiscPool(pool));
}

fn update(time: Res<Time>, view: Res<ViewRes>, pool: Res<DiscPool>, mut q: Query<(&mut Transform, &mut Visibility)>) {
    let r = ShurikenCfg::default().radius * 1.5; // drawn a little larger than the hitbox: it reads as the thing that hits
    let spin = time.elapsed_secs() * SPIN;
    for (i, e) in pool.0.iter().enumerate() {
        let Ok((mut t, mut v)) = q.get_mut(*e) else { continue };
        match view.0.discs.get(i) {
            Some((p, d)) => {
                let dir = Vec3::from(*d).normalize_or(Vec3::NEG_Z);
                let up = if dir.y.abs() > 0.95 { Vec3::X } else { Vec3::Y };
                *t = Transform::from_translation(Vec3::from(*p)).looking_to(dir, up);
                t.rotate_local_y(spin + i as f32);
                t.scale = Vec3::splat(r);
                *v = Visibility::Visible;
            }
            None => *v = Visibility::Hidden,
        }
    }
}
