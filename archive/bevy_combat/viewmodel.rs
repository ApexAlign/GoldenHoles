//! First-person view model. Weapon + hands live in *view space* on a dedicated
//! overlay camera (render layer 1, own depth buffer) so they never clip into walls.
//! Katana slash = blade sweeps a diagonal across the whole screen, leaving neon
//! arc trails (also in view space), while the main camera kicks FOV for the dash.
use std::f32::consts::PI;

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use super::{fx::Fx, CombatAssets};

pub const VIEW_LAYER: usize = 1;
const BLADE_LEN: f32 = 0.9;
/// Distances along the blade where trail ribbons are emitted (tip → mid).
const TRAIL_RADII: [f32; 3] = [0.9, 0.65, 0.4];

/// Put on the player's main (world) camera. Weapons aim/fire from it.
#[derive(Component)]
pub struct FpsCamera;

#[derive(Component)]
pub struct ViewModelCamera;

#[derive(Component)]
pub struct KatanaViewModel;

#[derive(Event)]
pub struct SlashEvent {
    pub flip: bool,
}

#[derive(Component)]
pub struct SlashAnim {
    t: f32,
    dur: f32,
    /// +1: swing lower-right → upper-left, -1: mirrored.
    sign: f32,
    last: [Option<Vec3>; 3],
}

#[derive(Component)]
pub struct DashKick {
    t: f32,
    dur: f32,
    base_fov: f32,
}

fn rest_pose() -> Transform {
    Transform::from_xyz(0.32, -0.28, -0.55).with_rotation(Quat::from_euler(EulerRot::XYZ, 0.25, 0.15, -0.1))
}

/// Call once after spawning the world camera (which needs `FpsCamera`).
/// Mirror the world camera's `hdr`/`Bloom`/tonemapping on the overlay camera
/// if you use them, or the neon won't glow on the view model.
pub fn spawn_viewmodel(commands: &mut Commands, assets: &CombatAssets, fps_camera: Entity) {
    let layer = RenderLayers::layer(VIEW_LAYER);
    commands.entity(fps_camera).with_children(|p| {
        p.spawn((
            ViewModelCamera,
            Camera3d::default(),
            Camera { order: 1, hdr: true, clear_color: ClearColorConfig::None, ..default() },
            Projection::Perspective(PerspectiveProjection { fov: 60f32.to_radians(), near: 0.01, ..default() }),
            Transform::default(),
            layer.clone(),
        ))
        .with_children(|c| {
            c.spawn((KatanaViewModel, rest_pose(), Visibility::default())).with_children(|k| {
                k.spawn((
                    Mesh3d(assets.blade_mesh.clone()),
                    MeshMaterial3d(assets.shuriken_mat.clone()),
                    Transform::from_xyz(0.0, 0.0, -0.55),
                    layer.clone(),
                ));
                k.spawn((
                    Mesh3d(assets.grip_mesh.clone()),
                    MeshMaterial3d(assets.grip_mat.clone()),
                    Transform::from_xyz(0.0, 0.0, 0.12),
                    layer.clone(),
                ));
            });
        });
    });
}

pub fn start_slash(
    mut commands: Commands,
    mut events: EventReader<SlashEvent>,
    katana: Query<Entity, With<KatanaViewModel>>,
    cams: Query<(Entity, &Projection, Option<&DashKick>), With<FpsCamera>>,
) {
    for ev in events.read() {
        for k in &katana {
            commands.entity(k).insert(SlashAnim {
                t: 0.0,
                dur: 0.16,
                sign: if ev.flip { 1.0 } else { -1.0 },
                last: [None; 3],
            });
        }
        for (c, proj, kick) in &cams {
            if let Projection::Perspective(p) = proj {
                // keep the original FOV if a kick is already running
                let base_fov = kick.map_or(p.fov, |k| k.base_fov);
                commands.entity(c).insert(DashKick { t: 0.0, dur: 0.3, base_fov });
            }
        }
    }
}

/// Hand position in view space for swing progress `u` ∈ [0,1].
fn hand_path(u: f32, sign: f32) -> Vec3 {
    let e = 1.0 - (1.0 - u).powi(3); // ease-out: explosive start
    let start = Vec3::new(0.9 * sign, -0.55, -0.9);
    let end = Vec3::new(-0.9 * sign, 0.55, -0.9);
    start.lerp(end, e) + Vec3::new(0.0, 0.0, -0.35 * (PI * e).sin()) // bows forward mid-swing
}

pub fn animate_slash(
    mut commands: Commands,
    time: Res<Time>,
    assets: Option<Res<CombatAssets>>,
    mut katana: Query<(Entity, &mut Transform, &mut SlashAnim), With<KatanaViewModel>>,
    vcam: Query<Entity, With<ViewModelCamera>>,
) {
    let (Some(assets), Ok(cam)) = (assets, vcam.get_single()) else { return };
    for (e, mut tf, mut a) in &mut katana {
        a.t += time.delta_secs();
        let u = (a.t / a.dur).min(1.0);

        let hand = hand_path(u, a.sign);
        let tangent = hand_path((u + 0.02).min(1.0), a.sign) - hand_path((u - 0.02).max(0.0), a.sign);
        // blade stays perpendicular to travel, so the edge leads the cut
        let n = (a.sign * Vec3::new(tangent.y, -tangent.x, 0.0)).normalize_or_zero();
        let n = if n == Vec3::ZERO { Vec3::Y } else { n };
        tf.translation = hand;
        tf.rotation = Quat::from_rotation_arc(Vec3::NEG_Z, n);

        for (i, r) in TRAIL_RADII.iter().enumerate() {
            let p = hand + n * *r;
            if let Some(prev) = a.last[i] {
                let d = p - prev;
                let len = d.length();
                if len > 1e-4 {
                    let scale = Vec3::new(1.0, 1.0, len * 1.2); // slight overlap = no gaps
                    commands.entity(cam).with_children(|c| {
                        c.spawn((
                            Mesh3d(assets.trail_mesh.clone()),
                            MeshMaterial3d(assets.trail_mat.clone()),
                            Transform::from_translation((p + prev) * 0.5)
                                .with_rotation(Quat::from_rotation_arc(Vec3::Z, d / len))
                                .with_scale(scale),
                            Fx::with_scale(0.18, true, scale),
                            RenderLayers::layer(VIEW_LAYER),
                        ));
                    });
                }
            }
            a.last[i] = Some(p);
        }

        if u >= 1.0 {
            *tf = rest_pose();
            commands.entity(e).remove::<SlashAnim>();
        }
    }
}

/// FOV punch that sells the forward dash.
pub fn dash_kick(
    mut commands: Commands,
    time: Res<Time>,
    mut cams: Query<(Entity, &mut Projection, &mut DashKick)>,
) {
    for (e, mut proj, mut k) in &mut cams {
        k.t += time.delta_secs();
        let u = (k.t / k.dur).min(1.0);
        if let Projection::Perspective(p) = &mut *proj {
            p.fov = if u >= 1.0 { k.base_fov } else { k.base_fov + 0.35 * (PI * u.sqrt()).sin() };
        }
        if u >= 1.0 {
            commands.entity(e).remove::<DashKick>();
        }
    }
}
