//! LOUD "you were hit" HUD: red/magenta vignette slam, chroma-split edge frames + scanline glitch bursts, directional
//! damage arcs (coloured by weapon), CRITICAL tag for big hits / headshots, persistent low-hp pulse border, a blue-white
//! shield ring when our shield took a hit (HitMsg with taken && amount == 0), and a small bounded camera shake.
//! Own overlay camera (layer 4, order 3) fed by one dynamic mesh; idle frames do nothing and allocate nothing.
//! Review aid: `SHOT_HIT=dmg,rel_angle_rad[,kind[,flags[,hp_frac]]]` pins the effect at its peak (flags: 1 head, 2 shield).

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::camera::{ClearColorConfig, Hdr, RenderTarget, ScalingMode};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use shinobi_client::hud::{Align, Prim, Rgba};
use shinobi_client::hud_geom::tessellate;
use shinobi_proto::gameplay::{KIND_ARROW, KIND_SHURIKEN, KIND_SLASH, KIND_TAG};

use crate::net::ViewRes;
use crate::play::{PlayerCam, ShotImage, ShotRequest};

const LAYER: usize = 4;
const MAX_HP: f32 = shinobi_sim::pvp::MAX_HP;
const ATTACK_S: f32 = 0.05;
const DECAY_S: f32 = 0.6;
const ARC_LIFE: f32 = 1.0;
const TAG_LIFE: f32 = 0.9;
const RING_LIFE: f32 = 0.5;
const ARCS: usize = 8;
const BIG_FRAC: f32 = 0.30;
const LOW_HP: f32 = 0.30;
const SEGS: usize = 48;
const TAU: f32 = std::f32::consts::TAU;

// ---------- pure logic ----------

/// Slam amplitude 0.3..=1.45 from damage, boosted for big hits / headshots.
pub fn strength(dmg: f32, head: bool) -> f32 {
    let base = (0.25 + dmg / MAX_HP * 2.2).clamp(0.3, 1.0);
    let boost = if is_big(dmg) { 1.2 } else { 1.0 } * if head { 1.2 } else { 1.0 };
    (base * boost).min(1.45)
}

pub fn is_big(dmg: f32) -> bool {
    dmg >= BIG_FRAC * MAX_HP - 1e-3
}

/// Attack/decay envelope 0..=1: linear rise over `ATTACK_S`, quadratic fall over `DECAY_S`, 0 afterwards.
pub fn envelope(t: f32) -> f32 {
    if t < 0.0 {
        0.0
    } else if t < ATTACK_S {
        t / ATTACK_S
    } else {
        let u = 1.0 - (t - ATTACK_S) / DECAY_S;
        if u <= 0.0 { 0.0 } else { u * u }
    }
}

/// Screen angle (0 = straight up, clockwise positive) of a horizontal world offset (dx, dz) seen with aim `yaw`
/// (yaw 0 faces -Z, positive turns toward -X). Ahead = 0, right = +PI/2, behind = PI.
pub fn arc_angle(dx: f32, dz: f32, yaw: f32) -> f32 {
    let (s, c) = yaw.sin_cos();
    let fwd = -s * dx - c * dz;
    let right = c * dx - s * dz;
    right.atan2(fwd)
}

/// 0 above `LOW_HP`, rising to 1 at 0 hp.
pub fn low_factor(frac: f32) -> f32 {
    ((LOW_HP - frac) / LOW_HP).clamp(0.0, 1.0)
}

/// Heartbeat rate in Hz: 1.2 at the threshold up to 4.7 near death.
pub fn pulse_hz(low: f32) -> f32 {
    1.2 + 3.5 * low
}

/// Low-hp border pulse 0..=1 for a phase in cycles.
pub fn pulse(phase: f32) -> f32 {
    0.5 + 0.5 * (phase * TAU).sin()
}

/// Arc colour by weapon: katana hot white-red, star cyan-white, arrow/tag orange, anything else red.
pub fn kind_colour(kind: u8) -> [f32; 3] {
    match kind {
        KIND_SLASH => [1.0, 0.62, 0.55],
        KIND_SHURIKEN => [0.65, 1.0, 1.0],
        KIND_ARROW | KIND_TAG => [1.0, 0.55, 0.1],
        _ => [1.0, 0.15, 0.2],
    }
}

fn hash(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B1) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 65535.0
}

fn ang_diff(a: f32, b: f32) -> f32 {
    (a - b + std::f32::consts::PI).rem_euclid(TAU) - std::f32::consts::PI
}

// ---------- state ----------

#[derive(Clone, Copy, Default)]
struct Arc {
    on: bool,
    angle: f32,
    age: f32,
    amp: f32,
    kind: u8,
}

#[derive(Clone, Copy)]
struct Ev {
    dmg: f32,
    head: bool,
    kind: u8,
    shield: bool,
    angle: Option<f32>,
}

#[derive(Resource)]
pub struct HitHud {
    amp: f32,
    t: f32,
    arcs: [Arc; ARCS],
    tag_age: f32,
    tag_head: bool,
    ring_age: f32,
    low: f32,
    phase: f32,
    frame: u32,
    drawn: bool,
}

impl Default for HitHud {
    fn default() -> Self {
        Self { amp: 0.0, t: 10.0, arcs: [Arc::default(); ARCS], tag_age: TAG_LIFE, tag_head: false, ring_age: RING_LIFE, low: 0.0, phase: 0.0, frame: 0, drawn: false }
    }
}

impl HitHud {
    fn trigger(&mut self, e: Ev) {
        if !e.shield {
            // a fresh hit restarts the slam, never weaker than what is still showing
            self.amp = strength(e.dmg, e.head).max(self.slam());
            self.t = 0.0;
            if is_big(e.dmg) || e.head {
                self.tag_age = 0.0;
                self.tag_head = e.head;
            }
        } else {
            self.ring_age = 0.0;
        }
        if let Some(angle) = e.angle {
            let amp = if e.shield { 0.7 } else { 0.6 + 0.4 * strength(e.dmg, e.head).min(1.0) };
            // merge into a close arc of the same weapon, else a free slot, else the oldest
            let i = self
                .arcs
                .iter()
                .position(|a| a.on && a.kind == e.kind && ang_diff(a.angle, angle).abs() < 0.35)
                .or_else(|| self.arcs.iter().position(|a| !a.on))
                .unwrap_or_else(|| (0..ARCS).max_by(|&a, &b| self.arcs[a].age.total_cmp(&self.arcs[b].age)).unwrap_or(0));
            self.arcs[i] = Arc { on: true, angle, age: 0.0, amp, kind: e.kind };
        }
    }
    fn slam(&self) -> f32 {
        self.amp * envelope(self.t)
    }
    fn active(&self) -> bool {
        self.slam() > 0.002 || self.arcs.iter().any(|a| a.on) || self.tag_age < TAG_LIFE || self.ring_age < RING_LIFE || self.low > 0.0
    }
    fn tick(&mut self, dt: f32) {
        self.t += dt;
        self.tag_age += dt;
        self.ring_age += dt;
        for a in self.arcs.iter_mut().filter(|a| a.on) {
            a.age += dt;
            a.on = a.age < ARC_LIFE;
        }
        self.phase = (self.phase + pulse_hz(self.low) * dt).fract();
        self.frame = self.frame.wrapping_add(1);
    }
    fn clear(&mut self) {
        self.amp = 0.0;
        self.tag_age = TAG_LIFE;
        self.ring_age = RING_LIFE;
        self.low = 0.0;
        for a in self.arcs.iter_mut() {
            a.on = false;
        }
    }
    /// View-frame camera offset (right, up) in metres: bounded, zero when the slam is over.
    pub fn shake(&self) -> (f32, f32) {
        let e = self.slam().min(1.0);
        let k = e * e * 0.06;
        ((self.t * 71.0).sin() * k, (self.t * 97.0).cos() * k * 0.8)
    }
}

/// `SHOT_HIT` pin: (event, hp fraction) re-triggered at peak every frame.
#[derive(Resource)]
struct Pin(Ev, Option<f32>);

fn parse_pin(s: &str) -> Option<Pin> {
    let n: Vec<f32> = s.split(',').filter_map(|v| v.trim().parse().ok()).collect();
    if n.len() < 2 {
        return None;
    }
    let flags = n.get(3).copied().unwrap_or(0.0) as u32;
    Some(Pin(
        Ev { dmg: n[0], head: flags & 1 != 0, kind: n.get(2).copied().unwrap_or(1.0) as u8, shield: flags & 2 != 0, angle: Some(n[1]) },
        n.get(4).copied(),
    ))
}

// ---------- geometry ----------

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
    /// Point (dx, dy) screen-height units from the centre, y down.
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
    /// Screen-edge vignette: alpha `a` at the edge fading to 0 at `depth` of the way to the centre.
    fn vignette(&mut self, rgb: [f32; 3], a: f32, depth: f32, shift: f32) {
        let k = 1.0 - depth;
        for i in 0..SEGS {
            let (a0, a1) = (i as f32 / SEGS as f32 * TAU, (i + 1) as f32 / SEGS as f32 * TAU);
            let o = |t: f32| {
                let (s, c) = t.sin_cos();
                let m = c.abs().max(s.abs()).max(1e-4);
                [c * 0.5 / m * 1.02, s * 0.5 / m * 1.02]
            };
            let (o0, o1) = (o(a0), o(a1));
            let p = |v: [f32; 2], f: f32| [0.5 + v[0] * f + shift, 0.5 + v[1] * f];
            let (oc, ic) = ([rgb[0], rgb[1], rgb[2], a], [rgb[0], rgb[1], rgb[2], 0.0]);
            self.quad([p(o0, 1.0), p(o1, 1.0), p(o1, k), p(o0, k)], [oc, oc, ic, ic]);
        }
    }
    /// Annular sector around the screen centre (radii in height units, angles clockwise from up).
    fn arc(&mut self, a0: f32, a1: f32, r0: f32, r1: f32, rgb: [f32; 3], alpha: f32, taper: bool) {
        let n = 12;
        for i in 0..n {
            let (t0, t1) = (a0 + (a1 - a0) * i as f32 / n as f32, a0 + (a1 - a0) * (i + 1) as f32 / n as f32);
            let w = |t: f32| if taper { 1.0 - ((t - a0) / (a1 - a0) * 2.0 - 1.0).powi(2) } else { 1.0 };
            let at = |t: f32, r: f32| self.pt(r * t.sin(), -r * t.cos());
            let (c0, c1) = ([rgb[0], rgb[1], rgb[2], alpha * w(t0)], [rgb[0], rgb[1], rgb[2], alpha * w(t1)]);
            let q = [at(t0, r0), at(t1, r0), at(t1, r1), at(t0, r1)];
            self.quad(q, [c0, c1, c1, c0]);
        }
    }
    fn ring(&mut self, r0: f32, r1: f32, rgb: [f32; 3], alpha: f32) {
        self.arc(0.0, TAU, r0, r1, rgb, alpha, false);
    }
    fn seg(&mut self, a: (f32, f32), b: (f32, f32), w: f32, rgb: [f32; 3], alpha: f32) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l = (dx * dx + dy * dy).sqrt().max(1e-5);
        let (nx, ny) = (-dy / l * w * 0.5, dx / l * w * 0.5);
        let c = [rgb[0], rgb[1], rgb[2], alpha];
        let q = [self.pt(a.0 + nx, a.1 + ny), self.pt(b.0 + nx, b.1 + ny), self.pt(b.0 - nx, b.1 - ny), self.pt(a.0 - nx, a.1 - ny)];
        self.quad(q, [c; 4]);
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

fn build(h: &HitHud, g: &mut Geo, reduced: bool) {
    g.clear();
    let e = h.slam();
    let ev = e.min(1.0);
    // low hp: tightening vignette + pulsing border, under everything else
    if h.low > 0.0 {
        let p = pulse(h.phase);
        g.vignette([0.9, 0.0, 0.08], (0.25 + 0.4 * h.low) * (0.55 + 0.45 * p), 0.12 + 0.30 * h.low, 0.0);
        let t = 0.012 + 0.02 * h.low * p;
        let tx = t / g.aspect;
        let c = [1.0, 0.05, 0.1, (0.5 + 0.5 * p) * (0.4 + 0.6 * h.low)];
        g.rect(0.0, 0.0, 1.0, t, c);
        g.rect(0.0, 1.0 - t, 1.0, t, c);
        g.rect(0.0, 0.0, tx, 1.0, c);
        g.rect(1.0 - tx, 0.0, tx, 1.0, c);
    }
    if e > 0.002 {
        // slam: red wash, magenta-tinged heavy vignette
        g.rect(0.0, 0.0, 1.0, 1.0, [1.0, 0.0, 0.1, (0.30 * e).min(0.5)]);
        let mag = (e - 0.6).clamp(0.0, 0.8);
        g.vignette([1.0, 0.0, 0.1 + 0.6 * mag], (0.55 + 0.4 * ev).min(1.0), 0.28 + 0.32 * e.min(1.2), 0.0);
        // chroma split: offset red / cyan edge frames + glitch scanline bursts
        let off = if reduced { 0.0 } else { 0.02 * e };
        g.vignette([1.0, 0.0, 0.0], 0.7 * ev, 0.12, off);
        g.vignette([0.0, 0.9, 1.0], 0.55 * ev, 0.12, -off);
        let bars = if reduced { 0 } else { (e * 7.0) as u32 };
        let step = h.frame / 2; // pattern changes every other frame
        for i in 0..bars {
            let r = |k: u32| hash(step.wrapping_mul(31).wrapping_add(i * 7 + k));
            let (y, hh) = (r(1), 0.003 + 0.016 * r(2));
            let w = 0.10 + 0.22 * r(3);
            let rgb = if r(4) > 0.5 { [1.0, 0.1, 0.2] } else { [0.1, 0.95, 1.0] };
            let a = (0.4 + 0.5 * r(5)) * ev;
            g.rect(0.0, y, w, hh, [rgb[0], rgb[1], rgb[2], a]);
            g.rect(1.0 - w * (0.5 + r(6)), 1.0 - y, w, hh, [rgb[0], rgb[1], rgb[2], a]);
        }
        // hit marker flash: X at the crosshair
        let (r0, r1) = (0.014 + 0.01 * (1.0 - ev), 0.05 + 0.03 * ev);
        let k = std::f32::consts::FRAC_1_SQRT_2;
        for (sx, sy) in [(1.0f32, 1.0f32), (-1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)] {
            g.seg((sx * r0 * k, sy * r0 * k), (sx * r1 * k, sy * r1 * k), 0.008, [1.0, 0.9, 0.9], ev);
        }
    }
    // directional arcs
    for a in h.arcs.iter().filter(|a| a.on) {
        let alpha = ((1.0 - a.age / ARC_LIFE).sqrt() * a.amp).min(1.0);
        let c = kind_colour(a.kind);
        let pop = 1.0 + 0.5 * (1.0 - (a.age / 0.15).min(1.0));
        let half = 0.55 * pop;
        let (r0, r1) = (0.17, 0.17 + 0.03 * pop);
        g.arc(a.angle - half, a.angle + half, r0, r1, c, alpha, true);
        g.arc(a.angle - half * 0.7, a.angle + half * 0.7, r0 + 0.004, r1 - 0.004, [1.0, 1.0, 1.0], alpha * 0.8, true);
        g.arc(a.angle - half * 1.2, a.angle + half * 1.2, r1, r1 + 0.03, c, alpha * 0.35, true);
    }
    // shield: blue-white ring
    if h.ring_age < RING_LIFE {
        let u = h.ring_age / RING_LIFE;
        let r = 0.10 + 0.07 * u;
        let a = (1.0 - u).powi(2);
        g.ring(r, r + 0.012, [0.75, 0.92, 1.0], a);
        g.ring(r - 0.03, r, [0.2, 0.55, 1.0], a * 0.5);
        g.ring(r + 0.012, r + 0.05, [0.4, 0.7, 1.0], a * 0.3);
    }
    // CRITICAL / HEADSHOT tag
    if h.tag_age < TAG_LIFE {
        let u = h.tag_age / TAG_LIFE;
        let pop = 1.0 + 0.6 * (1.0 - (h.tag_age / 0.12).min(1.0));
        let a = if u > 0.7 { (1.0 - u) / 0.3 } else { 1.0 };
        let size = 0.075 * pop;
        let (txt, col) = if h.tag_head { ("HEADSHOT", Rgba(1.0, 0.85, 0.3, a)) } else { ("CRITICAL", Rgba(1.0, 0.45, 0.5, a)) };
        let at = |dx: f32, c: Rgba| Prim::Text { pos: (0.5 + dx, 0.30 - 0.02 * u), text: txt.to_string(), size, color: c, align: Align::Center };
        let split = 0.006 * pop;
        let prims = [at(-split, Rgba(0.0, 0.9, 1.0, a * 0.8)), at(split, Rgba(1.0, 0.0, 0.2, a * 0.8)), at(0.0, col)];
        let t = tessellate(&prims, g.aspect);
        g.append(t);
    }
}

// ---------- systems ----------

#[derive(Resource)]
struct HudMesh(Handle<Mesh>);

#[derive(Resource)]
struct GeoRes(Geo);

pub struct HitIndicatorPlugin;

impl Plugin for HitIndicatorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HitHud>().insert_resource(GeoRes(Geo { aspect: 16.0 / 9.0, ..default() }));
        if let Some(p) = std::env::var("SHOT_HIT").ok().as_deref().and_then(parse_pin) {
            app.insert_resource(p);
        }
        app.add_systems(Startup, spawn.after(crate::play::setup))
            .add_systems(Update, update)
            .add_systems(PostUpdate, shake.before(TransformSystems::Propagate));
    }
}

fn blank() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3])
}

fn spawn(mut commands: Commands, shot: Option<Res<ShotImage>>, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let layer = RenderLayers::layer(LAYER);
    let mut cam = commands.spawn((
        Camera3d::default(),
        Camera { order: 3, clear_color: ClearColorConfig::None, ..default() },
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

#[allow(clippy::too_many_arguments)]
fn update(
    time: Res<Time>,
    view: Res<ViewRes>,
    window: Option<Single<&Window>>,
    shot: Option<Res<ShotRequest>>,
    settings: Option<Res<crate::hud_view::SettingsRes>>,
    pin: Option<Res<Pin>>,
    mut h: ResMut<HitHud>,
    mut geo: ResMut<GeoRes>,
    mesh: Res<HudMesh>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let v = &view.0;
    let dt = time.delta_secs();
    if v.death.is_some() {
        if h.active() {
            h.clear();
        }
    } else {
        if !v.hits.is_empty() {
            let (yaw, _) = v.look_angles();
            let eye = Vec3::from(v.eye);
            for m in v.hits.iter().filter(|m| m.taken) {
                let angle = v.remotes.iter().find(|(id, _)| *id == m.other && m.other != 0).map(|(_, s)| {
                    let d = Vec3::from(s.pos) - eye;
                    arc_angle(d.x, d.z, yaw)
                });
                h.trigger(Ev { dmg: m.amount as f32, head: m.head, kind: m.kind, shield: m.amount == 0, angle });
            }
        }
        if let Some(hp) = v.hp {
            h.low = low_factor(hp / MAX_HP);
        }
    }
    if let Some(p) = pin.as_deref() {
        h.trigger(p.0);
        h.t = ATTACK_S; // hold at the peak
        h.tag_age = 0.05;
        h.ring_age = 0.05;
        for a in h.arcs.iter_mut() {
            a.age = 0.05;
        }
        if let Some(f) = p.1 {
            h.low = low_factor(f);
        }
    }
    let was_active = h.active();
    h.tick(dt);
    if !was_active {
        if h.drawn {
            h.drawn = false;
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
    let reduced = settings.as_ref().map_or(false, |s| s.0.reduced_motion);
    build(&h, &mut geo.0, reduced);
    h.drawn = true;
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

/// Translation-only kick in the view frame (aim and crosshair never move); off with reduced motion.
fn shake(h: Res<HitHud>, settings: Option<Res<crate::hud_view::SettingsRes>>, mut cam: Single<&mut Transform, With<PlayerCam>>) {
    if h.slam() < 0.01 {
        return;
    }
    let Some(s) = settings.as_ref().map(|s| &s.0) else { return };
    if s.reduced_motion {
        return;
    }
    let (r, u) = h.shake();
    let k = s.fx_intensity.clamp(0.0, 1.0);
    let (right, up) = (cam.right(), cam.up());
    cam.translation += (right * r + up * u) * k;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(dmg: f32, angle: Option<f32>, kind: u8) -> Ev {
        Ev { dmg, head: false, kind, shield: false, angle }
    }

    #[test]
    fn strength_scales_and_boosts() {
        assert!(strength(10.0, false) < strength(30.0, false));
        assert!(strength(20.0, true) > strength(20.0, false));
        assert!(strength(255.0, true) <= 1.45 && strength(1.0, false) >= 0.3);
        assert!(is_big(30.0) && !is_big(29.0));
    }

    #[test]
    fn envelope_attacks_fast_and_decays_to_zero() {
        assert_eq!(envelope(-1.0), 0.0);
        assert!((envelope(ATTACK_S) - 1.0).abs() < 1e-6);
        assert!(envelope(0.025) > 0.45 && envelope(0.025) < 0.55);
        assert!(envelope(0.2) < envelope(0.1));
        assert_eq!(envelope(ATTACK_S + DECAY_S + 0.01), 0.0);
    }

    #[test]
    fn arc_points_to_the_attacker() {
        let pi = std::f32::consts::PI;
        assert!(arc_angle(0.0, -5.0, 0.0).abs() < 1e-5);
        assert!((arc_angle(5.0, 0.0, 0.0) - pi / 2.0).abs() < 1e-5);
        assert!((arc_angle(0.0, 5.0, 0.0).abs() - pi).abs() < 1e-5);
        assert!((arc_angle(-5.0, 0.0, 0.0) + pi / 2.0).abs() < 1e-5);
        // yaw +pi/2 faces -X: an attacker at -X is straight ahead, one at -Z is to the right
        assert!(arc_angle(-5.0, 0.0, pi / 2.0).abs() < 1e-5);
        assert!((arc_angle(0.0, -5.0, pi / 2.0) - pi / 2.0).abs() < 1e-5);
    }

    #[test]
    fn low_hp_pulse_tightens_and_speeds_up() {
        assert_eq!(low_factor(0.5), 0.0);
        assert_eq!(low_factor(0.3), 0.0);
        assert!((low_factor(0.15) - 0.5).abs() < 1e-5);
        assert_eq!(low_factor(0.0), 1.0);
        assert!(pulse_hz(1.0) > pulse_hz(0.2));
        for i in 0..20 {
            assert!((0.0..=1.0).contains(&pulse(i as f32 / 20.0)));
        }
    }

    #[test]
    fn weapon_colours_differ() {
        assert_ne!(kind_colour(KIND_SLASH), kind_colour(KIND_SHURIKEN));
        assert_eq!(kind_colour(KIND_ARROW), kind_colour(KIND_TAG));
        assert_ne!(kind_colour(KIND_ARROW), kind_colour(KIND_SLASH));
    }

    #[test]
    fn arcs_stack_merge_and_expire() {
        let mut h = HitHud::default();
        h.trigger(ev(10.0, Some(0.0), KIND_SLASH));
        h.trigger(ev(10.0, Some(0.1), KIND_SLASH)); // same direction + weapon merges
        h.trigger(ev(10.0, Some(2.0), KIND_SHURIKEN));
        assert_eq!(h.arcs.iter().filter(|a| a.on).count(), 2);
        h.tick(1.01);
        assert!(h.arcs.iter().all(|a| !a.on));
    }

    #[test]
    fn big_hit_raises_tag_shield_does_not_slam() {
        let mut h = HitHud::default();
        assert!(!h.active());
        h.trigger(ev(10.0, None, 1));
        assert!(h.tag_age >= TAG_LIFE);
        h.trigger(ev(35.0, None, 1));
        assert!(h.tag_age < TAG_LIFE);
        let mut s = HitHud::default();
        s.trigger(Ev { dmg: 0.0, head: false, kind: 1, shield: true, angle: None });
        assert!(s.ring_age < RING_LIFE && s.slam() == 0.0);
    }

    #[test]
    fn geometry_builds_valid_indices() {
        let mut h = HitHud::default();
        h.trigger(Ev { dmg: 40.0, head: true, kind: 1, shield: false, angle: Some(1.0) });
        h.trigger(Ev { dmg: 0.0, head: false, kind: 1, shield: true, angle: None });
        h.t = ATTACK_S;
        h.low = 0.6;
        let mut g = Geo { aspect: 1.78, ..default() };
        build(&h, &mut g, false);
        assert!(!g.idx.is_empty() && g.pos.len() == g.col.len());
        assert!(g.idx.iter().all(|&i| (i as usize) < g.pos.len()));
    }

    #[test]
    fn shake_is_bounded() {
        let mut h = HitHud::default();
        h.trigger(ev(255.0, None, 1));
        for i in 0..100 {
            h.t = i as f32 * 0.01;
            let (r, u) = h.shake();
            assert!(r.abs() <= 0.061 && u.abs() <= 0.061);
        }
    }

    #[test]
    fn pin_parses() {
        let p = parse_pin("40,1.57,1,3,0.2").unwrap();
        assert!(p.0.head && p.0.shield && p.1 == Some(0.2));
        assert!(parse_pin("40").is_none());
    }
}
