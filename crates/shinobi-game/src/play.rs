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
    /// Black back-face hulls, one per body part: a dark rim that separates the bright body from any background.
    outlines: [Entity; PARTS],
    colour: Option<u32>,
    /// Distance beacon: a faint light column above the player so far-away players still read.
    beacon: Entity,
    /// Soft additive glow ellipsoid around the body (depth-tested: only visible when the player is in view).
    aura: Entity,
    /// Spinning gem above the head within `GEM_MAX_DIST`: marks "a player is here" at mid range.
    gem: Entity,
    /// The block shield: a big glowing framed panel held out in front of the body while `FX_BLOCK` is set (root of
    /// the panel and its four frame bars).
    shield: Entity,
    shield_panel: Entity,
    shield_bars: [Entity; 4],
    /// Tron x shinobi dressing on the body (hood, plates, katana edge, scarf, poses): `character_look.rs`.
    rig: crate::character_look::Rig,
    /// The custom look the rig was built from; `None` = stock ninja.
    spec: Option<shinobi_proto::character::CharacterSpec>,
    /// Showing the white hit flash (`hit_feedback::HitFx`) instead of the player colour.
    flashing: bool,
}

/// Height of the kill plane on the FFA maps (`levels/ffa.lvl`), where the void grid is drawn.
const VOID_Y: f32 = -20.0;

/// Body materials per quantised player colour (and the additive beacon variant).
#[derive(Resource, Default)]
struct BodyMats(
    std::collections::HashMap<u32, Handle<StandardMaterial>>,
    std::collections::HashMap<u32, Handle<StandardMaterial>>,
    std::collections::HashMap<u32, Handle<StandardMaterial>>,
    /// The block shield panel (additive, bright) per player colour.
    std::collections::HashMap<u32, Handle<StandardMaterial>>,
);

/// Visibility tuning for remote players (see `body_colour`, `outline_grow`, `size_boost`). One place, so the death /
/// dissolve effect can drive the same material and scale paths.
///
/// The body is a hot core (player colour lifted toward white) rather than the pure hue: a cyan player against the cyan
/// grid floor is invisible by colour alone, but a bright core inside a black rim always reads by luminance.
pub const CORE_WHITE: f32 = 0.2;
/// HDR gain of the core (bloom threshold is ~1; this blooms a little without washing out).
pub const CORE_GAIN: f32 = 1.8;

/// Linear HDR body colour for a player hue (unit scale).
pub fn body_colour(hue: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| (hue[i] + (1.0 - hue[i]) * CORE_WHITE) * CORE_GAIN)
}

/// Outline half-thickness added to every part half-extent (m): ~3 px at any distance on a 540 px frame.
pub fn outline_grow(dist: f32) -> f32 {
    0.05 + 0.009 * dist
}

/// Width multiplier for the body parts: beyond ~12 m a player gets thin on screen, so the body is fattened (up to
/// 3x wide, a third of that tall) to stay a readable silhouette.
pub fn size_boost(dist: f32) -> (f32, f32) {
    let w = (dist / 12.0).clamp(1.0, 3.0);
    (w, 1.0 + (w - 1.0) * 0.33)
}

/// Head gem range (m).
const GEM_MAX_DIST: f32 = 110.0;

/// Aura width multiplier: grows with distance so the glow stays a few pixels wider than the body at 30-60 m.
pub fn aura_scale(dist: f32) -> f32 {
    1.0 + (dist / 25.0).min(3.0)
}

/// Beacons appear beyond this distance (m); width grows with distance so the column stays about the same on screen.
const BEACON_MIN_DIST: f32 = 18.0;

pub fn beacon_width(dist: f32) -> f32 {
    0.08 + 0.012 * dist
}

/// Head-gem edge length (m): about 4-5 px on screen from 10 m out to the gem's max range.
pub fn gem_size(dist: f32) -> f32 {
    0.14 + 0.016 * dist
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
    sphere: Handle<Mesh>,
    hostile: Handle<StandardMaterial>,
    /// Opaque black, front faces culled: drawn on slightly larger copies of the body parts as a rim.
    outline: Handle<StandardMaterial>,
}

pub struct PlayPlugin;

impl Plugin for PlayPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ClearColor(Color::linear_rgb(0.003, 0.004, 0.012)))
            .init_resource::<Remotes>()
            .init_resource::<BodyMats>()
            .init_resource::<crate::character_look::LookMats>()
            .insert_resource(crate::character_look::MyLook::startup())
            .init_resource::<crate::hit_feedback::HitFx>()
            .init_resource::<Lock>()
            .init_resource::<ModeRes>()
            .add_systems(Update, update_title)
            .add_systems(Update, crate::character_look::sync_my_look)
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
    // floating-structure maps (ffa_sky): no floor box and nothing reaches down near y = 0, so there is NO ground to draw
    let floating = big_floor.is_none() && level.as_ref().map_or(false, |l| shinobi_sim::level::is_floating(&l.0));
    let fog = crate::grid::FogFar::for_floor(big_floor.map(|(_, h)| h));
    commands.insert_resource(fog);
    let (floor_c, floor_h) = big_floor.unwrap_or(([0.0, 0.0], [200.0, 200.0]));
    let grid_mat = grids.add(GridFloor::from_client(&GridData::tron(&pal), fog.0));
    if !floating {
        commands.spawn((
            Mesh3d(meshes.add(Plane3d::new(Vec3::Y, Vec2::new(floor_h[0], floor_h[1])))),
            MeshMaterial3d(grid_mat.clone()),
            Transform::from_xyz(floor_c[0], 0.0, floor_c[1]),
        ));
    }
    // open-void maps: the kill plane drawn as a faint red grid far below the edges, so the world visibly ENDS
    // (players can see how far the drop is) instead of the floor seeming to run on into the sky
    if big_floor.is_some() || floating {
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
        for (bi, b) in l.0.boxes.iter().enumerate().filter(|(_, b)| b.center[1] + b.half[1] > 0.05) {
            let top = b.center[1] + b.half[1];
            let (tint, mode) = if floating {
                crate::env_look::city_surface() // floating city: tier colours by height, circuit traces (env_look.rs, shader mode 3)
            } else if b.center[1] - b.half[1] > 10.0 {
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
            let mut ent = commands.spawn((
                Mesh3d(unit.clone()),
                MeshMaterial3d(grids.add(GridFloor::for_box(&gd, b, tint, mode, fog.0))),
                Transform::from_translation(Vec3::from(b.center))
                    .with_rotation(Quat::from_rotation_z(b.rot_z))
                    .with_scale(Vec3::from(b.half) * 2.0),
            ));
            if b.hp > 0.0 {
                ent.insert(crate::destructible_fx::DestructBox(bi as u16)); // destructible_fx hides it while broken
            }
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
        sphere: meshes.add(Sphere::new(0.5)),
        hostile: mats.add(unlit(pal.magenta)),
        outline: mats.add(StandardMaterial {
            base_color: Color::BLACK,
            unlit: true,
            cull_mode: Some(bevy::render::render_resource::Face::Front),
            ..default()
        }),
    });

    // camera: LUT-free tonemapping (tonemapping_luts feature is not enabled), 90 deg vertical FOV, bloom
    let q = quality.0;
    let mut cam = commands.spawn((
        Camera3d::default(),
        Hdr,
        Tonemapping::None,
        Projection::Perspective(PerspectiveProjection { fov: 90f32.to_radians(), far: 4000.0, ..default() }), // far plane well past the 1000 m hook
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
/// The keyboard / mouse bindings of every action, as button events for this frame (key down / key up edges).
fn action_events(keys: &ButtonInput<KeyCode>, mouse: &ButtonInput<MouseButton>) -> Vec<ButtonEvent> {
    let mut out = Vec::new();
    let k = |code: KeyCode| (keys.just_pressed(code), keys.just_released(code));
    let m = |b: MouseButton| (mouse.just_pressed(b), mouse.just_released(b));
    for ((p, r), a) in [
        (k(KeyCode::Space), Action::Jump),
        (k(KeyCode::ShiftLeft), Action::Dash),
        (k(KeyCode::Digit4), Action::Roll),
        (k(KeyCode::KeyQ), Action::Fire), // throw the selected star (hold = MARK star); R / wheel switch the type
        (k(KeyCode::KeyC), Action::Slide),
        (k(KeyCode::KeyE), Action::Grapple), // wrist grapple (docs/grapple.md); E again releases
        (k(KeyCode::KeyW), Action::Cling), // pressing into a wall; the server validates actual contact
        (m(MouseButton::Left), Action::Slice),
        // weapon select: 1 = katana, 2 = yumi (bow up: left click draws it)
        (k(KeyCode::Digit1), Action::Katana),
        (k(KeyCode::Digit2), Action::Bow),
        // hold: shield; a timed raise is the parry. RMB is the main block; Ctrl is an extra key (some WMs eat it)
        (m(MouseButton::Right), Action::Block),
        (k(KeyCode::ControlLeft), Action::Block),
        (k(KeyCode::ControlRight), Action::Block),
        (m(MouseButton::Middle), Action::Block),
        (m(MouseButton::Back), Action::Block),
    ] {
        key_events(&mut out, p, r, a);
    }
    out
}

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
    build_holds: shinobi_client::input_mode::BuildHolds,
    /// Gamepad -> move / look / button edges (`shinobi_client::gamepad`).
    pad: shinobi_client::gamepad::PadMapper,
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
    scroll: Res<bevy::input::mouse::AccumulatedMouseScroll>,
    cursor: Option<Single<&mut CursorOptions>>,
    pads: Query<&Gamepad>,
    mut focus: MessageReader<WindowFocused>,
    mut lock: ResMut<Lock>,
    mut menu: ResMut<crate::hud_view::MenuRes>,
    mut mode: ResMut<ModeRes>,
    time: Res<Time>,
    mut net: ResMut<Net>,
    mut view: ResMut<ViewRes>,
) {
    let Some(mut cursor) = cursor else { return };
    let lock = &mut *lock;

    // input mode first: it may swallow this frame's Esc (closing the console / radial menu) before the pointer lock sees it
    let build_held_secs = mode.build_holds.update(time.delta_secs(), keys.pressed(KeyCode::KeyN), keys.pressed(KeyCode::KeyM));
    let mode_out = mode.machine.update(&ModeInput {
        locked: lock.p.locked(),
        tab_held: keys.pressed(KeyCode::Tab),
        build_held_secs,
        backquote_pressed: keys.just_pressed(KeyCode::Backquote),
        escape_pressed: keys.just_pressed(KeyCode::Escape),
    });
    mode.out = Some(mode_out);

    // gamepad: the first pad with any input this frame (or the first connected) becomes a snapshot for the mapper
    let pad_out = {
        let pad = pads.iter().find(|g| pad_snapshot(g).1).or_else(|| pads.iter().next());
        let snap = pad.map_or_else(Default::default, |g| pad_snapshot(g).0);
        mode.pad.update(&snap, time.delta_secs(), net.0.game.settings.mouse_sens)
    };

    // window events -> state machine (focus first, then Esc, then the click, so one frame behaves sensibly)
    let mut events = Vec::new();
    // Start on a pad grabs the pointer like a click does, so a controller player never has to touch the mouse
    if pad_out.start_pressed && !menu.open() && !lock.p.locked() {
        events.push(LockEvent::LeftClick);
    }
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
        // Mouse look turns the aim on every rendered frame, not only on the 60 Hz sim tick: the camera used to step at
        // 60 Hz (judder on faster displays or uneven frames). The next intent still carries the current aim.
        // a window grab / focus change can deliver one huge bogus delta (cursor warp on some compositors): drop it
        if motion.delta != Vec2::ZERO && motion.delta.abs().max_element() < 1500.0 {
            let g = &mut net.0.game;
            g.settings.look(&mut g.aim, motion.delta.x, motion.delta.y, false);
            view.0.aim = g.aim;
        }
        if pad_out.look != (0.0, 0.0) {
            let g = &mut net.0.game;
            g.settings.look(&mut g.aim, pad_out.look.0, pad_out.look.1, false);
            view.0.aim = g.aim;
        }
    }
    if mode_out.flags.allow_move {
        f.stick = keys_to_stick(
            keys.pressed(KeyCode::KeyW),
            keys.pressed(KeyCode::KeyA),
            keys.pressed(KeyCode::KeyS),
            keys.pressed(KeyCode::KeyD),
        );
        if pad_out.stick != (0.0, 0.0) && f.stick == (0.0, 0.0) {
            f.stick = pad_out.stick;
        }
    }
    // Build keys print on RELEASE so a long hold can open the radial menu instead (it replaces the print). Each key
    // is judged by its OWN hold, and this runs before the action gate so tapping M while N holds the radial open still
    // prints the wall. Not while the console owns the keyboard.
    if mode_out.mode != shinobi_client::input_mode::Mode::Console {
        use shinobi_client::input_mode::BuildKey;
        for (code, key, action) in [(KeyCode::KeyN, BuildKey::N, Action::BuildFloor), (KeyCode::KeyM, BuildKey::M, Action::BuildWall)] {
            if keys.just_released(code) && mode.machine.cfg.build_release_prints(mode.build_holds.prev(key)) {
                f.buttons.push(ButtonEvent { action, down: true });
                f.buttons.push(ButtonEvent { action, down: false });
            }
        }
    }
    if !mode_out.flags.allow_actions {
        return; // scoreboard / console / radial menu: clicks and keys belong to the overlay
    }

    // the latch in ClientGame turns these into one-shot sim-tick edges, so a sub-tick tap is not lost
    let mut out = action_events(&keys, &mouse);
    // the throwable: R or the wheel cycles it (Q is the roll); one-tick edges
    if keys.just_pressed(KeyCode::KeyR) || scroll.delta.y < -0.5 {
        out.push(ButtonEvent { action: Action::StarNext, down: true });
    }
    if scroll.delta.y > 0.5 {
        out.push(ButtonEvent { action: Action::StarPrev, down: true });
    }
    out.extend(pad_out.events.iter().copied());
    for e in out {
        lock.tracker.apply(&e);
        f.buttons.push(e);
    }
}

/// The mapper's view of a bevy gamepad, and whether anything on it is active (stick past a rough deadzone, a trigger or a
/// button down).
fn pad_snapshot(g: &Gamepad) -> (shinobi_client::gamepad::PadSnapshot, bool) {
    use shinobi_client::gamepad::{Pad, PadSnapshot};
    let mut s = PadSnapshot::default();
    let (l, r) = (g.left_stick(), g.right_stick());
    s.left = (l.x, l.y);
    s.right = (r.x, r.y);
    s.lt = g.get(GamepadButton::LeftTrigger2).unwrap_or(0.0);
    s.rt = g.get(GamepadButton::RightTrigger2).unwrap_or(0.0);
    for (b, p) in [
        (GamepadButton::South, Pad::South),
        (GamepadButton::East, Pad::East),
        (GamepadButton::West, Pad::West),
        (GamepadButton::North, Pad::North),
        (GamepadButton::LeftTrigger, Pad::LeftBumper),
        (GamepadButton::RightTrigger, Pad::RightBumper),
        (GamepadButton::DPadRight, Pad::DPadRight),
        (GamepadButton::DPadUp, Pad::DPadUp),
        (GamepadButton::DPadLeft, Pad::DPadLeft),
        (GamepadButton::Start, Pad::Start),
    ] {
        s.set(p, g.pressed(b));
    }
    let active = l.length() > 0.3 || r.length() > 0.3 || s.lt > 0.3 || s.rt > 0.3 || g.get_pressed().next().is_some();
    (s, active)
}

/// OS window title follows the lock state ("click the window to play" until the mouse is grabbed).
fn update_title(lock: Res<Lock>, window: Option<Single<&mut Window>>, time: Res<Time<Real>>, mut fps: Local<(f32, u32, u32)>, mut rep: Local<(f32, f32, u32)>, mut ft: Local<Vec<f32>>, clock: Res<crate::net::DisplayClockRes>) {
    let Some(mut w) = window else { return };
    // smoothed frame rate in the title (refreshed twice a second) so a slow machine is visible at a glance
    fps.0 += time.delta_secs();
    fps.1 += 1;
    // every 3 s: one line on stderr with the real numbers (fps, worst frame, fixed steps per second, which should be 60)
    rep.0 += time.delta_secs();
    rep.1 = rep.1.max(time.delta_secs());
    rep.2 += 1;
    ft.push(time.delta_secs());
    if rep.0 >= 3.0 {
        let steps = crate::net::FIXED_STEPS.swap(0, std::sync::atomic::Ordering::Relaxed);
        // frame-time spread: stddev and p99 (ms). Smooth motion needs an even frame time, not just a high fps
        let mean = ft.iter().sum::<f32>() / ft.len().max(1) as f32;
        let sd = (ft.iter().map(|d| (d - mean).powi(2)).sum::<f32>() / ft.len().max(1) as f32).sqrt();
        ft.sort_by(|a, b| a.total_cmp(b));
        let p99 = ft.get(((ft.len() as f32 * 0.99) as usize).min(ft.len().saturating_sub(1))).copied().unwrap_or(0.0);
        eprintln!(
            "PERF {:.0} fps | frame ms: mean {:.1} sd {:.1} p99 {:.1} worst {:.1} | fixed steps/s {:.0} (want 60) | display clock {} lag {:.2} ticks",
            rep.2 as f32 / rep.0, mean * 1000.0, sd * 1000.0, p99 * 1000.0, rep.1 * 1000.0, steps as f32 / rep.0,
            if clock.0.raw { "RAW" } else { "smoothed" }, clock.1
        );
        ft.clear();
        *rep = (0.0, 0.0, 0);
    }
    if fps.0 >= 0.5 {
        fps.2 = (fps.1 as f32 / fps.0).round() as u32;
        fps.0 = 0.0;
        fps.1 = 0;
    }
    let t = format!("{}  |  {} fps", overlay::window_title(lock.p.locked()), fps.2);
    if w.title != t {
        w.title = t;
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
    mut look_mats: ResMut<crate::character_look::LookMats>,
    hitfx: Res<crate::hit_feedback::HitFx>,
    time: Res<Time>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut tf: Query<&mut Transform>,
    mut vis: Query<&mut Visibility>,
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
            let outlines = std::array::from_fn(|_| {
                commands
                    .spawn((Mesh3d(look.cube.clone()), MeshMaterial3d(look.outline.clone()), Transform::default(), ChildOf(root)))
                    .id()
            });
            let beacon = commands
                .spawn((Mesh3d(look.cube.clone()), MeshMaterial3d(look.hostile.clone()), Transform::default(), Visibility::Hidden, ChildOf(root)))
                .id();
            let aura = commands
                .spawn((Mesh3d(look.sphere.clone()), MeshMaterial3d(look.hostile.clone()), Transform::default(), ChildOf(root)))
                .id();
            let gem = commands
                .spawn((Mesh3d(look.cube.clone()), MeshMaterial3d(look.hostile.clone()), Transform::default(), Visibility::Hidden, ChildOf(root)))
                .id();
            let shield = commands.spawn((Transform::default(), Visibility::Hidden, ChildOf(root))).id();
            let shield_panel = commands
                .spawn((Mesh3d(look.cube.clone()), MeshMaterial3d(look.hostile.clone()), Transform::default(), ChildOf(shield)))
                .id();
            let shield_bars = std::array::from_fn(|_| {
                commands.spawn((Mesh3d(look.cube.clone()), MeshMaterial3d(look.hostile.clone()), Transform::default(), ChildOf(shield))).id()
            });
            let rig = crate::character_look::Rig::spawn(&mut commands, root, &look.cube, None);
            RemoteEnt { root, parts, outlines, colour: None, beacon, aura, gem, shield, shield_panel, shield_bars, rig, spec: None, flashing: false }
        });
        // custom look (replicated by the server): rebuild the rig when it arrives or changes, then re-skin below
        let want = s.game.net.look_of(*id).filter(|c| !c.is_default());
        if want != ent.spec.as_ref() {
            ent.rig.despawn(&mut commands);
            ent.spec = want.cloned();
            ent.rig = crate::character_look::Rig::spawn(&mut commands, ent.root, &look.cube, ent.spec.as_ref());
            ent.colour = None;
            ent.flashing = false;
        }
        // player colour (team colour in team modes): re-skin the body parts only when it changes
        let hue = crate::team::colour_of(&net, *id);
        let k = crate::team::key(hue);
        let flashing = hitfx.flashing(*id);
        if !flashing && ent.flashing {
            ent.flashing = false;
            ent.colour = None; // flash over: reskin below
        }
        if ent.colour != Some(k) && !flashing {
            ent.colour = Some(k);
            ent.rig.skin(&mut commands, &mut look_mats, &mut mats, hue, ent.spec.as_ref());
            let m = body_mats.0.entry(k).or_insert_with(|| {
                mats.add(StandardMaterial {
                    base_color: {
                        let c = body_colour(hue);
                        Color::linear_rgb(c[0], c[1], c[2])
                    },
                    unlit: true,
                    ..default()
                })
            });
            for e in &ent.parts {
                commands.entity(*e).insert(MeshMaterial3d(m.clone()));
            }
            let bm: Handle<StandardMaterial> = body_mats.1.entry(k).or_insert_with(|| {
                mats.add(StandardMaterial {
                    base_color: Color::linear_rgb(hue[0] * 4.0, hue[1] * 4.0, hue[2] * 4.0),
                    unlit: true,
                    alpha_mode: AlphaMode::Add,
                    ..default()
                })
            }).clone();
            commands.entity(ent.beacon).insert(MeshMaterial3d(bm.clone()));
            commands.entity(ent.gem).insert(MeshMaterial3d(bm.clone()));
            let am = body_mats.2.entry(k).or_insert_with(|| {
                mats.add(StandardMaterial {
                    base_color: Color::linear_rgb(hue[0] * 0.38, hue[1] * 0.38, hue[2] * 0.38),
                    unlit: true,
                    alpha_mode: AlphaMode::Add,
                    ..default()
                })
            });
            commands.entity(ent.aura).insert(MeshMaterial3d(am.clone()));
            let sm = body_mats.3.entry(k).or_insert_with(|| {
                mats.add(StandardMaterial {
                    base_color: Color::linear_rgb(hue[0] * 1.3, hue[1] * 1.3, hue[2] * 1.3),
                    unlit: true,
                    alpha_mode: AlphaMode::Add,
                    ..default()
                })
            });
            commands.entity(ent.shield_panel).insert(MeshMaterial3d(sm.clone()));
            for b in &ent.shield_bars {
                commands.entity(*b).insert(MeshMaterial3d(bm.clone())); // the frame: the hot additive colour
            }
        }
        // hit flash: every body part and rig role goes bright white for a moment
        if flashing && !ent.flashing {
            ent.flashing = true;
            for e in &ent.parts {
                commands.entity(*e).insert(MeshMaterial3d(hitfx.white.clone()));
            }
            ent.rig.flash(&mut commands, &hitfx.white);
        }
        // distance beacon: a light column above the head once the player is far enough to be a few pixels
        let dist = Vec3::from(v.eye).distance(Vec3::from(d.pos));
        if let Ok(mut bt) = tf.get_mut(ent.beacon) {
            let w = beacon_width(dist);
            bt.translation = Vec3::new(0.0, 4.2, 0.0);
            bt.scale = Vec3::new(w, 6.5, w);
        }
        // aura: soft glow ellipsoid hugging the body, wider with distance
        if let Ok(mut at) = tf.get_mut(ent.aura) {
            // hugging the body (a rim of colour, not a ghost blob over it); the black outline and bright core carry the read
            let w = 0.8 * (1.0 + (aura_scale(dist) - 1.0) * 0.6);
            at.translation = Vec3::ZERO;
            at.scale = Vec3::new(w, 1.8 * (1.0 + (aura_scale(dist) - 1.0) * 0.25), w);
        }
        // block shield: a glowing panel held out in front (local -Z is the facing), shown while the player guards
        // a big framed panel (bigger with distance so it still reads): that is the whole "block pose" of a remote player
        {
            let flicker = 1.0 + 0.05 * (t * 17.0).sin();
            let g = 1.0 + 0.25 * (dist / 40.0).min(1.5);
            let (w, h) = (1.7 * g * flicker, 2.1 * g);
            let fb = 0.07 * g;
            if let Ok(mut sh) = tf.get_mut(ent.shield) {
                sh.translation = Vec3::new(0.0, 0.1, -0.75);
            }
            if let Ok(mut sp) = tf.get_mut(ent.shield_panel) {
                sp.scale = Vec3::new(w, h, 0.04);
            }
            let bars = [(Vec3::new(0.0, h * 0.5, 0.0), Vec3::new(w, fb, 0.06)), (Vec3::new(0.0, -h * 0.5, 0.0), Vec3::new(w, fb, 0.06)),
                        (Vec3::new(w * 0.5, 0.0, 0.0), Vec3::new(fb, h, 0.06)), (Vec3::new(-w * 0.5, 0.0, 0.0), Vec3::new(fb, h, 0.06))];
            for (e, (p, sc)) in ent.shield_bars.iter().zip(bars) {
                if let Ok(mut bt) = tf.get_mut(*e) {
                    bt.translation = p;
                    bt.scale = sc;
                }
            }
        }
        if let Ok(mut sv) = vis.get_mut(ent.shield) {
            sv.set_if_neq(if st.fx & shinobi_proto::snapshot::FX_BLOCK != 0 { Visibility::Inherited } else { Visibility::Hidden });
        }
        // head gem: a spinning diamond above the head at mid range
        if let Ok(mut gt) = tf.get_mut(ent.gem) {
            gt.translation = Vec3::new(0.0, 1.25 + 0.012 * dist + 0.05 * (t * 3.0).sin(), 0.0);
            gt.rotation = Quat::from_euler(EulerRot::YXZ, t * 2.4, 0.6, 0.78);
            gt.scale = Vec3::splat(gem_size(dist));
        }
        if let Ok(mut gv) = vis.get_mut(ent.gem) {
            gv.set_if_neq(if dist > 3.0 && dist < GEM_MAX_DIST { Visibility::Inherited } else { Visibility::Hidden });
        }
        if let Ok(mut bv) = vis.get_mut(ent.beacon) {
            bv.set_if_neq(if dist > BEACON_MIN_DIST { Visibility::Inherited } else { Visibility::Hidden });
        }

        if let Ok(mut r) = tf.get_mut(ent.root) {
            r.translation = Vec3::from(d.pos) + hitfx.shake(*id) + Vec3::Y * ent.rig.foot_lift();
            r.scale = ent.rig.root_scale();
            r.rotation = Quat::from_rotation_y(d.yaw) * Quat::from_rotation_z(d.roll);
        }
        let (bw, bh) = size_boost(dist);
        let grow = outline_grow(dist);
        for ((e, o), p) in ent.parts.iter().zip(ent.outlines.iter()).zip(d.parts.iter()) {
            let half = Vec3::new(p.half[0] * bw, p.half[1] * bh, p.half[2] * bw);
            if let Ok(mut pt) = tf.get_mut(*e) {
                pt.translation = Vec3::from(p.offset);
                pt.rotation = Quat::from_rotation_x(p.pitch);
                pt.scale = half * 2.0;
            }
            if let Ok(mut ot) = tf.get_mut(*o) {
                ot.translation = Vec3::from(p.offset);
                ot.rotation = Quat::from_rotation_x(p.pitch);
                ot.scale = (half + Vec3::splat(grow)) * 2.0;
            }
        }
        let vel = Vec3::new(st.vel[0] as f32, st.vel[1] as f32, st.vel[2] as f32) / 64.0;
        let anchor = s.game.net.rope_anchor(*id).map(Vec3::from);
        ent.rig.update(
            &crate::character_look::Ctx { draw: &d, kind: st.kind, flags: st.flags, fx: st.fx, vel, dist, dt: time.delta_secs(), t, boost: (bw, bh), anchor },
            &mut tf,
            &mut vis,
        );
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
        // `v.camera.fov` already carries the bow's zoom; the dash FOV kick fades out with it
        p.fov = (base + vm.katana.fov_kick_deg() * (1.0 - v.zoom.frac)).to_radians();
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
    fn aura_grows_with_distance_but_is_bounded() {
        assert!(aura_scale(50.0) > aura_scale(10.0));
        assert_eq!(aura_scale(0.0), 1.0);
        assert!(aura_scale(500.0) <= 4.0);
    }

    #[test]
    fn core_is_bright_and_never_darker_than_the_hue() {
        for hue in [[0.0, 1.0, 1.0], [1.0, 0.1, 0.8], [0.2, 0.2, 1.0]] {
            let c = body_colour(hue);
            for i in 0..3 {
                assert!(c[i] >= hue[i], "core lifts toward white");
            }
            // luminance floor independent of hue (cyan on a cyan floor must still read)
            assert!(0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2] > 0.55, "{c:?}");
        }
    }

    #[test]
    fn far_players_are_fattened_and_rimmed_within_bounds() {
        assert_eq!(size_boost(5.0), (1.0, 1.0));
        let (w, h) = size_boost(60.0);
        assert!(w > 2.0 && w <= 3.0 && h < w);
        assert!(outline_grow(60.0) > outline_grow(5.0));
    }

    #[test]
    fn far_markers_keep_a_screen_size() {
        // px per metre at 540 px / 90 deg = 270 / dist: marker width in px stays >= 3 out to the gem's range
        for d in [20.0f32, 60.0, 110.0] {
            assert!(gem_size(d) * 270.0 / d >= 3.0, "gem at {d} m");
            assert!(beacon_width(d) * 270.0 / d >= 3.0, "beacon at {d} m");
        }
    }

    #[test]
    fn beacon_widens_with_distance() {
        assert!(beacon_width(60.0) > beacon_width(20.0));
        assert!(beacon_width(0.0) > 0.0);
    }
}

#[cfg(test)]
mod binding_tests {
    use super::*;

    fn events_for_press(code: Option<KeyCode>, btn: Option<MouseButton>) -> (Vec<ButtonEvent>, Vec<ButtonEvent>) {
        let (mut keys, mut mouse) = (ButtonInput::<KeyCode>::default(), ButtonInput::<MouseButton>::default());
        if let Some(c) = code {
            keys.press(c);
        }
        if let Some(b) = btn {
            mouse.press(b);
        }
        let down = action_events(&keys, &mouse);
        keys.clear();
        mouse.clear();
        if let Some(c) = code {
            keys.release(c);
        }
        if let Some(b) = btn {
            mouse.release(b);
        }
        (down, action_events(&keys, &mouse))
    }

    /// Every block binding maps to the Block button: a down edge on press, an up edge on release.
    #[test]
    fn rmb_ctrl_middle_and_back_all_map_to_the_block_button() {
        for (code, btn) in [
            (Some(KeyCode::ControlLeft), None),
            (Some(KeyCode::ControlRight), None),
            (None, Some(MouseButton::Right)),
            (None, Some(MouseButton::Middle)),
            (None, Some(MouseButton::Back)),
        ] {
            let (down, up) = events_for_press(code, btn);
            assert_eq!(down, vec![ButtonEvent { action: Action::Block, down: true }], "{code:?} {btn:?} press");
            assert_eq!(up, vec![ButtonEvent { action: Action::Block, down: false }], "{code:?} {btn:?} release");
        }
        // F and V no longer block; Q throws a star, 4 rolls, LMB slices
        for code in [KeyCode::KeyF, KeyCode::KeyV] {
            assert!(events_for_press(Some(code), None).0.is_empty(), "{code:?} is unbound");
        }
        assert_eq!(events_for_press(Some(KeyCode::KeyQ), None).0, vec![ButtonEvent { action: Action::Fire, down: true }]);
        assert_eq!(events_for_press(Some(KeyCode::Digit4), None).0, vec![ButtonEvent { action: Action::Roll, down: true }]);
        // and the attack buttons are untouched
        let (d, _) = events_for_press(None, Some(MouseButton::Left));
        assert_eq!(d, vec![ButtonEvent { action: Action::Slice, down: true }]);
    }
}
