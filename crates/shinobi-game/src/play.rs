//! Windowed play: input -> `shinobi_client` input types, snapshot entities -> neon silhouettes, own camera.
//! Milestone 1: no prediction/reconciliation yet (own camera sits at the interpolated server position,
//! orientation from local `Aim`). Prediction belongs in `shinobi_client::reconcile`, not here.

use std::collections::HashMap;

use bevy::camera::{Hdr, RenderTarget};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::post_process::bloom::{Bloom, BloomCompositeMode, BloomPrefilter};
use bevy::prelude::*;
use bevy::render::render_resource::{TextureFormat, TextureUsages};
use bevy::window::{CursorGrabMode, CursorOptions, WindowFocused};

use shinobi_client::client_game::{Action, ButtonEvent};
use shinobi_client::hud::Palette;
use shinobi_client::overlay;
use shinobi_client::input_mode::{ModeInput, ModeMachine, ModeOut};
use shinobi_client::pointer_lock::{ButtonTracker, LockEvent, PointerLock};
use shinobi_client::input::keys_to_stick;
use shinobi_client::remote::{ninja_draw, NinjaCfg};
use shinobi_client::shaders::GridMaterial as GridData;

use shinobi_client::fx::{FxInput, FxState, FxCfg, FxUniforms, SlashInput};
use shinobi_client::shaders::CrtSettings;

use crate::crt::CrtEffect;
use crate::grid::GridFloor;
use crate::net::{Net, PendingInput, ViewRes};

const PARTS: usize = 4;

#[derive(Component)]
pub struct PlayerCam;

struct RemoteEnt {
    root: Entity,
    parts: [Entity; PARTS],
    colour: Option<u32>,
    /// Distance beacon: a faint light column above the player so far-away players still read.
    beacon: Entity,
}

/// Height of the kill plane on the FFA maps (`levels/ffa.lvl`), where the void grid is drawn.
const VOID_Y: f32 = -20.0;

/// Body materials per quantised player colour (and the additive beacon variant).
#[derive(Resource, Default)]
struct BodyMats(std::collections::HashMap<u32, Handle<StandardMaterial>>, std::collections::HashMap<u32, Handle<StandardMaterial>>);

/// Beacons appear beyond this distance (m); width grows with distance so the column stays about the same on screen.
const BEACON_MIN_DIST: f32 = 18.0;

pub fn beacon_width(dist: f32) -> f32 {
    0.05 + 0.004 * dist
}

#[derive(Resource, Default)]
struct Remotes(HashMap<u16, RemoteEnt>);

/// Cosmetic screen-FX driver. Milestone 1 has no server events or prediction, so dash/slash are inferred
/// from the local buttons (edge flags are consumed by `update_crt`). Replace with CombatEvent/CharacterSystem
/// state once the prediction path lands.
#[derive(Resource)]
pub struct FxRes {
    state: FxState,
    /// This frame's screen-FX output (already scaled by the player's feel settings).
    pub out: shinobi_client::fx::FxOutput,
    pub slash_edge: bool,
    /// The predicted swing (flip/lunge) when it came from `ClientGame`; None for scripted/offline shots.
    pub slash: Option<shinobi_client::client_game::PredictedSlash>,
    pub fire_edge: bool,
    pub dj_edge: bool,
    pub dash_held: bool,
    flip: bool,
}

/// Offscreen mode (`--shot`): render the camera into an image instead of a window.
#[derive(Resource)]
pub struct ShotRequest {
    pub width: u32,
    pub height: u32,
}

/// The image the offscreen camera renders into (inserted by `setup`).
#[derive(Resource)]
pub struct ShotImage(pub Handle<Image>);

#[derive(Resource)]
struct Look {
    cube: Handle<Mesh>,
    hostile: Handle<StandardMaterial>,
}

pub struct PlayPlugin;

impl Plugin for PlayPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(Color::linear_rgb(0.003, 0.004, 0.012)))
            .init_resource::<Remotes>()
            .init_resource::<BodyMats>()
            .init_resource::<Lock>()
            .init_resource::<ModeRes>()
            .add_systems(Update, update_title)
            .insert_resource(FxRes { state: FxState::new(FxCfg::default()), out: Default::default(), dash_held: false, slash_edge: false, slash: None, fire_edge: false, dj_edge: false, flip: false })
            .add_systems(Startup, setup)
            .add_systems(Update, (input, crate::mobile::touch_input, fx_from_view, sync_remotes, crate::vm::update, step_fx, follow_camera, update_crt, clear_edges).chain());
    }
}

pub fn setup(
    mut commands: Commands,
    quality: Res<crate::mobile::QualityRes>,
    level: Option<Res<crate::net::LevelRes>>,
    forced: Option<Res<ForcedScale>>,
    windows: Query<&Window>,
    shot: Option<Res<ShotRequest>>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut grids: ResMut<Assets<GridFloor>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    let pal = Palette::TRON;

    // ground: the level's own floor slab (top face at y = 0), so open void edges (FFA maps) really are void; 200 m
    // half-extent only when the level has no big floor box (the built-in arena)
    let big_floor = level
        .as_ref()
        .and_then(|l| {
            l.0.boxes
                .iter()
                .filter(|b| (b.center[1] + b.half[1]).abs() < 0.05 && b.rot_z == 0.0 && b.half[0] > 20.0 && b.half[2] > 20.0)
                .max_by(|a, b| (a.half[0] * a.half[2]).total_cmp(&(b.half[0] * b.half[2])))
                .map(|b| ([b.center[0], b.center[2]], [b.half[0], b.half[2]]))
        });
    let fog = crate::grid::FogFar::for_floor(big_floor.map(|(_, h)| h));
    commands.insert_resource(fog);
    let (floor_c, floor_h) = big_floor.unwrap_or(([0.0, 0.0], [200.0, 200.0]));
    let grid_mat = grids.add(GridFloor::from_client(&GridData::tron(&pal), fog.0));
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::new(Vec3::Y, Vec2::new(floor_h[0], floor_h[1])))),
        MeshMaterial3d(grid_mat.clone()),
        Transform::from_xyz(floor_c[0], 0.0, floor_c[1]),
    ));
    // open-void maps: the kill plane drawn as a faint red grid far below the edges, so the world visibly ENDS
    // (players can see how far the drop is) instead of the floor seeming to run on into the sky
    if big_floor.is_some() {
        let mut vd = GridData::tron(&pal);
        vd.base = [0.012, 0.0, 0.02, 1.0];
        vd.glow = [pal.magenta.0 * 0.9, pal.magenta.1 * 0.12, pal.magenta.2 * 0.3, 1.0];
        vd.cell = 8.0;
        vd.major_every = 4.0;
        commands.spawn((
            Mesh3d(meshes.add(Plane3d::new(Vec3::Y, Vec2::splat(fog.0 * 1.5)))),
            MeshMaterial3d(grids.add(GridFloor::from_client(&vd, fog.0 * 0.8))),
            Transform::from_xyz(floor_c[0], VOID_Y, floor_c[1]),
        ));
    }
    // every non-floor level box (walls, tiers, ramps, ledges, pillars): the geometry the server and our predictor
    // collide with. Each gets its own material so the shader knows the box frame for neon edges; the tint encodes
    // height (tier 1 cyan, tier 2 magenta, tier 3+ orange, tall walls cool blue).
    if let Some(l) = &level {
        let unit = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
        let gd = GridData::tron(&pal);
        for b in l.0.boxes.iter().filter(|b| b.center[1] + b.half[1] > 0.05) {
            let top = b.center[1] + b.half[1];
            let (tint, mode) = if b.center[1] - b.half[1] > 10.0 {
                ([0.15, 0.35, 0.6], 2.0) // ceiling: faint skylight grid
            } else if top > 5.0 {
                ([0.1, 0.55, 1.2], 1.0) // tall walls / perimeter
            } else if top > 3.3 {
                ([pal.orange.0 * 1.6, pal.orange.1 * 1.6, pal.orange.2 * 1.6], 1.0)
            } else if top > 2.0 {
                ([pal.magenta.0 * 1.5, pal.magenta.1 * 1.5, pal.magenta.2 * 1.5], 1.0)
            } else {
                ([pal.cyan.0 * 1.4, pal.cyan.1 * 1.4, pal.cyan.2 * 1.4], 1.0)
            };
            // FFA maps: tint by district (boulevards cyan, canyons magenta, ...); other levels keep the height tint
            let tint = match l.0.district_at(b.center[0], b.center[2]) {
                d if d.id != shinobi_sim::level::DistrictId::Arena && mode == 1.0 => district_tint(tint, d.tint),
                _ => tint,
            };
            commands.spawn((
                Mesh3d(unit.clone()),
                MeshMaterial3d(grids.add(GridFloor::for_box(&gd, b, tint, mode, fog.0))),
                Transform::from_translation(Vec3::from(b.center))
                    .with_rotation(Quat::from_rotation_z(b.rot_z))
                    .with_scale(Vec3::from(b.half) * 2.0),
            ));
        }
    }

    // unlit HDR silhouettes (values > 1 so bloom picks them up)
    let unlit = |c: shinobi_client::hud::Rgba| StandardMaterial {
        base_color: Color::linear_rgb(c.0 * 2.2, c.1 * 2.2, c.2 * 2.2),
        unlit: true,
        ..default()
    };
    commands.insert_resource(Look {
        cube: meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
        hostile: mats.add(unlit(pal.magenta)),
    });

    // camera: LUT-free tonemapping (tonemapping_luts feature is not enabled), 90 deg vertical FOV, bloom
    let q = quality.0;
    let mut cam = commands.spawn((
        Camera3d::default(),
        Hdr,
        Tonemapping::None,
        Projection::Perspective(PerspectiveProjection { fov: 90f32.to_radians(), ..default() }),
        Transform::from_xyz(0.0, 1.4, 0.0),
        PlayerCam,
    ));
    // neon glow: only HDR pixels above the shared threshold bloom. Tiers without bloom/CRT skip those passes.
    if let Some(b) = q.bloom {
        cam.insert(Bloom {
            intensity: b.intensity * BLOOM_SCALE,
            prefilter: BloomPrefilter { threshold: b.threshold, threshold_softness: b.knee },
            composite_mode: BloomCompositeMode::Additive,
            ..Bloom::NATURAL
        });
    }
    if q.crt {
        cam.insert(CrtEffect::default());
    }
    let cam_id = cam.id();

    // final target: the shot image offscreen, otherwise the window (default)
    let final_img = shot.as_ref().map(|r| {
        let mut target = Image::new_target_texture(r.width, r.height, TextureFormat::Rgba8UnormSrgb, None);
        target.texture_descriptor.usage |= TextureUsages::COPY_SRC; // GPU readback copies from it
        let h = images.add(target);
        commands.insert_resource(ShotImage(h.clone()));
        h
    });

    if q.dyn_res.is_some() {
        // world camera -> scaled scene image -> presenter (upscale) -> final target; viewmodel + HUD stay native
        let native = match (&shot, windows.single()) {
            (Some(r), _) => (r.width, r.height),
            (None, Ok(w)) => (w.physical_width(), w.physical_height()),
            _ => (1280, 720),
        };
        let scene = images.add(crate::mobile::scene_image(native.0, native.1));
        commands
            .entity(cam_id)
            .insert((RenderTarget::Image(scene.clone().into()), Camera { order: -1, ..default() }));
        crate::mobile::spawn_present(&mut commands, &mut meshes, &mut mats, scene, native, final_img, &q, forced.map(|f| f.0));
    } else if let Some(img) = final_img {
        commands.entity(cam_id).insert(RenderTarget::Image(img.into()));
    }
}

/// Our `Bloom::intensity` is in bevy units; `BloomSettings::intensity` is the renderer-agnostic strength.
const BLOOM_SCALE: f32 = 0.115;

/// `--scale F`: pin dynamic resolution (regression shots).
#[derive(Resource, Clone, Copy)]
pub struct ForcedScale(pub f32);

/// Edge-trigger a bevy key/mouse state into `ButtonEvent`s (press and release, in order).
fn key_events(out: &mut Vec<ButtonEvent>, pressed: bool, released: bool, action: Action) {
    if pressed {
        out.push(ButtonEvent { action, down: true });
    }
    if released {
        out.push(ButtonEvent { action, down: false });
    }
}

/// Pointer-lock state + held-button tracker (logic and tests live in `shinobi_client::pointer_lock`).
#[derive(Resource, Default)]
pub struct Lock {
    pub p: PointerLock,
    pub tracker: ButtonTracker,
}

/// Input mode (Play / Scoreboard / BuildRadial / Console, see `shinobi_client::input_mode`): what the game may read while
/// an interactive HUD overlay is up. HUD UI reads `ModeRes.out` (current mode, `cursor_free`, `radial_commit`) to draw the
/// console / radial menu and to know the cursor is free; `radial_enabled` is theirs to switch on.
#[derive(Resource, Default)]
pub struct ModeRes {
    pub machine: ModeMachine,
    pub out: Option<ModeOut>,
    /// Seconds a build key (N / M) has been held continuously.
    build_hold: f32,
    /// `build_hold` as of the previous frame: on the release frame it says whether the key was a tap or a hold.
    build_hold_prev: f32,
}

impl ModeRes {
    pub fn mode(&self) -> shinobi_client::input_mode::Mode {
        self.machine.mode()
    }
}

fn apply_cursor(c: &mut CursorOptions, lock: &PointerLock, mode_free: bool) {
    let s = lock.cursor();
    // an overlay mode (scoreboard, console, radial menu) frees the cursor even while the pointer is locked
    let grab = s.grab && !mode_free;
    // bevy_winit falls back to Confined where Locked is unsupported (X11)
    c.grab_mode = if grab { CursorGrabMode::Locked } else { CursorGrabMode::None };
    c.visible = !grab;
}

pub fn input(
    mut pending: ResMut<PendingInput>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    cursor: Option<Single<&mut CursorOptions>>,
    mut focus: MessageReader<WindowFocused>,
    mut lock: ResMut<Lock>,
    mut menu: ResMut<crate::hud_view::MenuRes>,
    mut mode: ResMut<ModeRes>,
    time: Res<Time>,
) {
    let Some(mut cursor) = cursor else { return };
    let lock = &mut *lock;

    // input mode first: it may swallow this frame's Esc (closing the console / radial menu) before the pointer lock sees it
    let build_down = keys.pressed(KeyCode::KeyN) || keys.pressed(KeyCode::KeyM);
    mode.build_hold_prev = mode.build_hold;
    mode.build_hold = if build_down { mode.build_hold + time.delta_secs() } else { 0.0 };
    let build_held_secs = mode.build_hold;
    let mode_out = mode.machine.update(&ModeInput {
        locked: lock.p.locked(),
        tab_held: keys.pressed(KeyCode::Tab),
        build_held_secs,
        backquote_pressed: keys.just_pressed(KeyCode::Backquote),
        escape_pressed: keys.just_pressed(KeyCode::Escape),
    });
    mode.out = Some(mode_out);

    // window events -> state machine (focus first, then Esc, then the click, so one frame behaves sensibly)
    let mut events = Vec::new();
    for e in focus.read() {
        events.push(if e.focused { LockEvent::FocusGained } else { LockEvent::FocusLost });
    }
    if keys.just_pressed(KeyCode::Escape) && !mode_out.consumed_escape {
        events.push(LockEvent::Escape);
    }
    // clicks on the pause menu must not grab the pointer; RESUME asks for the grab explicitly
    if (mouse.just_pressed(MouseButton::Left) && !menu.open()) || std::mem::take(&mut menu.resume) {
        events.push(LockEvent::LeftClick);
    }
    let mut swallow = false;
    for ev in events {
        let out = lock.p.on(ev);
        if out.changed {
            apply_cursor(&mut cursor, &lock.p, mode_out.flags.cursor_free);
        }
        if out.release_buttons {
            lock.tracker.release_all(&mut pending.0.buttons);
            pending.0.stick = (0.0, 0.0);
        }
        swallow |= out.swallow_click;
    }

    // overlay modes: free / re-grab the cursor when the mode flips, and drop every held button when entering one that blocks actions
    if mode_out.changed {
        apply_cursor(&mut cursor, &lock.p, mode_out.flags.cursor_free);
        if mode_out.release_buttons {
            lock.tracker.release_all(&mut pending.0.buttons);
        }
    }
    let f = &mut pending.0;
    if !mode_out.flags.allow_move {
        f.stick = (0.0, 0.0);
    }
    if !lock.p.locked() || swallow {
        f.stick = (0.0, 0.0);
        return;
    }
    if mode_out.flags.allow_look {
        f.look.0 += motion.delta.x;
        f.look.1 += motion.delta.y;
    }
    if mode_out.flags.allow_move {
        f.stick = keys_to_stick(
            keys.pressed(KeyCode::KeyW),
            keys.pressed(KeyCode::KeyA),
            keys.pressed(KeyCode::KeyS),
            keys.pressed(KeyCode::KeyD),
        );
    }
    if !mode_out.flags.allow_actions {
        return; // scoreboard / console / radial menu: clicks and keys belong to the overlay
    }

    // the latch in ClientGame turns these into one-shot sim-tick edges, so a sub-tick tap is not lost
    let mut out = Vec::new();
    let k = |code: KeyCode| (keys.just_pressed(code), keys.just_released(code));
    let m = |b: MouseButton| (mouse.just_pressed(b), mouse.just_released(b));
    for ((p, r), a) in [
        (k(KeyCode::Space), Action::Jump),
        (k(KeyCode::ShiftLeft), Action::Dash),
        (k(KeyCode::KeyQ), Action::Roll),
        (k(KeyCode::KeyC), Action::Slide),
        (k(KeyCode::KeyE), Action::Grapple), // wrist grapple (docs/grapple.md); E again releases
        (k(KeyCode::KeyW), Action::Cling), // pressing into a wall; the server validates actual contact
        (m(MouseButton::Left), Action::Slice),
        (m(MouseButton::Right), Action::Fire),
    ] {
        key_events(&mut out, p, r, a);
    }
    // Build keys print on RELEASE so a long hold can open the radial menu instead (it replaces the print): a tap
    // (released before the radial threshold, or radial disabled) prints now; a hold prints only what the radial picks.
    let hold = mode.machine.cfg.radial_enabled.then_some(mode.machine.cfg.radial_hold_secs);
    let was_hold = hold.map_or(false, |t| mode.build_hold_prev >= t);
    for (code, action) in [(KeyCode::KeyN, Action::BuildFloor), (KeyCode::KeyM, Action::BuildWall)] {
        if keys.just_released(code) && !was_hold {
            out.push(ButtonEvent { action, down: true });
            out.push(ButtonEvent { action, down: false });
        }
    }
    for e in out {
        lock.tracker.apply(&e);
        f.buttons.push(e);
    }
}

/// OS window title follows the lock state ("click the window to play" until the mouse is grabbed).
fn update_title(lock: Res<Lock>, window: Option<Single<&mut Window>>) {
    let Some(mut w) = window else { return };
    let t = overlay::window_title(lock.p.locked());
    if w.title != t {
        w.title = t.into();
    }
}

/// Box neon tint on a district map: mostly the district colour (HDR boosted), with a little of the height tint kept so
/// tiers still read apart inside a district.
pub fn district_tint(height: [f32; 3], district: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| height[i] * 0.25 + district[i] * 1.0 * 0.75)
}

fn sync_remotes(
    mut commands: Commands,
    net: Res<Net>,
    view: Res<ViewRes>,
    look: Res<Look>,
    mut remotes: ResMut<Remotes>,
    mut body_mats: ResMut<BodyMats>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut tf: Query<&mut Transform>,
) {
    let s = &net.0;
    let v = &view.0;
    let Some(snap) = s.game.net.interp.newest_tick().and_then(|t| s.game.net.interp.get(t)) else { return };
    let pal = Palette::TRON;
    let cfg = NinjaCfg { feet_below_pos: 0.81, height: 1.62 };
    let t = s.local_tick() as f32 / 60.0;

    let mut seen = Vec::new();
    for (id, sample) in &v.remotes {
        let Some((_, st)) = snap.entities.iter().find(|(e, _)| e == id) else { continue };
        let Some(d) = ninja_draw(sample, st, &pal, &cfg, false, t) else { continue };
        seen.push(*id);

        let ent = remotes.0.entry(*id).or_insert_with(|| {
            let root = commands.spawn((Transform::default(), Visibility::default())).id();
            let parts = std::array::from_fn(|_| {
                commands
                    .spawn((
                        Mesh3d(look.cube.clone()),
                        MeshMaterial3d(look.hostile.clone()),
                        Transform::default(),
                        ChildOf(root),
                    ))
                    .id()
            });
            let beacon = commands
                .spawn((Mesh3d(look.cube.clone()), MeshMaterial3d(look.hostile.clone()), Transform::default(), Visibility::Hidden, ChildOf(root)))
                .id();
            RemoteEnt { root, parts, colour: None, beacon }
        });
        // player colour (team colour in team modes): re-skin the body parts only when it changes
        let hue = crate::team::colour_of(&net, *id);
        let k = crate::team::key(hue);
        if ent.colour != Some(k) {
            ent.colour = Some(k);
            let m = body_mats.0.entry(k).or_insert_with(|| {
                mats.add(StandardMaterial {
                    base_color: Color::linear_rgb(hue[0] * 1.3, hue[1] * 1.3, hue[2] * 1.3),
                    unlit: true,
                    ..default()
                })
            });
            for e in &ent.parts {
                commands.entity(*e).insert(MeshMaterial3d(m.clone()));
            }
            let bm = body_mats.1.entry(k).or_insert_with(|| {
                mats.add(StandardMaterial {
                    base_color: Color::linear_rgb(hue[0] * 2.0, hue[1] * 2.0, hue[2] * 2.0),
                    unlit: true,
                    alpha_mode: AlphaMode::Add,
                    ..default()
                })
            });
            commands.entity(ent.beacon).insert(MeshMaterial3d(bm.clone()));
        }
        // distance beacon: a light column above the head once the player is far enough to be a few pixels
        let dist = Vec3::from(v.eye).distance(Vec3::from(d.pos));
        if let Ok(mut bt) = tf.get_mut(ent.beacon) {
            let w = beacon_width(dist);
            bt.translation = Vec3::new(0.0, 4.2, 0.0);
            bt.scale = Vec3::new(w, 6.5, w);
        }
        commands.entity(ent.beacon).insert(if dist > BEACON_MIN_DIST { Visibility::Inherited } else { Visibility::Hidden });

        if let Ok(mut r) = tf.get_mut(ent.root) {
            r.translation = Vec3::from(d.pos);
            r.rotation = Quat::from_rotation_y(d.yaw) * Quat::from_rotation_z(d.roll);
        }
        for (e, p) in ent.parts.iter().zip(d.parts.iter()) {
            if let Ok(mut pt) = tf.get_mut(*e) {
                pt.translation = Vec3::from(p.offset);
                pt.rotation = Quat::from_rotation_x(p.pitch);
                pt.scale = Vec3::from(p.half) * 2.0;
            }
        }
    }

    remotes.0.retain(|id, e| {
        let keep = seen.contains(id);
        if !keep {
            commands.entity(e.root).despawn();
        }
        keep
    });
}

/// Camera comes entirely from the predicted `RenderView`: eye position, aim, wall-run roll and FOV punch.
fn follow_camera(view: Res<ViewRes>, vm: Res<crate::vm::Vm>, _fx: Res<FxRes>, mut cam: Single<(&mut Transform, &mut Projection), With<PlayerCam>>) {
    let v = &view.0;
    if !v.connected {
        return;
    }
    let (tf, proj) = &mut *cam;
    tf.translation = Vec3::from(v.eye);
    // Aim yaw 0 faces -Z, positive turns toward -X == +Y rotation in Bevy; positive pitch looks up == +X rotation.
    // `CameraPose::roll > 0` leans the view clockwise; a clockwise camera is a NEGATIVE rotation about +Z.
    // Look angles are exactly the aim sent in intents (`look_angles`): the FX look-kick used to be added here and moved
    // the crosshair up to ~0.8 deg off the real shot. Shake now lives in vignette/FOV/viewmodel only.
    let (yaw, pitch) = v.look_angles();
    tf.rotation = Quat::from_euler(EulerRot::YXZ, yaw, pitch, -v.camera.roll);
    // katana hit shake / kick: a view-frame translation only, so the crosshair stays on the aim
    let (right, up) = (tf.right(), tf.up());
    tf.translation += right * v.camera.offset[0] + up * v.camera.offset[1];
    if let Projection::Perspective(p) = &mut **proj {
        // CameraPose is all-zero until the first sim tick has produced a pose: keep a sane FOV until then
        let base = if v.camera.fov > 1.0 { v.camera.fov } else { 90.0 };
        p.fov = (base + vm.katana.fov_kick_deg()).to_radians();
    }
}

/// One-frame input edges are consumed by whoever needs them (viewmodel, CRT) and cleared here, so tiers that skip
/// a consumer (CRT off) can never leave an edge stuck on.
fn clear_edges(mut fx: ResMut<FxRes>) {
    fx.slash_edge = false;
    fx.slash = None;
    fx.fire_edge = false;
    fx.dj_edge = false;
}

/// Predicted-sim effect edges (`LocalFx`) -> the screen/viewmodel FX drivers. OR-ed in, so scripted shots can
/// still raise edges by hand; `clear_edges` consumes them at the end of the frame.
fn fx_from_view(view: Res<ViewRes>, mut fx: ResMut<FxRes>) {
    let l = &view.0.fx;
    fx.dash_held = l.dashing;
    fx.dj_edge |= l.double_jumped;
    fx.fire_edge |= l.threw.is_some();
    if let Some(sl) = l.slash {
        fx.slash_edge = true;
        fx.slash = Some(sl);
    }
}

/// Steps the screen-FX state once per frame and scales it by the player's feel settings (reduced motion, shake
/// and warp strength). Runs before the camera so the look kick can be applied to it.
fn step_fx(time: Res<Time>, net: Res<Net>, view: Res<ViewRes>, mut fx: ResMut<FxRes>) {
    let slash = if fx.slash_edge {
        let flip = match fx.slash {
            Some(s) => s.flip,
            None => {
                fx.flip = !fx.flip;
                fx.flip
            }
        };
        Some(SlashInput { dir: [1.0, 0.0], flip })
    } else {
        None
    };
    let input = FxInput {
        dash_active: fx.dash_held,
        double_jumped: fx.dj_edge,
        slash,
        tick: net.0.local_tick() as u32,
        server_time: net.0.local_tick() / 60.0,
    };
    let raw = fx.state.update(time.delta_secs(), input);
    fx.out = view.0.feel.apply_fx(&raw);
}

fn update_crt(
    net: Res<Net>,
    fx: Res<FxRes>,
    window: Option<Single<&Window>>,
    shot: Option<Res<ShotRequest>>,
    crt: Option<Single<&mut CrtEffect, With<PlayerCam>>>,
) {
    let Some(mut crt) = crt else { return };
    let native = match (&window, &shot) {
        (Some(w), _) => (w.physical_width(), w.physical_height()),
        (None, Some(r)) => (r.width, r.height),
        _ => (1280, 720),
    };
    let u = FxUniforms::new(0.0, 90.0, &fx.out, net.0.local_tick() / 60.0, native, 1.0);
    **crt = CrtEffect::new(&u, &CrtSettings::tron(&Palette::TRON));
}

#[cfg(test)]
mod district_tests {
    use super::*;

    #[test]
    fn district_tint_is_dominated_by_the_district_colour() {
        let t = district_tint([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        assert!(t[1] > t[0] * 2.5, "{t:?}");
        assert!(t.iter().all(|v| *v >= 0.0));
    }
}

#[cfg(test)]
mod beacon_tests {
    use super::*;

    #[test]
    fn beacon_widens_with_distance() {
        assert!(beacon_width(60.0) > beacon_width(20.0));
        assert!(beacon_width(0.0) > 0.0);
    }
}
