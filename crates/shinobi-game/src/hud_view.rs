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
use shinobi_client::screens::{self, MenuItem, Screen, SettingRow};
use shinobi_client::console::{self, Console};
use shinobi_client::hud::BuildSlot;
use shinobi_client::input_mode::Mode;
use shinobi_client::perf::{self, Perf};
use shinobi_client::radial;
use shinobi_client::settings::Settings;
use bevy::input::keyboard::{Key, KeyboardInput};
use shinobi_client::overlay;

use crate::net::Net;
use crate::play::{ShotImage, ShotRequest};

const LAYER: usize = 2;

#[derive(Resource)]
struct HudMesh(Handle<Mesh>);

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        let (settings, warnings) = Settings::load();
        for w in warnings {
            eprintln!("settings: {w}");
        }
        app.insert_resource(HudCtl { bridge: HudBridge::default(), watch: ConnWatch::default(), perf: Perf::default(), screen: Screen::InGame, reconnect: false, last_sig: None })
            .insert_resource(SettingsRes(settings))
            .init_resource::<MenuRes>()
            .init_resource::<ConsoleRes>()
            .add_systems(Startup, enable_radial)
            .add_systems(Startup, spawn.after(crate::play::setup))
            .add_systems(Update, (menu_input.before(crate::play::input), reconnect_system, mode_ui.after(crate::play::input), sync_settings, sync_audio, perf_tick, update).chain());
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

/// Player settings: loaded at startup, edited from the pause menu, saved when the settings screen closes.
#[derive(Resource)]
pub struct SettingsRes(pub Settings);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Closed,
    Main(usize),
    Settings(usize),
    Board,
}

/// Pause menu state. Opens when the pointer lock is lost (Esc / focus loss); `resume` asks `play::input` to
/// re-grab the pointer with a swallowed click.
#[derive(Resource)]
pub struct MenuRes {
    pub menu: Menu,
    pub resume: bool,
}

impl Default for MenuRes {
    fn default() -> Self {
        Self { menu: Menu::Closed, resume: false }
    }
}

impl MenuRes {
    pub fn open(&self) -> bool {
        self.menu != Menu::Closed
    }
}

fn cursor_norm(window: &Option<Single<&Window>>) -> Option<(f32, f32)> {
    let w = window.as_ref()?;
    let c = w.cursor_position()?;
    Some((c.x / w.width().max(1.0), c.y / w.height().max(1.0)))
}

fn save(s: &Settings) {
    if let Err(e) = s.save() {
        eprintln!("settings: save failed: {e}");
    }
}

/// Keyboard + mouse for the pause menu / settings / scoreboard screens.
fn menu_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    window: Option<Single<&Window>>,
    lock: Res<crate::play::Lock>,
    mut menu: ResMut<MenuRes>,
    mut settings: ResMut<SettingsRes>,
    mut exit: MessageWriter<AppExit>,
    mut prev_locked: Local<bool>,
    mut ctl: ResMut<HudCtl>,
) {
    // kicked / disconnected: Esc quits, Enter rebuilds the session once the kick cooldown is over
    let wait = match &ctl.screen {
        Screen::Kicked { retry_in_s, .. } => Some(*retry_in_s),
        Screen::Disconnected { .. } => Some(0.0),
        _ => None,
    };
    if let Some(retry_in_s) = wait {
        if keys.just_pressed(KeyCode::Escape) {
            exit.write(AppExit::Success);
        } else if (keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space)) && retry_in_s <= 0.0 {
            ctl.reconnect = true;
        }
        return;
    }
    let locked = lock.p.locked();
    if *prev_locked && !locked && menu.menu == Menu::Closed {
        menu.menu = Menu::Main(0);
    }
    *prev_locked = locked;

    let up = keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW);
    let down = keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS);
    let enter = keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space);
    let esc = keys.just_pressed(KeyCode::Escape);
    let click = mouse.just_pressed(MouseButton::Left);
    let cur = cursor_norm(&window);

    match menu.menu {
        Menu::Closed => {}
        Menu::Main(sel) => {
            let mut sel = sel;
            if up || down {
                sel = screens::menu_step(sel, down);
            }
            if let Some((x, y)) = cur {
                if let Some(i) = MenuItem::ALL.iter().position(|m| Some(*m) == screens::menu_hit(x, y)) {
                    if click {
                        sel = i;
                    }
                }
            }
            let hit = cur.and_then(|(x, y)| screens::menu_hit(x, y)).is_some();
            let activate = enter || (click && hit);
            menu.menu = Menu::Main(sel);
            if esc {
                menu.menu = Menu::Closed;
                menu.resume = true;
            } else if activate {
                match MenuItem::ALL[sel] {
                    MenuItem::Resume => {
                        menu.menu = Menu::Closed;
                        menu.resume = true;
                    }
                    MenuItem::Settings => menu.menu = Menu::Settings(0),
                    MenuItem::Scoreboard => menu.menu = Menu::Board,
                    MenuItem::Disconnect => {
                        exit.write(AppExit::Success);
                    }
                }
            }
        }
        Menu::Settings(sel) => {
            let n = SettingRow::ALL.len();
            let mut sel = sel;
            if up {
                sel = (sel + n - 1) % n;
            }
            if down {
                sel = (sel + 1) % n;
            }
            let row = SettingRow::ALL[sel];
            let before = settings.0;
            let st = &mut settings.0;
            if let Some(f) = screens::setting_frac(st, row) {
                let step = row.key_step();
                if keys.just_pressed(KeyCode::ArrowLeft) {
                    screens::settings_apply(st, row, f - step, false);
                }
                if keys.just_pressed(KeyCode::ArrowRight) {
                    screens::settings_apply(st, row, f + step, false);
                }
            } else if enter || keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::ArrowRight) {
                screens::settings_apply(st, row, 0.0, true);
            }
            if mouse.pressed(MouseButton::Left) {
                if let Some((row, frac)) = cur.and_then(|(x, y)| screens::settings_hit(x, y)) {
                    sel = SettingRow::ALL.iter().position(|r| *r == row).unwrap_or(sel);
                    screens::settings_apply(st, row, frac, click);
                }
            }
            menu.menu = Menu::Settings(sel);
            // persist every edit as soon as it is final (not mid-drag): a crash or kill never loses a tuned sensitivity
            if settings.0 != before && !mouse.pressed(MouseButton::Left) || mouse.just_released(MouseButton::Left) {
                save(&settings.0);
            }
            if esc {
                save(&settings.0);
                menu.menu = Menu::Main(1);
            }
        }
        Menu::Board => {
            if esc || enter || click {
                menu.menu = Menu::Main(2);
            }
        }
    }
}

/// Settings -> predicted game (look sensitivity, FOV, feel) and the touch overlay. Runs when they change.
fn sync_settings(
    settings: Res<SettingsRes>,
    mut net: ResMut<Net>,
    mut touch: ResMut<crate::mobile::TouchRes>,
    mut pending: ResMut<crate::net::PendingInput>,
) {
    if !settings.is_changed() {
        return;
    }
    net.0.game.apply_settings(&settings.0);
    touch.s.apply_settings(&settings.0, &mut pending.0.buttons);
}

/// Console state (typed line + log), shown in `Mode::Console`.
#[derive(Resource, Default)]
pub struct ConsoleRes(pub Console);

/// The radial build menu exists now: let Cam&Mov's mode machine open it on a build-key hold.
fn enable_radial(mut mode: ResMut<crate::play::ModeRes>) {
    mode.machine.cfg.radial_enabled = true;
    // shorter than Cam&Mov's 0.35 s default: build taps print on release, so the hold threshold is the added latency
    mode.machine.cfg.radial_hold_secs = 0.25;
}

/// Enter on the kicked / disconnected screen: `UdpGame::reconnect` (new socket, new HELLO, clean `ClientGame`) replaces
/// the dead session and every per-session HUD / input state is reset (live-tested in shinobi-server/tests/reconnect.rs). The player's settings are re-applied by `sync_settings`.
fn reconnect_system(
    mut ctl: ResMut<HudCtl>,
    mut net: ResMut<Net>,
    label: Option<Res<ServerLabel>>,
    level: Option<Res<crate::net::LevelRes>>,
    mut settings: ResMut<SettingsRes>,
    mut pending: ResMut<crate::net::PendingInput>,
    mut menu: ResMut<MenuRes>,
) {
    if !std::mem::take(&mut ctl.reconnect) {
        return;
    }
    let Some(level) = level else { return };
    match net.0.reconnect(level.0.clone(), shinobi_client::client_game::ClientGameCfg::default()) {
        Ok(()) => {
            ctl.watch = ConnWatch::default();
            ctl.bridge = HudBridge::default();
            ctl.screen = Screen::Connecting { elapsed_s: 0.0 };
            pending.0 = Default::default();
            menu.menu = Menu::Closed;
            settings.set_changed(); // re-apply sensitivity / fov / touch layout to the new game
            eprintln!("reconnecting to {}", label.as_ref().map_or("?", |l| l.0.as_str()));
        }
        Err(e) => eprintln!("reconnect failed: {e}"),
    }
}

/// F3 toggles the performance overlay; every frame feeds it the frame time, fixed steps run and the reconcile offset.
fn perf_tick(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time<Real>>,
    view: Res<crate::net::ViewRes>,
    mut ctl: ResMut<HudCtl>,
    mut last_steps: Local<u32>,
) {
    if keys.just_pressed(KeyCode::F3) {
        ctl.perf.toggle();
    }
    // FIXED_STEPS is a free-running counter that play.rs's PERF line also zeroes every 3 s: a smaller value = a reset
    let now = crate::net::FIXED_STEPS.load(std::sync::atomic::Ordering::Relaxed);
    let steps = if now >= *last_steps { now - *last_steps } else { now };
    *last_steps = now;
    let c = view.0.correction_offset;
    ctl.perf.record(time.delta_secs(), steps, (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt());
}

/// Pickup points of the loaded level file (the client's `LevelDesc` is geometry only): set once at start-up by
/// `main::load_level`, read by `sync_audio` into the bridge for the radar.
static LEVEL_PICKUPS: std::sync::OnceLock<Vec<[f32; 3]>> = std::sync::OnceLock::new();

pub fn set_level_pickups(p: Vec<[f32; 3]>) {
    let _ = LEVEL_PICKUPS.set(p);
}

/// Master volume / mute -> the telemetry speaker glyph (absent when the game runs without the audio plugin).
fn sync_audio(audio: Option<Res<crate::audio::AudioSettings>>, level: Option<Res<crate::net::LevelRes>>, mut ctl: ResMut<HudCtl>) {
    // the void edge on the radar: the level's floor extent (static, so cheap to refresh)
    ctl.bridge.map_half = level.and_then(|l| l.0.floor_half());
    if ctl.bridge.pickups.is_empty() {
        if let Some(p) = LEVEL_PICKUPS.get() {
            ctl.bridge.pickups = p.clone();
        }
    }
    ctl.bridge.audio = audio.map(|a| (a.volume, a.muted));
}

/// Interactive overlay modes: radial commit (print the highlighted piece) and console typing.
fn mode_ui(
    mode: Res<crate::play::ModeRes>,
    window: Option<Single<&Window>>,
    mut typed: MessageReader<KeyboardInput>,
    mut console: ResMut<ConsoleRes>,
    mut settings: ResMut<SettingsRes>,
    keys: Res<ButtonInput<KeyCode>>,
    ctl: Res<HudCtl>,
    mut net: ResMut<Net>,
    mut audio: Option<ResMut<crate::audio::AudioSettings>>,
) {
    // F1: bring the first-30-seconds controls strip back (or put it away)
    if keys.just_pressed(KeyCode::F1) {
        net.0.game.toggle_controls();
    }
    let Some(out) = mode.out else {
        typed.clear();
        return;
    };
    if out.radial_commit {
        // the hold replaced the instant tap-print (see play.rs): only the chosen piece prints, none on a cancel
        let aspect = window.as_ref().map_or(16.0 / 9.0, |w| w.width() / w.height().max(1.0));
        if let Some(slot) = cursor_norm(&window).and_then(|c| radial::pick_with(c, aspect)) {
            let kind = match slot {
                BuildSlot::Floor => shinobi_sim::blocks::PieceKind::Floor,
                BuildSlot::Ramp => shinobi_sim::blocks::PieceKind::Ramp,
                BuildSlot::Wall => shinobi_sim::blocks::PieceKind::Wall,
            };
            net.0.game.print_piece(kind);
        }
    }
    if mode.mode() != Mode::Console {
        typed.clear();
        return;
    }
    for ev in typed.read() {
        if !ev.state.is_pressed() {
            continue;
        }
        match &ev.logical_key {
            Key::Character(s) => s.chars().for_each(|c| console.0.type_char(c)),
            Key::Space => console.0.type_char(' '),
            Key::Backspace => console.0.backspace(),
            Key::Enter => {
                let me = net.0.game.net.player_id().unwrap_or(0);
                let kills = ctl.bridge.killfeed();
                if let Some(a) = audio.as_deref() {
                    console.0.audio = (a.volume, a.muted);
                }
                let res = console.0.submit(&mut settings.0, kills, me);
                if res.settings_changed {
                    save(&settings.0);
                }
                if let Some(a) = audio.as_deref_mut() {
                    for cmd in res.audio {
                        match cmd {
                            console::AudioCmd::Volume(vol) => a.volume = vol,
                            console::AudioCmd::Mute(m) => a.muted = m,
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Controllers that live across frames: the HUD state bridge and the connection watcher.
#[derive(Resource)]
struct HudCtl {
    bridge: HudBridge,
    watch: ConnWatch,
    /// F3 performance overlay state.
    perf: Perf,
    /// Last screen the connection watcher chose (the kicked / disconnected screens read their retry time from it).
    screen: Screen,
    /// Enter on a kicked / disconnected screen: `reconnect_system` rebuilds the session.
    reconnect: bool,
    /// Signature of the geometry last uploaded to the HUD mesh (skip identical uploads).
    last_sig: Option<u64>,
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
    menu: Res<MenuRes>,
    settings: Res<SettingsRes>,
    mode: Res<crate::play::ModeRes>,
    console: Res<ConsoleRes>,
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

    ctl.bridge.aspect = aspect;
    let mut state: HudState = ctl.bridge.update(dt, &view.0, &net.0.game.net, tab);
    let cur = match (&window, mode.mode()) {
        (Some(w), Mode::Scoreboard | Mode::BuildRadial) => w.cursor_position().map(|c| (c.x / w.width().max(1.0), c.y / w.height().max(1.0))),
        _ => None,
    };
    if mode.mode() == Mode::Scoreboard {
        state.cursor = cur;
    }
    let screen = ctl.watch.update(
        dt,
        &net.0.game.net,
        view.0.connect_failed,
        label.as_ref().map_or("?", |l| l.0.as_str()),
    );

    ctl.screen = screen.clone();
    let mut prims = Hud::build(&state);
    if let Some(t) = touch.as_ref().filter(|t| t.visible) {
        prims.extend(t.s.draw(state.dash_ready, state.shuriken_ready, &pal));
    }
    let t = time.elapsed_secs();
    if screen != Screen::InGame {
        // connecting / unreachable / connection lost / kicked: never a blank view
        prims.extend(screens::draw(&screen, &pal, t));
    } else if menu.open() && shot.is_none() {
        match menu.menu {
            Menu::Main(selected) => prims.extend(screens::draw(&Screen::Paused { selected }, &pal, t)),
            Menu::Settings(sel) => prims.extend(screens::draw_settings(&settings.0, sel, &pal)),
            Menu::Board => prims.extend(screens::draw(&Screen::Scoreboard { rows: state.scoreboard.clone() }, &pal, t)),
            Menu::Closed => {}
        }
    } else if !lock.p.locked() && shot.is_none() && window.is_some() {
        prims.extend(overlay::click_to_play(&pal, t));
    } else {
        match mode.mode() {
            Mode::BuildRadial => {
                let cur_slot = state.build.map_or(BuildSlot::Floor, |b| b.selected);
                prims.extend(radial::draw_with(cur.and_then(|c| radial::pick_with(c, aspect)), cur_slot, &pal, aspect));
            }
            Mode::Console => prims.extend(console::draw(&console.0, t, &pal)),
            _ => {}
        }
    }
    let mut prims = shinobi_client::hud::with_text_shadows(prims, &pal);
    if ctl.perf.visible {
        use shinobi_sim::combat::aim_assist::LockPhase;
        let lock = match view.0.lock.map(|l| l.phase) {
            Some(LockPhase::Acquire) => "ACQUIRE",
            Some(LockPhase::Locked) => "LOCKED",
            Some(LockPhase::Break) => "BREAK",
            _ => "SEARCH",
        };
        let c = view.0.correction_offset;
        let inputs = perf::PerfInputs { ping_ms: state.ping_ms, correction_m: (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt(), lock };
        prims.extend(perf::draw(&ctl.perf, &inputs, &pal));
    }
    let g = tessellate(&prims, aspect);

    // a replaced mesh is re-extracted and re-uploaded (buffer free + alloc) by the renderer: skip it when the HUD
    // geometry is bit-identical to last frame's (idle screens, paused menus)
    let sig = geometry_signature(&g.pos, &g.col, &g.idx);
    if ctl.last_sig == Some(sig) {
        return;
    }
    ctl.last_sig = Some(sig);

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

/// FNV-1a over the exact bits of a tessellated HUD (positions, colours, indices).
fn geometry_signature(pos: &[[f32; 2]], col: &[[f32; 4]], idx: &[u32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut eat = |v: u32| {
        h ^= v as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for p in pos {
        p.iter().for_each(|f| eat(f.to_bits()));
    }
    for c in col {
        c.iter().for_each(|f| eat(f.to_bits()));
    }
    idx.iter().for_each(|i| eat(*i));
    eat(pos.len() as u32);
    h
}

#[cfg(test)]
mod sig_tests {
    use super::*;

    #[test]
    fn signature_changes_with_any_vertex_colour_or_index() {
        let (p, c, i) = (vec![[0.0, 1.0], [2.0, 3.0]], vec![[1.0; 4], [0.5; 4]], vec![0u32, 1, 0]);
        let base = geometry_signature(&p, &c, &i);
        assert_eq!(base, geometry_signature(&p, &c, &i), "stable");
        let mut p2 = p.clone();
        p2[1][0] = 2.0001;
        assert_ne!(base, geometry_signature(&p2, &c, &i));
        let mut c2 = c.clone();
        c2[0][3] = 0.9;
        assert_ne!(base, geometry_signature(&p, &c2, &i));
        assert_ne!(base, geometry_signature(&p, &c, &[0, 1, 1]));
    }
}
