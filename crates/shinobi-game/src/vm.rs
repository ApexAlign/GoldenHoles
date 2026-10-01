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
use shinobi_client::viewmodel_draw::{katana_draw, ShurikenViewModel};

use crate::net::{Net, ViewRes};
use crate::play::{FxRes, ShotImage};

const LAYER: usize = 1;

#[derive(Resource)]
pub struct Vm {
    pub katana: KatanaViewModel,
    pub shuriken: ShurikenViewModel,
    flip: bool,
}

#[derive(Resource)]
pub struct VmEnts {
    blade: Entity,
    ribbons_mesh: Handle<Mesh>,
    ribbons: Entity,
    shuriken: Entity,
}

pub struct VmPlugin;

impl Plugin for VmPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Vm { katana: KatanaViewModel::new(ViewCfg::default()), shuriken: ShurikenViewModel::default(), flip: false })
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
    let (outer, inner, n) = (0.075f32, 0.026f32, 8usize);
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

    // blade: core + hot edge + hilt, parented to one root that follows the katana pose
    let len = ViewCfg::default().blade_len;
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let core = mats.add(unlit(pal.cyan, 1.4));
    let edge = mats.add(unlit(pal.orange, 3.2));
    let hilt = mats.add(unlit(pal.magenta, 1.6));
    let blade = commands.spawn((Transform::default(), Visibility::default(), layer.clone())).id();
    for (mat, scale, t) in [
        (core, Vec3::new(0.045, 0.012, len), Vec3::new(0.0, 0.0, -len * 0.5)),
        (edge, Vec3::new(0.008, 0.014, len), Vec3::new(0.0265, 0.0, -len * 0.5)),
        (hilt, Vec3::new(0.034, 0.034, 0.15), Vec3::new(0.0, 0.0, 0.075)),
    ] {
        commands.spawn((
            Mesh3d(cube.clone()),
            MeshMaterial3d(mat),
            Transform::from_translation(t).with_scale(scale),
            ChildOf(blade),
            layer.clone(),
        ));
    }

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
            MeshMaterial3d(mats.add(unlit(pal.cyan, 2.4))),
            Transform::from_translation(Vec3::from(ShurikenViewModel::REST)),
            layer,
        ))
        .id();

    commands.insert_resource(VmEnts { blade, ribbons_mesh, ribbons, shuriken });
}

pub fn update(
    time: Res<Time>,
    view: Res<ViewRes>,
    net: Res<Net>,
    mut fx: ResMut<FxRes>,
    mut vm: ResMut<Vm>,
    ents: Res<VmEnts>,
    mut meshes: ResMut<Assets<Mesh>>,
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
    vm.katana.update(dt);
    vm.shuriken.update(dt);

    let server_time = net.0.local_tick() / 60.0;
    let draw = katana_draw(&vm.katana, &Palette::TRON, server_time, 0.5 + 0.5 * (server_time as f32 * 7.85).sin());

    // blade pose = katana pose + weapon recoil/dip from the camera rig
    let pose = vm.katana.pose();
    let w = view.0.weapon;
    let t = pose.translation;
    let q = pose.rotation;
    if let Ok(mut b) = tf.get_mut(ents.blade) {
        b.translation = Vec3::new(t.x, t.y - w.y_dip, t.z + w.back);
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
        st.translation = Vec3::from(s.pos);
        st.rotation = Quat::from_rotation_x(s.tilt) * Quat::from_rotation_z(s.spin);
        st.scale = Vec3::splat(s.scale.max(1e-4));
    }
    if let Ok(mut v) = vis.get_mut(ents.shuriken) {
        *v = if s.scale <= 0.001 { Visibility::Hidden } else { Visibility::Visible };
    }
}
