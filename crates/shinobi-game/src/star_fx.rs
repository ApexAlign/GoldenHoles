//! World FX of the star variants (docs/stars.md). Fed by `RenderView::discs` / `disc_kinds` (flying barrels and
//! canisters), `RenderView::star_objs` (resting barrels, stuck canisters) and `NetEvent::StarFx` (one-shot bursts).
//! Purely cosmetic; the sim never reads any of it.
//!
//! * BARREL: a chunky steel drum with hazard stripes and a glowing band. Tumbles in flight; at rest the fuse sparks and
//!   the glow blinks faster as the fuse runs down (`fuse_blink_hz`). BURST: layered fireball, shock ring, sparks, debris
//!   chunks, smoke, a light flash, screen shake and a screen flash that fall off with distance (`blast_proximity`).
//! * CANISTER: a small yellow-green gas bottle with a valve and a blinking LED. Sticks and rides its host. BURST: the
//!   same recipe, smaller, shorter and sharper, with a greenish-yellow gas puff.
//! * FLASH: a white shell, a ring out to the 14 m reach, a light flash. (The screen white-out and the DAZED cue come from
//!   the server's `StarBlind` through `RenderView::blind` / `dazed`.)

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use shinobi_sim::combat::stars::{StarCfg, StarKind};
use shinobi_sim::events::{
    NetEvent, STAR_FX_BARREL_BURST, STAR_FX_BARREL_GONE, STAR_FX_BARREL_LANDED, STAR_FX_CANISTER_BURST, STAR_FX_CANISTER_GONE, STAR_FX_CANISTER_STUCK,
    STAR_FX_FLASH_BURST,
};

use crate::impacts::{Impact, ImpactKind, ImpactQueue};
use crate::net::ViewRes;
use crate::play::PlayerCam;

const BARREL_POOL: usize = 12;
const CANISTER_POOL: usize = 12;
/// Most sparks / debris / embers alive at once.
const MAX_BITS: usize = 420;
/// Barrel: half height and radius of the drawn drum (the flight ball is 0.45 m).
const BARREL_HH: f32 = 0.40;
const BARREL_R: f32 = 0.32;
/// Canister: half length and radius.
const CAN_HH: f32 = 0.19;
const CAN_R: f32 = 0.085;

type Rgb = [f32; 3];

#[derive(Clone, Copy)]
enum Paint {
    Solid(Rgb),
    /// Alternating stripes: colours and the number of stripes around.
    Stripes(Rgb, Rgb, usize),
}

#[derive(Clone, Copy)]
struct Band {
    y0: f32,
    y1: f32,
    r0: f32,
    r1: f32,
    paint: Paint,
}

const SEG: usize = 24;

/// Fake directional light on a lathe: a fixed light in the object's frame, so the drum never reads as a flat silhouette.
fn shade(a: f32) -> f32 {
    0.6 + 0.4 * (0.5 + 0.5 * (a - 0.9).cos())
}

/// A solid of revolution about Y from stacked bands (+ flat caps `(y, radius, colour)`), HDR vertex colours.
fn lathe(bands: &[Band], caps: &[(f32, f32, Rgb)]) -> Mesh {
    let (mut pos, mut col, mut idx): (Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<u32>) = (Vec::new(), Vec::new(), Vec::new());
    let at = |a: f32, r: f32, y: f32| [r * a.cos(), y, r * a.sin()];
    for b in bands {
        for i in 0..SEG {
            let (a0, a1) = (i as f32 / SEG as f32 * std::f32::consts::TAU, (i + 1) as f32 / SEG as f32 * std::f32::consts::TAU);
            let c = match b.paint {
                Paint::Solid(c) => c,
                Paint::Stripes(x, y, n) => {
                    if (i * n / SEG) % 2 == 0 {
                        x
                    } else {
                        y
                    }
                }
            };
            let k = shade((a0 + a1) * 0.5);
            let base = pos.len() as u32;
            pos.extend([at(a0, b.r0, b.y0), at(a1, b.r0, b.y0), at(a1, b.r1, b.y1), at(a0, b.r1, b.y1)]);
            col.extend([[c[0] * k, c[1] * k, c[2] * k, 1.0]; 4]);
            idx.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
    for (y, r, c) in caps {
        let base = pos.len() as u32;
        pos.push([0.0, *y, 0.0]);
        col.push([c[0], c[1], c[2], 1.0]);
        for i in 0..SEG {
            pos.push(at(i as f32 / SEG as f32 * std::f32::consts::TAU, *r, *y));
            col.push([c[0] * 0.8, c[1] * 0.8, c[2] * 0.8, 1.0]);
        }
        for i in 0..SEG as u32 {
            idx.extend([base, base + 1 + i, base + 1 + (i + 1) % SEG as u32]);
        }
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
        .with_inserted_indices(Indices::U32(idx))
}

const STEEL: Rgb = [0.20, 0.22, 0.27];
const STEEL_DARK: Rgb = [0.09, 0.10, 0.13];
const RIB: Rgb = [0.34, 0.36, 0.42];
const HAZARD: Rgb = [1.0, 0.72, 0.04];
const HAZARD_BLACK: Rgb = [0.04, 0.04, 0.05];

/// The drum: dark steel with ribs, two hazard-stripe rings, a bung and a fuse nub with a hot tip.
pub fn barrel_mesh() -> Mesh {
    let h = BARREL_HH;
    let r = BARREL_R;
    let band = |y0, y1, r0, r1, paint| Band { y0, y1, r0, r1, paint };
    lathe(
        &[
            band(-h, -h + 0.06, r * 0.84, r * 0.97, Paint::Solid(STEEL_DARK)),
            band(-h + 0.06, -0.14, r, r, Paint::Stripes(HAZARD, HAZARD_BLACK, 12)),
            band(-0.14, -0.09, r * 1.045, r * 1.045, Paint::Solid(RIB)),
            band(-0.09, 0.09, r, r, Paint::Solid(STEEL)),
            band(0.09, 0.14, r * 1.045, r * 1.045, Paint::Solid(RIB)),
            band(0.14, h - 0.06, r, r, Paint::Stripes(HAZARD, HAZARD_BLACK, 12)),
            band(h - 0.06, h, r * 0.97, r * 0.84, Paint::Solid(STEEL_DARK)),
            band(h, h + 0.03, 0.07, 0.07, Paint::Solid(RIB)),
            band(h + 0.03, h + 0.1, 0.035, 0.03, Paint::Solid([2.4, 0.7, 0.12])),
        ],
        &[(-h, r * 0.84, STEEL_DARK), (h, r * 0.84, STEEL), (h + 0.1, 0.03, [4.0, 1.4, 0.3])],
    )
}

/// The drum's glowing band (a thin ring a hair outside the steel); tinted and blinked by its material.
pub fn barrel_band_mesh() -> Mesh {
    lathe(&[Band { y0: -0.055, y1: 0.055, r0: BARREL_R * 1.012, r1: BARREL_R * 1.012, paint: Paint::Solid([1.0, 1.0, 1.0]) }], &[])
}

/// The gas bottle: yellow-green body, dark stripe, silver collar, valve stem and a red hand wheel.
pub fn canister_mesh() -> Mesh {
    let (h, r) = (CAN_HH, CAN_R);
    let body = [0.55, 0.78, 0.08];
    let silver = [0.72, 0.74, 0.8];
    let band = |y0, y1, r0, r1, paint| Band { y0, y1, r0, r1, paint };
    lathe(
        &[
            band(-h, -h + 0.02, r * 0.7, r, Paint::Solid(STEEL_DARK)),
            band(-h + 0.02, -0.06, r, r, Paint::Solid(body)),
            band(-0.06, 0.0, r * 1.03, r * 1.03, Paint::Solid(HAZARD_BLACK)),
            band(0.0, 0.1, r, r, Paint::Solid(body)),
            band(0.1, 0.12, r * 1.08, r * 1.08, Paint::Solid(silver)),
            band(0.12, 0.15, r, r * 0.45, Paint::Solid(body)),
            band(0.15, h, r * 0.36, r * 0.36, Paint::Solid(silver)),
            band(h, h + 0.014, r * 0.85, r * 0.85, Paint::Solid([0.8, 0.12, 0.1])),
        ],
        &[(-h, r * 0.7, STEEL_DARK), (h + 0.014, r * 0.85, [0.8, 0.12, 0.1])],
    )
}

/// Flat shock ring in the XZ plane, unit outer radius, white (the material tints it).
fn ring_mesh() -> Mesh {
    let (mut pos, mut idx): (Vec<[f32; 3]>, Vec<u32>) = (Vec::new(), Vec::new());
    let n = 64u32;
    for i in 0..n {
        let (a0, a1) = (i as f32 / n as f32 * std::f32::consts::TAU, (i + 1) as f32 / n as f32 * std::f32::consts::TAU);
        let b = pos.len() as u32;
        pos.extend([[0.86 * a0.cos(), 0.0, 0.86 * a0.sin()], [a0.cos(), 0.0, a0.sin()], [a1.cos(), 0.0, a1.sin()], [0.86 * a1.cos(), 0.0, 0.86 * a1.sin()]]);
        idx.extend([b, b + 1, b + 2, b, b + 2, b + 3]);
    }
    let col = vec![[1.0f32; 4]; pos.len()];
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
        .with_inserted_indices(Indices::U32(idx))
}

// ---------------------------------------------------------------- pure curves

/// Fuse blink rate (Hz) with `left` of the fuse remaining (1 fresh .. 0 about to go off): a slow heartbeat that
/// accelerates as it burns down, to a near-solid flicker at the end.
pub fn fuse_blink_hz(left: f32) -> f32 {
    let gone = 1.0 - left.clamp(0.0, 1.0);
    2.5 + 17.5 * gone.powf(1.6)
}

/// Blink brightness 0..=1 at a blink phase (cycles): a soft pulse, dark half the time.
pub fn blink_level(phase: f32) -> f32 {
    (0.5 + 0.5 * (phase.fract() * std::f32::consts::TAU).sin()).powf(1.6)
}

/// Fuse sparks per second at the nub: a trickle that turns into a spray at the end.
pub fn fuse_spark_rate(left: f32) -> f32 {
    22.0 + 70.0 * (1.0 - left.clamp(0.0, 1.0))
}

/// Fireball size factor over its life `u` (0..1): pops out fast (ease-out cubic over the first 30 %), then keeps
/// swelling a little as it dies.
pub fn fireball_scale(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    let pop = 1.0 - (1.0 - (u / 0.3).min(1.0)).powi(3);
    pop * (1.0 + 0.18 * u)
}

/// Fireball brightness over its life: hot at the start, gone at the end.
pub fn fireball_alpha(u: f32) -> f32 {
    (1.0 - u.clamp(0.0, 1.0)).powi(2)
}

/// How hard a blast of `radius` is felt `dist` metres from its centre, 0..=1: full inside the blast, gone at three radii.
/// Scales the camera shake, the screen flash and the ear muffle.
pub fn blast_proximity(dist: f32, radius: f32) -> f32 {
    if radius <= 0.0 {
        return 0.0;
    }
    (1.0 - dist.max(0.0) / (radius * 3.0)).clamp(0.0, 1.0).powi(2)
}

fn hash01(mut x: u32) -> f32 {
    x = x.wrapping_mul(0x9e37_79b1) ^ (x >> 15);
    x = x.wrapping_mul(0x85eb_ca6b) ^ (x >> 13);
    (x & 0xffff) as f32 / 65536.0
}

/// Tiny deterministic random stream for the particles.
struct Rng(u32);

impl Rng {
    fn f(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        hash01(self.0)
    }
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
    fn dir(&mut self) -> Vec3 {
        Vec3::new(self.f() - 0.5, self.f() - 0.5, self.f() - 0.5).normalize_or(Vec3::Y)
    }
}

// ---------------------------------------------------------------- resources / components

struct Prop {
    root: Entity,
    tip_mat: Handle<StandardMaterial>,
    band_mat: Option<Handle<StandardMaterial>>,
}

#[derive(Resource)]
struct Look {
    barrels: Vec<Prop>,
    canisters: Vec<Prop>,
    sphere: Handle<Mesh>,
    cube: Handle<Mesh>,
    ring: Handle<Mesh>,
    spark: Handle<StandardMaterial>,
    ember: Handle<StandardMaterial>,
    gas: Handle<StandardMaterial>,
    steel: Handle<StandardMaterial>,
    hazard: Handle<StandardMaterial>,
}

/// Camera shake and screen flash left from nearby blasts.
#[derive(Resource, Default)]
struct Blast {
    shake: f32,
    age: f32,
    flash: f32,
}

#[derive(Component)]
struct Shell {
    age: f32,
    life: f32,
    size: f32,
    col: [f32; 3],
}

#[derive(Component)]
struct Ring {
    age: f32,
    life: f32,
    size: f32,
    col: [f32; 3],
}

/// A spark / ember / debris chunk: ballistic, shrinks out.
#[derive(Component)]
struct Bit {
    vel: Vec3,
    grav: f32,
    drag: f32,
    age: f32,
    life: f32,
    size: f32,
    spin: Vec3,
}

#[derive(Component)]
struct Puff {
    age: f32,
    life: f32,
    from: f32,
    to: f32,
    rise: Vec3,
    col: [f32; 3],
    alpha: f32,
}

#[derive(Component)]
struct BlastLight {
    age: f32,
    life: f32,
    peak: f32,
}

pub struct StarFxPlugin;

impl Plugin for StarFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Blast>()
            .add_systems(Startup, setup.after(crate::play::setup))
            .add_systems(PreUpdate, blast_react)
            .add_systems(Update, (update_props, spawn_bursts, animate_shells, animate_rings, animate_bits, animate_puffs, animate_lights).chain())
            .add_systems(PostUpdate, shake.before(TransformSystems::Propagate));
    }
}

fn add_mat(mats: &mut Assets<StandardMaterial>, c: [f32; 4], mode: AlphaMode) -> Handle<StandardMaterial> {
    mats.add(StandardMaterial { base_color: Color::linear_rgba(c[0], c[1], c[2], c[3]), unlit: true, alpha_mode: mode, double_sided: true, cull_mode: None, ..default() })
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let sphere = meshes.add(Sphere::new(1.0).mesh().ico(3).unwrap());
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let ring = meshes.add(ring_mesh());
    let barrel_body = meshes.add(barrel_mesh());
    let barrel_band = meshes.add(barrel_band_mesh());
    let can_body = meshes.add(canister_mesh());
    let body_mat = mats.add(StandardMaterial { base_color: Color::WHITE, unlit: true, double_sided: true, cull_mode: None, ..default() });

    let make = |commands: &mut Commands, mats: &mut Assets<StandardMaterial>, body: &Handle<Mesh>, band: Option<&Handle<Mesh>>, tip_at: Vec3, tip_size: f32| {
        let tip_mat = add_mat(mats, [0.0, 0.0, 0.0, 1.0], AlphaMode::Add);
        let band_mat = band.map(|_| add_mat(mats, [0.0, 0.0, 0.0, 1.0], AlphaMode::Add));
        let root = commands.spawn((Transform::default(), Visibility::Hidden)).id();
        commands.entity(root).with_children(|c| {
            c.spawn((Mesh3d(body.clone()), MeshMaterial3d(body_mat.clone()), Transform::default(), NoFrustumCulling));
            if let (Some(b), Some(m)) = (band, &band_mat) {
                c.spawn((Mesh3d(b.clone()), MeshMaterial3d(m.clone()), Transform::default(), NoFrustumCulling));
            }
            c.spawn((Mesh3d(sphere.clone()), MeshMaterial3d(tip_mat.clone()), Transform::from_translation(tip_at).with_scale(Vec3::splat(tip_size)), NoFrustumCulling));
        });
        Prop { root, tip_mat, band_mat }
    };
    let barrels = (0..BARREL_POOL).map(|_| make(&mut commands, &mut mats, &barrel_body, Some(&barrel_band), Vec3::Y * (BARREL_HH + 0.12), 0.07)).collect();
    let canisters = (0..CANISTER_POOL).map(|_| make(&mut commands, &mut mats, &can_body, None, Vec3::Y * (CAN_HH + 0.05), 0.035)).collect();

    commands.insert_resource(Look {
        barrels,
        canisters,
        sphere,
        cube,
        ring,
        spark: add_mat(&mut mats, [7.0, 3.2, 0.7, 1.0], AlphaMode::Add),
        ember: add_mat(&mut mats, [3.5, 0.6, 0.1, 1.0], AlphaMode::Add),
        gas: add_mat(&mut mats, [2.2, 3.2, 0.5, 1.0], AlphaMode::Add),
        steel: mats.add(StandardMaterial { base_color: Color::linear_rgb(0.16, 0.17, 0.21), unlit: true, ..default() }),
        hazard: mats.add(StandardMaterial { base_color: Color::linear_rgb(0.9, 0.62, 0.04), unlit: true, ..default() }),
    });
}

// ---------------------------------------------------------------- live props

struct Item {
    kind: StarKind,
    pos: Vec3,
    rot: Quat,
    /// Fuse burning: `Some(fraction left)`.
    fuse: Option<f32>,
}

fn update_props(
    mut commands: Commands,
    time: Res<Time>,
    view: Res<ViewRes>,
    look: Option<Res<Look>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    bits: Query<(), With<Bit>>,
    mut q: Query<(&mut Transform, &mut Visibility)>,
    mut frame: Local<u32>,
    mut phases: Local<[Vec<f32>; 2]>,
) {
    let Some(look) = look else { return };
    let (t, dt) = (time.elapsed_secs(), time.delta_secs());
    *frame = frame.wrapping_add(1);
    let v = &view.0;
    let mut items: Vec<Item> = Vec::new();
    // flying: barrels tumble end over end, canisters spin like darts
    for (i, (p, d)) in v.discs.iter().enumerate() {
        let kind = v.disc_kinds.get(i).copied().unwrap_or(StarKind::Standard);
        if !matches!(kind, StarKind::Barrel | StarKind::Canister) {
            continue;
        }
        let dir = Vec3::from(*d).normalize_or(Vec3::NEG_Z);
        let rot = if kind == StarKind::Barrel {
            let side = dir.cross(Vec3::Y).normalize_or(Vec3::X);
            Quat::from_axis_angle(side, t * 9.0 + i as f32 * 1.7)
        } else {
            Quat::from_rotation_arc(Vec3::Y, dir) * Quat::from_rotation_y(t * 22.0)
        };
        items.push(Item { kind, pos: Vec3::from(*p), rot, fuse: None });
    }
    // landed: upright on the surface it rests on (a canister points out of it), a stable yaw per spot
    for (i, o) in v.star_objs.iter().enumerate() {
        if !matches!(o.kind, StarKind::Barrel | StarKind::Canister) {
            continue;
        }
        let n = Vec3::from(o.normal).normalize_or(Vec3::Y);
        let yaw = hash01((o.pos[0] * 31.0 + o.pos[2] * 17.0 + i as f32) as i32 as u32) * std::f32::consts::TAU;
        items.push(Item { kind: o.kind, pos: Vec3::from(o.pos), rot: Quat::from_rotation_arc(Vec3::Y, n) * Quat::from_rotation_y(yaw), fuse: o.armed.then_some(o.life_left) });
    }
    let mut budget = MAX_BITS.saturating_sub(bits.iter().count());
    let mut rng = Rng((*frame).wrapping_mul(2_654_435_761) ^ 0x51ed);
    for (ki, (kind, props)) in [(StarKind::Barrel, &look.barrels), (StarKind::Canister, &look.canisters)].into_iter().enumerate() {
        let mut mine = items.iter().filter(|it| it.kind == kind);
        phases[ki].resize(props.len(), 0.0);
        for (pi, prop) in props.iter().enumerate() {
            let Ok((mut tr, mut vis)) = q.get_mut(prop.root) else { continue };
            let Some(it) = mine.next() else {
                *vis = Visibility::Hidden;
                continue;
            };
            *tr = Transform { translation: it.pos, rotation: it.rot, scale: Vec3::ONE };
            *vis = Visibility::Visible;
            let (band_gain, tip_gain) = match it.fuse {
                Some(left) => {
                    phases[ki][pi] += fuse_blink_hz(left) * dt;
                    // the last moments burn solid
                    let lit = if left < 0.1 { 1.0 } else { blink_level(phases[ki][pi]) };
                    (0.7 + 5.0 * lit, 0.8 + 9.0 * lit)
                }
                None => (1.6, 2.2),
            };
            let (band_c, tip_c) = if kind == StarKind::Barrel { ([1.0, 0.2, 0.04], [1.0, 0.5, 0.1]) } else { ([0.0; 3], [0.7, 1.0, 0.1]) };
            if let (Some(h), true) = (&prop.band_mat, kind == StarKind::Barrel) {
                if let Some(mut m) = mats.get_mut(h) {
                    m.base_color = Color::linear_rgb(band_c[0] * band_gain, band_c[1] * band_gain, band_c[2] * band_gain);
                }
            }
            if let Some(mut m) = mats.get_mut(&prop.tip_mat) {
                m.base_color = Color::linear_rgb(tip_c[0] * tip_gain, tip_c[1] * tip_gain, tip_c[2] * tip_gain);
            }
            // fuse sparks (barrel) / a thin hiss of gas (canister) at the nub
            if let Some(left) = it.fuse {
                let up = it.rot * Vec3::Y;
                let (nub, rate) = if kind == StarKind::Barrel { (it.pos + up * (BARREL_HH + 0.1), fuse_spark_rate(left)) } else { (it.pos + up * (CAN_HH + 0.05), 10.0) };
                let n = rate * dt;
                let count = n.floor() as usize + (rng.f() < n.fract()) as usize;
                for _ in 0..count.min(budget) {
                    budget -= 1;
                    let (vel, mat, life, size) = if kind == StarKind::Barrel {
                        (up * rng.range(1.0, 3.0) + rng.dir() * rng.range(1.0, 3.5), if rng.f() < 0.35 { look.ember.clone() } else { look.spark.clone() }, rng.range(0.25, 0.55), rng.range(0.018, 0.035))
                    } else {
                        (up * rng.range(0.6, 1.4) + rng.dir() * 0.5, look.gas.clone(), rng.range(0.3, 0.6), rng.range(0.025, 0.05))
                    };
                    spawn_bit(&mut commands, &look.sphere, mat, nub, vel, 7.0, 0.5, life, size, Vec3::ZERO);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_bit(commands: &mut Commands, mesh: &Handle<Mesh>, mat: Handle<StandardMaterial>, pos: Vec3, vel: Vec3, grav: f32, drag: f32, life: f32, size: f32, spin: Vec3) {
    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(mat),
        Transform::from_translation(pos).with_scale(Vec3::splat(size)),
        Bit { vel, grav, drag, age: 0.0, life, size, spin },
        NoFrustumCulling,
    ));
}

// ---------------------------------------------------------------- bursts

#[derive(Clone, Copy, PartialEq)]
enum Style {
    Barrel,
    Canister,
    Flash,
}

fn spawn_bursts(mut commands: Commands, look: Option<Res<Look>>, view: Res<ViewRes>, mut mats: ResMut<Assets<StandardMaterial>>, mut impacts: ResMut<ImpactQueue>, time: Res<Time>) {
    let Some(look) = look else { return };
    let sc = StarCfg::default();
    for (n, e) in view.0.events.iter().enumerate() {
        let NetEvent::StarFx { what, pos, .. } = e else { continue };
        let p = Vec3::from(shinobi_proto::quant::dq_pos3(*pos));
        let mut rng = Rng(((p.x * 97.0 + p.z * 61.0) as i32 as u32) ^ (time.elapsed_secs() * 1000.0) as u32 ^ n as u32);
        match *what {
            STAR_FX_BARREL_BURST => explode(&mut commands, &look, &mut mats, &mut impacts, &mut rng, Style::Barrel, p, sc.barrel_radius),
            STAR_FX_CANISTER_BURST => explode(&mut commands, &look, &mut mats, &mut impacts, &mut rng, Style::Canister, p, sc.canister_radius),
            STAR_FX_FLASH_BURST => explode(&mut commands, &look, &mut mats, &mut impacts, &mut rng, Style::Flash, p, sc.flash_radius),
            // a barrel / canister vanished without a blast (its fuse failsafe): a fizzle and a wisp
            STAR_FX_BARREL_GONE | STAR_FX_CANISTER_GONE => {
                for _ in 0..10 {
                    let (vel, size, life) = (rng.dir() * rng.range(1.5, 4.0) + Vec3::Y * 2.0, rng.range(0.02, 0.04), rng.range(0.3, 0.6));
                    spawn_bit(&mut commands, &look.sphere, look.ember.clone(), p, vel, 9.0, 0.6, life, size, Vec3::ZERO);
                }
                puff(&mut commands, &look, &mut mats, p, 0.3, 0.9, 1.0, [0.45, 0.45, 0.5], 0.3, Vec3::Y * 0.6);
            }
            // landing: a dust puff and a few sparks off the steel
            STAR_FX_BARREL_LANDED | STAR_FX_CANISTER_STUCK => {
                let n = if *what == STAR_FX_BARREL_LANDED { 14 } else { 8 };
                for _ in 0..n {
                    let (vel, size, life) = (rng.dir() * rng.range(2.0, 5.0) + Vec3::Y * 1.5, rng.range(0.015, 0.03), rng.range(0.15, 0.4));
                    spawn_bit(&mut commands, &look.sphere, look.spark.clone(), p, vel, 12.0, 0.8, life, size, Vec3::ZERO);
                }
                if *what == STAR_FX_BARREL_LANDED {
                    puff(&mut commands, &look, &mut mats, p - Vec3::Y * 0.2, 0.25, 0.9, 0.7, [0.5, 0.48, 0.45], 0.25, Vec3::Y * 0.3);
                }
            }
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn shell(commands: &mut Commands, look: &Look, mats: &mut Assets<StandardMaterial>, p: Vec3, size: f32, life: f32, col: [f32; 3]) {
    let m = add_mat(mats, [col[0], col[1], col[2], 1.0], AlphaMode::Add);
    commands.spawn((Mesh3d(look.sphere.clone()), MeshMaterial3d(m), Transform::from_translation(p).with_scale(Vec3::splat(0.05)), Shell { age: 0.0, life, size, col }, NoFrustumCulling));
}

fn ring(commands: &mut Commands, look: &Look, mats: &mut Assets<StandardMaterial>, p: Vec3, size: f32, life: f32, col: [f32; 3]) {
    let m = add_mat(mats, [col[0], col[1], col[2], 1.0], AlphaMode::Add);
    commands.spawn((Mesh3d(look.ring.clone()), MeshMaterial3d(m), Transform::from_translation(p).with_scale(Vec3::splat(0.05)), Ring { age: 0.0, life, size, col }, NoFrustumCulling));
}

#[allow(clippy::too_many_arguments)]
fn puff(commands: &mut Commands, look: &Look, mats: &mut Assets<StandardMaterial>, p: Vec3, from: f32, to: f32, life: f32, col: [f32; 3], alpha: f32, rise: Vec3) {
    let m = add_mat(mats, [col[0], col[1], col[2], alpha], AlphaMode::Blend);
    commands.spawn((Mesh3d(look.sphere.clone()), MeshMaterial3d(m), Transform::from_translation(p).with_scale(Vec3::splat(from)), Puff { age: 0.0, life, from, to, rise, col, alpha }, NoFrustumCulling));
}

fn light(commands: &mut Commands, p: Vec3, col: Color, peak: f32, range: f32, life: f32) {
    commands.spawn((PointLight { color: col, intensity: 0.0, range, shadow_maps_enabled: false, ..default() }, Transform::from_translation(p), BlastLight { age: 0.0, life, peak }));
}

#[allow(clippy::too_many_arguments)]
fn explode(commands: &mut Commands, look: &Look, mats: &mut Assets<StandardMaterial>, impacts: &mut ImpactQueue, rng: &mut Rng, style: Style, p: Vec3, radius: f32) {
    match style {
        Style::Barrel => {
            // layered fireball: white-hot core, orange body, dull red skirt
            shell(commands, look, mats, p, radius * 0.5, 0.4, [5.0, 3.4, 1.4]);
            shell(commands, look, mats, p + Vec3::Y * 0.3, radius * 0.62, 0.65, [3.0, 1.0, 0.18]);
            shell(commands, look, mats, p + Vec3::Y * 0.5, radius * 0.8, 0.9, [2.4, 0.4, 0.06]);
            ring(commands, look, mats, p - Vec3::Y * 0.2, radius * 1.05, 0.5, [5.0, 2.4, 0.6]);
            ring(commands, look, mats, p, radius * 0.7, 0.38, [7.0, 5.0, 3.0]);
            light(commands, p + Vec3::Y * 0.5, Color::linear_rgb(1.0, 0.55, 0.2), 4.0e6, radius * 3.5, 0.5);
            for _ in 0..34 {
                let (vel, size, life) = (rng.dir() * rng.range(7.0, 20.0) + Vec3::Y * 3.0, rng.range(0.03, 0.07), rng.range(0.5, 1.1));
                spawn_bit(commands, &look.sphere, if rng.f() < 0.4 { look.ember.clone() } else { look.spark.clone() }, p, vel, 9.0, 0.5, life, size, Vec3::ZERO);
            }
            for i in 0..9 {
                let (vel, size, life) = (rng.dir() * rng.range(6.0, 13.0) + Vec3::Y * 5.0, rng.range(0.08, 0.2), rng.range(1.0, 1.6));
                let spin = rng.dir() * rng.range(6.0, 16.0);
                spawn_bit(commands, &look.cube, if i % 3 == 0 { look.hazard.clone() } else { look.steel.clone() }, p, vel, 20.0, 0.1, life, size, spin);
            }
            for _ in 0..5 {
                let off = rng.dir() * radius * 0.25;
                puff(commands, look, mats, p + off, radius * 0.25, radius * rng.range(0.55, 0.8), rng.range(1.6, 2.4), [0.16, 0.15, 0.15], 0.5, Vec3::new(0.0, rng.range(1.4, 2.6), 0.0) + rng.dir() * 0.6);
            }
            impacts.0.push(Impact { pos: p, color: [1.0, 0.45, 0.08], kind: ImpactKind::Hit { kill: true } });
        }
        Style::Canister => {
            // smaller, shorter, sharper, with a yellow-green gas puff left behind
            shell(commands, look, mats, p, radius * 0.45, 0.22, [9.0, 8.5, 4.0]);
            shell(commands, look, mats, p, radius * 0.6, 0.4, [3.4, 3.8, 0.4]);
            ring(commands, look, mats, p, radius * 1.1, 0.28, [5.0, 6.0, 1.2]);
            light(commands, p, Color::linear_rgb(0.85, 1.0, 0.4), 1.6e6, radius * 3.0, 0.3);
            for _ in 0..18 {
                let (vel, size, life) = (rng.dir() * rng.range(9.0, 22.0), rng.range(0.025, 0.05), rng.range(0.3, 0.7));
                spawn_bit(commands, &look.sphere, if rng.f() < 0.5 { look.gas.clone() } else { look.spark.clone() }, p, vel, 8.0, 0.8, life, size, Vec3::ZERO);
            }
            for _ in 0..3 {
                let spin = rng.dir() * rng.range(8.0, 18.0);
                let (vel, size, life) = (rng.dir() * rng.range(7.0, 14.0) + Vec3::Y * 3.0, rng.range(0.05, 0.1), rng.range(0.7, 1.1));
                spawn_bit(commands, &look.cube, look.steel.clone(), p, vel, 20.0, 0.1, life, size, spin);
            }
            for _ in 0..4 {
                let off = rng.dir() * radius * 0.3;
                puff(commands, look, mats, p + off, radius * 0.2, radius * rng.range(0.55, 0.75), rng.range(1.0, 1.5), [0.62, 0.8, 0.12], 0.32, Vec3::new(0.0, rng.range(0.4, 1.0), 0.0) + rng.dir() * 0.4);
            }
            impacts.0.push(Impact { pos: p, color: [0.75, 0.95, 0.15], kind: ImpactKind::Hit { kill: false } });
        }
        Style::Flash => {
            shell(commands, look, mats, p, radius * 0.45, 0.4, [10.0, 10.0, 10.0]);
            ring(commands, look, mats, p, radius, 0.35, [8.0, 8.0, 8.0]);
            light(commands, p, Color::WHITE, 6.0e6, radius * 2.0, 0.4);
            for _ in 0..22 {
                let (vel, size, life) = (rng.dir() * rng.range(8.0, 20.0), rng.range(0.025, 0.05), rng.range(0.25, 0.6));
                spawn_bit(commands, &look.sphere, look.spark.clone(), p, vel, 3.0, 0.6, life, size, Vec3::ZERO);
            }
            impacts.0.push(Impact { pos: p, color: [1.0, 1.0, 1.0], kind: ImpactKind::Hit { kill: false } });
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
        t.scale = Vec3::splat(s.size * fireball_scale(u).max(0.02));
        if let Some(mut mat) = mats.get_mut(&m.0) {
            let a = fireball_alpha(u);
            mat.base_color = Color::linear_rgb(s.col[0] * a, s.col[1] * a, s.col[2] * a);
        }
    }
}

fn animate_rings(mut commands: Commands, time: Res<Time>, mut mats: ResMut<Assets<StandardMaterial>>, mut q: Query<(Entity, &mut Transform, &mut Ring, &MeshMaterial3d<StandardMaterial>)>) {
    let dt = time.delta_secs();
    for (e, mut t, mut r, m) in &mut q {
        r.age += dt;
        if r.age >= r.life {
            mats.remove(&m.0);
            commands.entity(e).despawn();
            continue;
        }
        let u = r.age / r.life;
        let out = 1.0 - (1.0 - u) * (1.0 - u);
        t.scale = Vec3::new(r.size * out.max(0.02), 1.0, r.size * out.max(0.02));
        if let Some(mut mat) = mats.get_mut(&m.0) {
            let a = (1.0 - u).powi(2);
            mat.base_color = Color::linear_rgb(r.col[0] * a, r.col[1] * a, r.col[2] * a);
        }
    }
}

fn animate_bits(mut commands: Commands, time: Res<Time>, mut q: Query<(Entity, &mut Transform, &mut Bit)>) {
    let dt = time.delta_secs();
    for (e, mut t, mut b) in &mut q {
        b.age += dt;
        if b.age >= b.life {
            commands.entity(e).despawn();
            continue;
        }
        b.vel.y -= b.grav * dt;
        let drag = (1.0 - b.drag * dt).max(0.0);
        b.vel *= drag;
        t.translation += b.vel * dt;
        t.scale = Vec3::splat(b.size * (1.0 - b.age / b.life).powf(0.7));
        if b.spin != Vec3::ZERO {
            let ax = b.spin.normalize_or(Vec3::Y);
            t.rotate(Quat::from_axis_angle(ax, b.spin.length() * dt));
        }
    }
}

fn animate_puffs(mut commands: Commands, time: Res<Time>, mut mats: ResMut<Assets<StandardMaterial>>, mut q: Query<(Entity, &mut Transform, &mut Puff, &MeshMaterial3d<StandardMaterial>)>) {
    let dt = time.delta_secs();
    for (e, mut t, mut p, m) in &mut q {
        p.age += dt;
        if p.age >= p.life {
            mats.remove(&m.0);
            commands.entity(e).despawn();
            continue;
        }
        let u = p.age / p.life;
        t.translation += p.rise * dt * (1.0 - u);
        t.scale = Vec3::splat(p.from + (p.to - p.from) * (1.0 - (1.0 - u).powi(2)));
        if let Some(mut mat) = mats.get_mut(&m.0) {
            // fades in over the first tenth, then thins out
            let a = p.alpha * (u / 0.1).min(1.0) * (1.0 - u);
            mat.base_color = Color::linear_rgba(p.col[0], p.col[1], p.col[2], a);
        }
    }
}

fn animate_lights(mut commands: Commands, time: Res<Time>, mut q: Query<(Entity, &mut PointLight, &mut BlastLight)>) {
    let dt = time.delta_secs();
    for (e, mut l, mut b) in &mut q {
        b.age += dt;
        if b.age >= b.life {
            commands.entity(e).despawn();
            continue;
        }
        let u = b.age / b.life;
        l.intensity = b.peak * (1.0 - u).powi(2);
    }
}

// ---------------------------------------------------------------- screen shake / flash

/// Blasts near the camera: shake and a brief white wash (through `RenderView::blind`, which the HUD draws), both scaled
/// by `blast_proximity`. Runs right after the view is published.
fn blast_react(time: Res<Time>, mut view: ResMut<ViewRes>, mut b: ResMut<Blast>, settings: Option<Res<crate::hud_view::SettingsRes>>) {
    let dt = time.delta_secs();
    let sc = StarCfg::default();
    let eye = Vec3::from(view.0.eye);
    let (k, still) = settings.as_ref().map_or((1.0, false), |s| (s.0.fx_intensity.clamp(0.0, 1.0), s.0.reduced_motion));
    for e in &view.0.events {
        let NetEvent::StarFx { what, pos, .. } = e else { continue };
        let radius = match *what {
            STAR_FX_BARREL_BURST => sc.barrel_radius,
            STAR_FX_CANISTER_BURST => sc.canister_radius,
            _ => continue,
        };
        let near = blast_proximity(eye.distance(Vec3::from(shinobi_proto::quant::dq_pos3(*pos))), radius);
        let size = radius / sc.barrel_radius;
        if near > 0.0 {
            b.shake = b.shake.max(0.22 * near * size.max(0.5));
            b.age = 0.0;
            b.flash = b.flash.max(0.6 * near * size.max(0.6));
        }
    }
    b.age += dt;
    b.shake *= (-dt * 5.0).exp();
    b.flash = (b.flash - dt * 3.0).max(0.0);
    if b.flash > 0.01 {
        let f = b.flash * k * if still { 0.5 } else { 1.0 };
        view.0.blind = (view.0.blind + f).min(1.0);
    }
}

fn shake(b: Res<Blast>, settings: Option<Res<crate::hud_view::SettingsRes>>, mut cam: Single<&mut Transform, With<PlayerCam>>) {
    if b.shake < 0.002 {
        return;
    }
    let Some(s) = settings.as_ref().map(|s| &s.0) else { return };
    if s.reduced_motion {
        return;
    }
    let k = s.fx_intensity.clamp(0.0, 1.0);
    let (r, u) = ((b.age * 83.0).sin() * b.shake, (b.age * 67.0 + 1.3).sin() * b.shake * 0.7);
    let (right, up) = (cam.right(), cam.up());
    cam.translation += (right * r + up * u) * k;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuse_blinks_faster_as_it_runs_down() {
        let mut last = 0.0;
        for i in 0..=20 {
            let left = 1.0 - i as f32 / 20.0;
            let hz = fuse_blink_hz(left);
            assert!(hz >= last, "monotonic: {hz} after {last}");
            last = hz;
        }
        assert!((fuse_blink_hz(1.0) - 2.5).abs() < 1e-5, "a fresh fuse is a slow heartbeat");
        assert!(fuse_blink_hz(0.0) > 15.0, "the last moments flicker");
        assert_eq!(fuse_blink_hz(5.0), fuse_blink_hz(1.0));
        assert_eq!(fuse_blink_hz(-1.0), fuse_blink_hz(0.0));
    }

    #[test]
    fn blink_is_a_bounded_pulse_that_goes_dark() {
        let vals: Vec<f32> = (0..100).map(|i| blink_level(i as f32 / 100.0)).collect();
        assert!(vals.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!(vals.iter().cloned().fold(0.0, f32::max) > 0.99 && vals.iter().cloned().fold(1.0, f32::min) < 0.01);
        assert!((blink_level(0.25) - blink_level(1.25)).abs() < 1e-4, "periodic");
    }

    #[test]
    fn spark_rate_grows_toward_the_blast() {
        assert!(fuse_spark_rate(0.0) > 2.0 * fuse_spark_rate(1.0));
    }

    #[test]
    fn fireball_pops_fast_then_swells_slowly_and_fades_out() {
        assert!(fireball_scale(0.0) < 0.01);
        assert!(fireball_scale(0.15) > 0.8, "most of the size in the first third");
        let mut last = 0.0;
        for i in 0..=100 {
            let s = fireball_scale(i as f32 / 100.0);
            assert!(s >= last - 1e-6, "never shrinks: {s} after {last}");
            last = s;
        }
        assert!(fireball_scale(1.0) < 1.25, "bounded");
        assert!((fireball_alpha(0.0) - 1.0).abs() < 1e-6 && fireball_alpha(1.0) == 0.0);
        assert!(fireball_alpha(0.3) > fireball_alpha(0.6));
    }

    #[test]
    fn blast_is_felt_inside_and_fades_to_nothing_at_three_radii() {
        assert_eq!(blast_proximity(0.0, 6.0), 1.0);
        assert!(blast_proximity(6.0, 6.0) > 0.4 && blast_proximity(6.0, 6.0) < 0.5);
        assert_eq!(blast_proximity(18.0, 6.0), 0.0);
        assert_eq!(blast_proximity(100.0, 6.0), 0.0);
        assert!(blast_proximity(3.0, 6.0) > blast_proximity(9.0, 6.0));
        assert_eq!(blast_proximity(1.0, 0.0), 0.0);
    }

    fn bounds(m: &Mesh) -> (Vec3, Vec3) {
        let pos = m.attribute(Mesh::ATTRIBUTE_POSITION).unwrap().as_float3().unwrap();
        pos.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(Vec3::from(*p)), hi.max(Vec3::from(*p))))
    }

    #[test]
    fn barrel_is_a_drum_with_a_fuse_nub_and_the_band_sits_on_it() {
        let (lo, hi) = bounds(&barrel_mesh());
        assert!((lo.y + BARREL_HH).abs() < 1e-4, "bottom at -half height: {}", lo.y);
        assert!(hi.y > BARREL_HH + 0.05 && hi.y < BARREL_HH + 0.15, "fuse nub on top: {}", hi.y);
        assert!(hi.x > BARREL_R && hi.x < BARREL_R * 1.1 && lo.x < -BARREL_R, "ribs a bit wider than the stripes");
        let (blo, bhi) = bounds(&barrel_band_mesh());
        assert!(bhi.y < 0.06 && blo.y > -0.06 && bhi.x > BARREL_R, "band is a thin ring around the middle");
    }

    #[test]
    fn canister_is_small_and_finite() {
        let m = canister_mesh();
        let (lo, hi) = bounds(&m);
        assert!(hi.y - lo.y < 0.5 && hi.x < 0.12 && lo.x > -0.12, "{lo:?} {hi:?}");
        let pos = m.attribute(Mesh::ATTRIBUTE_POSITION).unwrap().as_float3().unwrap();
        assert!(pos.iter().all(|p| p.iter().all(|c| c.is_finite())));
    }

    #[test]
    fn ring_is_flat_with_unit_outer_radius() {
        let (lo, hi) = bounds(&ring_mesh());
        assert!(lo.y == 0.0 && hi.y == 0.0 && (hi.x - 1.0).abs() < 1e-4);
    }
}
