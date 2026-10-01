//! Mobile-facing pieces: quality tier resource, frame-time driven dynamic resolution, and touch input.
//!
//! Dynamic resolution: when the tier enables it the world camera renders into a scaled image (bloom and CRT run
//! at that size) and a presenter camera draws it upscaled into the real target. The viewmodel and HUD cameras
//! draw afterwards at native resolution, so the vector HUD stays sharp.

use bevy::camera::visibility::RenderLayers;
use bevy::camera::{RenderTarget, ScalingMode};
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

const PRESENT_LAYER: usize = 3;

/// Present path state (only exists when the tier enables dynamic resolution).
#[derive(Resource)]
pub struct DynScale {
    pub dr: DynRes,
    pub forced: Option<f32>,
    scene: Handle<Image>,
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
    commands.spawn((
        Mesh3d(meshes.add(Rectangle::new(1.0, 1.0))),
        MeshMaterial3d(mats.add(StandardMaterial { base_color_texture: Some(scene.clone()), unlit: true, ..default() })),
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
    commands.insert_resource(DynScale { dr, forced, scene, cur: native });
}

/// Frame-time -> scale -> scene image size. Native size follows the window (or the shot request).
pub fn update_scale(
    time: Res<Time>,
    view: Res<ViewRes>,
    window: Option<Single<&Window>>,
    shot: Option<Res<ShotRequest>>,
    ds: Option<ResMut<DynScale>>,
    mut images: ResMut<Assets<Image>>,
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
    let size = ds.dr.scene_size(native);
    if size != ds.cur {
        if let Some(mut img) = images.get_mut(&ds.scene) {
            img.resize(Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 });
            ds.cur = size;
        }
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
    pub touch_overlay: bool,
    pub screen: (f32, f32),
}

impl Plugin for MobilePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(QualityRes(self.quality))
            .insert_resource(TouchRes { s: TouchSession::new(self.screen.0, self.screen.1), visible: self.touch_overlay })
            .add_systems(Update, update_scale);
    }
}
