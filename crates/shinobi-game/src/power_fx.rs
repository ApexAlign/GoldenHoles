//! World FX of the power weapons (docs/powerups.md), purely cosmetic: yumi arrows with a streak trail, the digital bow held
//! out with its string drawn back and a thin laser line (a faint glint for everyone, bright when it points at your head), fuda
//! tags with a paper glyph and a countdown ring of pips, blast shells, and the glowing pedestals. Fed by
//! `RenderView::power_world` (snapshot arrows / tags / drawn bows), `RenderView::power` (our own drawn bow) and
//! `NetEvent::PowerFx` / `PickupSpawned` / `PickupTaken` (one-shots and pedestals).

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use shinobi_client::hud_power::glint_at;
use shinobi_client::power_view::{DrawObj, PowerWorld};
use shinobi_sim::combat::stars::view_dir;
use shinobi_sim::events::NetEvent;
use shinobi_sim::power::{PowerCfg, PowerKind};
use shinobi_sim::power_pass::*;

use crate::impacts::{Impact, ImpactKind, ImpactQueue};
use crate::net::{Net, ViewRes};
use crate::team::colour_of;

const ARROWS: usize = 12;
const DRAWS: usize = 6;
const TAGS: usize = 8;
const PEDS: usize = 12;
const PIPS: usize = 12;
const BOW_SEGS: usize = 8;
/// Zoom fraction at which our own bow leaves the screen.
const LOCAL_BOW_HIDE_ZOOM: f32 = 0.35;

/// Length of the laser line of a drawn bow (m).
const LASER_LEN: f32 = 90.0;
const TAG_RING_R: f32 = 0.55;
/// Floor drops vanish unseen after this long (the server removes them silently).
const DROP_LIFE: f32 = 25.0;
const DROP_ID: u32 = POWER_DROP_ID_BASE;
const GOLD: [f32; 3] = [1.0, 0.82, 0.25];
const ORANGE: [f32; 3] = [1.0, 0.34, 0.04];

struct ArrowSlot {
    shaft: Entity,
    trail: Entity,
    mat: Handle<StandardMaterial>,
}

struct DrawSlot {
    /// Dim limb body (the taper from riser to tips), additive so it never occludes the edge strip in front of it.
    bow: [Entity; BOW_SEGS],
    /// Bright neon strip along the outer (forward-facing) curve of each limb segment.
    edge: [Entity; BOW_SEGS],
    /// Dark matte grip/riser block at the centre.
    riser: Entity,
    /// Small bright caps at the two limb tips.
    tips: [Entity; 2],
    string: [Entity; 2],
    arrow: Entity,
    laser: Entity,
    /// Bright material: edge strip, tips, string, arrow.
    mat: Handle<StandardMaterial>,
    /// Dim material: the limb body panels.
    body_mat: Handle<StandardMaterial>,
    laser_mat: Handle<StandardMaterial>,
}

struct TagSlot {
    glyph: Entity,
    pips: [Entity; PIPS],
    gmat: Handle<StandardMaterial>,
    pmat: Handle<StandardMaterial>,
}

struct PedSlot {
    ring: Entity,
    beam: Entity,
    gem: Entity,
    mat: Handle<StandardMaterial>,
}

#[derive(Resource)]
struct Look {
    sphere: Handle<Mesh>,
    ring: Handle<Mesh>,
    arrows: Vec<ArrowSlot>,
    draws: Vec<DrawSlot>,
    tags: Vec<TagSlot>,
    peds: Vec<PedSlot>,
}

#[derive(Clone, Copy)]
struct Ped {
    pos: Vec3,
    weapon: u8,
    up: bool,
    born: f32,
}

#[derive(Resource, Default)]
struct Pedestals(HashMap<u32, Ped>);

#[derive(Component)]
struct Shell {
    age: f32,
    life: f32,
    size: f32,
    col: [f32; 3],
    flat: bool,
}

pub struct PowerFxPlugin;

impl Plugin for PowerFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Pedestals>().add_systems(Startup, setup.after(crate::play::setup)).add_systems(
            Update,
            (track_pedestals, update_arrows, update_draws, update_tags, update_pedestals, spawn_shells, animate_shells).chain(),
        );
    }
}

fn glow(mats: &mut Assets<StandardMaterial>) -> Handle<StandardMaterial> {
    mats.add(StandardMaterial { base_color: Color::BLACK, unlit: true, alpha_mode: AlphaMode::Add, double_sided: true, cull_mode: None, ..default() })
}

/// A dark, non-glowing, opaque material: the bow's riser/grip block (never tinted per-frame).
fn matte(mats: &mut Assets<StandardMaterial>) -> Handle<StandardMaterial> {
    mats.add(StandardMaterial { base_color: Color::linear_rgb(0.035, 0.035, 0.045), unlit: true, alpha_mode: AlphaMode::Opaque, double_sided: true, cull_mode: None, ..default() })
}

fn hidden(commands: &mut Commands, mesh: &Handle<Mesh>, mat: &Handle<StandardMaterial>) -> Entity {
    commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone()), Transform::default(), Visibility::Hidden, NoFrustumCulling)).id()
}

/// A streak ribbon: head at z = 0 (hot), tail at z = 1 (dark), two crossed strips so it reads from any angle.
pub fn trail_mesh() -> Mesh {
    let (mut pos, mut col, mut idx): (Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<u32>) = (Vec::new(), Vec::new(), Vec::new());
    let w = 0.11;
    for axis in 0..2 {
        let b = pos.len() as u32;
        let (a, c) = if axis == 0 { ([w, 0.0], [-w, 0.0]) } else { ([0.0, w], [0.0, -w]) };
        for (z, g) in [(0.0f32, 6.0f32), (0.25, 2.2), (1.0, 0.0)] {
            let k = 1.0 - 0.8 * z;
            pos.push([a[0] * k, a[1] * k, z]);
            pos.push([c[0] * k, c[1] * k, z]);
            col.extend([[g, g, g, 1.0]; 2]);
        }
        idx.extend([b, b + 1, b + 2, b + 1, b + 3, b + 2, b + 2, b + 3, b + 4, b + 3, b + 5, b + 4]);
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
        .with_inserted_indices(Indices::U32(idx))
}

/// A paper tag in the XY plane facing +Z: pale body, bright border, a fuda stroke pattern. Greyscale HDR (the material tints it).
pub fn tag_glyph_mesh() -> Mesh {
    let (mut pos, mut col, mut idx): (Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<u32>) = (Vec::new(), Vec::new(), Vec::new());
    let mut quad = |x0: f32, y0: f32, x1: f32, y1: f32, g: f32| {
        let b = pos.len() as u32;
        pos.extend([[x0, y0, 0.0], [x1, y0, 0.0], [x1, y1, 0.0], [x0, y1, 0.0]]);
        col.extend([[g, g, g, 1.0]; 4]);
        idx.extend([b, b + 1, b + 2, b, b + 2, b + 3]);
    };
    let (hw, hh, t) = (0.1, 0.18, 0.012);
    quad(-hw, -hh, hw, hh, 0.9);
    // border
    quad(-hw, -hh, hw, -hh + t, 3.5);
    quad(-hw, hh - t, hw, hh, 3.5);
    quad(-hw, -hh, -hw + t, hh, 3.5);
    quad(hw - t, -hh, hw, hh, 3.5);
    // a vertical brush stroke and three bars: reads as a talisman
    quad(-0.012, -0.14, 0.012, 0.14, 4.5);
    for y in [-0.09f32, 0.0, 0.09] {
        quad(-0.06, y - 0.008, 0.06, y + 0.008, 4.0);
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
        .with_inserted_indices(Indices::U32(idx))
}

/// Flat annulus in the XZ plane, unit outer radius.
pub fn ring_mesh() -> Mesh {
    let (mut pos, mut col, mut idx): (Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<u32>) = (Vec::new(), Vec::new(), Vec::new());
    let n = 48usize;
    for i in 0..n {
        let (a0, a1) = (i as f32 / n as f32 * std::f32::consts::TAU, (i + 1) as f32 / n as f32 * std::f32::consts::TAU);
        let b = pos.len() as u32;
        let p = |a: f32, r: f32| [r * a.cos(), 0.0, r * a.sin()];
        pos.extend([p(a0, 0.9), p(a0, 1.0), p(a1, 1.0), p(a1, 0.9)]);
        col.extend([[3.0, 3.0, 3.0, 1.0]; 4]);
        idx.extend([b, b + 1, b + 2, b, b + 2, b + 3]);
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
        .with_inserted_indices(Indices::U32(idx))
}

/// Transform of a unit cuboid (1 x 1 x 1, long axis = z) stretched between two points.
pub fn seg(a: Vec3, b: Vec3, thickness: f32) -> Transform {
    let d = b - a;
    let l = d.length().max(1e-4);
    Transform { translation: (a + b) * 0.5, rotation: Quat::from_rotation_arc(Vec3::Z, d / l), scale: Vec3::new(thickness, thickness, l) }
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let unit = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let trail = meshes.add(trail_mesh());
    let glyph = meshes.add(tag_glyph_mesh());
    let ring = meshes.add(ring_mesh());
    let sphere = meshes.add(Sphere::new(1.0).mesh().ico(3).unwrap());
    let arrows = (0..ARROWS)
        .map(|_| {
            let mat = glow(&mut mats);
            ArrowSlot { shaft: hidden(&mut commands, &unit, &mat), trail: hidden(&mut commands, &trail, &mat), mat }
        })
        .collect();
    let draws = (0..DRAWS)
        .map(|_| {
            let mat = glow(&mut mats);
            let body_mat = glow(&mut mats);
            let riser_mat = matte(&mut mats);
            let laser_mat = glow(&mut mats);
            DrawSlot {
                bow: std::array::from_fn(|_| hidden(&mut commands, &unit, &body_mat)),
                edge: std::array::from_fn(|_| hidden(&mut commands, &unit, &mat)),
                riser: hidden(&mut commands, &unit, &riser_mat),
                tips: std::array::from_fn(|_| hidden(&mut commands, &sphere, &mat)),
                string: std::array::from_fn(|_| hidden(&mut commands, &unit, &mat)),
                arrow: hidden(&mut commands, &unit, &mat),
                laser: hidden(&mut commands, &unit, &laser_mat),
                mat,
                body_mat,
                laser_mat,
            }
        })
        .collect();
    let tags = (0..TAGS)
        .map(|_| {
            let gmat = glow(&mut mats);
            let pmat = glow(&mut mats);
            TagSlot { glyph: hidden(&mut commands, &glyph, &gmat), pips: std::array::from_fn(|_| hidden(&mut commands, &unit, &pmat)), gmat, pmat }
        })
        .collect();
    let peds = (0..PEDS)
        .map(|_| {
            let mat = glow(&mut mats);
            PedSlot { ring: hidden(&mut commands, &ring, &mat), beam: hidden(&mut commands, &unit, &mat), gem: hidden(&mut commands, &unit, &mat), mat }
        })
        .collect();
    commands.insert_resource(Look { sphere, ring, arrows, draws, tags, peds });
}

fn tint(mats: &mut Assets<StandardMaterial>, h: &Handle<StandardMaterial>, c: [f32; 3], gain: f32) {
    if let Some(mut m) = mats.get_mut(h) {
        m.base_color = Color::linear_rgb(c[0] * gain, c[1] * gain, c[2] * gain);
    }
}

// ---- pedestals ----

fn track_pedestals(time: Res<Time>, view: Res<ViewRes>, mut peds: ResMut<Pedestals>) {
    let now = time.elapsed_secs();
    for e in &view.0.events {
        match e {
            NetEvent::PickupSpawned { id, pos, kind, .. } if *kind > 0 => {
                peds.0.insert(*id, Ped { pos: Vec3::from(shinobi_proto::quant::dq_pos3(*pos)), weapon: *kind, up: true, born: now });
            }
            NetEvent::PickupTaken { id, .. } => {
                if *id >= DROP_ID {
                    peds.0.remove(id);
                } else if let Some(p) = peds.0.get_mut(id) {
                    p.up = false;
                }
            }
            NetEvent::MapChange { .. } => peds.0.clear(),
            _ => {}
        }
    }
    peds.0.retain(|id, p| *id < DROP_ID || now - p.born < DROP_LIFE);
}

/// Pedestal glow 0..~1.5: a slow breath, a flash right after it respawns.
pub fn pedestal_gain(up: bool, t: f32, since_spawn: f32) -> f32 {
    if !up {
        return 0.05;
    }
    let breath = 0.55 + 0.25 * (t * 2.2).sin();
    breath + 0.9 * (1.0 - since_spawn / 0.8).clamp(0.0, 1.0)
}

fn update_pedestals(time: Res<Time>, peds: Res<Pedestals>, look: Option<Res<Look>>, mut mats: ResMut<Assets<StandardMaterial>>, mut q: Query<(&mut Transform, &mut Visibility)>) {
    let Some(look) = look else { return };
    let t = time.elapsed_secs();
    let mut list: Vec<(&u32, &Ped)> = peds.0.iter().collect();
    list.sort_by_key(|(id, _)| **id);
    for (i, slot) in look.peds.iter().enumerate() {
        let Some((_, p)) = list.get(i) else {
            for e in [slot.ring, slot.beam, slot.gem] {
                if let Ok((_, mut v)) = q.get_mut(e) {
                    *v = Visibility::Hidden;
                }
            }
            continue;
        };
        let col = if p.weapon == PowerKind::Yumi as u8 { GOLD } else { ORANGE };
        let gain = pedestal_gain(p.up, t + i as f32, t - p.born);
        tint(&mut mats, &slot.mat, col, gain * 1.1);
        let floor = p.pos - Vec3::Y * 0.95;
        let spin = Quat::from_rotation_y(t * 1.4);
        if let Ok((mut tr, mut v)) = q.get_mut(slot.ring) {
            *tr = Transform::from_translation(floor).with_rotation(spin).with_scale(Vec3::splat(0.85 + 0.08 * (t * 3.0).sin()));
            *v = Visibility::Visible;
        }
        if let Ok((mut tr, mut v)) = q.get_mut(slot.beam) {
            *tr = seg(floor, floor + Vec3::Y * if p.up { 4.5 } else { 0.0 }, 0.06);
            *v = if p.up { Visibility::Visible } else { Visibility::Hidden };
        }
        if let Ok((mut tr, mut v)) = q.get_mut(slot.gem) {
            let bob = 0.12 * (t * 2.0).sin();
            *tr = Transform::from_translation(p.pos + Vec3::Y * bob).with_rotation(Quat::from_euler(EulerRot::YXZ, t * 2.0, 0.7, 0.5)).with_scale(Vec3::splat(if p.weapon == PowerKind::Yumi as u8 { 0.3 } else { 0.24 }));
            *v = if p.up { Visibility::Visible } else { Visibility::Hidden };
        }
    }
}

// ---- arrows ----

fn update_arrows(view: Res<ViewRes>, net: Res<Net>, look: Option<Res<Look>>, mut mats: ResMut<Assets<StandardMaterial>>, mut q: Query<(&mut Transform, &mut Visibility)>) {
    let Some(look) = look else { return };
    let w = &view.0.power_world;
    for (i, slot) in look.arrows.iter().enumerate() {
        let Some(a) = w.arrows.get(i) else {
            for e in [slot.shaft, slot.trail] {
                if let Ok((_, mut v)) = q.get_mut(e) {
                    *v = Visibility::Hidden;
                }
            }
            continue;
        };
        let c = if a.reflected { [0.4, 1.0, 1.0] } else { colour_of(&net, a.owner) };
        tint(&mut mats, &slot.mat, c, 1.0);
        let p = Vec3::from(a.pos);
        let v = Vec3::from(a.vel);
        let dir = v.normalize_or(Vec3::NEG_Z);
        if let Ok((mut tr, mut vis)) = q.get_mut(slot.shaft) {
            *tr = seg(p - dir * 0.7, p + dir * 0.7, 0.1);
            *vis = Visibility::Visible;
        }
        if let Ok((mut tr, mut vis)) = q.get_mut(slot.trail) {
            // the streak runs back along the flight, longer the faster it goes
            let len = (v.length() * 0.09).clamp(2.0, 9.0);
            *tr = Transform::from_translation(p - dir * 0.5).with_rotation(Quat::from_rotation_arc(Vec3::Z, -dir)).with_scale(Vec3::new(1.0, 1.0, len));
            *vis = Visibility::Visible;
        }
    }
}

// ---- drawn bows ----

/// The bow of a drawer: 9 limb points in the plane of `up` and `dir`, bulging toward the target.
pub fn bow_points(centre: Vec3, up: Vec3, dir: Vec3) -> [Vec3; BOW_SEGS + 1] {
    bow_points_flexed(centre, up, dir, 1.0)
}

/// Same shape as `bow_points`, with the forward bulge scaled by `bend` (>1 flexes the recurve limbs further
/// forward): used so the bow visibly flexes more the further it's drawn back. On top of the broad mid-limb belly,
/// a small extra forward flick kicks in past `|t| > 0.78` (the "ears" curling toward the target at the tips),
/// so the silhouette reads as a recurve rather than a plain arc. The flick is identical at both tips, so it cancels
/// out of the tip-to-tip distance and doesn't disturb `the_bow_bulges_toward_the_target_and_its_tips_are_a_metre_apart`.
pub fn bow_points_flexed(centre: Vec3, up: Vec3, dir: Vec3, bend: f32) -> [Vec3; BOW_SEGS + 1] {
    std::array::from_fn(|k| {
        let t = -1.0 + 2.0 * k as f32 / BOW_SEGS as f32;
        let belly = 1.0 - t * t;
        let flick = (0.35 * ((t.abs() - 0.78) / 0.22).clamp(0.0, 1.0)).powi(2);
        centre + up * (t * 0.5) + dir * (bend * (0.14 * belly + 0.085 * flick))
    })
}

/// Limb thickness profile 0..1 at the midpoint of segment `si` of `BOW_SEGS`: thick at the riser, tapering to a
/// sharp point at the tips.
fn limb_profile(si: usize) -> f32 {
    let t0 = -1.0 + 2.0 * si as f32 / BOW_SEGS as f32;
    let t1 = -1.0 + 2.0 * (si + 1) as f32 / BOW_SEGS as f32;
    let tm = (t0 + t1) * 0.5;
    0.22 + 0.78 * (1.0 - tm * tm)
}

/// Everything shown for one drawn bow.
struct BowDraw {
    eye: Vec3,
    dir: Vec3,
    owner: u16,
    frac: f32,
    local: bool,
}

fn update_draws(view: Res<ViewRes>, net: Res<Net>, look: Option<Res<Look>>, mut mats: ResMut<Assets<StandardMaterial>>, mut q: Query<(&mut Transform, &mut Visibility)>, time: Res<Time>) {
    let Some(look) = look else { return };
    let v = &view.0;
    let me = net.0.own_id().unwrap_or(0);
    let mut list: Vec<BowDraw> = v.power_world.draws.iter().filter(|d| d.owner != me).map(|d| BowDraw { eye: Vec3::from(d.pos), dir: Vec3::from(d.dir).normalize_or(Vec3::NEG_Z), owner: d.owner, frac: d.frac, local: false }).collect();
    // zoomed in, our own bow is out of frame (gone by `LOCAL_BOW_HIDE_ZOOM`); on the way there it is scaled with the FOV so it keeps
    // its hip-fire size on screen instead of ballooning in the narrowed view
    let fov_k = if v.camera.fov > 1.0 { ((v.camera.fov.to_radians() * 0.5).tan() / (45f32).to_radians().tan()).clamp(0.1, 1.5) } else { 1.0 };
    if let Some(p) = v.power.filter(|p| p.kind == PowerKind::Yumi && p.draw > 0.0 && v.zoom.frac < LOCAL_BOW_HIDE_ZOOM) {
        list.insert(0, BowDraw { eye: Vec3::from(v.eye), dir: Vec3::from(view_dir(v.aim.yaw, v.aim.pitch)), owner: me, frac: p.draw, local: true });
    }
    let t = time.elapsed_secs();
    for (i, slot) in look.draws.iter().enumerate() {
        let all: Vec<Entity> =
            slot.bow.iter().chain(slot.edge.iter()).chain(slot.string.iter()).copied().chain(slot.tips).chain([slot.riser, slot.arrow, slot.laser]).collect();
        let Some(d) = list.get(i) else {
            for e in all {
                if let Ok((_, mut vis)) = q.get_mut(e) {
                    *vis = Visibility::Hidden;
                }
            }
            continue;
        };
        let right = d.dir.cross(Vec3::Y).normalize_or(Vec3::X);
        let up = right.cross(d.dir).normalize_or(Vec3::Y);
        // our own bow sits low and to the left of the view; another player's out in front of their chest
        let k = if d.local { fov_k } else { 1.0 };
        let centre = if d.local { d.eye + d.dir * 0.78 - (right * 0.42 + up * 0.3) * k } else { d.eye + d.dir * 0.8 - up * 0.28 };
        let th = if d.local { 0.6 * k } else { 1.0 };
        let col = if d.local { GOLD } else { colour_of(&net, d.owner) };
        let bright = if d.local { 1.1 + 0.6 * d.frac } else { 2.0 + 1.0 * d.frac };
        tint(&mut mats, &slot.mat, col, bright);
        tint(&mut mats, &slot.body_mat, col, bright * 0.48);
        // the recurve limbs flex further forward the more the bow is drawn
        let bend = 1.0 + 0.6 * d.frac;
        // first person looks straight down `d.dir`, the same axis the belly bulges along: left as-is, the recurve
        // silhouette would be seen dead-on edge and read as a flat line. Tilting the shape's own forward axis off
        // the view by ~35 degrees (toward `right`) turns the bow broadside enough to actually show the curve,
        // without changing where the bow sits or which way the arrow points (those stay on the true `d.dir`).
        let shape_dir = if d.local { (d.dir * 0.82 + right * 0.57).normalize_or(d.dir) } else { d.dir };
        let pts = bow_points_flexed(centre, up, shape_dir, bend).map(|q| centre + (q - centre) * k);
        // the glowing edge traces the outer (forward-facing) curve, offset just in front of the dim limb body so
        // it never gets occluded by it; both are additive so there's no z-fighting either way
        let edge_off = shape_dir * (0.03 * th);
        for si in 0..BOW_SEGS {
            let profile = limb_profile(si);
            if let Ok((mut tr, mut vis)) = q.get_mut(slot.bow[si]) {
                *tr = seg(pts[si], pts[si + 1], 0.052 * th * profile);
                *vis = Visibility::Visible;
            }
            if let Ok((mut tr, mut vis)) = q.get_mut(slot.edge[si]) {
                *tr = seg(pts[si] + edge_off, pts[si + 1] + edge_off, 0.03 * th * profile.max(0.5));
                *vis = Visibility::Visible;
            }
        }
        // riser/grip: a dark, non-glowing block at the centre, set slightly behind the limbs' forward bulge
        let riser_c = centre - shape_dir * (0.012 * th);
        if let Ok((mut tr, mut vis)) = q.get_mut(slot.riser) {
            *tr = Transform { translation: riser_c, rotation: Quat::from_rotation_arc(Vec3::Z, up), scale: Vec3::new(0.062 * th, 0.042 * th, 0.20 * th) };
            *vis = Visibility::Visible;
        }
        // small bright caps at the limb tips, alongside the string run (tip -> nock)
        let nock = centre - d.dir * (0.32 * d.frac * k);
        for (si, tip) in [pts[0], pts[BOW_SEGS]].into_iter().enumerate() {
            if let Ok((mut tr, mut vis)) = q.get_mut(slot.tips[si]) {
                *tr = Transform::from_translation(tip + edge_off).with_scale(Vec3::splat(0.028 * th));
                *vis = Visibility::Visible;
            }
            if let Ok((mut tr, mut vis)) = q.get_mut(slot.string[si]) {
                *tr = seg(tip, nock, 0.009 * th);
                *vis = Visibility::Visible;
            }
        }
        if let Ok((mut tr, mut vis)) = q.get_mut(slot.arrow) {
            *tr = seg(nock - d.dir * 0.05 * k, nock + d.dir * 0.95 * k, 0.03 * th);
            *vis = Visibility::Visible;
        }
        // the aim line: a faint glint for everyone, a bright one on the head it points at
        if let Ok((mut tr, mut vis)) = q.get_mut(slot.laser) {
            if d.local {
                *vis = Visibility::Hidden;
            } else {
                let world = PowerWorld { draws: vec![DrawObj { pos: d.eye.to_array(), dir: d.dir.to_array(), owner: d.owner, frac: d.frac }], ..Default::default() };
                let on_me = glint_at(&world, me, Vec3::from(v.eye).to_array());
                let flicker = 0.85 + 0.15 * (t * 31.0 + i as f32).sin();
                tint(&mut mats, &slot.laser_mat, [1.0, 0.18, 0.5], (0.10 + 0.35 * d.frac) * flicker + 2.2 * on_me * flicker);
                // pointing at our head, the line stops a metre short of the camera (it would fill the screen otherwise)
                let reach = if on_me > 0.1 { (Vec3::from(v.eye) - d.eye).dot(d.dir) - 1.2 } else { LASER_LEN };
                let a = centre + d.dir * 0.5;
                *tr = seg(a, d.eye + d.dir * reach.clamp(1.0, LASER_LEN), if on_me > 0.1 { 0.02 } else { 0.012 });
                *vis = Visibility::Visible;
            }
        }
    }
}

// ---- fuda tags ----

/// How fast a stuck tag blinks (Hz): slow while it arms, faster and faster as the fuse runs down.
pub fn tag_blink_hz(armed: bool, fuse_left: f32) -> f32 {
    if !armed {
        2.0
    } else {
        4.0 + 18.0 * (1.0 - fuse_left.clamp(0.0, 1.0))
    }
}

fn update_tags(time: Res<Time>, view: Res<ViewRes>, net: Res<Net>, look: Option<Res<Look>>, mut mats: ResMut<Assets<StandardMaterial>>, mut q: Query<(&mut Transform, &mut Visibility)>) {
    let Some(look) = look else { return };
    let t = time.elapsed_secs();
    let tags = &view.0.power_world.tags;
    for (i, slot) in look.tags.iter().enumerate() {
        let all: Vec<Entity> = std::iter::once(slot.glyph).chain(slot.pips.iter().copied()).collect();
        let Some(g) = tags.get(i) else {
            for e in all {
                if let Ok((_, mut vis)) = q.get_mut(e) {
                    *vis = Visibility::Hidden;
                }
            }
            continue;
        };
        let p = Vec3::from(g.pos);
        let owner = colour_of(&net, g.owner);
        let rot = if g.flying {
            let dir = Vec3::from(g.vel).normalize_or(Vec3::NEG_Z);
            Quat::from_rotation_arc(Vec3::Z, dir) * Quat::from_rotation_z(t * 14.0 + i as f32)
        } else {
            Quat::from_rotation_arc(Vec3::Z, Vec3::from(g.normal).normalize_or(Vec3::Y))
        };
        let hz = tag_blink_hz(g.armed, g.fuse_left);
        let blink = if g.flying { 0.8 } else { 0.55 + 0.45 * (t * hz * std::f32::consts::TAU).sin().abs() };
        let hot = if g.armed { [1.0, 0.12, 0.1] } else { ORANGE };
        tint(&mut mats, &slot.gmat, hot, 0.9 + 0.8 * blink);
        tint(&mut mats, &slot.pmat, if g.armed { hot } else { owner }, 1.2 + 2.0 * blink);
        if let Ok((mut tr, mut vis)) = q.get_mut(slot.glyph) {
            *tr = Transform::from_translation(p + rot * Vec3::Z * 0.03).with_rotation(rot).with_scale(Vec3::splat(if g.flying { 1.2 } else { 2.0 }));
            *vis = Visibility::Visible;
        }
        // the countdown ring: one pip per twelfth of the fuse left, clockwise from the top
        let lit = if g.flying { 0 } else { (g.fuse_left * PIPS as f32).ceil() as usize };
        for (k, e) in slot.pips.iter().enumerate() {
            if let Ok((mut tr, mut vis)) = q.get_mut(*e) {
                let a = std::f32::consts::FRAC_PI_2 - k as f32 / PIPS as f32 * std::f32::consts::TAU;
                let off = rot * Vec3::new(a.cos(), a.sin(), 0.0) * TAG_RING_R;
                *tr = Transform::from_translation(p + off + rot * Vec3::Z * 0.04).with_rotation(rot).with_scale(Vec3::new(0.07, 0.07, 0.012));
                *vis = if k < lit { Visibility::Visible } else { Visibility::Hidden };
            }
        }
    }
}

// ---- one-shots ----

fn spawn_shells(mut commands: Commands, look: Option<Res<Look>>, view: Res<ViewRes>, net: Res<Net>, mut mats: ResMut<Assets<StandardMaterial>>, mut impacts: ResMut<ImpactQueue>) {
    let Some(look) = look else { return };
    let tc = PowerCfg::default().tag;
    let mut shell = |commands: &mut Commands, mats: &mut Assets<StandardMaterial>, p: Vec3, col: [f32; 3], gain: f32, size: f32, life: f32, flat: bool| {
        let m = mats.add(StandardMaterial { base_color: Color::linear_rgb(col[0] * gain, col[1] * gain, col[2] * gain), unlit: true, alpha_mode: AlphaMode::Add, double_sided: true, cull_mode: None, ..default() });
        let mesh = if flat { look.ring.clone() } else { look.sphere.clone() };
        commands.spawn((Mesh3d(mesh), MeshMaterial3d(m), Transform::from_translation(p).with_scale(Vec3::splat(0.05)), Shell { age: 0.0, life, size, col: [col[0] * gain, col[1] * gain, col[2] * gain], flat }));
    };
    for e in &view.0.events {
        let NetEvent::PowerFx { what, owner, pos, .. } = e else { continue };
        let p = Vec3::from(shinobi_proto::quant::dq_pos3(*pos));
        let oc = colour_of(&net, *owner);
        match *what {
            FX_TAG_BLAST => {
                shell(&mut commands, &mut mats, p, [1.0, 0.5, 0.1], 4.0, tc.radius, 0.35, false);
                shell(&mut commands, &mut mats, p, [1.0, 0.85, 0.5], 3.0, tc.radius, 0.5, true);
                impacts.0.push(Impact { pos: p, color: [1.0, 0.45, 0.08], kind: ImpactKind::Hit { kill: true } });
            }
            FX_ARROW_HIT | FX_ARROW_HEAD => impacts.0.push(Impact { pos: p, color: oc, kind: ImpactKind::Hit { kill: *what == FX_ARROW_HEAD } }),
            FX_ARROW_BLOCKED => {
                shell(&mut commands, &mut mats, p, [0.3, 0.9, 1.0], 3.0, 0.9, 0.18, false);
                impacts.0.push(Impact { pos: p, color: [0.3, 0.9, 1.0], kind: ImpactKind::Gone });
            }
            FX_ARROW_PARRIED => {
                shell(&mut commands, &mut mats, p, [0.4, 1.0, 1.0], 5.0, 1.8, 0.3, false);
                shell(&mut commands, &mut mats, p, [0.4, 1.0, 1.0], 3.0, 1.6, 0.35, true);
            }
            FX_ARROW_CUT | FX_TAG_CUT => {
                shell(&mut commands, &mut mats, p, [1.0, 1.0, 1.0], 5.0, 0.7, 0.16, false);
                impacts.0.push(Impact { pos: p, color: [1.0, 1.0, 1.0], kind: ImpactKind::Gone });
            }
            FX_ARROW_WALL => impacts.0.push(Impact { pos: p, color: oc, kind: ImpactKind::Gone }),
            FX_LOOSE => shell(&mut commands, &mut mats, p, GOLD, 4.0, 0.9, 0.12, false),
            FX_TAG_STUCK => shell(&mut commands, &mut mats, p, ORANGE, 3.0, 0.5, 0.15, false),
            FX_TAG_ARMED => shell(&mut commands, &mut mats, p, [1.0, 0.1, 0.1], 3.0, 0.7, 0.2, true),
            _ => {}
        }
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
        let r = 1.0 - (1.0 - u) * (1.0 - u);
        t.scale = Vec3::splat(s.size * r.max(0.02));
        if s.flat {
            t.scale.y = 1.0; // a ring on the floor stays flat
        }
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
    fn a_segment_spans_its_two_points() {
        let t = seg(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, -4.0), 0.1);
        assert!((t.scale.z - 4.0).abs() < 1e-5 && (t.translation.z + 2.0).abs() < 1e-5);
        let end = t.transform_point(Vec3::new(0.0, 0.0, 0.5));
        assert!((end - Vec3::new(0.0, 0.0, -4.0)).length() < 1e-4, "{end:?}");
    }

    #[test]
    fn the_bow_bulges_toward_the_target_and_its_tips_are_a_metre_apart() {
        let pts = bow_points(Vec3::ZERO, Vec3::Y, Vec3::NEG_Z);
        assert!((pts[0] - pts[BOW_SEGS]).length() - 1.0 < 1e-4);
        assert!(pts[BOW_SEGS / 2].z < pts[0].z, "the grip is forward of the tips");
    }

    #[test]
    fn tags_blink_faster_as_the_fuse_runs_down_and_pedestals_flash_on_respawn() {
        assert!(tag_blink_hz(true, 0.1) > tag_blink_hz(true, 0.9));
        assert!(tag_blink_hz(false, 1.0) < tag_blink_hz(true, 1.0));
        assert!(pedestal_gain(true, 1.0, 0.05) > pedestal_gain(true, 1.0, 5.0));
        assert!(pedestal_gain(false, 1.0, 5.0) < 0.1);
    }

    #[test]
    fn the_meshes_are_well_formed() {
        for m in [trail_mesh(), tag_glyph_mesh(), ring_mesh()] {
            let n = m.attribute(Mesh::ATTRIBUTE_POSITION).unwrap().len();
            assert_eq!(m.attribute(Mesh::ATTRIBUTE_COLOR).unwrap().len(), n);
            assert!(m.indices().unwrap().iter().all(|i| i < n));
        }
    }
}
