//! HUD: `shinobi_client::hud::Hud::build` draw list -> `hud_geom::tessellate` -> one dynamic alpha-blended
//! mesh, drawn by an orthographic overlay camera (layer 2, after the CRT/viewmodel) at native resolution.
//! Normalized HUD space maps 1:1 onto an ortho area of [0,1]x[0,1] with y flipped.

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::camera::{ClearColorConfig, Hdr, RenderTarget, ScalingMode};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use shinobi_client::conn_watch::ConnWatch;
use shinobi_client::hud::{Hud, HudState, Palette};
use shinobi_client::hud_bridge::HudBridge;
use shinobi_client::hud_geom::tessellate;
use shinobi_client::screens::{self, Screen};
use shinobi_client::overlay;

use crate::net::Net;
use crate::play::{ShotImage, ShotRequest};

const LAYER: usize = 2;

#[derive(Resource)]
struct HudMesh(Handle<Mesh>);

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(HudCtl { bridge: HudBridge::default(), watch: ConnWatch::default() })
            .add_systems(Startup, spawn.after(crate::play::setup))
            .add_systems(Update, update);
    }
}

fn blank() -> Mesh {
    // zero-vertex meshes upset Bevy's mesh slab allocator
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3])
}

fn spawn(
    mut commands: Commands,
    shot: Option<Res<ShotImage>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    let layer = RenderLayers::layer(LAYER);
    let mut cam = commands.spawn((
        Camera3d::default(),
        Camera { order: 2, clear_color: ClearColorConfig::None, ..default() },
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
        MeshMaterial3d(mats.add(StandardMaterial {
            base_color: Color::WHITE,
            unlit: true,
            alpha_mode: AlphaMode::Blend,
            double_sided: true,
            cull_mode: None,
            ..default()
        })),
        Transform::default(),
        layer,
        NoFrustumCulling,
    ));
    commands.insert_resource(HudMesh(mesh));
}

/// Controllers that live across frames: the HUD state bridge and the connection watcher.
#[derive(Resource)]
struct HudCtl {
    bridge: HudBridge,
    watch: ConnWatch,
}

/// Printable server address for the "server unreachable" screen.
#[derive(Resource, Clone)]
pub struct ServerLabel(pub String);

fn update(
    time: Res<Time>,
    net: Res<Net>,
    view: Res<crate::net::ViewRes>,
    window: Option<Single<&Window>>,
    shot: Option<Res<ShotRequest>>,
    hud: Res<HudMesh>,
    touch: Option<Res<crate::mobile::TouchRes>>,
    lock: Res<crate::play::Lock>,
    label: Option<Res<ServerLabel>>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mut ctl: ResMut<HudCtl>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let aspect = match (&window, &shot) {
        (Some(w), _) => w.physical_width() as f32 / w.physical_height().max(1) as f32,
        (None, Some(r)) => r.width as f32 / r.height.max(1) as f32,
        _ => 16.0 / 9.0,
    };
    let dt = time.delta_secs();
    let pal = Palette::TRON;
    let tab = keys.as_ref().map_or(false, |k| k.pressed(KeyCode::Tab));
    let ctl = &mut *ctl;

    let state: HudState = ctl.bridge.update(dt, &view.0, &net.0.game.net, tab);
    let screen = ctl.watch.update(
        dt,
        &net.0.game.net,
        view.0.connect_failed,
        label.as_ref().map_or("?", |l| l.0.as_str()),
    );

    let mut prims = Hud::build(&state);
    if let Some(t) = touch.as_ref().filter(|t| t.visible) {
        prims.extend(t.s.draw(state.dash_ready, state.shuriken_ready, &pal));
    }
    let t = time.elapsed_secs();
    if screen != Screen::InGame {
        // connecting / unreachable / connection lost / kicked: never a blank view
        prims.extend(screens::draw(&screen, &pal, t));
    } else if !lock.p.locked() && shot.is_none() && window.is_some() {
        prims.extend(overlay::click_to_play(&pal, t));
    }
    let g = tessellate(&prims, aspect);

    if let Some(mut m) = meshes.get_mut(&hud.0) {
        *m = if g.idx.is_empty() {
            blank()
        } else {
            Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, g.pos.iter().map(|p| [p[0], 1.0 - p[1], 0.0]).collect::<Vec<_>>())
                .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, g.col)
                .with_inserted_indices(Indices::U32(g.idx))
        };
    }
}
