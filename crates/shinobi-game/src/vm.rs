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

use crate::net::{Net, ViewRes};
use crate::play::{FxRes, ShotImage};

const LAYER: usize = 1;

#[derive(Resource)]
pub struct Vm {
    pub katana: KatanaViewModel,
    pub shuriken: ShurikenViewModel,
    pub pulse: HitPulse,
    flip: bool,
}

#[derive(Resource)]
pub struct VmEnts {
    blade: Entity,
    ribbons_mesh: Handle<Mesh>,
    ribbons: Entity,
    shuriken: Entity,
    /// (material, base colour, gain) flared by hit confirmation
    glow: Vec<(Handle<StandardMaterial>, shinobi_client::hud::Rgba, f32)>,
}

pub struct VmPlugin;

impl Plugin for VmPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Vm { katana: KatanaViewModel::new(ViewCfg::default()), shuriken: ShurikenViewModel::default(), pulse: HitPulse::default(), flip: false })
            .add_systems(Startup, spawn.after(crate::play::setup));
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
    let (outer, inner, n) = (0.14f32, 0.05f32, 8usize);
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
        .spawn((Mesh3d(ribbons_mesh.clone()), MeshMaterial3d(ribbon_mat), Transform::default(), layer.clone(), NoFrustumCulling))
        .id();

    let shuriken = commands
        .spawn((
            Mesh3d(meshes.add(star_mesh())),
            MeshMaterial3d(star),
            Transform::from_translation(Vec3::from(ShurikenViewModel::REST)),
            layer,
        ))
        .id();

    commands.insert_resource(VmEnts { blade, ribbons_mesh, ribbons, shuriken, glow });
}

pub fn update(
    time: Res<Time>,
    view: Res<ViewRes>,
    net: Res<Net>,
    mut fx: ResMut<FxRes>,
    mut vm: ResMut<Vm>,
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
        vm.shuriken.throw_now();
    }
    // a landed hit (own hit message from the server) jolts and flares both held weapons
    for h in view.0.hits.iter().filter(|h| !h.taken) {
        vm.pulse.on_hit(h.kill);
    }
    vm.katana.update(dt);
    vm.shuriken.update(dt);
    vm.pulse.update(dt);
    let pulse = vm.pulse.amount();
    for (m, c, g) in &ents.glow {
        if let Some(mut mat) = mats.get_mut(m) {
            let k = g * (1.0 + 2.5 * pulse);
            mat.base_color = Color::linear_rgb(c.0 * k, c.1 * k, c.2 * k);
        }
    }

    let server_time = net.0.local_tick() / 60.0;
    let draw = katana_draw(&vm.katana, &Palette::TRON, server_time, 0.5 + 0.5 * (server_time as f32 * 7.85).sin());

    // blade pose = katana pose + weapon recoil/dip from the camera rig
    let pose = vm.katana.pose();
    let w = view.0.weapon;
    let t = pose.translation;
    let q = pose.rotation;
    if let Ok(mut b) = tf.get_mut(ents.blade) {
        b.translation = Vec3::new(t.x, t.y - w.y_dip, t.z + w.back - 0.06 * pulse);
        b.rotation = Quat::from_rotation_x(w.pitch) * Quat::from_xyzw(q.i, q.j, q.k, q.w);
    }

    // arc ribbons -> quads
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
        let base = pos.len() as u32;
        for (p, wid) in [(a, r.p0[3]), (b, r.p1[3])] {
            pos.push((p + n * wid * 0.5).to_array());
            pos.push((p - n * wid * 0.5).to_array());
            col.push(r.color);
            col.push(r.color);
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
        st.translation = Vec3::new(s.pos[0], s.pos[1] - w.y_dip, s.pos[2] + w.back - 0.05 * pulse);
        st.rotation = Quat::from_rotation_x(w.pitch + s.tilt) * Quat::from_rotation_z(s.spin);
        st.scale = Vec3::splat((s.scale * (1.0 + 0.3 * pulse)).max(1e-4));
    }
    if let Ok(mut v) = vis.get_mut(ents.shuriken) {
        *v = if s.scale <= 0.001 { Visibility::Hidden } else { Visibility::Visible };
    }
}
