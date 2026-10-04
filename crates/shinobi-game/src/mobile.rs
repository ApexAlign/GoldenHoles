//! Mobile-facing pieces: quality tier resource, frame-time driven dynamic resolution, and touch input.
//!
//! Dynamic resolution: when the tier enables it the world camera renders into a scaled image (bloom and CRT run
//! at that size) and a presenter camera draws it upscaled into the real target. The viewmodel and HUD cameras
//! draw afterwards at native resolution, so the vector HUD stays sharp.

use bevy::camera::visibility::RenderLayers;
use bevy::camera::{RenderTarget, ScalingMode, Viewport};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::input::touch::{TouchInput, TouchPhase};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureFormat};

use shinobi_client::fx::{CombatLoad, DynRes};
use shinobi_client::input::TouchEvent;
use shinobi_client::quality::Quality;
use shinobi_client::touch_session::TouchSession;

use crate::net::{PendingInput, ViewRes};
use crate::play::ShotRequest;

#[derive(Resource, Clone, Copy)]
pub struct QualityRes(pub Quality);

/// Automatic quality: pick the starting tier from the GPU class, then step down while the frame time p95 stays over
/// the 60 fps budget. Off when `--tier` is given explicitly (and in shots, which must be deterministic).
#[derive(Resource)]
pub struct AutoTier {
    pub on: bool,
    gov: shinobi_client::quality::FrameGovernor,
}

impl AutoTier {
    pub fn new(on: bool) -> Self {
        let mut gov = shinobi_client::quality::FrameGovernor::new();
        gov.warmup(6.0);
        Self { on, gov }
    }
}

/// PreStartup: log the adapter and, in auto mode, choose the starting tier from its device type, before `play::setup`
/// builds the cameras from `QualityRes`.
fn pick_tier(
    info: Option<Res<bevy::render::renderer::RenderAdapterInfo>>,
    auto: Res<AutoTier>,
    mut q: ResMut<QualityRes>,
) {
    use shinobi_client::quality::{DeviceKind, Tier};
    let Some(info) = info else { return };
    let mut kind = DeviceKind::from_wgpu_name(&format!("{:?}", info.device_type));
    // SHINOBI_DEVICE=discrete|integrated|software overrides the detected class (testing, odd drivers)
    match std::env::var("SHINOBI_DEVICE").ok().as_deref() {
        Some("discrete") => kind = DeviceKind::Discrete,
        Some("integrated") => kind = DeviceKind::Integrated,
        Some("software") => kind = DeviceKind::Software,
        _ => {}
    }
    let how = if auto.on {
        let t = Tier::for_device(kind);
        q.0 = Quality::from_tier(t);
        "auto"
    } else {
        "forced"
    };
    eprintln!("GPU: {} | {:?} | {:?} | driver {} -> quality tier {:?} ({how})", info.name, info.device_type, info.backend, info.driver, q.0.tier);
    if kind == DeviceKind::Software {
        eprintln!("GPU: SOFTWARE rasteriser: no hardware GPU was selected (check drivers, VK_ICD_FILENAMES, SHINOBI_GPU=igpu|dgpu)");
    }
}

/// Update: drop a tier when p95 frame time has been over budget for 2 s. Live only where the cheaper tier needs no
/// new render targets: Medium -> Low (both already render through the scaled scene image). A slow High tier just says so.
fn govern(
    time: Res<Time<Real>>,
    mut auto: ResMut<AutoTier>,
    mut q: ResMut<QualityRes>,
    ds: Option<ResMut<DynScale>>,
    cam: Query<Entity, With<crate::play::PlayerCam>>,
    mut commands: Commands,
) {
    if !auto.on {
        return;
    }
    let Some(p95) = auto.gov.push(time.delta_secs()) else { return };
    let cur = q.0.tier;
    let Some(next) = cur.lower() else { return };
    match (cur, ds) {
        (shinobi_client::quality::Tier::Medium, Some(mut ds)) => {
            let low = Quality::from_tier(next);
            ds.dr = DynRes::new(low.dyn_res.expect("low tier has dynamic resolution"));
            if let Ok(c) = cam.single() {
                commands.entity(c).remove::<bevy::post_process::bloom::Bloom>().remove::<crate::crt::CrtEffect>();
            }
            q.0 = low;
            eprintln!("QUALITY: p95 frame {p95:.1} ms > 16.6 -> tier {cur:?} -> {next:?}");
        }
        _ => eprintln!("QUALITY: p95 frame {p95:.1} ms > 16.6 on tier {cur:?}; cannot drop live from here: restart with --tier {}", format!("{next:?}").to_lowercase()),
    }
}

const PRESENT_LAYER: usize = 3;

/// Present path state (only exists when the tier enables dynamic resolution).
#[derive(Resource)]
pub struct DynScale {
    pub dr: DynRes,
    pub forced: Option<f32>,
    scene: Handle<Image>,
    /// Presenter material: its `uv_transform` crops the scene image to the rendered sub-rect.
    mat: Handle<StandardMaterial>,
    /// Allocated size of the scene image (the native window size; only changes when the window is resized).
    alloc: (u32, u32),
    /// Rendered size inside it (native * scale): the world camera's viewport.
    cur: (u32, u32),
}

/// A render target image the scene camera draws into (sRGB 8-bit, matches the final target format).
pub fn scene_image(w: u32, h: u32) -> Image {
    Image::new_target_texture(w.max(8), h.max(8), TextureFormat::Rgba8UnormSrgb, None)
}

/// Spawns the presenter (fullscreen textured quad + ortho camera, order 0) and the `DynScale` resource.
pub fn spawn_present(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    mats: &mut Assets<StandardMaterial>,
    scene: Handle<Image>,
    native: (u32, u32),
    final_target: Option<Handle<Image>>,
    q: &Quality,
    forced: Option<f32>,
) {
    let layer = RenderLayers::layer(PRESENT_LAYER);
    let mat = mats.add(StandardMaterial { base_color_texture: Some(scene.clone()), unlit: true, ..default() });
    commands.spawn((
        Mesh3d(meshes.add(Rectangle::new(1.0, 1.0))),
        MeshMaterial3d(mat.clone()),
        Transform::from_xyz(0.5, 0.5, 0.0),
        layer.clone(),
    ));
    let mut cam = commands.spawn((
        Camera3d::default(),
        Camera { order: 0, ..default() },
        bevy::camera::Hdr,
        Tonemapping::None,
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::Fixed { width: 1.0, height: 1.0 },
            near: -10.0,
            far: 10.0,
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(0.5, 0.5, 5.0),
        layer,
    ));
    if let Some(t) = final_target {
        cam.insert(RenderTarget::Image(t.into()));
    }
    let mut dr = DynRes::new(q.dyn_res.expect("spawn_present needs a dyn_res tier"));
    if let Some(f) = forced {
        dr.force_scale(f);
    }
    commands.insert_resource(DynScale { dr, forced, scene, mat, alloc: native, cur: native });
}

/// Frame-time -> scale -> rendered sub-rect. The scene image stays allocated at the native window size; changing the
/// scale only moves the world camera's viewport and the presenter's UV crop, so there is no render-target
/// reallocation per scale step (resizing the image on consecutive frames left the scene black, and a texture
/// reallocation is exactly the kind of hitch dynamic resolution is meant to avoid).
pub fn update_scale(
    time: Res<Time>,
    view: Res<ViewRes>,
    window: Option<Single<&Window>>,
    shot: Option<Res<ShotRequest>>,
    ds: Option<ResMut<DynScale>>,
    mut images: ResMut<Assets<Image>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut cam: Query<&mut Camera, With<crate::play::PlayerCam>>,
) {
    let Some(mut ds) = ds else { return };
    let native = match (&window, &shot) {
        (Some(w), _) => (w.physical_width(), w.physical_height()),
        (None, Some(r)) => (r.width, r.height),
        _ => return,
    };
    match ds.forced {
        Some(f) => ds.dr.force_scale(f),
        None => {
            let load = CombatLoad { visible_enemies: view.0.remotes.len() as u32, projectiles: 0 };
            ds.dr.update(time.delta_secs() * 1000.0, load);
        }
    }
    // window resized: reallocate the scene image (rare)
    if native != ds.alloc && native.0 > 0 && native.1 > 0 {
        if let Some(mut img) = images.get_mut(&ds.scene) {
            img.resize(Extent3d { width: native.0, height: native.1, depth_or_array_layers: 1 });
            ds.alloc = native;
            ds.cur = (0, 0); // force the viewport / crop below to refresh
        }
    }
    let size = ds.dr.scene_size(ds.alloc);
    if size != ds.cur {
        if std::env::var("SHINOBI_DEBUG_SCALE").is_ok() {
            eprintln!("SCALE {:?} -> {:?} (alloc {:?}, scale {:.2})", ds.cur, size, ds.alloc, ds.dr.scale());
        }
        if let Ok(mut c) = cam.single_mut() {
            c.viewport = Some(Viewport { physical_position: UVec2::ZERO, physical_size: UVec2::new(size.0, size.1), ..default() });
        }
        if let Some(mut m) = mats.get_mut(&ds.mat) {
            m.uv_transform = bevy::math::Affine2::from_scale(Vec2::new(size.0 as f32 / ds.alloc.0 as f32, size.1 as f32 / ds.alloc.1 as f32));
        }
        ds.cur = size;
    }
}

// ---------------------------------------------------------------------------------------------------------

#[derive(Resource)]
pub struct TouchRes {
    pub s: TouchSession,
    /// Draw the overlay: set by `--touch` or once any touch event has been seen.
    pub visible: bool,
}

/// bevy touch messages -> `TouchSession` -> buttons/stick/look in `PendingInput`.
pub fn touch_input(
    mut reader: MessageReader<TouchInput>,
    mut touch: ResMut<TouchRes>,
    mut pending: ResMut<PendingInput>,
    settings: Res<crate::hud_view::SettingsRes>,
    window: Option<Single<&Window>>,
) {
    let t = &mut *touch;
    if let Some(w) = &window {
        let (lw, lh) = (w.width(), w.height());
        if (lw - t.s.ui.layout.w).abs() > 0.5 || (lh - t.s.ui.layout.h).abs() > 0.5 {
            t.s.resize(lw, lh, &mut pending.0.buttons);
            t.s.apply_settings(&settings.0, &mut pending.0.buttons); // resize rebuilds the stick controller
        }
    }
    for ev in reader.read() {
        t.visible = true;
        let id = ev.id as u32;
        let (x, y) = (ev.position.x, ev.position.y);
        match ev.phase {
            TouchPhase::Started => t.s.event(TouchEvent::Down { id, x, y }, &mut pending.0.buttons),
            TouchPhase::Moved => t.s.event(TouchEvent::Move { id, x, y }, &mut pending.0.buttons),
            TouchPhase::Ended => t.s.event(TouchEvent::Up { id }, &mut pending.0.buttons),
            TouchPhase::Canceled => t.s.cancel_all(&mut pending.0.buttons),
        }
    }
    let f = &mut pending.0;
    t.s.fill(f);
}

pub struct MobilePlugin {
    pub quality: Quality,
    /// Pick / adapt the tier automatically (live game without `--tier`).
    pub auto_tier: bool,
    pub touch_overlay: bool,
    pub screen: (f32, f32),
}

impl Plugin for MobilePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(QualityRes(self.quality))
            .insert_resource(AutoTier::new(self.auto_tier))
            .add_systems(PreStartup, pick_tier)
            .add_systems(Update, govern)
            .insert_resource(TouchRes { s: TouchSession::new(self.screen.0, self.screen.1), visible: self.touch_overlay })
            .add_systems(Update, update_scale);
    }
}
