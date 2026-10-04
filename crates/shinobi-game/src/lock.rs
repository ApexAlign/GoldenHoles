//! Predator-style lock-on: three glowing laser dots in a triangle on the target's chest (world space, additive HDR,
//! in OUR neon colour). ACQUIRE: wide, slowly rotating dots converging as `progress` rises. LOCK: tight triangle that
//! snaps and pulses. BREAK: dots scatter outward and shrink away. While locked, a small shoulder emitter fires three
//! thin beams at the dots. Driven by `RenderView::lock` / `lock_edges`; cosmetic only.

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use shinobi_sim::combat::aim_assist::{LockEdge, LockPhase};

use crate::net::ViewRes;
use crate::team::LocalColour;

const SNAP_S: f32 = 0.22;

#[derive(Resource)]
struct LockViz {
    dots: [Entity; 3],
    /// Dark translucent discs behind the dots: contrast against pale bodies and bright walls.
    halos: [Entity; 3],
    edges: Entity,
    edge_mesh: Handle<Mesh>,
    emitter: Entity,
    beams: Entity,
    beam_mesh: Handle<Mesh>,
    mat: Handle<StandardMaterial>,
    /// Seconds left of the lock "snap" flare.
    snap: f32,
    /// Expanding pulse ring at the moment of lock.
    ring: Entity,
    ring_mat: Handle<StandardMaterial>,
    /// Seconds since the lock snapped (large = no ring).
    ring_t: f32,
    /// Render-side smoothed aim point / progress / target (the sim steps at 60 Hz; the eye does not).
    smooth_pos: Option<Vec3>,
    smooth_p: f32,
    last_target: Option<u16>,
}

pub struct LockPlugin;

impl Plugin for LockPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup).add_systems(Update, update);
    }
}

fn degenerate() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3])
        .with_inserted_indices(Indices::U32(vec![0, 1, 2]))
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let mat = mats.add(StandardMaterial { base_color: Color::WHITE, unlit: true, alpha_mode: AlphaMode::Add, double_sided: true, cull_mode: None, ..default() });
    let sphere = meshes.add(Sphere::new(0.5));
    let spawn = |commands: &mut Commands| {
        commands
            .spawn((Mesh3d(sphere.clone()), MeshMaterial3d(mat.clone()), Transform::default(), Visibility::Hidden, NoFrustumCulling))
            .id()
    };
    let dots = [spawn(&mut commands), spawn(&mut commands), spawn(&mut commands)];
    let halo_mat = mats.add(StandardMaterial { base_color: Color::srgba(0.0, 0.0, 0.03, 0.55), unlit: true, alpha_mode: AlphaMode::Blend, depth_bias: -100.0, ..default() });
    let halos: [Entity; 3] = std::array::from_fn(|_| {
        commands
            .spawn((Mesh3d(sphere.clone()), MeshMaterial3d(halo_mat.clone()), Transform::default(), Visibility::Hidden, NoFrustumCulling))
            .id()
    });
    let emitter = spawn(&mut commands);
    let beam_mesh = meshes.add(degenerate());
    let beams = commands
        .spawn((Mesh3d(beam_mesh.clone()), MeshMaterial3d(mat.clone()), Transform::default(), Visibility::Hidden, NoFrustumCulling))
        .id();
    let edge_mesh = meshes.add(degenerate());
    let edges = commands
        .spawn((Mesh3d(edge_mesh.clone()), MeshMaterial3d(mat.clone()), Transform::default(), Visibility::Hidden, NoFrustumCulling))
        .id();
    let ring_mat = mats.add(StandardMaterial { base_color: Color::BLACK, unlit: true, alpha_mode: AlphaMode::Add, double_sided: true, cull_mode: None, ..default() });
    let ring = commands
        .spawn((
            Mesh3d(meshes.add(Annulus::new(0.45, 0.5))),
            MeshMaterial3d(ring_mat.clone()),
            Transform::default(),
            Visibility::Hidden,
            NoFrustumCulling,
        ))
        .id();
    commands.insert_resource(LockViz {
        dots,
        halos,
        edges,
        edge_mesh,
        emitter,
        beams,
        beam_mesh,
        mat,
        snap: 0.0,
        ring,
        ring_mat,
        ring_t: 9.0,
        smooth_pos: None,
        smooth_p: 0.0,
        last_target: None,
    });
}

/// Triangle layout for one frame: (radius in metres on the chest, rotation angle, size multiplier).
/// `p` = lock progress 0..1, `t` = clock seconds, `snap` = seconds left of the lock-snap flare.
pub fn layout(phase: LockPhase, p: f32, t: f32, snap: f32) -> (f32, f32, f32) {
    let p = p.clamp(0.0, 1.0);
    match phase {
        LockPhase::Search => (0.0, 0.0, 0.0),
        LockPhase::Acquire => {
            // dots fly in from wide and spiral onto the target: ease-out (fast at first, settling softly)
            let e = 1.0 - (1.0 - p).powi(3);
            (1.1 + (0.12 - 1.1) * e, t * 1.2 + (1.0 - e) * 2.4, 1.0)
        }
        LockPhase::Locked => {
            let flare = (snap / SNAP_S).clamp(0.0, 1.0);
            (0.10 + 0.012 * (t * 22.0).sin() - 0.03 * flare, t * 0.4, 1.0 + 1.6 * flare)
        }
        // progress decays 1 -> 0 while breaking: scatter outward and shrink away
        LockPhase::Break => (0.12 + (1.0 - p).sqrt() * 1.8, t * 9.0, p.max(0.0)),
    }
}

/// Exponential smoothing toward `target`, frame-rate independent (`rate` per second; ~25 follows within a frame or two).
pub fn smooth_toward(cur: Vec3, target: Vec3, dt: f32, rate: f32) -> Vec3 {
    cur.lerp(target, 1.0 - (-rate * dt).exp())
}

/// The lock pulse ring `secs` after the snap: (progress 0..1 across its life, HDR gain). `None` once finished.
pub fn ring_state(secs: f32) -> Option<(f32, f32)> {
    const LIFE: f32 = 0.32;
    (secs >= 0.0 && secs < LIFE).then(|| {
        let u = secs / LIFE;
        (u, 9.0 * (1.0 - u) * (1.0 - u))
    })
}

/// The three dot offsets in the plane facing the viewer (`right`, `up` span that plane).
pub fn triangle(r: f32, angle: f32, right: Vec3, up: Vec3) -> [Vec3; 3] {
    std::array::from_fn(|i| {
        let a = angle + i as f32 * std::f32::consts::TAU / 3.0;
        (right * a.cos() + up * a.sin()) * r
    })
}

fn update(
    time: Res<Time>,
    view: Res<ViewRes>,
    colour: Res<LocalColour>,
    mut viz: ResMut<LockViz>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut tf: Query<(&mut Transform, &mut Visibility)>,
) {
    let t = time.elapsed_secs();
    viz.snap = (viz.snap - time.delta_secs()).max(0.0);
    if view.0.lock_edges.iter().any(|e| matches!(e, LockEdge::Locked)) {
        viz.snap = SNAP_S;
    }
    let dt = time.delta_secs();
    viz.ring_t += dt;
    if view.0.lock_edges.iter().any(|e| matches!(e, LockEdge::Locked)) {
        viz.ring_t = 0.0;
    }
    let hide = |viz: &LockViz, tf: &mut Query<(&mut Transform, &mut Visibility)>| {
        for e in viz.dots.iter().chain(viz.halos.iter()).chain([&viz.emitter, &viz.beams, &viz.edges, &viz.ring]) {
            if let Ok((_, mut v)) = tf.get_mut(*e) {
                *v = Visibility::Hidden;
            }
        }
    };
    let Some(lock) = view.0.lock.filter(|l| l.phase != LockPhase::Search) else {
        hide(&viz, &mut tf);
        viz.smooth_pos = None;
        viz.last_target = None;
        return;
    };
    // render-side smoothing: the sim steps at 60 Hz, the picture should not
    let raw = Vec3::from(lock.pos);
    let smoothed = match (viz.smooth_pos, viz.last_target == Some(lock.target)) {
        (Some(cur), true) => smooth_toward(cur + Vec3::from(lock.target_vel) * dt, raw, dt, 28.0), // lead along the target velocity
        _ => raw, // new target: start on it (no sweep across the map)
    };
    viz.smooth_pos = Some(smoothed);
    viz.last_target = Some(lock.target);
    viz.smooth_p = match lock.phase {
        LockPhase::Locked => 1.0,
        LockPhase::Break => lock.progress,
        _ => viz.smooth_p + (lock.progress - viz.smooth_p) * (1.0 - (-18.0 * dt).exp()),
    };
    let lock_progress = viz.smooth_p;

    let eye = Vec3::from(view.0.eye);
    // the triangle floats just in front of the chest surface (the aim point is the body centre: dots inside the torso
    // would be hidden by the depth test)
    let pos = smoothed;
    let to_eye = (eye - pos).normalize_or(Vec3::Z);
    let pos = pos + to_eye * 0.35;
    let right = to_eye.cross(Vec3::Y).normalize_or(Vec3::X);
    let up = right.cross(to_eye).normalize_or(Vec3::Y);
    let dist = eye.distance(pos);
    let (r, angle, mult) = layout(lock.phase, if lock.phase == LockPhase::Break { (1.0 - lock.t_since_phase / 0.25).clamp(0.0, 1.0) } else { lock_progress }, t, viz.snap);
    // screen-constant size: layout metres are scaled so the locked triangle is ~15 px in radius and each dot ~6 px across
    // at any range (1080p-ish: 270 px per metre at 1 m, / dist)
    let d = dist.clamp(2.0, 80.0);
    let k = 0.463 * d;
    let size = 0.0222 * d * mult;
    let r = r * k;
    // HDR owner colour; brighter once locked
    let g = if lock.phase == LockPhase::Locked { 4.5 } else { 3.0 };
    let c = colour.0;
    if let Some(mut m) = mats.get_mut(&viz.mat) {
        m.base_color = Color::linear_rgb(c[0] * g, c[1] * g, c[2] * g);
    }

    let dots = triangle(r, angle, right, up);
    for (e, d) in viz.dots.iter().zip(dots) {
        if let Ok((mut t, mut v)) = tf.get_mut(*e) {
            *t = Transform::from_translation(pos + d).with_scale(Vec3::splat(size.max(0.0005)));
            *v = Visibility::Visible;
        }
    }
    // dark halo just behind each dot (toward the target), 2.4x wider
    for (e, d) in viz.halos.iter().zip(dots) {
        if let Ok((mut t, mut v)) = tf.get_mut(*e) {
            *t = Transform::from_translation(pos + d - to_eye * 0.02).with_scale(Vec3::splat((size * 2.2).max(0.001)));
            *v = Visibility::Visible;
        }
    }

    // faint triangle edges joining the dots (brighter once locked), camera-facing thin quads
    {
        let (mut pv, mut cv, mut iv) = (Vec::new(), Vec::new(), Vec::new());
        let al = if lock.phase == LockPhase::Locked { 0.7 } else { 0.35 } * mult.min(1.0) * if lock.phase == LockPhase::Break { mult * mult } else { 1.0 };
        for i in 0..3 {
            let (a, b) = (pos + dots[i], pos + dots[(i + 1) % 3]);
            let side = (b - a).cross(to_eye).normalize_or(Vec3::Y) * (0.003 * d);
            let base = pv.len() as u32;
            for p in [a, b] {
                pv.push((p + side).to_array());
                pv.push((p - side).to_array());
                cv.extend([[c[0] * 4.0, c[1] * 4.0, c[2] * 4.0, al]; 2]);
            }
            iv.extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
        }
        if let Some(mut m) = meshes.get_mut(&viz.edge_mesh) {
            *m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pv)
                .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, cv)
                .with_inserted_indices(Indices::U32(iv));
        }
        if let Ok((_, mut v)) = tf.get_mut(viz.edges) {
            *v = if mult > 0.01 { Visibility::Visible } else { Visibility::Hidden };
        }
    }

    // lock pulse ring: expands from the triangle outward and fades, facing the camera
    {
        // timing comes from the lock state itself (`t_since_phase`); the edge-driven clock is the fallback
        let since = if lock.t_since_phase > 0.0 || viz.ring_t > 1.0 { lock.t_since_phase } else { viz.ring_t };
        let ring = ring_state(since).filter(|_| lock.phase == LockPhase::Locked);
        if let Some((u, gain)) = ring {
            let r_px = 15.0 + 70.0 * u;
            let diameter = 2.0 * r_px * d / 270.0;
            if let Some(mut m) = mats.get_mut(&viz.ring_mat) {
                m.base_color = Color::linear_rgb(c[0] * gain, c[1] * gain, c[2] * gain);
            }
            if let Ok((mut rt, mut v)) = tf.get_mut(viz.ring) {
                *rt = Transform::from_translation(pos).with_rotation(Quat::from_rotation_arc(Vec3::Z, to_eye)).with_scale(Vec3::splat(diameter));
                *v = Visibility::Visible;
            }
        } else if let Ok((_, mut v)) = tf.get_mut(viz.ring) {
            *v = Visibility::Hidden;
        }
    }

    // shoulder emitter + beams only while fully locked
    let beams_on = lock.phase == LockPhase::Locked;
    let shoulder = {
        let fwd = (pos - eye).normalize_or(Vec3::NEG_Z);
        let side = fwd.cross(Vec3::Y).normalize_or(Vec3::X);
        eye + side * 0.38 - Vec3::Y * 0.12 + fwd * 0.25
    };
    if let Ok((mut et, mut v)) = tf.get_mut(viz.emitter) {
        *et = Transform::from_translation(shoulder).with_scale(Vec3::splat(0.012 * (1.0 + 0.25 * (t * 14.0).sin())));
        *v = if beams_on { Visibility::Visible } else { Visibility::Hidden };
    }
    if beams_on {
        let (mut pos_v, mut col, mut idx) = (Vec::new(), Vec::new(), Vec::new());
        for d in dots {
            let (a, b) = (shoulder, pos + d);
            let dir = (b - a).normalize_or(Vec3::NEG_Z);
            let side = dir.cross(Vec3::Y).normalize_or(Vec3::X);
            let w = 0.006;
            let base = pos_v.len() as u32;
            for (p, al) in [(a, 0.15), (b, 0.55)] {
                pos_v.push((p + side * w).to_array());
                pos_v.push((p - side * w).to_array());
                let cc = [c[0] * 3.0, c[1] * 3.0, c[2] * 3.0, al];
                col.extend([cc, cc]);
            }
            idx.extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
        }
        // tether: a thin line from just in front of the crosshair to the target (foreshortened: crosshair -> target on screen)
        {
            let dir = (pos - eye).normalize_or(Vec3::NEG_Z);
            let a = eye + dir * 0.7;
            let b = pos;
            let side = dir.cross(Vec3::Y).normalize_or(Vec3::X) * 0.0025 * d.max(1.0);
            let base = pos_v.len() as u32;
            for (p, al) in [(a, 0.0), (b, 0.35)] {
                pos_v.push((p + side).to_array());
                pos_v.push((p - side).to_array());
                let cc = [c[0] * 2.5, c[1] * 2.5, c[2] * 2.5, al];
                col.extend([cc, cc]);
            }
            idx.extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
        }
        if let Some(mut m) = meshes.get_mut(&viz.beam_mesh) {
            *m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos_v)
                .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
                .with_inserted_indices(Indices::U32(idx));
        }
    }
    if let Ok((_, mut v)) = tf.get_mut(viz.beams) {
        *v = if beams_on { Visibility::Visible } else { Visibility::Hidden };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_converges_as_progress_rises() {
        let r = |p| layout(LockPhase::Acquire, p, 0.0, 0.0).0;
        assert!(r(0.0) > r(0.5) && r(0.5) > r(1.0));
        assert!((r(1.0) - 0.12).abs() < 1e-5);
    }

    #[test]
    fn lock_is_tight_and_snap_flares() {
        let (r, _, s) = layout(LockPhase::Locked, 1.0, 0.0, 0.0);
        let (rs, _, ss) = layout(LockPhase::Locked, 1.0, 0.0, SNAP_S);
        assert!(r < 0.15 && rs < r, "snap tightens further");
        assert!(ss > s, "snap enlarges the dots");
    }

    #[test]
    fn break_scatters_and_fades_as_progress_decays() {
        let at = |p| layout(LockPhase::Break, p, 0.0, 0.0);
        assert!(at(0.2).0 > at(0.9).0, "radius grows as progress falls");
        assert!(at(0.2).2 < at(0.9).2, "dots shrink away");
        assert_eq!(layout(LockPhase::Search, 0.0, 0.0, 0.0).2, 0.0);
    }

    #[test]
    fn acquire_is_ease_out_fast_at_first_and_settles() {
        let r = |p| layout(LockPhase::Acquire, p, 0.0, 0.0).0;
        // the first third of the progress covers more than half of the distance travelled
        let total = r(0.0) - r(1.0);
        assert!((r(0.0) - r(0.33)) > 0.5 * total);
        assert!(r(0.0) > 1.0, "dots start wide");
    }

    #[test]
    fn smoothing_converges_and_is_frame_rate_independent() {
        let (a, b) = (Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0));
        let one = smooth_toward(a, b, 1.0 / 30.0, 25.0);
        let two = smooth_toward(smooth_toward(a, b, 1.0 / 60.0, 25.0), b, 1.0 / 60.0, 25.0);
        assert!((one - two).length() < 1e-4, "two 60 Hz steps == one 30 Hz step");
        assert!(smooth_toward(a, b, 1.0, 25.0).x > 9.99);
        assert_eq!(smooth_toward(a, b, 0.0, 25.0), a);
    }

    #[test]
    fn lock_ring_lives_a_third_of_a_second_and_fades() {
        assert!(ring_state(-0.1).is_none());
        let (u0, g0) = ring_state(0.0).unwrap();
        let (u1, g1) = ring_state(0.2).unwrap();
        assert!(u1 > u0 && g1 < g0);
        assert!(ring_state(0.33).is_none());
    }

    #[test]
    fn triangle_is_equilateral_in_the_view_plane() {
        let d = triangle(1.0, 0.3, Vec3::X, Vec3::Y);
        let (a, b, c) = ((d[0] - d[1]).length(), (d[1] - d[2]).length(), (d[2] - d[0]).length());
        assert!((a - b).abs() < 1e-5 && (b - c).abs() < 1e-5);
        assert!(d.iter().all(|p| p.z.abs() < 1e-6));
    }
}
