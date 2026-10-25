//! First-person viewmodel pass: katana blade + swing-arc ribbons + held shuriken, drawn by a second camera
//! on render layer 1 (own depth, no CRT warp) after the world camera, so nothing clips into world geometry.
//! All poses come from `shinobi_client::viewmodel` / `viewmodel_draw`; this file only turns them into
//! Bevy transforms and meshes.

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::camera::{ClearColorConfig, Hdr, RenderTarget};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::mesh::PrimitiveTopology;
use bevy::prelude::*;

use shinobi_client::hud::Palette;
use shinobi_client::viewmodel::{KatanaViewModel, ViewCfg};
use shinobi_client::viewmodel_draw::{katana_draw, HitPulse, ShurikenViewModel};

use crate::gore::GoreCfg;
use crate::impacts::{Impact, ImpactKind, ImpactQueue};
use crate::net::{Net, ViewRes};
use crate::team::LocalColour;
use crate::play::{FxRes, ShotImage};

const LAYER: usize = 1;
const FLARE_S: f32 = 0.2;
const WHIFF_S: f32 = 0.3;
const RECOIL_S: f32 = 0.18;
/// Flow tier-up speed-line kick duration (vm.rs's own beat, distinct from the kill streak / bounty audio stingers).
const TIER_KICK_S: f32 = 0.3;

/// Shoulder recoil of the held weapons after a throw: (kick back in metres, kick up in radians), snapping in and
/// easing out over `RECOIL_S`.
pub fn recoil_kick(left: f32) -> (f32, f32) {
    let k = (left / RECOIL_S).clamp(0.0, 1.0);
    let e = k * k;
    (0.05 * e, 0.06 * e)
}

/// Target speed-line intensity 0..1: dash is full, a grapple pull strong, a grind (landed from a jump, jump held) steady,
/// plain running fades in above ~14 m/s.
/// A flow streak (`combat::flow`, docs/traversal_melee.md "Flow") raises the floor by tier (0..3), so the lines
/// read as "building toward something" through a long chain of wall-runs / hooks / dashes, not just the latest move.
pub fn streak_target(dashing: bool, grappling: bool, grinding: bool, speed: f32, flow_tier: u32) -> f32 {
    let run = ((speed - 14.0) / 18.0).clamp(0.0, 0.45);
    let mut t = run;
    if grappling {
        t = t.max(0.7);
    }
    if grinding {
        t = t.max(0.45 + ((speed - 8.0) / 20.0).clamp(0.0, 0.25));
    }
    if dashing {
        t = 1.0;
    }
    if flow_tier > 0 {
        t = t.max(0.25 + 0.18 * flow_tier.min(3) as f32);
    }
    t
}

/// Radial speed lines in view space (the same units as the katana arc: depth plane `depth`, aspect-stretched): lines
/// stream outward from the screen centre, longer and brighter with `intensity`. Deterministic in `t`.
pub fn streak_quads(t: f32, intensity: f32, aspect: f32, depth: f32, rgb: [f32; 3]) -> (Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<u32>) {
    let (mut pos, mut col, mut idx) = (Vec::new(), Vec::new(), Vec::new());
    if intensity < 0.02 {
        return (pos, col, idx);
    }
    let hh = depth * (45.0f32).to_radians().tan(); // half height of the view at `depth` (90 deg vertical fov)
    let n = (8.0 + 24.0 * intensity) as u32;
    for i in 0..n {
        let h = |k: u32| {
            let mut x = (i * 3 + k).wrapping_mul(0x9e37_79b1) ^ 0x85eb_ca6b;
            x ^= x >> 15;
            x = x.wrapping_mul(0x2c1b_3c6d);
            ((x >> 8) & 0xffff) as f32 / 65536.0
        };
        let ang = h(0) * std::f32::consts::TAU;
        let phase = (h(1) + t * (1.3 + 1.2 * h(2)) * (0.6 + intensity)).fract();
        let r0 = (0.30 + 0.75 * phase) * hh;
        let len = (0.10 + 0.28 * intensity) * hh * (0.5 + h(3));
        let (c, s) = (ang.cos(), ang.sin());
        let w = 0.0025 * hh * (1.0 + 2.0 * h(3));
        // stay inside the screen ellipse (the CRT barrel curves the corners away)
        let b = (r0 + len).min(0.97 * hh);
        let a = r0.min(b - 0.02 * hh);
        let alpha = intensity * (1.0 - phase) * 0.7;
        let base = pos.len() as u32;
        for (r, al) in [(a, alpha * 0.2), (b, alpha)] {
            let p = [c * r * aspect, s * r];
            let n = [-s * w * aspect, c * w];
            pos.push([p[0] + n[0], p[1] + n[1], -depth]);
            pos.push([p[0] - n[0], p[1] - n[1], -depth]);
            let cc = [rgb[0] * 2.5, rgb[1] * 2.5, rgb[2] * 2.5, al];
            col.extend([cc, cc]);
        }
        idx.extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
    }
    (pos, col, idx)
}

/// Arc ribbon multipliers: (brightness, alpha). Flare after a connect, dimmed after a whiff, neutral otherwise.
pub fn arc_gain(flare: f32, whiff: f32) -> (f32, f32) {
    let f = (flare / FLARE_S).clamp(0.0, 1.0);
    let w = (whiff / WHIFF_S).clamp(0.0, 1.0);
    (1.0 + 2.5 * f, 1.0 - 0.65 * w)
}

#[derive(Resource)]
pub struct Vm {
    pub katana: KatanaViewModel,
    pub shuriken: ShurikenViewModel,
    pub pulse: HitPulse,
    flip: bool,
    /// Hit-stop: seconds the held weapons stay frozen after a katana hit (client-only; the sim never pauses).
    freeze: f32,
    /// Seconds left of the dimmed whiff arc after a swing that hit nothing.
    whiff: f32,
    /// Seconds left of the bright arc flare after a katana connect.
    flare: f32,
    /// Seconds left of the shoulder recoil after a shuriken throw.
    recoil: f32,
    /// Smoothed speed-line intensity 0..1 (dash / grapple / fast running).
    streak: f32,
    /// Flow tier (`combat::flow`) as of last frame: a rise triggers `tier_kick`.
    prev_flow_tier: u32,
    /// Seconds left of the flow tier-up kick: briefly snaps the speed lines to white-hot and a touch brighter,
    /// distinct from their steady climb, so crossing a tier reads as a beat and not just "a bit more VFX".
    tier_kick: f32,
    /// Block pose blend 0..1 (blade raised across the screen, shield up) and the shield flash (absorbed hit / parry).
    guard: f32,
    guard_flash: f32,
    /// Blood left on the blade/gloves after a landed hit, 0..1; fades over a few seconds.
    blood: f32,
}

#[derive(Resource)]
pub struct VmEnts {
    blade: Entity,
    /// First-person shield: a big additive panel with a bright frame, in front of the camera while blocking.
    shield: Entity,
    shield_mat: Handle<StandardMaterial>,
    frame_mat: Handle<StandardMaterial>,
    ribbons_mesh: Handle<Mesh>,
    ribbons: Entity,
    /// Speed lines (dash / grapple / fast running), on the viewmodel layer.
    streaks: Entity,
    streaks_mesh: Handle<Mesh>,
    streaks_mat: Handle<StandardMaterial>,
    shuriken: Entity,
    /// Gloved left hand holding the star, and the wrist grapple launcher (rope origin), both on the viewmodel layer.
    left_hand: Entity,
    launcher: Entity,
    /// Glove and sleeve materials (recoloured by a custom character, see `apply_look`).
    glove: Handle<StandardMaterial>,
    sleeve: Handle<StandardMaterial>,
    /// Blood flecks parented to the blade and both gloves, shown after a connect.
    blood: Vec<Entity>,
    blood_mat: Handle<StandardMaterial>,
    /// (material, base colour, gain) flared by hit confirmation
    glow: Vec<(Handle<StandardMaterial>, shinobi_client::hud::Rgba, f32)>,
}

pub struct VmPlugin;

impl Plugin for VmPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Vm { katana: KatanaViewModel::new(ViewCfg::default()), shuriken: ShurikenViewModel::default(), pulse: HitPulse::default(), flip: false, freeze: 0.0, whiff: 0.0, flare: 0.0, recoil: 0.0, streak: 0.0, prev_flow_tier: 0, tier_kick: 0.0, guard: 0.0, guard_flash: 0.0, blood: 0.0 })
            .add_systems(Startup, spawn.after(crate::play::setup))
            .add_systems(Update, apply_look.before(update));
    }
}

fn degenerate() -> Mesh {
    // zero-vertex meshes upset Bevy's mesh slab allocator; a transparent point-triangle is harmless
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3])
}

/// Four-point throwing star in the XY plane (fan around the centre).
fn star_mesh() -> Mesh {
    let (outer, inner, n) = (0.05f32, 0.019f32, 8usize);
    let mut pos = vec![[0.0f32, 0.0, 0.0]];
    for i in 0..n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        let r = if i % 2 == 0 { outer } else { inner };
        pos.push([r * a.cos(), r * a.sin(), 0.0]);
    }
    let mut idx = Vec::new();
    for i in 0..n {
        idx.extend_from_slice(&[0u32, 1 + i as u32, 1 + ((i + 1) % n) as u32]);
    }
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_indices(bevy::mesh::Indices::U32(idx))
}

/// Curved katana blade along -Z (hilt end at z=0, tip at z=-len), edge on +X. The sori bows the edge side outward
/// (spine is the concave side) while the tip returns to the axis, so the ribbon/tip maths in `viewmodel.rs` stay
/// valid. Cross-section: flat spine, shinogi ridge, bevel down to the cutting edge; the kissaki converges to a
/// point. Returns (body with baked shading, thin glowing edge strip).
fn blade_meshes(len: f32) -> (Mesh, Mesh) {
    const SAG: f32 = 0.04;
    // (x, y, shade) of the cross-section: spine top, ridge top, edge, ridge bottom, spine bottom
    const SEC: [(f32, f32, f32); 5] =
        [(-0.011, 0.0065, 0.22), (0.004, 0.0060, 0.55), (0.021, 0.0, 1.0), (0.004, -0.0060, 0.55), (-0.011, -0.0065, 0.22)];
    let mut us: Vec<f32> = (0..=22).map(|i| i as f32 / 22.0 * 0.94).collect();
    us.extend([0.97, 1.0]);
    let ring = |u: f32| -> (f32, f32) {
        // (z, x offset, width scale)
        let bow = SAG * 4.0 * u * (1.0 - u);
        let taper = 1.0 - 0.3 * u;
        let tip = if u > 0.94 { ((1.0 - u) / 0.06).max(0.0) } else { 1.0 };
        (bow, taper * tip)
    };
    let (mut bp, mut bc, mut bi) = (Vec::<[f32; 3]>::new(), Vec::<[f32; 4]>::new(), Vec::<u32>::new());
    let (mut ep, mut ei) = (Vec::<[f32; 3]>::new(), Vec::<u32>::new());
    for &u in &us {
        let (bow, w) = ring(u);
        let z = -u * len;
        for &(x, y, sh) in &SEC {
            bp.push([bow + x * w, y * w, z]);
            // shading also brightens slightly toward the tip so the point catches bloom
            let k = sh * (0.85 + 0.35 * u);
            bc.push([k, k, k, 1.0]);
        }
        let xe = SEC[2].0;
        for (dx, dy) in [(-0.0035f32, 0.0018f32), (0.0015, 0.0), (-0.0035, -0.0018)] {
            ep.push([bow + (xe + dx) * w, dy * w, z]);
        }
    }
    for i in 0..us.len() - 1 {
        let (a, b) = (i as u32 * 5, (i as u32 + 1) * 5);
        for k in 0..5u32 {
            let k2 = (k + 1) % 5;
            bi.extend_from_slice(&[a + k, b + k, b + k2, a + k, b + k2, a + k2]);
        }
        let (a, b) = (i as u32 * 3, (i as u32 + 1) * 3);
        for k in 0..2u32 {
            ei.extend_from_slice(&[a + k, b + k, b + k + 1, a + k, b + k + 1, a + k + 1]);
        }
    }
    let body = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, bp)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, bc)
        .with_inserted_indices(bevy::mesh::Indices::U32(bi));
    let edge = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, ep)
        .with_inserted_indices(bevy::mesh::Indices::U32(ei));
    (body, edge)
}

fn spawn(
    mut commands: Commands,
    shot: Option<Res<ShotImage>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    let pal = Palette::TRON;
    let layer = RenderLayers::layer(LAYER);
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let unlit = |c: shinobi_client::hud::Rgba, gain: f32| StandardMaterial {
        base_color: Color::linear_rgb(c.0 * gain, c.1 * gain, c.2 * gain),
        unlit: true,
        double_sided: true,
        cull_mode: None,
        ..default()
    };

    // overlay camera: draws on top of the world camera's output, clears only depth
    let mut cam = commands.spawn((
        Camera3d::default(),
        Camera { order: 1, clear_color: ClearColorConfig::None, ..default() },
        Hdr,
        Tonemapping::None,
        // no Bloom here: the overlay shares the world camera's HDR target, so a second bloom would re-blur the whole frame
        // view-model fov matches ViewCfg::fov_deg because the arc extents are derived from it
        Projection::Perspective(PerspectiveProjection { fov: 90f32.to_radians(), near: 0.01, far: 10.0, ..default() }),
        Transform::default(),
        layer.clone(),
    ));
    if let Some(img) = shot {
        cam.insert(RenderTarget::Image(img.0.clone().into()));
    }

    // katana: curved bevelled blade + neon edge strip, habaki, tsuba (guard), wrapped tsuka, kashira. One root
    // follows the katana pose; blade tip is at -blade_len, hand near the origin.
    let len = ViewCfg::default().blade_len;
    let (core, edge, guard) = (mats.add(unlit(pal.cyan, 1.2)), mats.add(unlit(pal.orange, 3.4)), mats.add(unlit(pal.magenta, 0.7)));
    let (trim, grip) = (mats.add(unlit(pal.cyan, 2.4)), mats.add(unlit(pal.magenta, 0.3)));
    let star = mats.add(unlit(pal.cyan, 3.0));
    let glow = vec![
        (core.clone(), pal.cyan, 1.2),
        (edge.clone(), pal.orange, 3.4),
        (guard.clone(), pal.magenta, 0.7),
        (trim.clone(), pal.cyan, 2.4),
        (grip.clone(), pal.magenta, 0.3),
        (star.clone(), pal.cyan, 3.0),
    ];
    let blade = commands.spawn((Transform::default(), Visibility::default(), layer.clone())).id();
    let (body_mesh, edge_mesh) = blade_meshes(len);
    let along_z = Quat::from_rotation_x(std::f32::consts::FRAC_PI_2); // Bevy cylinders/tori have their axis on Y
    let mut part = |mesh: Handle<Mesh>, mat: &Handle<StandardMaterial>, t: Transform| {
        commands.spawn((Mesh3d(mesh), MeshMaterial3d(mat.clone()), t, ChildOf(blade), layer.clone()));
    };
    part(meshes.add(body_mesh), &core, Transform::IDENTITY);
    part(meshes.add(edge_mesh), &edge, Transform::IDENTITY);
    // habaki (blade collar)
    part(meshes.add(Cuboid::new(0.030, 0.016, 0.045)), &edge, Transform::from_xyz(0.004, 0.0, -0.02));
    // tsuba: oval disc with a glowing rim
    part(meshes.add(Cylinder::new(0.052, 0.007)), &guard, Transform::from_xyz(0.0, 0.0, 0.006).with_rotation(along_z).with_scale(Vec3::new(1.0, 1.0, 0.8)));
    part(meshes.add(Torus::new(0.0485, 0.0535)), &trim, Transform::from_xyz(0.0, 0.0, 0.006).with_rotation(along_z).with_scale(Vec3::new(1.0, 1.0, 0.8)));
    // tsuka: dark core wrapped with alternating glowing bands, fuchi collar and kashira pommel
    part(meshes.add(Cylinder::new(0.0155, 0.27)), &grip, Transform::from_xyz(0.0, 0.0, 0.15).with_rotation(along_z));
    let wrap = meshes.add(Torus::new(0.0150, 0.0185));
    for i in 0..10 {
        let tilt = if i % 2 == 0 { 0.55 } else { -0.55 };
        part(wrap.clone(), &trim, Transform::from_xyz(0.0, 0.0, 0.034 + i as f32 * 0.026).with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2 + tilt)));
    }
    part(meshes.add(Cylinder::new(0.020, 0.014)), &edge, Transform::from_xyz(0.0, 0.0, 0.020).with_rotation(along_z));
    part(meshes.add(Sphere::new(0.021)), &trim, Transform::from_xyz(0.0, 0.0, 0.292).with_scale(Vec3::new(1.0, 1.0, 0.7)));

    // swing-arc ribbons: rebuilt every frame (additive, vertex-coloured)
    let ribbons_mesh = meshes.add(degenerate());
    let ribbon_mat = mats.add(StandardMaterial {
        base_color: Color::WHITE,
        unlit: true,
        alpha_mode: AlphaMode::Add,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    let ribbons = commands
        .spawn((Mesh3d(ribbons_mesh.clone()), MeshMaterial3d(ribbon_mat.clone()), Transform::default(), layer.clone(), NoFrustumCulling))
        .id();
    // speed lines: same additive vertex-colour material, own mesh (empty until moving fast)
    let streaks_mesh = meshes.add(degenerate());
    let streaks = commands
        .spawn((Mesh3d(streaks_mesh.clone()), MeshMaterial3d(ribbon_mat.clone()), Transform::default(), Visibility::Hidden, layer.clone(), NoFrustumCulling))
        .id();

    let shuriken = commands
        .spawn((
            Mesh3d(meshes.add(star_mesh())),
            MeshMaterial3d(star),
            Transform::from_translation(Vec3::from(ShurikenViewModel::REST)),
            layer.clone(),
        ))
        .id();

    // first-person shield: translucent panel + four hot frame bars, parented to one root that sits across the screen
    let shield_mat = mats.add(StandardMaterial { base_color: Color::linear_rgb(0.0, 0.5, 0.6), unlit: true, alpha_mode: AlphaMode::Add, double_sided: true, cull_mode: None, ..default() });
    let frame_mat = mats.add(unlit(pal.cyan, 3.5));
    let shield = commands.spawn((Transform::default(), Visibility::Hidden, layer.clone())).id();
    for (mat, p, sc) in [
        (shield_mat.clone(), Vec3::ZERO, Vec3::new(1.0, 0.7, 0.004)),
        (frame_mat.clone(), Vec3::new(0.0, 0.35, 0.0), Vec3::new(1.0, 0.012, 0.01)),
        (frame_mat.clone(), Vec3::new(0.0, -0.35, 0.0), Vec3::new(1.0, 0.012, 0.01)),
        (frame_mat.clone(), Vec3::new(0.5, 0.0, 0.0), Vec3::new(0.012, 0.7, 0.01)),
        (frame_mat.clone(), Vec3::new(-0.5, 0.0, 0.0), Vec3::new(0.012, 0.7, 0.01)),
    ] {
        commands.spawn((Mesh3d(cube.clone()), MeshMaterial3d(mat), Transform::from_translation(p).with_scale(sc), ChildOf(shield), layer.clone()));
    }

    // hands: the katana hand grips the hilt (glove + sleeve + lit bracer, parented to the blade so it swings with it); the
    // left hand holds the star; the wrist launcher sits where the rope leaves (`rope::hand_from`: right, below, ahead of the eye)
    let glove = mats.add(unlit(pal.cyan, 0.16));
    let sleeve = mats.add(unlit(pal.cyan, 0.08));
    let bracer = mats.add(unlit(pal.cyan, 1.8));
    let muzzle = mats.add(unlit(pal.orange, 3.8));
    let glow_extra = [(bracer.clone(), pal.cyan, 1.8), (muzzle.clone(), pal.orange, 3.8)];
    let hand_mesh = meshes.add(crate::character_look::hand_mesh());
    let hand = |commands: &mut Commands, parent: Entity, mesh: &Handle<Mesh>, p: [f32; 3], sc: [f32; 3], rot: Quat, m: &Handle<StandardMaterial>| {
        commands.spawn((Mesh3d(mesh.clone()), MeshMaterial3d(m.clone()), Transform { translation: Vec3::from(p), rotation: rot, scale: Vec3::from(sc) }, ChildOf(parent), layer.clone()));
    };
    let q = Quat::IDENTITY;
    // Right hand: the open hand mesh turned so the fingers wrap forward around the tsuka instead of a block fist.
    let grip_q = Quat::from_rotation_x(std::f32::consts::FRAC_PI_2) * Quat::from_rotation_z(0.12);
    hand(&mut commands, blade, &hand_mesh, [0.0, -0.006, 0.075], [0.72, 0.86, 1.08], grip_q, &glove);
    hand(&mut commands, blade, &cube, [0.02, -0.075, 0.33], [0.075, 0.075, 0.42], Quat::from_rotation_x(0.25), &sleeve); // forearm back toward the shoulder
    hand(&mut commands, blade, &cube, [0.02, -0.052, 0.17], [0.082, 0.082, 0.03], Quat::from_rotation_x(0.25), &bracer); // lit cuff
    let left_hand = commands.spawn((Transform::default(), Visibility::default(), layer.clone())).id();
    hand(&mut commands, left_hand, &hand_mesh, [0.0, -0.05, 0.0], [0.8, 0.8, 0.8], Quat::from_rotation_y(-0.25), &glove);
    hand(&mut commands, left_hand, &cube, [-0.05, -0.16, 0.2], [0.075, 0.075, 0.42], Quat::from_rotation_x(0.35) * Quat::from_rotation_y(-0.2), &sleeve);
    hand(&mut commands, left_hand, &cube, [-0.03, -0.11, 0.07], [0.082, 0.082, 0.03], Quat::from_rotation_x(0.35) * Quat::from_rotation_y(-0.2), &bracer);
    // blood on the blade and the gloves: hidden until a landed hit, then faded out over a few seconds
    let blood_mat = mats.add(StandardMaterial {
        base_color: Color::linear_rgba(0.42, 0.008, 0.012, 0.0),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    let blood_mesh = meshes.add(Sphere::new(0.5));
    let mut blood = Vec::new();
    for (parent, p, sc) in [
        (blade, [0.014, 0.002, -0.14], [0.030, 0.010, 0.060]),
        (blade, [0.018, -0.004, -0.30], [0.020, 0.008, 0.085]),
        (blade, [0.008, 0.004, -0.46], [0.014, 0.007, 0.045]),
        (blade, [0.010, -0.001, 0.078], [0.020, 0.013, 0.028]),
        (blade, [-0.006, -0.018, 0.092], [0.014, 0.010, 0.020]),
        (left_hand, [-0.020, -0.032, 0.012], [0.018, 0.012, 0.024]),
        (left_hand, [0.014, -0.064, 0.018], [0.013, 0.009, 0.018]),
    ] {
        blood.push(
            commands
                .spawn((
                    Mesh3d(blood_mesh.clone()),
                    MeshMaterial3d(blood_mat.clone()),
                    Transform::from_translation(Vec3::from(p)).with_scale(Vec3::from(sc)),
                    ChildOf(parent),
                    layer.clone(),
                    Visibility::Hidden,
                ))
                .id(),
        );
    }
    let launcher = commands.spawn((Transform::from_xyz(0.22, -0.2, -0.4), Visibility::default(), layer.clone())).id();
    hand(&mut commands, launcher, &cube, [0.0, 0.0, 0.04], [0.06, 0.04, 0.12], q, &glove);
    hand(&mut commands, launcher, &cube, [0.0, 0.022, 0.04], [0.026, 0.006, 0.1], q, &bracer);
    hand(&mut commands, launcher, &cube, [0.0, 0.0, -0.035], [0.024, 0.024, 0.03], q, &muzzle);
    let mut glow = glow;
    glow.extend(glow_extra);

    commands.insert_resource(VmEnts { blade, left_hand, launcher, glove, sleeve, blood, blood_mat, shield, shield_mat, frame_mat, ribbons_mesh, ribbons, streaks, streaks_mesh, streaks_mat: ribbon_mat, shuriken, glow });
}

/// A custom character recolours the first-person hands and weapons: gloves (or skin), sleeves (suit base), the lit
/// bracer and blade core (glow), blade edge, hilt (trim), star and muzzle. Stock looks keep the stock colours. Runs
/// only when the look changes.
pub fn apply_look(look: Res<crate::character_look::MyLook>, ents: Option<ResMut<VmEnts>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    use shinobi_client::hud::Rgba;
    use shinobi_proto::character::*;
    let Some(mut ents) = ents else { return };
    if !look.is_changed() {
        return;
    }
    let pal = Palette::TRON;
    let spec = look.0.as_ref();
    let rgba = |s: Option<[u8; 3]>| s.map(|c| Rgba(c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0));
    let towards_white = |c: Rgba, w: f32| Rgba(c.0 + (1.0 - c.0) * w, c.1 + (1.0 - c.1) * w, c.2 + (1.0 - c.2) * w, 1.0);
    let glow = spec.and_then(|s| rgba(s.rgb(C_GLOW)));
    let edge = spec.and_then(|s| rgba(s.rgb(C_EDGE))).map(|c| towards_white(c, 0.5));
    let trim = spec.and_then(|s| rgba(s.rgb(C_TRIM)));
    let star = spec.and_then(|s| rgba(s.rgb(C_STAR)));
    let muzzle = spec.and_then(|s| rgba(s.rgb(C_VISOR))).or(edge);
    // order of `VmEnts::glow`: blade core, edge, hilt, star, bracer, muzzle
    let stock = [pal.cyan, pal.orange, pal.magenta, pal.cyan, pal.cyan, pal.orange];
    let custom = [glow, edge, trim, star, glow, muzzle];
    for (i, entry) in ents.glow.iter_mut().enumerate().take(6) {
        entry.1 = custom[i].unwrap_or(stock[i]);
    }
    let set = |mats: &mut Assets<StandardMaterial>, h: &Handle<StandardMaterial>, c: Rgba, gain: f32| {
        if let Some(mut m) = mats.get_mut(h) {
            m.base_color = Color::linear_rgb(c.0 * gain, c.1 * gain, c.2 * gain);
        }
    };
    let glove = spec.and_then(|s| rgba(s.rgb(C_GLOVE).or_else(|| s.rgb(C_SKIN))));
    let sleeve = spec.and_then(|s| rgba(s.rgb(C_BASE)));
    set(&mut mats, &ents.glove, glove.unwrap_or(pal.cyan), if glove.is_some() { 1.0 } else { 0.16 });
    set(&mut mats, &ents.sleeve, sleeve.unwrap_or(pal.cyan), if sleeve.is_some() { 1.0 } else { 0.08 });
}

pub fn update(
    time: Res<Time>,
    view: Res<ViewRes>,
    net: Res<Net>,
    mut fx: ResMut<FxRes>,
    mut vm: ResMut<Vm>,
    local: Res<LocalColour>,
    mut impacts: ResMut<ImpactQueue>,
    gore: Option<Res<GoreCfg>>,
    ents: Res<VmEnts>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut tf: Query<&mut Transform>,
    mut vis: Query<&mut Visibility>,
) {
    let dt = time.delta_secs();
    // slash/throw edges come from the predicted sim (`LocalFx`); the CRT pass reads the same slash edge later in
    // this frame and `clear_edges` consumes it
    if fx.slash_edge {
        let (flip, lunge) = match fx.slash {
            Some(s) => (s.flip, s.lunge),
            None => {
                vm.flip = !vm.flip;
                (vm.flip, fx.dash_held)
            }
        };
        vm.katana.start_swing(flip, lunge);
    }
    if std::mem::take(&mut fx.fire_edge) {
        vm.recoil = RECOIL_S;
        vm.shuriken.throw_now();
    }
    // a landed hit (own hit message from the server) jolts and flares both held weapons and leaves blood on them
    let gore_on = gore.as_ref().is_some_and(|g| g.intensity > 0.0);
    for h in view.0.hits.iter().filter(|h| !h.taken) {
        vm.pulse.on_hit(h.kill);
        if gore_on {
            vm.blood = (vm.blood + if h.kill { 0.9 } else { 0.55 }).min(1.0);
        }
    }
    if !gore_on {
        vm.blood = 0.0;
    }
    vm.blood = (vm.blood - dt * 0.30).max(0.0);
    // katana feedback from the predicted sim: connect = flash + arc flare + hit-stop, whiff = dim arc
    let l = &view.0.fx;
    if let Some(h) = l.slash_hit {
        vm.pulse.on_hit(h.killed);
        vm.flare = FLARE_S;
        impacts.0.push(Impact { pos: Vec3::from(h.point), color: local.0, kind: ImpactKind::Slash { killed: h.killed } });
    }
    if l.slash_miss {
        vm.whiff = WHIFF_S;
    }
    vm.freeze = vm.freeze.max(l.hit_stop);
    // hit-stop freezes the held weapons (not the pulse/flash, which sells the impact); sim never pauses
    let step = if vm.freeze > 0.0 { 0.0 } else { dt };
    vm.freeze = (vm.freeze - dt).max(0.0);
    vm.flare = (vm.flare - dt).max(0.0);
    vm.whiff = (vm.whiff - dt).max(0.0);
    vm.recoil = (vm.recoil - dt).max(0.0);
    vm.katana.update(step);
    vm.shuriken.update(step);
    vm.pulse.update(dt);
    let pulse = vm.pulse.amount();
    for (m, c, g) in &ents.glow {
        if let Some(mut mat) = mats.get_mut(m) {
            let k = g * (1.0 + 2.5 * pulse);
            mat.base_color = Color::linear_rgb(c.0 * k, c.1 * k, c.2 * k);
        }
    }
    if let Some(mut mat) = mats.get_mut(&ents.blood_mat) {
        mat.base_color = Color::linear_rgba(0.42, 0.008, 0.012, 0.88 * vm.blood);
    }
    for e in &ents.blood {
        if let Ok(mut v) = vis.get_mut(*e) {
            v.set_if_neq(if vm.blood > 0.02 { Visibility::Inherited } else { Visibility::Hidden });
        }
    }

    // block: the blade swings up across the screen, a big shield panel rises in front of it, every absorbed hit flashes it
    // and a parry flashes it much bigger. SHOT_FX=guard forces the pose for offline review frames.
    let blocking = view.0.blocking;
    vm.guard += ((blocking as u32 as f32) - vm.guard) * (1.0 - (-16.0 * dt).exp());
    vm.guard_flash = (vm.guard_flash - dt * 2.4).max(0.0);
    for h in view.0.hits.iter().filter(|h| h.taken && h.other != 0) {
        if blocking {
            vm.guard_flash = vm.guard_flash.max(if h.amount == 0 { 1.0 } else { 0.55 });
        }
    }
    let g = vm.guard;
    if let Ok(mut sh) = tf.get_mut(ents.shield) {
        let pop = 1.0 + 0.12 * vm.guard_flash;
        sh.translation = Vec3::new(0.0, -0.02, -0.55);
        sh.rotation = Quat::IDENTITY;
        sh.scale = Vec3::new(0.62 * pop, 0.62 * pop, 1.0) * (0.55 + 0.45 * g);
    }
    if let Ok(mut sv) = vis.get_mut(ents.shield) {
        sv.set_if_neq(if g > 0.02 { Visibility::Inherited } else { Visibility::Hidden });
    }
    if let Some(mut m) = mats.get_mut(&ents.shield_mat) {
        let k = (0.07 + 0.015 * (time.elapsed_secs() * 9.0).sin() + 1.4 * vm.guard_flash) * g;
        m.base_color = Color::linear_rgb(local.0[0] * k, local.0[1] * k, local.0[2] * k);
    }
    if let Some(mut m) = mats.get_mut(&ents.frame_mat) {
        let k = (1.1 + 4.0 * vm.guard_flash) * g;
        m.base_color = Color::linear_rgb(local.0[0] * k, local.0[1] * k, local.0[2] * k);
    }

    let server_time = net.0.local_tick() / 60.0;
    let draw = katana_draw(&vm.katana, &Palette::TRON, server_time, 0.5 + 0.5 * (server_time as f32 * 7.85).sin());

    // blade pose = katana pose + weapon recoil/dip from the camera rig
    let pose = vm.katana.pose();
    let w = view.0.weapon;
    let t = pose.translation;
    let q = pose.rotation;
    if let Ok(mut b) = tf.get_mut(ents.blade) {
        // hit-stop: the frozen blade trembles (viewmodel only; the crosshair/aim is never moved)
        let shake = if vm.freeze > 0.0 { (time.elapsed_secs() * 140.0).sin() * 0.006 } else { 0.0 };
        let (rb, rp) = recoil_kick(vm.recoil);
        b.translation = Vec3::new(t.x + shake, t.y - w.y_dip + shake * 0.6, t.z + w.back - 0.06 * pulse + rb);
        b.rotation = Quat::from_rotation_x(w.pitch + rp) * Quat::from_xyzw(q.i, q.j, q.k, q.w);
        // guard pose: the blade lies across the lower screen, tip to the left, hilt in the right hand
        if g > 0.001 {
            let guard_t = Vec3::new(0.2, -0.2, -0.42);
            let guard_q = Quat::from_rotation_y(-1.35) * Quat::from_rotation_z(0.18);
            b.translation = b.translation.lerp(guard_t, g);
            b.rotation = b.rotation.slerp(guard_q, g);
        }
    }

    // speed lines (dash / grapple / fast running), smoothed so they ease in and out
    {
        // flow tier-up kick (combat::flow): a rise snaps the lines white-hot and a touch brighter for a moment,
        // so 2 -> 4 -> 6 reads as a punchy beat distinct from the steady climb the streak intensity already is
        if l.flow_tier > vm.prev_flow_tier {
            vm.tier_kick = TIER_KICK_S;
        }
        vm.prev_flow_tier = l.flow_tier;
        let kick = (vm.tier_kick / TIER_KICK_S).clamp(0.0, 1.0);
        vm.tier_kick = (vm.tier_kick - dt).max(0.0);

        let target = streak_target(l.dashing, l.grappling, l.grinding, l.speed, l.flow_tier);
        vm.streak += (target - vm.streak) * (1.0 - (-9.0 * dt).exp());
        let boosted = (vm.streak + 0.35 * kick).min(1.0);
        let rgb = [local.0[0] + (1.0 - local.0[0]) * kick, local.0[1] + (1.0 - local.0[1]) * kick, local.0[2] + (1.0 - local.0[2]) * kick];
        let (pos, col, idx) = streak_quads(time.elapsed_secs(), boosted, 16.0 / 9.0, 0.8, rgb);
        if let Some(mut m) = meshes.get_mut(&ents.streaks_mesh) {
            *m = if idx.is_empty() {
                degenerate()
            } else {
                Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
                    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
                    .with_inserted_indices(bevy::mesh::Indices::U32(idx))
            };
        }
        if let Ok(mut v) = vis.get_mut(ents.streaks) {
            v.set_if_neq(if vm.streak > 0.02 { Visibility::Inherited } else { Visibility::Hidden });
        }
    }

    // arc ribbons -> quads
    let gain = arc_gain(vm.flare, vm.whiff);
    let mut pos = Vec::<[f32; 3]>::new();
    let mut col = Vec::<[f32; 4]>::new();
    let mut idx = Vec::<u32>::new();
    for r in &draw.ribbons {
        let (a, b) = (Vec3::new(r.p0[0], r.p0[1], r.p0[2]), Vec3::new(r.p1[0], r.p1[1], r.p1[2]));
        let d = b - a;
        if d.length_squared() < 1e-10 {
            continue;
        }
        let n = Vec3::new(-d.y, d.x, 0.0).normalize_or(Vec3::Y);
        let c = [r.color[0] * gain.0, r.color[1] * gain.0, r.color[2] * gain.0, r.color[3] * gain.1];
        let base = pos.len() as u32;
        for (p, wid) in [(a, r.p0[3]), (b, r.p1[3])] {
            pos.push((p + n * wid * 0.5).to_array());
            pos.push((p - n * wid * 0.5).to_array());
            col.push(c);
            col.push(c);
        }
        idx.extend_from_slice(&[base, base + 1, base + 2, base + 1, base + 3, base + 2]);
    }
    // swept-blade sheet: quads between the tip trail and the inner (0.3 blade) trail, fading toward the hilt
    {
        let (mut tip, mut inner) = (Vec::new(), Vec::new());
        for sg in vm.katana.segments() {
            match sg.ribbon {
                0 => tip.push(sg),
                2 => inner.push(sg),
                _ => {}
            }
        }
        let pal = Palette::TRON;
        for (t, i) in tip.iter().zip(inner.iter()) {
            if (t.alpha - i.alpha).abs() > 0.05 {
                continue;
            }
            let v = |p: shinobi_sim::rapier3d::na::Vector3<f32>| [p.x, p.y, p.z];
            let base = pos.len() as u32;
            let (hot, cool) = (pal.orange, pal.magenta);
            pos.extend_from_slice(&[v(t.a), v(t.b), v(i.b), v(i.a)]);
            let ha = t.alpha * 0.3;
            col.extend_from_slice(&[
                [hot.0 * 1.6, hot.1 * 1.6, hot.2 * 1.6, ha],
                [hot.0 * 1.6, hot.1 * 1.6, hot.2 * 1.6, ha],
                [cool.0, cool.1, cool.2, 0.0],
                [cool.0, cool.1, cool.2, 0.0],
            ]);
            idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
    if let Some(mut m) = meshes.get_mut(&ents.ribbons_mesh) {
        *m = if pos.is_empty() {
            degenerate()
        } else {
            Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
                .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
                .with_inserted_indices(bevy::mesh::Indices::U32(idx))
        };
    }
    let _ = ents.ribbons;

    // shuriken
    let s = vm.shuriken.draw();
    if let Ok(mut st) = tf.get_mut(ents.shuriken) {
        st.translation = Vec3::new(s.pos[0], s.pos[1] - w.y_dip, s.pos[2] + w.back - 0.05 * pulse + recoil_kick(vm.recoil).0);
        st.rotation = Quat::from_rotation_x(w.pitch + s.tilt) * Quat::from_rotation_z(s.spin);
        st.scale = Vec3::splat((s.scale * (1.0 + 0.3 * pulse)).max(1e-4));
    }
    // left hand: holds the star at rest and follows it through the throw wind-up
    if let Ok(mut h) = tf.get_mut(ents.left_hand) {
        let rest = Vec3::from(ShurikenViewModel::REST);
        let held = if s.scale <= 0.001 { rest } else { Vec3::from(s.pos).lerp(rest, 0.5) };
        h.translation = Vec3::new(held.x, held.y - w.y_dip, held.z + w.back + recoil_kick(vm.recoil).0);
        h.rotation = Quat::from_rotation_x(w.pitch);
    }
    // the wrist launcher rides the weapon sway; it is the rope origin while a hook is out
    if let Ok(mut l) = tf.get_mut(ents.launcher) {
        let k = if view.0.rope.is_some() { 1.3 } else { 1.0 };
        l.translation = Vec3::new(0.22, -0.2 - w.y_dip, -0.42 + w.back);
        l.rotation = Quat::from_rotation_x(w.pitch);
        l.scale = Vec3::splat(k);
    }
    if let Ok(mut v) = vis.get_mut(ents.shuriken) {
        *v = if s.scale <= 0.001 { Visibility::Hidden } else { Visibility::Visible };
    }

    // the bow is the weapon in hand (key 2): no katana to hold alongside it, so the blade (and anything that only shows
    // on a swing) stays away the whole time the bow is out, not only while actually zoomed in
    let bow_out = view.0.power.is_some_and(|p| p.kind == shinobi_sim::power::PowerKind::Yumi && p.up);

    // the bow's zoom: the hands sink out of frame (the overlay camera keeps its own 90 deg FOV, so they would not match the
    // narrowed world anyway), and the swing arc and speed lines go with them
    let k = shinobi_client::viewmodel::zoom_fade(view.0.zoom.frac);
    let hidden = k < 0.02;
    if k < 1.0 {
        for e in [ents.blade, ents.shuriken, ents.left_hand, ents.launcher, ents.shield] {
            if let Ok(mut t) = tf.get_mut(e) {
                t.translation.y -= (1.0 - k) * 0.8;
            }
        }
    }
    for e in [ents.blade, ents.left_hand, ents.launcher, ents.ribbons] {
        if let Ok(mut v) = vis.get_mut(e) {
            v.set_if_neq(if hidden || (e == ents.blade && bow_out) || (e == ents.ribbons && (view.0.zoom.frac > 0.05 || bow_out)) {
                Visibility::Hidden
            } else {
                Visibility::Inherited
            });
        }
    }
    if hidden || view.0.zoom.frac > 0.05 {
        for e in [ents.streaks, ents.shield, ents.shuriken] {
            if let Ok(mut v) = vis.get_mut(e) {
                v.set_if_neq(Visibility::Hidden);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recoil_snaps_then_settles() {
        assert_eq!(recoil_kick(0.0), (0.0, 0.0));
        let (b, p) = recoil_kick(RECOIL_S);
        assert!(b > 0.04 && p > 0.05);
        assert!(recoil_kick(RECOIL_S * 0.5).0 < b, "eases out");
    }

    #[test]
    fn speed_lines_scale_with_intensity_and_stay_on_screen() {
        assert!(streak_quads(0.0, 0.0, 1.78, 0.8, [1.0; 3]).2.is_empty(), "off at rest");
        let few = streak_quads(1.0, 0.2, 1.78, 0.8, [1.0; 3]).2.len();
        let many = streak_quads(1.0, 1.0, 1.78, 0.8, [1.0; 3]).2.len();
        assert!(many > few * 2, "more lines when dashing: {few} vs {many}");
        let (pos, col, _) = streak_quads(2.0, 1.0, 1.78, 0.8, [1.0; 3]);
        assert!(pos.iter().all(|p| p[0].abs() < 1.45 && p[1].abs() < 0.82 && p[2] == -0.8), "inside the screen");
        assert!(col.iter().all(|c| (0.0..=1.0).contains(&c[3])));
        assert!(streak_target(false, false, true, 12.0, 0) >= 0.45 && streak_target(false, false, true, 15.0, 0) < 0.75, "a grind has lines of its own");
        assert!(streak_target(true, false, false, 0.0, 0) == 1.0 && streak_target(false, true, false, 0.0, 0) > 0.5);
        assert!(streak_target(false, false, false, 30.0, 0) > streak_target(false, false, false, 16.0, 0));
        assert_eq!(streak_target(false, false, false, 5.0, 0), 0.0);
        // flow raises the floor by tier, growing as the streak builds, even standing still
        assert!(streak_target(false, false, false, 0.0, 1) > 0.0);
        assert!(streak_target(false, false, false, 0.0, 2) > streak_target(false, false, false, 0.0, 1));
        assert!(streak_target(false, false, false, 0.0, 3) > streak_target(false, false, false, 0.0, 2));
        assert!(streak_target(true, false, false, 0.0, 3) == 1.0, "dash still wins outright");
    }

    #[test]
    fn arc_flares_on_connect_dims_on_whiff() {
        assert_eq!(arc_gain(0.0, 0.0), (1.0, 1.0));
        let (b, a) = arc_gain(FLARE_S, 0.0);
        assert!(b > 3.0 && a == 1.0, "connect brightens the arc");
        let (b, a) = arc_gain(0.0, WHIFF_S);
        assert!(b == 1.0 && a < 0.4, "whiff dims the arc");
        assert!(arc_gain(FLARE_S * 5.0, WHIFF_S * 5.0).0 <= 3.5, "gains clamp");
    }
}
