//! Player-printed blocks, drawn as part of "The Grid": each block is a neon-edged cube in its owner's colour that is
//! PRINTED into the world (a bright scan layer rises through it while it builds up) and DE-REZZES when removed
//! (it vanishes into glowing voxel fragments that drift up and flicker out; no physical debris).
//! Driven purely by `RenderView::blocks` (server-confirmed + our pending), so new / removed cells are found by diffing
//! frames. Cosmetic only.

use std::collections::HashMap;

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

fn slab(b: &BlockInfo, size: f32, thick: f32) -> (Vec3, Vec3) {
    let scale = b.scale_q as f32 / SCALE_ONE as f32;
    (Vec3::from(b.piece.center(b.cell, size, thick)), Vec3::from(b.piece.half_extents(size, thick, scale)) * 2.0)
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
    mut tf: Query<&mut Transform>,
) {
    let (size, thick) = (view.0.block_size.max(0.1), view.0.block_thickness.max(0.02));
    let Some(cube) = viz.cube.clone() else { return };
    let dt = time.delta_secs();
    let prev: HashMap<Cell, u16> = viz.live.iter().map(|(c, l)| (*c, l.owner)).collect();
    let now: Vec<(u16, Cell)> = view.0.blocks.iter().map(|b| (b.owner, b.cell)).collect();
    let (added, removed) = diff(&prev, &now);
    let gd = GridData::tron(&Palette::TRON);

    for (owner, cell) in added {
        let Some(info) = view.0.blocks.iter().find(|b| b.cell == cell) else { continue };
        let hue = colour_of(&net, owner);
        let (c, full) = slab(info, size, thick);
        let b = LevelBox::new(c.to_array(), (full * 0.5).to_array());
        let tint = hue.map(|v| v * 1.4);
        let entity = commands
            .spawn((
                Mesh3d(cube.clone()),
                MeshMaterial3d(grids.add(GridFloor::for_box(&gd, &b, tint, 1.0, 300.0))),
                // starts collapsed; the loop below grows it while printing
                Transform::from_translation(c).with_scale(Vec3::new(0.001, 0.001, 0.001)),
            ))
            .id();
        let m = glow_mat(&mut viz, &mut mats, hue);
        commands.spawn((
            Mesh3d(cube.clone()),
            MeshMaterial3d(m),
            Transform::from_translation(c).with_scale(Vec3::new(full.x * 1.04, 0.03, full.z * 1.04)),
            PrintHead { y0: c.y - full.y * 0.5, y1: c.y + full.y * 0.5, age: 0.0, foot: Vec2::new(full.x, full.z) },
        ));
        viz.live.insert(cell, Live { entity, owner, age: 0.0, center: c, full });
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
    fn print_scan_eases_from_zero_to_one() {
        assert_eq!(print_progress(0.0), 0.0);
        assert_eq!(print_progress(PRINT_S), 1.0);
        assert!(print_progress(PRINT_S * 0.5) > 0.5, "eases out");
        assert!(print_progress(1.0) <= 1.0);
    }
}
