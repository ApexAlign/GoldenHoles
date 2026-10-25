//! Environmental events drawn (sim side: `shinobi_sim::env_events`; the client queues `EnvFx`):
//!
//! * LIGHTNING: a red-white pulsing ground ring + faint light column where a bolt WILL land, brighter and faster as the
//!   warning runs out (the ring is removed on the strike); the strike is a jagged sky-to-ground bolt, a full-screen flash
//!   scaled by proximity, a point light, an expanding shock ring and a spark / debris burst. An edge arrow points at the
//!   nearest marked bolt when it is off-screen.
//!
//! This module is the only consumer of `take_env_fx` and republishes the events as `StormFeed` for `storm_audio` (order
//! after `StormSet`). Everything is cosmetic.
//!
//! Review aid: `SHOT_STORM=bolt` (optionally `SHOT_STORM_AT=seconds`, default 1.0) fakes the event sequence locally.

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::camera::{ClearColorConfig, Hdr, RenderTarget, ScalingMode};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use shinobi_client::hud::{Align, Prim, Rgba};
use shinobi_client::hud_geom::tessellate;
use shinobi_client::netclient::EnvFx;

use crate::net::{Net, ViewRes};
use crate::play::{PlayerCam, ShotImage, ShotRequest};

const LAYER: usize = 5;
const TAU: f32 = std::f32::consts::TAU;
/// Danger radius drawn for a marked bolt (the client does not know the server's `bolt_radius`).
pub const BOLT_RADIUS: f32 = 8.0;
const BOLT_TOP: f32 = 120.0;
const BOLT_SEGS: usize = 9;
const BOLT_LIFE: f32 = 0.15;
/// Live transient entities allowed at once.
const MAX_FX: usize = 360;
/// A marked bolt more than this far off the view axis gets an edge arrow.
const ARROW_MIN_ANGLE: f32 = 0.9;

// ----------------------------------------------------------------------------------------------- pure logic

/// Ground ring of a marked bolt with `left` of `warn` seconds to go: `(brightness, pulse Hz)`. Both rise as it nears.
pub fn ring_look(left: f32, warn: f32) -> (f32, f32) {
    let u = (1.0 - left / warn.max(0.05)).clamp(0.0, 1.0);
    (0.6 + 2.2 * u * u, 1.5 + 8.0 * u)
}

/// Seconds between whine retriggers, pitch and volume of a marked bolt at progress `u` (0 just marked .. 1 strike).
pub fn whine_look(u: f32) -> (f32, f32, f32) {
    let u = u.clamp(0.0, 1.0);
    (0.6 - 0.42 * u, 0.8 + 0.8 * u, 0.25 + 0.75 * u)
}

fn hash01(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B1) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 65535.0
}

/// Jagged polyline from `top` to `bottom` in `n` segments (n + 1 points); the ends stay put, the middle wanders sideways
/// by up to ~45 % of a segment length. Deterministic in `seed`.
pub fn bolt_points(seed: u32, top: [f32; 3], bottom: [f32; 3], n: usize) -> Vec<[f32; 3]> {
    let n = n.max(1);
    let (a, b) = (Vec3::from(top), Vec3::from(bottom));
    let seg = (b - a).length() / n as f32;
    (0..=n)
        .map(|i| {
            let u = i as f32 / n as f32;
            let base = a.lerp(b, u);
            if i == 0 || i == n {
                return base.to_array();
            }
            let h = |k: u32| hash01(seed.wrapping_mul(0x2545_F491).wrapping_add(i as u32 * 31 + k)) * 2.0 - 1.0;
            let env = (u * std::f32::consts::PI).sin().max(0.3);
            (base + Vec3::new(h(1), 0.0, h(2)) * seg * 0.45 * env).to_array()
        })
        .collect()
}

/// White screen flash for a strike `dist` metres away, `age` seconds after it: stronger when near, never fully absent
/// (the sky lights up), gone after 0.4 s.
pub fn flash_alpha(dist: f32, age: f32) -> f32 {
    if !(0.0..0.4).contains(&age) {
        return 0.0;
    }
    let near = (1.0 - dist / 120.0).clamp(0.0, 1.0);
    let peak = 0.12 + 0.78 * near * near.sqrt();
    let t = if age < 0.04 { 1.0 } else { (1.0 - (age - 0.04) / 0.36).powi(2) };
    peak * t
}

/// Signed screen angle (0 ahead, +PI/2 right) of a horizontal offset seen with aim `yaw` (yaw 0 faces -Z).
pub fn rel_angle(dx: f32, dz: f32, yaw: f32) -> f32 {
    let (s, c) = yaw.sin_cos();
    let fwd = -s * dx - c * dz;
    let right = c * dx - s * dz;
    right.atan2(fwd)
}

/// A marked bolt as the HUD needs it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkInfo {
    pub left: f32,
    pub warn: f32,
    /// Horizontal offset from the eye.
    pub d: [f32; 2],
}

/// Index of the nearest mark by horizontal distance.
pub fn nearest_mark(marks: &[MarkInfo]) -> Option<usize> {
    (0..marks.len()).min_by(|&a, &b| {
        let da = marks[a].d[0].hypot(marks[a].d[1]);
        let db = marks[b].d[0].hypot(marks[b].d[1]);
        da.total_cmp(&db)
    })
}

// ----------------------------------------------------------------------------------------------- overlay geometry

#[derive(Default)]
struct Geo {
    pos: Vec<[f32; 3]>,
    col: Vec<[f32; 4]>,
    idx: Vec<u32>,
    aspect: f32,
}

impl Geo {
    fn clear(&mut self) {
        self.pos.clear();
        self.col.clear();
        self.idx.clear();
    }
    fn pt(&self, dx: f32, dy: f32) -> [f32; 2] {
        [0.5 + dx / self.aspect, 0.5 + dy]
    }
    fn quad(&mut self, p: [[f32; 2]; 4], c: [[f32; 4]; 4]) {
        let b = self.pos.len() as u32;
        for i in 0..4 {
            self.pos.push([p[i][0], 1.0 - p[i][1], 0.0]);
            self.col.push(c[i]);
        }
        self.idx.extend_from_slice(&[b, b + 1, b + 2, b, b + 2, b + 3]);
    }
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        self.quad([[x, y], [x + w, y], [x + w, y + h], [x, y + h]], [c; 4]);
    }
    /// Edge glow on all four sides: alpha `a` at the edge fading to 0 at `depth` (screen fractions).
    fn edge_glow(&mut self, rgb: [f32; 3], a: f32, depth: f32) {
        let (e, z) = ([rgb[0], rgb[1], rgb[2], a], [rgb[0], rgb[1], rgb[2], 0.0]);
        let dx = depth / self.aspect.max(0.1) * 1.2;
        self.quad([[0.0, 0.0], [1.0, 0.0], [1.0, depth], [0.0, depth]], [e, e, z, z]);
        self.quad([[0.0, 1.0], [1.0, 1.0], [1.0, 1.0 - depth], [0.0, 1.0 - depth]], [e, e, z, z]);
        self.quad([[0.0, 0.0], [dx, 0.0], [dx, 1.0], [0.0, 1.0]], [e, z, z, e]);
        self.quad([[1.0, 0.0], [1.0 - dx, 0.0], [1.0 - dx, 1.0], [1.0, 1.0]], [e, z, z, e]);
    }
    /// Triangle at `angle` (0 up, clockwise) on a circle of radius `r` (height units), pointing outwards.
    fn arrow(&mut self, angle: f32, r: f32, size: f32, rgb: [f32; 3], alpha: f32) {
        let (s, c) = angle.sin_cos();
        let at = |rr: f32, side: f32| {
            let (px, py) = (rr * s + side * c, -rr * c + side * s);
            self.pt(px, py)
        };
        let tip = at(r + size, 0.0);
        let l = at(r - size * 0.2, -size * 0.7);
        let rr = at(r - size * 0.2, size * 0.7);
        let notch = at(r + size * 0.15, 0.0);
        let col = [rgb[0], rgb[1], rgb[2], alpha];
        self.quad([tip, rr, notch, l], [col; 4]);
    }
    fn append(&mut self, g: shinobi_client::hud_geom::Geom) {
        let b = self.pos.len() as u32;
        for p in &g.pos {
            self.pos.push([p[0], 1.0 - p[1], 0.0]);
        }
        self.col.extend_from_slice(&g.col);
        self.idx.extend(g.idx.iter().map(|i| i + b));
    }
}

/// What the overlay shows this frame.
#[derive(Default)]
struct HudView {
    marks: Vec<MarkInfo>,
    /// Yaw of the view, for the arrow.
    yaw: f32,
    flash: f32,
    t: f32,
}

fn build(g: &mut Geo, v: &HudView) {
    g.clear();
    let banner = |g: &mut Geo, text: &str, y: f32, size: f32, rgb: (f32, f32, f32), a: f32| {
        let split = 0.004;
        let at = |dx: f32, c: Rgba| Prim::Text { pos: (0.5 + dx, y), text: text.to_string(), size, color: c, align: Align::Center };
        let prims = [at(-split, Rgba(0.0, 0.9, 1.0, a * 0.6)), at(split, Rgba(1.0, 0.0, 0.2, a * 0.6)), at(0.0, Rgba(rgb.0, rgb.1, rgb.2, a))];
        let t = tessellate(&prims, g.aspect);
        g.append(t);
    };
    // LIGHTNING banner + arrow to the nearest mark
    if let Some(i) = nearest_mark(&v.marks) {
        let m = v.marks[i];
        let dist = m.d[0].hypot(m.d[1]);
        let urgent = (1.0 - m.left / m.warn.max(0.05)).clamp(0.0, 1.0);
        let blink = 0.6 + 0.4 * (v.t * TAU * (1.5 + 6.0 * urgent)).sin();
        let inside = dist < BOLT_RADIUS + 1.0;
        let y = 0.15;
        if inside {
            g.edge_glow([1.0, 0.1, 0.1], (0.18 + 0.4 * urgent) * blink, 0.12);
            banner(g, "MOVE", y, 0.07, (1.0, 0.3, 0.25), blink);
        } else {
            banner(g, "LIGHTNING", y, 0.04, (1.0, 0.45, 0.4), 0.9 * blink.max(0.5));
        }
        banner(g, &format!("{:.1}", m.left.max(0.0)), y + 0.05, 0.03, (1.0, 0.9, 0.9), 0.9);
        let a = rel_angle(m.d[0], m.d[1], v.yaw);
        if a.abs() > ARROW_MIN_ANGLE && !inside {
            g.arrow(a, 0.36, 0.045 + 0.02 * urgent, [1.0, 0.25 + 0.5 * blink, 0.25 + 0.5 * blink], 0.55 + 0.45 * blink);
        }
    }
    // strike flash last, on top of everything
    if v.flash > 0.003 {
        g.rect(0.0, 0.0, 1.0, 1.0, [0.92, 0.96, 1.0, v.flash.min(0.95)]);
    }
}

// ----------------------------------------------------------------------------------------------- bevy side

/// Events drained this frame, republished for `storm_audio`. Order audio systems `.after(StormSet)`.
#[derive(Resource, Default)]
pub struct StormFeed {
    pub events: Vec<EnvFx>,
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct StormSet;

struct Mark {
    id: u8,
    pos: Vec3,
    left: f32,
    warn: f32,
    phase: f32,
    ents: [Entity; 3],
    mats: [Handle<StandardMaterial>; 3],
}

#[derive(Resource)]
struct State {
    t: f32,
    marks: Vec<Mark>,
    /// Strike flash: distance and age of the latest one.
    flash: Option<(f32, f32)>,
    ring: Option<Handle<Mesh>>,
    disc: Option<Handle<Mesh>>,
    cyl: Option<Handle<Mesh>>,
    cube: Option<Handle<Mesh>>,
    bolt_mat: Option<Handle<StandardMaterial>>,
    spark_mat: Option<Handle<StandardMaterial>>,
    chunk_mat: Option<Handle<StandardMaterial>>,
    /// Review aid script: (time, event).
    script: Vec<(f32, EnvFx)>,
    script_kind: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            t: 0.0,
            marks: Vec::new(),
            flash: None,
            ring: None,
            disc: None,
            cyl: None,
            cube: None,
            bolt_mat: None,
            spark_mat: None,
            chunk_mat: None,
            script: Vec::new(),
            script_kind: std::env::var("SHOT_STORM").ok(),
        }
    }
}

#[derive(Component)]
struct Fx {
    age: f32,
    life: f32,
    kind: Kind,
}

enum Kind {
    Seg { w: f32, len: f32 },
    Shock { mat: Handle<StandardMaterial>, r1: f32 },
    Light { peak: f32 },
    /// Free-flying bit (sparks, debris, dust): `base` size shrinks over its life.
    Bit { vel: Vec3, spin: Vec3, grav: f32, floor: f32, base: Vec3 },
}

#[derive(Resource)]
struct HudMesh(Handle<Mesh>);

#[derive(Resource)]
struct GeoRes(Geo, bool);

pub struct StormFxPlugin;

impl Plugin for StormFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<State>()
            .init_resource::<StormFeed>()
            .insert_resource(GeoRes(Geo { aspect: 16.0 / 9.0, ..default() }, false))
            .add_systems(Startup, setup)
            .add_systems(Update, (gather.in_set(StormSet), handle, drive_marks, animate, overlay).chain());
    }
}

fn blank() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, Vec::<[f32; 3]>::new())
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, Vec::<[f32; 4]>::new())
        .with_inserted_indices(Indices::U32(Vec::new()))
}

fn emissive(mats: &mut Assets<StandardMaterial>, rgb: [f32; 3]) -> Handle<StandardMaterial> {
    mats.add(StandardMaterial {
        base_color: Color::linear_rgb(rgb[0], rgb[1], rgb[2]),
        unlit: true,
        alpha_mode: AlphaMode::Add,
        double_sided: true,
        cull_mode: None,
        ..default()
    })
}

fn setup(
    mut st: ResMut<State>,
    mut commands: Commands,
    shot: Option<Res<ShotImage>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    st.ring = Some(meshes.add(Annulus::new(0.93, 1.0)));
    st.disc = Some(meshes.add(Circle::new(1.0)));
    st.cyl = Some(meshes.add(Cylinder::new(1.0, 1.0)));
    st.cube = Some(meshes.add(Cuboid::new(1.0, 1.0, 1.0)));
    st.bolt_mat = Some(emissive(&mut mats, [5.0, 6.0, 9.0]));
    st.spark_mat = Some(emissive(&mut mats, [8.0, 7.0, 4.0]));
    st.chunk_mat = Some(emissive(&mut mats, [1.4, 0.7, 0.35]));
    // overlay camera (same recipe as hit_indicator, one layer above)
    let layer = RenderLayers::layer(LAYER);
    let mut cam = commands.spawn((
        Camera3d::default(),
        Camera { order: 4, clear_color: ClearColorConfig::None, ..default() },
        Hdr,
        Tonemapping::None,
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::Fixed { width: 1.0, height: 1.0 },
            near: -10.0,
            far: 10.0,
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(0.5, 0.5, 5.0),
        layer.clone(),
    ));
    if let Some(img) = shot {
        cam.insert(RenderTarget::Image(img.0.clone().into()));
    }
    let mesh = meshes.add(blank());
    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(mats.add(StandardMaterial { base_color: Color::WHITE, unlit: true, alpha_mode: AlphaMode::Blend, double_sided: true, cull_mode: None, ..default() })),
        Transform::default(),
        layer,
        NoFrustumCulling,
    ));
    commands.insert_resource(HudMesh(mesh));
}

/// Drain the client's queue (this module owns it), add the review script, republish.
fn gather(time: Res<Time>, view: Res<ViewRes>, mut net: ResMut<Net>, mut st: ResMut<State>, mut feed: ResMut<StormFeed>) {
    let dt = time.delta_secs();
    st.t += dt;
    feed.events.clear();
    feed.events.extend(net.0.game.net.take_env_fx());
    // review aid
    if let Some(kind) = st.script_kind.take() {
        let at = std::env::var("SHOT_STORM_AT").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0f32);
        let v = &view.0;
        let (yaw, _) = v.look_angles();
        let eye = Vec3::from(v.eye);
        let ahead = eye + Vec3::new(-yaw.sin(), 0.0, -yaw.cos()) * 12.0;
        let ground = [ahead.x, (v.eye[1] - 1.6).max(0.0), ahead.z];
        match kind.as_str() {
            "bolt" => st.script = vec![(at, EnvFx::BoltMark { id: 1, pos: ground, warn_s: 2.0 }), (at + 2.0, EnvFx::BoltStrike { id: 1, pos: ground })],
            _ => {}
        }
    }
    if !st.script.is_empty() {
        let now = st.t;
        let mut i = 0;
        while i < st.script.len() {
            if st.script[i].0 <= now {
                let (_, e) = st.script.remove(i);
                feed.events.push(e);
            } else {
                i += 1;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle(
    mut st: ResMut<State>,
    feed: Res<StormFeed>,
    view: Res<ViewRes>,
    mut commands: Commands,
    mut mats: ResMut<Assets<StandardMaterial>>,
    live: Query<(), With<Fx>>,
) {
    let eye = Vec3::from(view.0.eye);
    let mut budget = MAX_FX.saturating_sub(live.iter().count());
    for ev in feed.events.iter().copied() {
        match ev {
            EnvFx::BoltMark { id, pos, warn_s } => {
                st.marks.retain(|m| {
                    if m.id == id {
                        for e in m.ents {
                            commands.entity(e).despawn();
                        }
                    }
                    m.id != id
                });
                let p = Vec3::from(pos);
                let (ring, disc, cyl) = (st.ring.clone().unwrap(), st.disc.clone().unwrap(), st.cyl.clone().unwrap());
                let m = [emissive(&mut mats, [1.0, 0.3, 0.3]), emissive(&mut mats, [0.2, 0.05, 0.05]), emissive(&mut mats, [0.1, 0.02, 0.02])];
                let flat = Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2);
                let r = BOLT_RADIUS;
                let e_ring = commands.spawn((Mesh3d(ring), MeshMaterial3d(m[0].clone()), Transform::from_translation(p + Vec3::Y * 0.08).with_rotation(flat).with_scale(Vec3::new(r, r, 1.0)))).id();
                let e_disc = commands.spawn((Mesh3d(disc), MeshMaterial3d(m[1].clone()), Transform::from_translation(p + Vec3::Y * 0.06).with_rotation(flat).with_scale(Vec3::new(r, r, 1.0)))).id();
                let e_col = commands.spawn((Mesh3d(cyl), MeshMaterial3d(m[2].clone()), Transform::from_translation(p + Vec3::Y * 15.0).with_scale(Vec3::new(r * 0.5, 30.0, r * 0.5)))).id();
                st.marks.push(Mark { id, pos: p, left: warn_s.max(0.1), warn: warn_s.max(0.1), phase: 0.0, ents: [e_ring, e_disc, e_col], mats: m });
            }
            EnvFx::BoltStrike { id, pos } => {
                if let Some(i) = st.marks.iter().position(|m| m.id == id) {
                    let m = st.marks.remove(i);
                    for e in m.ents {
                        commands.entity(e).despawn();
                    }
                }
                let p = Vec3::from(pos);
                let dist = (p - eye).length();
                st.flash = Some((dist, 0.0));
                strike(&mut st, &mut commands, &mut mats, p, id, &mut budget);
            }
        }
    }
}

fn strike(st: &mut State, commands: &mut Commands, mats: &mut Assets<StandardMaterial>, p: Vec3, id: u8, budget: &mut usize) {
    let bolt = st.bolt_mat.clone().unwrap();
    let cube = st.cube.clone().unwrap();
    // the bolt: a main trunk plus two short forks off its upper half
    let seed = (p.x.to_bits() ^ p.z.to_bits().rotate_left(13)).wrapping_add(id as u32 * 977);
    let top = [p.x + (hash01(seed) - 0.5) * 18.0, p.y + BOLT_TOP, p.z + (hash01(seed ^ 5) - 0.5) * 18.0];
    let pts = bolt_points(seed, top, p.to_array(), BOLT_SEGS);
    let mut lines: Vec<(Vec3, Vec3, f32)> = pts.windows(2).map(|w| (Vec3::from(w[0]), Vec3::from(w[1]), 0.5)).collect();
    for (k, from) in [(2usize, 1u32), (4, 2)] {
        let a = Vec3::from(pts[k]);
        let dir = Vec3::new(hash01(seed ^ (from * 77)) - 0.5, -0.6, hash01(seed ^ (from * 91)) - 0.5).normalize();
        let end = a + dir * 22.0;
        for (i, w) in bolt_points(seed ^ (from * 13), a.to_array(), end.to_array(), 3).windows(2).enumerate() {
            lines.push((Vec3::from(w[0]), Vec3::from(w[1]), 0.22 - 0.04 * i as f32));
        }
    }
    for (a, b, w) in lines {
        let d = b - a;
        let l = d.length().max(1e-3);
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        commands.spawn((
            Mesh3d(cube.clone()),
            MeshMaterial3d(bolt.clone()),
            Transform { translation: (a + b) * 0.5, rotation: Quat::from_rotation_arc(Vec3::Z, d / l), scale: Vec3::new(w, w, l) },
            Fx { age: 0.0, life: BOLT_LIFE, kind: Kind::Seg { w, len: l } },
        ));
    }
    // ground shock ring
    let ring = st.ring.clone().unwrap();
    let shock = emissive(mats, [4.0, 5.0, 8.0]);
    commands.spawn((
        Mesh3d(ring),
        MeshMaterial3d(shock.clone()),
        Transform::from_translation(p + Vec3::Y * 0.1).with_rotation(Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)).with_scale(Vec3::new(1.0, 1.0, 1.0)),
        Fx { age: 0.0, life: 0.45, kind: Kind::Shock { mat: shock, r1: BOLT_RADIUS * 1.5 } },
    ));
    // light flash
    commands.spawn((
        PointLight { color: Color::srgb(0.75, 0.88, 1.0), intensity: 0.0, range: 90.0, shadow_maps_enabled: false, ..default() },
        Transform::from_translation(p + Vec3::Y * 4.0),
        Fx { age: 0.0, life: 0.35, kind: Kind::Light { peak: 6.0e7 } },
    ));
    // sparks and debris
    let (spark, chunk) = (st.spark_mat.clone().unwrap(), st.chunk_mat.clone().unwrap());
    for i in 0..44u32 {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let r = |n: u32| hash01(seed.wrapping_add(i * 131 + n * 17 + 3));
        let out = Vec3::new(r(1) - 0.5, 0.0, r(2) - 0.5).normalize_or_zero();
        let is_spark = i < 30;
        let (size, vel, life, mat) = if is_spark {
            (Vec3::splat(0.07 + 0.07 * r(3)), (out * (4.0 + 9.0 * r(4)) + Vec3::Y * (3.0 + 8.0 * r(5))), 0.5 + 0.5 * r(6), spark.clone())
        } else {
            (Vec3::splat(0.15 + 0.3 * r(3)), (out * (2.0 + 5.0 * r(4)) + Vec3::Y * (4.0 + 6.0 * r(5))), 1.0 + 0.6 * r(6), chunk.clone())
        };
        commands.spawn((
            Mesh3d(cube.clone()),
            MeshMaterial3d(mat),
            Transform::from_translation(p + out * (r(7) * BOLT_RADIUS * 0.3) + Vec3::Y * 0.2).with_scale(size),
            Fx { age: 0.0, life, kind: Kind::Bit { vel, spin: Vec3::new(r(8) - 0.5, r(9) - 0.5, r(10) - 0.5) * 12.0, grav: 16.0, floor: p.y, base: size } },
        ));
    }
}

/// Marked-bolt rings.
fn drive_marks(time: Res<Time>, mut st: ResMut<State>, mut mats: ResMut<Assets<StandardMaterial>>, mut tr: Query<&mut Transform, Without<PlayerCam>>) {
    let dt = time.delta_secs();
    // marked-bolt rings: brighter and faster as the strike nears
    for m in st.marks.iter_mut() {
        m.left = (m.left - dt).max(0.0);
        let (bright, hz) = ring_look(m.left, m.warn);
        m.phase = (m.phase + hz * TAU * dt) % TAU;
        let pulse = 0.55 + 0.45 * m.phase.sin();
        let b = bright * pulse;
        if let Some(mut mm) = mats.get_mut(&m.mats[0]) {
            mm.base_color = Color::linear_rgb(b * 1.0, b * (0.25 + 0.5 * pulse * (1.0 - m.left / m.warn)), b * (0.25 + 0.5 * pulse * (1.0 - m.left / m.warn)));
        }
        if let Some(mut mm) = mats.get_mut(&m.mats[1]) {
            let f = 0.05 + 0.2 * b;
            mm.base_color = Color::linear_rgb(f, f * 0.2, f * 0.2);
        }
        if let Some(mut mm) = mats.get_mut(&m.mats[2]) {
            let f = 0.03 + 0.07 * b;
            mm.base_color = Color::linear_rgb(f, f * 0.3, f * 0.3);
        }
        // the ring breathes a little in size too
        if let Ok(mut t) = tr.get_mut(m.ents[0]) {
            let s = BOLT_RADIUS * (1.0 + 0.012 * m.phase.sin());
            t.scale = Vec3::new(s, s, 1.0);
        }
    }
    if let Some((_, age)) = st.flash.as_mut() {
        *age += dt;
    }
    if st.flash.is_some_and(|(_, a)| a > 0.4) {
        st.flash = None;
    }
}

fn animate(time: Res<Time>, mut commands: Commands, mut mats: ResMut<Assets<StandardMaterial>>, mut q: Query<(Entity, &mut Fx, &mut Transform, Option<&mut PointLight>)>) {
    let dt = time.delta_secs();
    for (e, mut fx, mut tr, light) in q.iter_mut() {
        fx.age += dt;
        if fx.age >= fx.life {
            if let Kind::Shock { mat, .. } = &fx.kind {
                mats.remove(mat);
            }
            commands.entity(e).despawn();
            continue;
        }
        let age = fx.age;
        let u = age / fx.life;
        match &mut fx.kind {
            Kind::Seg { w, len } => {
                let flick = 1.0 - 0.5 * ((age * 140.0).sin() * 0.5 + 0.5);
                let k = (1.0 - u) * flick + 0.15;
                tr.scale = Vec3::new(*w * k, *w * k, *len);
            }
            Kind::Shock { mat, r1 } => {
                let r = 1.0 + (*r1 - 1.0) * (1.0 - (1.0 - u).powi(3));
                tr.scale = Vec3::new(r, r, 1.0);
                if let Some(mut m) = mats.get_mut(&*mat) {
                    let f = 4.0 * (1.0 - u).powi(2);
                    m.base_color = Color::linear_rgb(f * 0.8, f, f * 1.6);
                }
            }
            Kind::Light { peak } => {
                if let Some(mut l) = light {
                    l.intensity = *peak * (1.0 - u).powi(3) * (0.7 + 0.3 * (age * 90.0).sin().abs());
                }
            }
            Kind::Bit { vel, spin, grav, floor, base } => {
                vel.y -= *grav * dt;
                tr.translation += *vel * dt;
                if tr.translation.y < *floor && *grav > 0.0 {
                    tr.translation.y = *floor;
                    vel.y = vel.y.abs() * 0.3;
                    vel.x *= 0.6;
                    vel.z *= 0.6;
                }
                tr.rotation *= Quat::from_euler(EulerRot::XYZ, spin.x * dt, spin.y * dt, spin.z * dt);
                tr.scale = *base * (1.0 - u * u).max(0.05);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn overlay(
    time: Res<Time>,
    view: Res<ViewRes>,
    window: Option<Single<&Window>>,
    shot: Option<Res<ShotRequest>>,
    st: Res<State>,
    mut geo: ResMut<GeoRes>,
    mesh: Res<HudMesh>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let _ = time;
    let flash = st.flash.map_or(0.0, |(d, a)| flash_alpha(d, a));
    let active = !st.marks.is_empty() || flash > 0.0;
    if !active {
        if geo.1 {
            geo.1 = false;
            if let Some(mut m) = meshes.get_mut(&mesh.0) {
                *m = blank();
            }
        }
        return;
    }
    geo.0.aspect = match (&window, &shot) {
        (Some(w), _) => w.physical_width() as f32 / w.physical_height().max(1) as f32,
        (None, Some(r)) => r.width as f32 / r.height.max(1) as f32,
        _ => 16.0 / 9.0,
    };
    let v = &view.0;
    let (yaw, _) = v.look_angles();
    let hv = HudView {
        marks: st.marks.iter().map(|m| MarkInfo { left: m.left, warn: m.warn, d: [m.pos.x - v.eye[0], m.pos.z - v.eye[2]] }).collect(),
        yaw,
        flash,
        t: st.t,
    };
    build(&mut geo.0, &hv);
    geo.1 = true;
    let g = &geo.0;
    if let Some(mut m) = meshes.get_mut(&mesh.0) {
        *m = if g.idx.is_empty() {
            blank()
        } else {
            Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, g.pos.clone())
                .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, g.col.clone())
                .with_inserted_indices(Indices::U32(g.idx.clone()))
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_gets_brighter_and_faster_as_the_strike_nears() {
        let (b0, h0) = ring_look(2.0, 2.0);
        let (b1, h1) = ring_look(1.0, 2.0);
        let (b2, h2) = ring_look(0.0, 2.0);
        assert!(b0 < b1 && b1 < b2 && h0 < h1 && h1 < h2);
        assert!(h2 < 12.0, "never a strobe");
        let (wi0, wp0, wv0) = whine_look(0.0);
        let (wi1, wp1, wv1) = whine_look(1.0);
        assert!(wi1 < wi0 && wp1 > wp0 && wv1 > wv0 && wi1 > 0.1);
    }

    #[test]
    fn bolt_is_deterministic_anchored_and_jagged() {
        let (top, bot) = ([0.0, 120.0, 0.0], [3.0, 0.0, -4.0]);
        let a = bolt_points(42, top, bot, 9);
        assert_eq!(a, bolt_points(42, top, bot, 9));
        assert_ne!(a, bolt_points(43, top, bot, 9));
        assert_eq!(a.len(), 10);
        assert_eq!(a[0], top);
        assert_eq!(a[9], bot);
        // descends monotonically, wanders sideways but stays close to the straight line
        for w in a.windows(2) {
            assert!(w[1][1] < w[0][1]);
        }
        let seg = 120.0 / 9.0;
        let mid = a[4];
        let line = Vec3::from(top).lerp(Vec3::from(bot), 4.0 / 9.0);
        assert!((Vec3::from(mid) - line).length() <= seg * 0.46 * 1.5);
        assert!(a[1..9].iter().enumerate().any(|(i, p)| (Vec3::from(*p) - Vec3::from(top).lerp(Vec3::from(bot), (i + 1) as f32 / 9.0)).length() > 0.5));
    }

    #[test]
    fn flash_scales_with_proximity_and_dies() {
        assert!(flash_alpha(3.0, 0.0) > flash_alpha(60.0, 0.0));
        assert!(flash_alpha(60.0, 0.0) > flash_alpha(300.0, 0.0));
        assert!(flash_alpha(500.0, 0.0) > 0.05, "a distant strike still lights the sky");
        assert!(flash_alpha(3.0, 0.0) <= 0.95);
        assert!(flash_alpha(3.0, 0.2) < flash_alpha(3.0, 0.05));
        assert_eq!(flash_alpha(3.0, 0.4), 0.0);
        assert_eq!(flash_alpha(3.0, -0.1), 0.0);
    }

    #[test]
    fn arrow_angle_and_nearest_mark() {
        assert!(rel_angle(0.0, -10.0, 0.0).abs() < 1e-5, "straight ahead");
        assert!((rel_angle(10.0, 0.0, 0.0) - std::f32::consts::FRAC_PI_2).abs() < 1e-5, "right");
        assert!((rel_angle(0.0, 10.0, 0.0).abs() - std::f32::consts::PI).abs() < 1e-5, "behind");
        let m = |x: f32, z: f32| MarkInfo { left: 1.0, warn: 2.0, d: [x, z] };
        assert_eq!(nearest_mark(&[]), None);
        assert_eq!(nearest_mark(&[m(30.0, 0.0), m(0.0, -5.0), m(9.0, 9.0)]), Some(1));
    }

    #[test]
    fn overlay_builds_finite_geometry_for_every_state() {
        let mut g = Geo { aspect: 16.0 / 9.0, ..default() };
        let marks = vec![MarkInfo { left: 1.0, warn: 2.0, d: [-20.0, 5.0] }, MarkInfo { left: 0.4, warn: 2.0, d: [2.0, 1.0] }];
        for mk in [vec![], marks.clone(), marks[..1].to_vec()] {
            build(&mut g, &HudView { marks: mk.clone(), yaw: 0.3, flash: 0.5, t: 1.7 });
            assert_eq!(g.pos.len(), g.col.len());
            assert_eq!(g.idx.len() % 3, 0);
            assert!(g.idx.iter().all(|i| (*i as usize) < g.pos.len()));
            assert!(g.pos.iter().all(|p| p.iter().all(|v| v.is_finite())));
            assert!(!g.idx.is_empty());
        }
        build(&mut g, &HudView::default());
        assert!(g.idx.is_empty(), "nothing going on, nothing drawn");
    }
}
