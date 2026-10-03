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
        app.insert_resource(HudCtl { bridge: HudBridge::default(), watch: ConnWatch::default() })
            .insert_resource(SettingsRes(settings))
            .init_resource::<MenuRes>()
            .init_resource::<ConsoleRes>()
            .add_systems(Startup, enable_radial)
            .add_systems(Startup, spawn.after(crate::play::setup))
            .add_systems(Update, (menu_input.before(crate::play::input), mode_ui.after(crate::play::input), sync_settings, update).chain());
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
) {
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
            let st = &mut settings.0;
            if let Some(f) = screens::setting_frac(st, row) {
                let step = 0.05;
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
}

/// Interactive overlay modes: radial commit (print the highlighted piece) and console typing.
fn mode_ui(
    mode: Res<crate::play::ModeRes>,
    window: Option<Single<&Window>>,
    mut typed: MessageReader<KeyboardInput>,
    mut console: ResMut<ConsoleRes>,
    mut settings: ResMut<SettingsRes>,
    ctl: Res<HudCtl>,
    mut net: ResMut<Net>,
) {
    let Some(out) = mode.out else {
        typed.clear();
        return;
    };
    if out.radial_commit {
        // the hold replaced the instant tap-print (see play.rs): only the chosen piece prints, none on a cancel
        if let Some(slot) = cursor_norm(&window).and_then(radial::pick) {
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
                if console.0.submit(&mut settings.0, kills, me) {
                    save(&settings.0);
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
                prims.extend(radial::draw(cur.and_then(radial::pick), cur_slot, &pal));
            }
            Mode::Console => prims.extend(console::draw(&console.0, t, &pal)),
            _ => {}
        }
    }
    let prims = shinobi_client::hud::with_text_shadows(prims, &pal);
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
