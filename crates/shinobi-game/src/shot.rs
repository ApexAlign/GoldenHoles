//! Offscreen renders of the real scene (grid, sky, CRT, bloom, walls, remote ninjas, viewmodel, HUD) with no
//! window and no winit: render to an image, read it back from the GPU, write a PPM.
//!
//!   --shot OUT.ppm          live: connects to a server (needs one running); timing follows the wall clock
//!   --shot-offline OUT.ppm  deterministic: no network, synthetic snapshots, manual 60 Hz clock, world frozen
//!                           at capture. This is the one the golden-image regression compares.

use std::net::SocketAddr;
use std::time::Duration;

use bevy::app::ScheduleRunnerPlugin;
use bevy::prelude::*;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::time::TimeUpdateStrategy;
use bevy::window::ExitCondition;
use bevy::winit::WinitPlugin;

use shinobi_proto::datagram::Welcome;
use shinobi_proto::snapshot::{EntityState, MoveKind, Snapshot};

use shinobi_client::input::TouchEvent;
use shinobi_client::quality::Quality;
use shinobi_client::touch_ui::TouchButton;

use crate::net::{Net, NetPlugin, PendingInput};
use crate::play::{FxRes, PlayPlugin, ShotImage, ShotRequest};
use crate::mobile::{MobilePlugin, TouchRes};
use crate::{crt, grid, hud_view, sky, vm};

const W: u32 = 960;
const H: u32 = 540;

/// Options shared by both shot modes.
pub struct ShotOpts {
    pub idle: bool,
    pub touch: bool,
    pub quality: Quality,
    pub scale: Option<f32>,
    pub level: shinobi_sim::level::LevelDesc,
    /// Offline scene: which level spawn point we stand on.
    pub spawn: usize,
}

#[derive(Resource)]
struct ShotCfg {
    out: String,
    secs: f32,
    done: bool,
    slashed: bool,
    fired: bool,
    offline: bool,
}

pub fn base_app(server: SocketAddr, offline: bool, o: &ShotOpts, dims: (u32, u32)) -> App {
    let mut app = App::new();
    if let Some(f) = o.scale {
        app.insert_resource(crate::play::ForcedScale(f));
    }
    app.add_plugins(
        DefaultPlugins
            .build()
            .disable::<WinitPlugin>()
            .set(WindowPlugin { primary_window: None, exit_condition: ExitCondition::DontExit, ..default() }),
    )
    .add_plugins(NetPlugin { server, offline, level: o.level.clone() })
    .add_plugins(MobilePlugin { quality: o.quality, touch_overlay: o.touch, screen: (dims.0 as f32, dims.1 as f32) })
    .add_plugins(grid::GridPlugin)
    .add_plugins(crt::CrtPlugin)
    .add_plugins(sky::SkyPlugin)
    .add_plugins(PlayPlugin)
    .add_plugins((vm::VmPlugin, crate::discs::DiscPlugin, crate::impacts::ImpactPlugin, crate::rope::RopePlugin, crate::team::TeamPlugin, crate::blocks::BlocksPlugin, crate::lock::LockPlugin))
    .add_plugins(hud_view::HudPlugin)
    .insert_resource(ShotRequest { width: dims.0, height: dims.1 });
    app
}

pub fn run(server: SocketAddr, secs: f32, out: String, o: ShotOpts) {
    let idle = o.idle;
    let mut app = base_app(server, false, &o, (W, H));
    app.add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(1.0 / 60.0)))
        .insert_resource(ShotCfg { out, secs, done: false, slashed: idle, fired: idle, offline: false })
        .add_systems(Update, drive);
    app.run();
}

pub fn run_offline(secs: f32, out: String, o: ShotOpts) {
    let idle = o.idle;
    let dummy: SocketAddr = "127.0.0.1:9".parse().unwrap(); // never used: offline sessions send nothing
    let mut app = base_app(dummy, true, &o, (W, H));
    app.add_plugins(ScheduleRunnerPlugin::run_loop(Duration::ZERO))
        // exactly 1/60 s per update: the fixed clock then advances one tick per frame, independent of CPU speed
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(1.0 / 60.0)))
        .insert_resource(ShotCfg { out, secs, done: false, slashed: idle, fired: idle, offline: true })
        .insert_resource(OfflineSpawn(o.level.spawn(o.spawn)))
        .add_systems(Startup, connect_offline)
        .add_systems(PreUpdate, inject_fx)
        .add_systems(FixedUpdate, (offline_world, offline_touch))
        .add_systems(Update, drive);
    app.run();
}

#[derive(Resource)]
struct OfflineSpawn([f32; 3]);

/// Visual-review aid: `SHOT_FX=lock:0.5,locked,break:0.4,pieces,discs` (comma list) overlays synthetic lock-on state,
/// pieces and discs on the published view so the offline shot shows them. Unset = untouched (goldens stay exact).
fn inject_fx(mut view: ResMut<crate::net::ViewRes>, base: Res<OfflineSpawn>) {
    use shinobi_client::client_game::LockOn;
    use shinobi_sim::blocks::{BlockInfo, Piece, SCALE_ONE};
    use shinobi_sim::combat::aim_assist::LockPhase;
    let [bx, by, bz] = base.0;
    let v = &mut view.0;
    // SHOT_EYE=x,y,z and SHOT_LOOK=yaw,pitch (radians) move the camera for review shots of edges, ceilings, etc.
    let nums = |k: &str| std::env::var(k).ok().map(|s| s.split(',').filter_map(|n| n.parse::<f32>().ok()).collect::<Vec<_>>());
    if let Some(e) = nums("SHOT_EYE").filter(|e| e.len() == 3) {
        v.eye = [e[0], e[1], e[2]];
    }
    if let Some(l) = nums("SHOT_LOOK").filter(|l| l.len() == 2) {
        v.aim.yaw = l[0];
        v.aim.pitch = l[1];
    }
    let Ok(spec) = std::env::var("SHOT_FX") else { return };
    for item in spec.split(',') {
        let (name, arg) = item.split_once(':').unwrap_or((item, ""));
        let p: f32 = arg.parse().unwrap_or(0.5);
        match name {
            "lock" | "locked" | "break" => {
                // remote 4 hovers at (+8, ~1.6, -14) of our spawn: aim the triangle at its chest
                let pos = v.remotes.iter().find(|(id, _)| *id == 4).map_or([bx + 8.0, by + 1.4, bz - 14.0], |(_, s)| [s.pos[0], s.pos[1] + 0.2, s.pos[2]]);
                v.lock = Some(LockOn {
                    target: 4,
                    pos,
                    progress: if name == "locked" { 1.0 } else { p },
                    locked: name == "locked",
                    phase: match name {
                        "locked" => LockPhase::Locked,
                        "break" => LockPhase::Break,
                        _ => LockPhase::Acquire,
                    },
                });
            }
            "pieces" => {
                let sz = v.block_size.max(0.5);
                let c = |x: i32, y: i32, z: i32| [(bx / sz).floor() as i32 + x * 2, (by / sz).floor() as i32 + y, (bz / sz).floor() as i32 + z * 2];
                let pieces = [
                    (2, c(-2, 0, -2), Piece::FLOOR),
                    (3, c(-1, 0, -3), Piece::WALL_X),
                    (5, c(1, 0, -3), Piece::WALL_Z),
                    (7, c(0, 0, -4), Piece { kind: shinobi_sim::blocks::PieceKind::Ramp, axis: 0 }),
                    (9, c(2, 0, -2), Piece { kind: shinobi_sim::blocks::PieceKind::Ramp, axis: 1 }),
                ];
                for (owner, cell, piece) in pieces {
                    v.blocks.push(BlockInfo { owner, cell, piece, scale_q: SCALE_ONE });
                }
            }
            "rope" => {
                let e = v.eye;
                v.rope = Some((e, [e[0] + 4.0, e[1] + 7.0, e[2] - 18.0]));
                v.fx.grappling = true;
            }
            "discs" => {
                v.discs.push(([bx - 2.0, by + 1.0, bz - 6.0], [0.0, 0.0, -1.0]));
                v.disc_owners.push(2);
                v.discs.push(([bx + 2.0, by + 1.2, bz - 8.0], [0.0, 0.0, -1.0]));
                v.disc_owners.push(5);
            }
            _ => {}
        }
    }
}

fn connect_offline(mut net: ResMut<Net>) {
    let _ = net.0.game.on_datagram(&Welcome { player_id: 1, server_tick: 0 }.encode(), 0.0);
}

/// 1/512 m, 1/64 m/s, yaw 0..65535 = 0..2pi (yaw 0 faces -Z).
fn ent(pos: [f32; 3], vel: [f32; 3], kind: MoveKind) -> EntityState {
    let q = |v: f32, s: f32| (v * s).round();
    EntityState {
        pos: [q(pos[0], 512.0) as i32, q(pos[1], 512.0) as i32, q(pos[2], 512.0) as i32],
        vel: [q(vel[0], 64.0) as i16, q(vel[1], 64.0) as i16, q(vel[2], 64.0) as i16],
        yaw: 0,
        kind,
        flags: 0,
    }
}

/// Scripted world, a pure function of the tick: us standing still, a runner, a dasher and a jumper.
fn offline_world(mut net: ResMut<Net>, base: Res<OfflineSpawn>) {
    net.0.advance_clock();
    let t = net.0.local_tick();
    let sec = (t / 60.0) as f32;
    let [bx, by, bz] = base.0;
    // others are placed relative to our spawn so every level gets the same composition
    let at = |dx: f32, dy: f32, dz: f32| [bx + dx, by + dy, bz + dz];
    let snap = Snapshot {
        tick: t as u32,
        entities: vec![
            (1, ent(at(0.0, 0.0, 0.0), [0.0; 3], MoveKind::Grounded)),
            (2, ent(at(4.0, 0.0, -3.0 - 9.0 * sec), [0.0, 0.0, -9.0], MoveKind::Grounded)),
            (3, ent(at(-3.5, 0.0, -6.0 - 14.0 * sec.min(1.2)), [0.0, 0.0, -14.0], MoveKind::Evading)),
            (4, ent(at(8.0, 1.6 + 1.2 * (sec * 3.0).sin(), -14.0), [0.0; 3], MoveKind::Airborne)),
        ],
    };
    net.0.game.net.interp.push(snap, t);
}

fn drive(
    time: Res<Time>,
    mut commands: Commands,
    mut cfg: ResMut<ShotCfg>,
    mut fx: ResMut<FxRes>,
    img: Option<Res<ShotImage>>,
    mut strategy: Option<ResMut<TimeUpdateStrategy>>,
) {
    let t = time.elapsed_secs();
    let lead: f32 = std::env::var("SHOT_SLASH_LEAD").ok().and_then(|v| v.parse().ok()).unwrap_or(0.12);
    if !cfg.slashed && t >= cfg.secs - lead {
        fx.slash_edge = true;
        cfg.slashed = true;
    }
    if !cfg.fired && t >= cfg.secs - 0.08 {
        fx.fire_edge = true;
        cfg.fired = true;
    }
    let Some(img) = img else { return };
    if t >= cfg.secs && !cfg.done {
        cfg.done = true;
        if cfg.offline {
            // freeze the world so every frame until the readback lands is identical
            if let Some(s) = strategy.as_mut() {
                **s = TimeUpdateStrategy::ManualDuration(Duration::ZERO);
            }
        }
        let out = cfg.out.clone();
        commands.spawn(Readback::texture(img.0.clone())).observe(
            move |ev: On<ReadbackComplete>, mut exit: MessageWriter<AppExit>| {
                write_ppm(&out, &ev.data);
                exit.write(AppExit::Success);
            },
        );
    }
}

fn write_ppm(path: &str, rgba: &[u8]) {
    use std::io::Write;
    let mut rgb = Vec::with_capacity((W * H * 3) as usize);
    let (mut lit, mut bright, mut max) = (0u64, 0u64, 0u8);
    for px in rgba.chunks_exact(4).take((W * H) as usize) {
        rgb.extend_from_slice(&px[..3]);
        let m = px[0].max(px[1]).max(px[2]);
        max = max.max(m);
        lit += (m > 8) as u64;
        bright += (m > 200) as u64;
    }
    let mut f = std::fs::File::create(path).expect("create ppm");
    write!(f, "P6\n{W} {H}\n255\n").unwrap();
    f.write_all(&rgb).unwrap();
    let n = (W * H) as f64;
    println!(
        "shot: {path} bytes_read={} lit={:.1}% bright={:.2}% max_channel={}",
        rgba.len(), lit as f64 / n * 100.0, bright as f64 / n * 100.0, max
    );
}

/// `--touch`: scripted fingers (a pure function of the tick): left thumb pushes the stick forward-right,
/// another finger holds Jump, a later one taps the shuriken button. Only runs when the overlay is on.
fn offline_touch(net: Res<Net>, mut touch: ResMut<TouchRes>, mut pending: ResMut<PendingInput>) {
    if !touch.visible {
        return;
    }
    let t = net.0.local_tick() as u32;
    let b = &mut pending.0.buttons;
    let jump = touch.s.ui.layout.slot(TouchButton::Jump).center;
    let shur = touch.s.ui.layout.slot(TouchButton::Shuriken).center;
    match t {
        20 => touch.s.event(TouchEvent::Down { id: 1, x: 150.0, y: 420.0 }, b),
        25 => touch.s.event(TouchEvent::Down { id: 2, x: jump[0], y: jump[1] }, b),
        60 => touch.s.event(TouchEvent::Down { id: 3, x: shur[0], y: shur[1] }, b),
        21..=50 => {
            let k = (t - 20) as f32 / 30.0;
            touch.s.event(TouchEvent::Move { id: 1, x: 150.0 + 45.0 * k, y: 420.0 - 55.0 * k }, b);
        }
        _ => {}
    }
    pending.0.buttons.clear(); // offline: nothing drains the game's input, so keep the queue from growing
}
