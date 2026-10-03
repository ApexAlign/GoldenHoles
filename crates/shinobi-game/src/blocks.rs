//! Player-printed blocks, drawn as part of "The Grid": each block is a neon-edged cube in its owner's colour that is
//! PRINTED into the world (a bright scan layer rises through it while it builds up) and DE-REZZES when removed
//! (it vanishes into glowing voxel fragments that drift up and flicker out; no physical debris).
//! Driven purely by `RenderView::blocks` (server-confirmed + our pending), so new / removed cells are found by diffing
//! frames. Cosmetic only.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::PrimitiveTopology;
use bevy::prelude::*;

use shinobi_client::shaders::GridMaterial as GridData;
use shinobi_client::hud::Palette;
use shinobi_sim::blocks::{BlockInfo, SCALE_ONE};
use shinobi_sim::level::LevelBox;

use crate::grid::GridFloor;
use crate::net::{Net, ViewRes};
use crate::team::{colour_of, key};

type Cell = [i32; 3];

const PRINT_S: f32 = 0.35;
const DEREZ_S: f32 = 0.65;
const VOXELS: usize = 22;

#[derive(Component)]
struct PrintHead {
    /// Vertical extent of the slab being printed (bottom, top); the scan layer sweeps through it.
    y0: f32,
    y1: f32,
    age: f32,
    /// Full horizontal footprint (x, z) of the slab.
    foot: Vec2,
}

#[derive(Component)]
struct Voxel {
    vel: Vec3,
    age: f32,
    life: f32,
    size: f32,
    phase: f32,
}

struct Live {
    entity: Entity,
    owner: u16,
    age: f32,
    center: Vec3,
    /// Full extents of the finished slab.
    full: Vec3,
    /// Crack child entity and the severity bucket it was built for.
    crack: Option<(Entity, u8)>,
    seed: u32,
    thin: usize,
}

#[derive(Resource, Default)]
struct BlockViz {
    live: HashMap<Cell, Live>,
    /// HDR additive materials per quantised colour (fragments + print head).
    glow: HashMap<u32, Handle<StandardMaterial>>,
    cube: Option<Handle<Mesh>>,
}

pub struct BlocksPlugin;

impl Plugin for BlocksPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BlockViz>()
            .add_systems(Startup, setup)
            .add_systems(Update, (sync_blocks, animate_prints, animate_voxels).chain());
    }
}

fn setup(mut viz: ResMut<BlockViz>, mut meshes: ResMut<Assets<Mesh>>) {
    viz.cube = Some(meshes.add(Cuboid::new(1.0, 1.0, 1.0)));
}

/// Cells in `now` that are not in `prev` (printed) and cells in `prev` that are not in `now` (de-rezzed).
pub fn diff(prev: &HashMap<Cell, u16>, now: &[(u16, Cell)]) -> (Vec<(u16, Cell)>, Vec<(u16, Cell)>) {
    let added = now.iter().filter(|(_, c)| !prev.contains_key(c)).copied().collect();
    let removed = prev.iter().filter(|(c, _)| !now.iter().any(|(_, n)| n == *c)).map(|(c, o)| (*o, *c)).collect();
    (added, removed)
}

/// Height (0..1) of the print scan layer after `age` seconds; eases out, saturating at 1.
pub fn print_progress(age: f32) -> f32 {
    let t = (age / PRINT_S).clamp(0.0, 1.0);
    1.0 - (1.0 - t) * (1.0 - t)
}

fn glow_mat(viz: &mut BlockViz, mats: &mut Assets<StandardMaterial>, c: [f32; 3]) -> Handle<StandardMaterial> {
    viz.glow
        .entry(key(c))
        .or_insert_with(|| {
            mats.add(StandardMaterial {
                base_color: Color::linear_rgb(c[0] * 5.0, c[1] * 5.0, c[2] * 5.0),
                unlit: true,
                alpha_mode: AlphaMode::Add,
                ..default()
            })
        })
        .clone()
}

fn hash01(mut x: u32) -> f32 {
    x = x.wrapping_mul(0x9e37_79b1) ^ (x >> 15);
    x = x.wrapping_mul(0x85eb_ca6b) ^ (x >> 13);
    (x & 0xffff) as f32 / 65536.0
}

/// Centre, full extents and orientation of a piece (ramps are tilted by `Piece::rotation`).
fn slab(b: &BlockInfo, size: f32, thick: f32) -> (Vec3, Vec3, Quat) {
    let scale = b.scale_q as f32 / SCALE_ONE as f32;
    let [rx, ry, rz] = b.piece.rotation();
    (
        Vec3::from(b.piece.center(b.cell, size, thick)),
        Vec3::from(b.piece.half_extents(size, thick, scale)) * 2.0,
        Quat::from_euler(EulerRot::XYZ, rx, ry, rz),
    )
}

/// Index (0 = X, 1 = Y, 2 = Z) of the slab's thin axis in its own frame.
fn thin_axis(full: Vec3) -> usize {
    let a = full.to_array();
    (0..3).min_by(|&i, &j| a[i].total_cmp(&a[j])).unwrap_or(1)
}

/// Crack severity bucket 0..=8 from the wire health (255 = untouched, lower = more damaged).
pub fn crack_level(hp_q: u8) -> u8 {
    ((255 - hp_q as u32) * 8 / 255) as u8
}

/// Jagged fracture polylines on a slab face, in unit-cube coordinates (-0.5..0.5 on both face axes). More cracks and
/// longer branches as `level` (1..=8) rises. Deterministic from `seed`.
pub fn crack_paths(seed: u32, level: u8) -> Vec<Vec<[f32; 2]>> {
    let n = level.clamp(0, 8) as u32;
    (0..n)
        .map(|i| {
            let s = seed.wrapping_add(i * 104729);
            let mut p = [(hash01(s) - 0.5) * 0.6, (hash01(s ^ 0xa1) - 0.5) * 0.6];
            let mut ang = hash01(s ^ 0xb2) * std::f32::consts::TAU;
            let segs = 3 + (level as u32).min(5);
            let mut path = vec![p];
            for k in 0..segs {
                let h = hash01(s.wrapping_add(k * 31) ^ 0xc3);
                ang += (h - 0.5) * 1.1;
                let len = 0.10 + 0.08 * hash01(s.wrapping_add(k * 17) ^ 0xd4);
                p = [(p[0] + ang.cos() * len).clamp(-0.5, 0.5), (p[1] + ang.sin() * len).clamp(-0.5, 0.5)];
                path.push(p);
            }
            path
        })
        .collect()
}

/// Crack mesh for a slab whose thin axis is `thin`: the paths on both large faces, as thin quads in unit-cube space.
fn crack_mesh(seed: u32, level: u8, thin: usize) -> Mesh {
    let (u, v) = match thin {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    let w = 0.010 + 0.003 * level as f32;
    let (mut pos, mut idx) = (Vec::<[f32; 3]>::new(), Vec::<u32>::new());
    for side in [-0.503f32, 0.503] {
        for path in crack_paths(seed, level) {
            for seg in path.windows(2) {
                let (a, b) = (Vec2::from(seg[0]), Vec2::from(seg[1]));
                let n = (b - a).perp().normalize_or(Vec2::Y) * w * 0.5;
                let base = pos.len() as u32;
                for q in [a + n, a - n, b + n, b - n] {
                    let mut p = [0.0f32; 3];
                    p[u] = q.x;
                    p[v] = q.y;
                    p[thin] = side;
                    pos.push(p);
                }
                idx.extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
            }
        }
    }
    if pos.is_empty() {
        pos = vec![[0.0; 3]; 3];
        idx = vec![0, 1, 2];
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_indices(bevy::mesh::Indices::U32(idx))
}

/// Fragment cloud for the de-rez of a slab: random points inside it.
fn cloud_point(seed: u32, full: Vec3) -> Vec3 {
    Vec3::new(hash01(seed) - 0.5, hash01(seed ^ 0x1111) - 0.5, hash01(seed ^ 0x2222) - 0.5) * full
}

fn sync_blocks(
    mut commands: Commands,
    view: Res<ViewRes>,
    net: Res<Net>,
    time: Res<Time>,
    mut viz: ResMut<BlockViz>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut grids: ResMut<Assets<GridFloor>>,
    fog: Option<Res<crate::grid::FogFar>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut tf: Query<&mut Transform>,
) {
    let (size, thick) = (view.0.block_size.max(0.1), view.0.block_thickness.max(0.02));
    let Some(cube) = viz.cube.clone() else { return };
    let dt = time.delta_secs();
    let prev: HashMap<Cell, u16> = viz.live.iter().map(|(c, l)| (*c, l.owner)).collect();
    let now: Vec<(u16, Cell)> = view.0.blocks.iter().map(|b| (b.owner, b.cell)).collect();
    let (added, removed) = diff(&prev, &now);
    let gd = GridData::tron(&Palette::TRON);
    let fog_far = fog.as_ref().map_or(crate::grid::FogFar::DEFAULT, |f| f.0);

    for (owner, cell) in added {
        let Some(info) = view.0.blocks.iter().find(|b| b.cell == cell) else { continue };
        let hue = colour_of(&net, owner);
        let (c, full, rot) = slab(info, size, thick);
        // the grid shader can express a roll about Z (ramps rising along X); other tilts keep an axis-aligned frame
        let b = LevelBox::new(c.to_array(), (full * 0.5).to_array()).rolled(info.piece.rotation()[2]);
        let tint = hue.map(|v| v * 1.4);
        let entity = commands
            .spawn((
                Mesh3d(cube.clone()),
                MeshMaterial3d(grids.add(GridFloor::for_box(&gd, &b, tint, 1.0, fog_far))),
                // starts collapsed; the loop below grows it while printing
                Transform::from_translation(c).with_rotation(rot).with_scale(Vec3::new(0.001, 0.001, 0.001)),
            ))
            .id();
        let m = glow_mat(&mut viz, &mut mats, hue);
        commands.spawn((
            Mesh3d(cube.clone()),
            MeshMaterial3d(m),
            Transform::from_translation(c).with_scale(Vec3::new(full.x * 1.04, 0.03, full.z * 1.04)),
            PrintHead { y0: c.y - full.y * 0.5, y1: c.y + full.y * 0.5, age: 0.0, foot: Vec2::new(full.x, full.z) },
        ));
        let seed = (cell[0].wrapping_mul(73856093) ^ cell[1].wrapping_mul(19349663) ^ cell[2].wrapping_mul(83492791)) as u32;
        viz.live.insert(cell, Live { entity, owner, age: 0.0, center: c, full, crack: None, seed, thin: thin_axis(full) });
    }

    for (owner, cell) in removed {
        let Some(l) = viz.live.remove(&cell) else { continue };
        commands.entity(l.entity).despawn();
        let hue = colour_of(&net, owner);
        let m = glow_mat(&mut viz, &mut mats, hue);
        let seed = (cell[0].wrapping_mul(73856093) ^ cell[1].wrapping_mul(19349663) ^ cell[2].wrapping_mul(83492791)) as u32;
        let span = l.full.max_element().max(size * 0.5);
        for i in 0..VOXELS as u32 {
            let s = seed.wrapping_add(i * 7919);
            let off = cloud_point(s, l.full);
            let vel = Vec3::new(off.x, 0.4 + hash01(s ^ 0x3333) * 1.4, off.z) * 1.2;
            commands.spawn((
                Mesh3d(cube.clone()),
                MeshMaterial3d(m.clone()),
                Transform::from_translation(l.center + off).with_scale(Vec3::splat(span * 0.1)),
                Voxel { vel, age: 0.0, life: DEREZ_S * (0.6 + 0.4 * hash01(s ^ 0x4444)), size: span * (0.06 + 0.1 * hash01(s ^ 0x5555)), phase: hash01(s ^ 0x6666) * 6.28 },
            ));
        }
    }

    // cracks: glowing fracture lines that multiply as the piece's wire health drops
    let t = time.elapsed_secs();
    let cells: Vec<Cell> = viz.live.keys().copied().collect();
    for cell in cells {
        let level = crack_level(net.0.game.net.block_hp_q(cell));
        let (owner, seed, thin, entity, have) = {
            let l = &viz.live[&cell];
            (l.owner, l.seed, l.thin, l.entity, l.crack)
        };
        if have.map_or(0, |(_, lv)| lv) != level {
            if let Some((e, _)) = have {
                commands.entity(e).despawn();
            }
            let new = if level > 0 {
                let m = glow_mat(&mut viz, &mut mats, colour_of(&net, owner));
                let mesh = meshes.add(crack_mesh(seed, level, thin));
                let e = commands
                    .spawn((Mesh3d(mesh), MeshMaterial3d(m), Transform::default(), Visibility::Visible, ChildOf(entity)))
                    .id();
                Some((e, level))
            } else {
                None
            };
            if let Some(l) = viz.live.get_mut(&cell) {
                l.crack = new;
            }
        }
        // heavily damaged pieces flicker
        if let Some((e, lv)) = viz.live[&cell].crack {
            let on = lv < 5 || (t * 22.0 + seed as f32 * 0.37).sin() > -0.5;
            commands.entity(e).insert(if on { Visibility::Visible } else { Visibility::Hidden });
        }
    }

    // grow printing slabs from a point to full size
    for l in viz.live.values_mut() {
        if l.age >= PRINT_S {
            continue;
        }
        l.age += dt;
        let p = print_progress(l.age).max(0.001);
        if let Ok(mut t) = tf.get_mut(l.entity) {
            // the thin axis (slab thickness) is always full; the others grow
            let thin = l.full.min_element();
            let grow = |v: f32| if (v - thin).abs() < 1e-4 { v } else { v * p };
            t.scale = Vec3::new(grow(l.full.x), grow(l.full.y), grow(l.full.z));
        }
    }
}

fn animate_prints(mut commands: Commands, time: Res<Time>, mut q: Query<(Entity, &mut Transform, &mut PrintHead)>) {
    let dt = time.delta_secs();
    for (e, mut t, mut h) in &mut q {
        h.age += dt;
        if h.age >= PRINT_S {
            commands.entity(e).despawn();
            continue;
        }
        t.translation.y = h.y0 + (h.y1 - h.y0) * print_progress(h.age) + 0.02;
        let fade = 1.0 - h.age / PRINT_S;
        t.scale = Vec3::new(h.foot.x * 1.04, 0.02 + 0.04 * fade, h.foot.y * 1.04);
    }
}

fn animate_voxels(mut commands: Commands, time: Res<Time>, mut q: Query<(Entity, &mut Transform, &mut Voxel)>) {
    let dt = time.delta_secs();
    for (e, mut t, mut v) in &mut q {
        v.age += dt;
        if v.age >= v.life {
            commands.entity(e).despawn();
            continue;
        }
        t.translation += v.vel * dt;
        v.vel.y += 0.8 * dt; // data fragments float up
        let k = 1.0 - v.age / v.life;
        // digital flicker: fragments blink in and out as they fade
        let on = if (v.age * 38.0 + v.phase).sin() > -0.35 { 1.0 } else { 0.0 };
        t.scale = Vec3::splat(v.size * k * on);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_finds_printed_and_removed_cells() {
        let prev: HashMap<Cell, u16> = [([0, 0, 0], 1), ([1, 0, 0], 1), ([2, 0, 0], 2)].into();
        let now = [(1, [1, 0, 0]), (2, [2, 0, 0]), (3, [5, 5, 5])];
        let (added, removed) = diff(&prev, &now);
        assert_eq!(added, vec![(3, [5, 5, 5])]);
        assert_eq!(removed, vec![(1, [0, 0, 0])]);
        assert_eq!(diff(&prev, &[(1, [0, 0, 0]), (1, [1, 0, 0]), (2, [2, 0, 0])]), (vec![], vec![]));
    }

    #[test]
    fn crack_level_follows_health() {
        assert_eq!(crack_level(255), 0);
        assert_eq!(crack_level(1), 7);
        assert!(crack_level(128) >= 3 && crack_level(128) <= 4);
        assert!(crack_level(10) > crack_level(200));
    }

    #[test]
    fn cracks_grow_with_level_stay_on_the_face_and_are_deterministic() {
        assert!(crack_paths(7, 0).is_empty());
        let (a, b) = (crack_paths(7, 2), crack_paths(7, 8));
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 8);
        assert!(b[0].len() > a[0].len(), "deeper cracks branch longer");
        assert!(b.iter().flatten().all(|p| p[0].abs() <= 0.5 && p[1].abs() <= 0.5));
        assert_eq!(crack_paths(7, 4), crack_paths(7, 4));
    }

    #[test]
    fn thin_axis_is_the_slab_thickness() {
        assert_eq!(thin_axis(Vec3::new(1.0, 0.1, 1.0)), 1);
        assert_eq!(thin_axis(Vec3::new(0.1, 1.0, 1.0)), 0);
        assert_eq!(thin_axis(Vec3::new(1.0, 1.0, 0.1)), 2);
    }

    #[test]
    fn print_scan_eases_from_zero_to_one() {
        assert_eq!(print_progress(0.0), 0.0);
        assert_eq!(print_progress(PRINT_S), 1.0);
        assert!(print_progress(PRINT_S * 0.5) > 0.5, "eases out");
        assert!(print_progress(1.0) <= 1.0);
    }
}
