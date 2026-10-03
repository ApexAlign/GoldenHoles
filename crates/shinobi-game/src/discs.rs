//! World-space shurikens in flight: big spinning tron discs (glowing ring, hot blade tips, bright hub), drawn on the
//! world camera so bloom picks them up. Fed by `RenderView::discs` (the local player's predicted throws).

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use std::collections::VecDeque;

use shinobi_client::fx::{lightwall_alpha_age, LightWallCfg};
use shinobi_client::hud::Palette;
use shinobi_sim::combat::shuriken::ShurikenCfg;

use crate::impacts::{Impact, ImpactKind, ImpactQueue};
use crate::net::ViewRes;
use crate::team::LocalColour;

const POOL: usize = 32;
const SPIN: f32 = 28.0; // rad/s
/// Flight trail: seconds a trail point lives, min spacing between points, half-width of the ribbon.
const TRAIL_LIFE: f32 = 0.35;
const TRAIL_SPACING: f32 = 0.18;
const TRAIL_HALF: f32 = 0.22;
const TRAIL_MAX: usize = 24;

/// One trail per pool slot. A slot whose disc jumps (a different disc took the slot) restarts its history.
struct Trail {
    /// Last drawn disc position when the slot was live (to place the end-of-flight puff).
    last: Option<Vec3>,
    pts: VecDeque<(Vec3, f32)>,
    mesh: Handle<Mesh>,
    entity: Entity,
}

#[derive(Resource)]
struct Trails(Vec<Trail>);

#[derive(Resource)]
pub struct DiscPool(Vec<Entity>);

pub struct DiscPlugin;

impl Plugin for DiscPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn.after(crate::play::setup)).add_systems(Update, (update, update_trails));
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

    // trails: additive HDR vertex-colour ribbons, one per disc slot
    let tmat = mats.add(StandardMaterial {
        base_color: Color::WHITE,
        unlit: true,
        alpha_mode: AlphaMode::Add,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    let trails = (0..POOL)
        .map(|_| {
            let mesh = meshes.add(empty_trail());
            let entity = commands
                .spawn((Mesh3d(mesh.clone()), MeshMaterial3d(tmat.clone()), Transform::default(), Visibility::Hidden, NoFrustumCulling))
                .id();
            Trail { last: None, pts: VecDeque::new(), mesh, entity }
        })
        .collect();
    commands.insert_resource(Trails(trails));
}

/// Degenerate transparent triangle: zero-vertex meshes upset Bevy's mesh slab allocator.
fn empty_trail() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleStrip, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3])
}

/// Two crossed strips (one along `up`, one along `side`) per live point, so the path reads from any angle.
/// Returns triangle-list data: positions, colours, indices.
fn trail_geometry(pts: &VecDeque<(Vec3, f32)>, now: f32, pal: &Palette) -> (Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<u32>) {
    let cfg = LightWallCfg { life_s: TRAIL_LIFE, fade_start: 0.0, max_alpha: 0.9, pulse_depth: 0.0, ..Default::default() };
    let (mut pos, mut col, mut idx) = (Vec::new(), Vec::new(), Vec::new());
    for k in 0..pts.len().saturating_sub(1) {
        let (a, b) = (pts[k], pts[k + 1]);
        let fwd = (b.0 - a.0).normalize_or(Vec3::NEG_Z);
        let side = fwd.cross(Vec3::Y).normalize_or(Vec3::X);
        let up = side.cross(fwd).normalize_or(Vec3::Y);
        for axis in [up, side] {
            let base = pos.len() as u32;
            for (p, born) in [a, b] {
                let heat = (1.0 - (now - born) / TRAIL_LIFE).clamp(0.0, 1.0);
                let w = TRAIL_HALF * (0.1 + 0.9 * heat); // taper to a point at the tail
                let al = lightwall_alpha_age(&cfg, now - born, 0, 0.0);
                let c = [pal.cyan.0 * 3.0 * heat + pal.orange.0 * 0.4, pal.cyan.1 * 3.0 * heat + pal.orange.1 * 0.4, pal.cyan.2 * 3.0 * heat, al];
                pos.push((p + axis * w).to_array());
                pos.push((p - axis * w).to_array());
                col.extend([c, c]);
            }
            idx.extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
        }
    }
    (pos, col, idx)
}

fn update_trails(
    time: Res<Time>,
    view: Res<ViewRes>,
    mut trails: ResMut<Trails>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut vis: Query<&mut Visibility>,
    mut impacts: ResMut<ImpactQueue>,
    local: Res<LocalColour>,
) {
    let now = time.elapsed_secs();
    let pal = Palette::TRON;
    for (i, tr) in trails.0.iter_mut().enumerate() {
        match view.0.discs.get(i) {
            Some((p, _)) => {
                let p = Vec3::from(*p);
                if tr.last.is_none() {
                    impacts.0.push(Impact { pos: p, color: local.0, kind: ImpactKind::Muzzle });
                }
                tr.last = Some(p);
                match tr.pts.back() {
                    Some((l, _)) if l.distance(p) > 3.0 => tr.pts.clear(), // slot reused by a different disc
                    _ => {}
                }
                if tr.pts.back().map_or(true, |(l, _)| l.distance(p) >= TRAIL_SPACING) {
                    tr.pts.push_back((p, now));
                    if tr.pts.len() > TRAIL_MAX {
                        tr.pts.pop_front();
                    }
                }
            }
            None => {
                if let Some(p) = tr.last.take() {
                    impacts.0.push(Impact { pos: p, color: local.0, kind: ImpactKind::Gone });
                }
            }
        }
        while tr.pts.front().map_or(false, |(_, b)| now - b > TRAIL_LIFE) {
            tr.pts.pop_front();
        }
        let live = view.0.discs.get(i).is_some();
        // head segment follows the disc exactly so the trail never lags behind it
        let mut pts = tr.pts.clone();
        if let (true, Some((p, _))) = (live, view.0.discs.get(i)) {
            pts.push_back((Vec3::from(*p), now));
        }
        let (pos, col, idx) = trail_geometry(&pts, now, &pal);
        let Ok(mut v) = vis.get_mut(tr.entity) else { continue };
        if idx.is_empty() {
            *v = Visibility::Hidden;
            continue;
        }
        if let Some(mut m) = meshes.get_mut(&tr.mesh) {
            *m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
                .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
                .with_inserted_indices(Indices::U32(idx));
        }
        *v = Visibility::Visible;
    }
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
