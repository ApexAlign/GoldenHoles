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

use crate::impacts::{Impact, ImpactKind, ImpactQueue};
use crate::net::{Net, ViewRes};
use crate::team::LocalColour;
use crate::play::{FxRes, ShotImage};

const LAYER: usize = 1;
const FLARE_S: f32 = 0.2;
const WHIFF_S: f32 = 0.3;
const RECOIL_S: f32 = 0.18;

/// Shoulder recoil of the held weapons after a throw: (kick back in metres, kick up in radians), snapping in and
/// easing out over `RECOIL_S`.
pub fn recoil_kick(left: f32) -> (f32, f32) {
    let k = (left / RECOIL_S).clamp(0.0, 1.0);
    let e = k * k;
    (0.05 * e, 0.06 * e)
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
        app.insert_resource(Vm { katana: KatanaViewModel::new(ViewCfg::default()), shuriken: ShurikenViewModel::default(), pulse: HitPulse::default(), flip: false, freeze: 0.0, whiff: 0.0, flare: 0.0, recoil: 0.0 })
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
    let (core, edge, hilt) = (mats.add(unlit(pal.cyan, 1.4)), mats.add(unlit(pal.orange, 3.2)), mats.add(unlit(pal.magenta, 1.6)));
    let star = mats.add(unlit(pal.cyan, 3.0));
    let glow = vec![(core.clone(), pal.cyan, 1.4), (edge.clone(), pal.orange, 3.2), (hilt.clone(), pal.magenta, 1.6), (star.clone(), pal.cyan, 3.0)];
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
    local: Res<LocalColour>,
    mut impacts: ResMut<ImpactQueue>,
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
    // a landed hit (own hit message from the server) jolts and flares both held weapons
    for h in view.0.hits.iter().filter(|h| !h.taken) {
        vm.pulse.on_hit(h.kill);
    }
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
    if let Ok(mut v) = vis.get_mut(ents.shuriken) {
        *v = if s.scale <= 0.001 { Visibility::Hidden } else { Visibility::Visible };
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
    fn arc_flares_on_connect_dims_on_whiff() {
        assert_eq!(arc_gain(0.0, 0.0), (1.0, 1.0));
        let (b, a) = arc_gain(FLARE_S, 0.0);
        assert!(b > 3.0 && a == 1.0, "connect brightens the arc");
        let (b, a) = arc_gain(0.0, WHIFF_S);
        assert!(b == 1.0 && a < 0.4, "whiff dims the arc");
        assert!(arc_gain(FLARE_S * 5.0, WHIFF_S * 5.0).0 <= 3.5, "gains clamp");
    }
}
