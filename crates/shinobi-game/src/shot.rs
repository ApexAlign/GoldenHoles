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
    .add_plugins(MobilePlugin { quality: o.quality, auto_tier: false, touch_overlay: o.touch, screen: (dims.0 as f32, dims.1 as f32) })
    .add_plugins(grid::GridPlugin)
    .add_plugins(crt::CrtPlugin)
    .add_plugins(sky::SkyPlugin)
    .add_plugins(PlayPlugin)
    .add_plugins(crate::env_look::EnvLookPlugin)
    .add_plugins((vm::VmPlugin, crate::discs::DiscPlugin, crate::impacts::ImpactPlugin, crate::rope::RopePlugin, crate::team::TeamPlugin, crate::blocks::BlocksPlugin, crate::lock::LockPlugin, crate::death::DeathFxPlugin, crate::hit_feedback::HitFeedbackPlugin, crate::hit_indicator::HitIndicatorPlugin, crate::star_fx::StarFxPlugin, crate::power_fx::PowerFxPlugin))
    .add_plugins(crate::destructible_fx::DestructibleFxPlugin)
    .add_plugins(hud_view::HudPlugin)
    .add_plugins(crate::destructible_audio::DestructibleAudioPlugin)
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

/// Review shots of the reworked weapons: `SHOT_WEAPONS=flash|hook` (static frames at ~2 s), plus `dazed` (white-out
/// clearing + the DAZED cue) and `hookaim` (crosshair on a player: red). Offline only; unset = untouched.
fn weapons_preview(v: &mut shinobi_client::client_game::RenderView, what: &str, frame: u32, base: [f32; 3]) {
    use shinobi_sim::events::{NetEvent, STAR_FX_FLASH_BURST};
    let [bx, by, bz] = base;
    let burst = |what: u8, dz: f32| NetEvent::StarFx { tick: 0, what, owner: 2, pos: shinobi_proto::quant::q_pos3([bx, by + 0.2, bz + dz]) };
    match what {
        "flash" if frame == 112 => v.events.push(burst(STAR_FX_FLASH_BURST, -7.0)),
        // the white-out clearing (45 % strength left) with the DAZED cue and its draining bar
        "dazed" => {
            v.blind = 0.45;
            v.dazed = 0.55;
        }
        // our rope has caught player 4: from frame 112 (the catch snaps on, then it throbs)
        "hook" | "hookaim" => {
            let target = v.remotes.iter().find(|(id, _)| *id == 4).map_or([bx + 8.0, by + 1.4, bz - 14.0], |(_, s)| [s.pos[0], s.pos[1] + 0.9, s.pos[2]]);
            if what == "hook" && frame >= 112 {
                v.rope = Some((v.eye, target));
                v.fx.grappling = true;
                v.fx.grapple_pulling_enemy = true;
            }
            v.fx.grapple_hookable = true;
            v.fx.grapple_target_player = what == "hookaim";
        }
        _ => {}
    }
}

/// Visual-review aid: `SHOT_FX=lock:0.5,locked,break:0.4,pieces,discs` (comma list) overlays synthetic lock-on state,
/// pieces and discs on the published view so the offline shot shows them. Unset = untouched (goldens stay exact).
fn inject_fx(mut commands: Commands, mut view: ResMut<crate::net::ViewRes>, base: Res<OfflineSpawn>, mut frame: Local<u32>) {
    *frame += 1;
    let frame = *frame;
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
    if let Ok(w) = std::env::var("SHOT_WEAPONS") {
        weapons_preview(v, &w, frame, base.0);
    }
    let Ok(spec) = std::env::var("SHOT_FX") else { return };
    for item in spec.split(',') {
        let (name, arg) = item.split_once(':').unwrap_or((item, ""));
        let p: f32 = arg.parse().unwrap_or(0.5);
        match name {
            "lock" | "locked" | "break" => {
                // `locked`: the Locked edge fires at frame 112 so the snap ring is mid-flight at capture
                if name == "locked" && frame == 112 {
                    v.lock_edges.push(shinobi_sim::combat::aim_assist::LockEdge::Locked);
                }
                // remote 4 hovers at (+8, ~1.6, -14) of our spawn: aim the triangle at its chest
                let pos = v.remotes.iter().find(|(id, _)| *id == 4).map_or([bx + 8.0, by + 1.4, bz - 14.0], |(_, s)| [s.pos[0], s.pos[1] + 0.2, s.pos[2]]);
                let since = match name {
                    "locked" => (frame.saturating_sub(112)) as f32 / 60.0,
                    "break" => 0.12,
                    _ => 1.0,
                };
                let progress = if name == "locked" { 1.0 } else if name == "break" { 1.0 - since / 0.25 } else { p };
                v.lock = Some(LockOn {
                    target: 4,
                    pos,
                    progress,
                    locked: name == "locked",
                    phase: match name {
                        "locked" => LockPhase::Locked,
                        "break" => LockPhase::Break,
                        _ => LockPhase::Acquire,
                    },
                    eased: progress * progress * (3.0 - 2.0 * progress),
                    t_since_phase: since,
                    // a strafing target, so the review frame shows the lead marker
                    target_vel: if name == "locked" { [6.0, 0.0, 0.0] } else { [0.0; 3] },
                    blend: progress,
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
            // pieces that appear late (frame 108 of ~120): the print scan-in caught mid-build
            "print" => {
                if frame >= 108 {
                    v.blocks.extend(review_pieces(v.block_size, base.0));
                }
            }
            // pieces that vanish at frame 100: the voxel de-rez caught ~0.3 s into the dissolve
            "derez" => {
                if frame < 100 {
                    v.blocks.extend(review_pieces(v.block_size, base.0));
                }
            }
            // `cracks:N` (0..=8): every piece shows that damage bucket
            "cracks" => {
                v.blocks.extend(review_pieces(v.block_size, base.0));
                commands.insert_resource(crate::blocks::CrackOverride(Some(p as u8)));
            }
            // katana connect at frame 114: hit flash + kill burst + arc flare + hit-stop (`katana` = hit, `katanakill`)
            "katana" | "katanakill" => {
                if frame == 114 {
                    let e = v.eye;
                    v.fx.slash_hit = Some(shinobi_client::client_game::PredictedSlashHit { point: [e[0], e[1] - 0.2, e[2] - 4.0], killed: name == "katanakill" });
                    v.fx.hit_stop = 0.06;
                }
            }
            // `death:T`: we die at frame 100 (the snapshot flags us dead, see `offline_world`); the shot is taken T s in. Runs
            // the real camera function (`death_cam`) and fills `RenderView::death` the way `ClientGame::view` would.
            "death" => {
                if frame >= 100 {
                    let t = (frame - 100) as f32 / 60.0;
                    let pos = [bx, by, bz];
                    let killer = v.remotes.iter().find(|(id, _)| *id == 4).map(|(_, s)| s.pos);
                    let pose = shinobi_client::death_cam::death_pose(t, pos, v.aim.yaw, v.aim.pitch, killer, 0.7, 1.0);
                    v.aim.yaw = pose.yaw;
                    v.aim.pitch = pose.pitch;
                    v.eye = pose.eye;
                    v.camera.roll = pose.roll;
                    v.camera.offset = pose.shake;
                    v.weapon.y_dip = 1.2;
                    v.death = Some(shinobi_client::client_game::DeathView { t, total: 2.5, pos, killer: Some(4), fresh: frame == 100 });
                }
            }
            // speed lines: `dashfx` (full), `grapplefx`
            "dashfx" => v.fx.dashing = true,
            "grapplefx" => v.fx.grappling = true,
            "rope" => {
                let e = v.eye;
                v.rope = Some((e, [e[0] + 4.0, e[1] + 7.0, e[2] - 18.0]));
                v.fx.grappling = true;
            }
            // a 300 m hook (pulse packets spread over the length) with the hook-strike ring armed
            "longrope" => {
                let e = v.eye;
                v.rope = Some((e, [e[0] + 40.0, e[1] + 60.0, e[2] - 290.0]));
                v.fx.grappling = true;
                v.fx.strike_armed = 1;
            }
            "wallslash" => v.fx.strike_armed = 2,
            // a rope on a player: rope slash + yank cue
            "yankrope" => {
                let e = v.eye;
                v.rope = Some((e, [e[0] + 3.0, e[1] - 0.4, e[2] - 22.0]));
                v.fx.grappling = true;
                v.fx.strike_armed = 4;
            }
            // `hits` (or `hitkill`): at frame 112 server hit messages land on players 2 (30 slash), 3 (three star hits),
            // 4 (head star; `hitkill` = a slash kill) and one slash hit on us from 2: numbers, flash, bursts, bars, wedge
            "hits" | "hitkill" => {
                use shinobi_proto::gameplay::{HitMsg, KIND_SHURIKEN, KIND_SLASH};
                if frame == 112 {
                    let m = |other, amount, kind, taken, kill, head, bonus| HitMsg { other, amount, kind, taken, kill, head, bonus };
                    v.hits.push(m(2, 30, KIND_SLASH, false, false, false, false));
                    for _ in 0..3 {
                        v.hits.push(m(3, 12, KIND_SHURIKEN, false, false, false, false));
                    }
                    v.hits.push(if name == "hitkill" { m(4, 60, KIND_SLASH, false, true, false, true) } else { m(4, 24, KIND_SHURIKEN, false, false, true, false) });
                    v.hits.push(m(2, 35, KIND_SLASH, true, false, false, false));
                }
            }
            // star variants: `blind:X` (white-out strength X on the HUD); the weapons themselves are `SHOT_WEAPONS`
            "blind" => v.blind = p,
            // power weapons (docs/powerups.md). `yumi`: our bow drawn (gold, bottom of view), a player drawing a bow straight at us
            // (the bright glint, owner 2) and one aiming off to the side (faint, owner 5), two arrows streaking past (one parried back)
            "yumi" => {
                use shinobi_client::power_view::{ArrowObj, DrawObj, PowerView};
                use shinobi_sim::power::{PowerCfg, PowerState};
                let cfg = PowerCfg::default();
                let s = PowerState::default();
                let mut pv = PowerView::of(&s, &cfg, 0, 0).unwrap();
                pv.up = true;
                pv.draw = 0.85;
                v.power = Some(pv);
                v.power_world.draws.push(DrawObj { pos: [bx - 0.4, by + 0.7, bz - 16.0], dir: [0.02, -0.03, 1.0], owner: 2, frac: 1.0 });
                v.power_world.draws.push(DrawObj { pos: [bx + 7.0, by + 0.7, bz - 22.0], dir: [-0.5, 0.0, 0.87], owner: 5, frac: 0.6 });
                v.power_world.arrows.push(ArrowObj { pos: [bx - 3.0, by + 0.5, bz - 9.0], vel: [0.0, 0.0, -90.0], owner: 2, reflected: false });
                v.power_world.arrows.push(ArrowObj { pos: [bx + 2.2, by + 0.8, bz - 6.0], vel: [0.0, 0.0, 90.0], owner: 5, reflected: true });
            }
            // `bowzoom:D`: our bow drawn to fraction D (0..1) with the focus zoom that goes with it (FOV, reticle ring, range
            // readout to a fixed 118 m, drop tick, vignette, hands faded). Pair with `lineup` + SHOT_LINE for a target.
            "bowzoom" => {
                use shinobi_client::power_view::PowerView;
                use shinobi_sim::power::{self, PowerCfg, PowerState};
                let cfg = PowerCfg::default();
                let mut s = PowerState::default();
                s.drawing = true;
                s.draw = cfg.yumi.draw_s * p.clamp(0.0, 1.0);
                let mut pv = PowerView::of(&s, &cfg, 0, 0).unwrap();
                pv.up = true;
                v.power = Some(pv);
                let base = 90.0;
                let frac = power::zoom_of_draw(p, &cfg.yumi);
                let range = Some(118.0);
                v.zoom = shinobi_client::zoom::ZoomView {
                    frac,
                    fov_deg: power::zoom_fov_deg(base, frac, &cfg.yumi),
                    focus: p >= 1.0,
                    range,
                    drop_rad: power::arrow_drop_angle(118.0, &cfg.yumi),
                };
                v.camera.fov = v.zoom.fov_deg;
            }
            // `reload`: our bow just loosed, 40 % of the 2.2 s reload done (the bar under the crosshair)
            "reload" => {
                use shinobi_client::power_view::PowerView;
                use shinobi_sim::power::{PowerCfg, PowerState};
                let cfg = PowerCfg::default();
                let mut s = PowerState::default();
                s.cooldown = cfg.yumi.reload_s * 0.6;
                let mut pv = PowerView::of(&s, &cfg, 0, 0).unwrap();
                pv.up = true;
                v.power = Some(pv);
            }
            // `arrows`: arrows in flight from three owners at different ranges (streaks)
            "arrows" => {
                use shinobi_client::power_view::ArrowObj;
                // crossing the view from the side, like a shot from a flank: three owners, three ranges
                for (owner, p, vel) in [(2u16, [3.5f32, 0.9f32, -7.0f32], [-90.0f32, -0.4f32, 0.0f32]), (5, [-4.0, 1.4, -11.0], [90.0, -0.3, -6.0]), (7, [1.0, 0.5, -5.0], [-64.0, -0.4, -64.0])] {
                    v.power_world.arrows.push(ArrowObj { pos: [bx + p[0], by + p[1], bz + p[2]], vel, owner, reflected: false });
                }
            }
            // `tags`: a tag stuck on the floor arming, one armed with the ring half gone, one flying, one stuck on player 4's torso
            "tags" => {
                use shinobi_client::power_view::TagObj;
                let t = |pos: [f32; 3], vel: [f32; 3], normal: [f32; 3], owner, flying, on_player: Option<u16>, armed, fuse_left| TagObj { pos, vel, normal, owner, flying, on_player, armed, fuse_left };
                v.power_world.tags.push(t([bx - 1.6, by - 0.77, bz - 5.0], [0.0; 3], [0.0, 1.0, 0.0], 2, false, None, false, 0.95));
                v.power_world.tags.push(t([bx + 1.2, by - 0.77, bz - 6.5], [0.0; 3], [0.0, 1.0, 0.0], 5, false, None, true, 0.5));
                v.power_world.tags.push(t([bx + 0.2, by + 0.7, bz - 3.5], [3.0, 2.0, -20.0], [0.0, 1.0, 0.0], 2, true, None, false, 1.0));
                let on = v.remotes.iter().find(|(id, _)| *id == 4).map_or([bx + 8.0, by + 1.4, bz - 14.0], |(_, s)| [s.pos[0], s.pos[1] + 0.3, s.pos[2] + 0.5]);
                v.power_world.tags.push(t(on, [0.0; 3], [0.0, 0.0, 1.0], 2, false, Some(4), true, 0.25));
            }
            // `tagblast`: the blast goes off at frame 112 (shell + floor ring + sparks), with a stuck tag still ticking nearby
            "tagblast" => {
                if frame == 112 {
                    v.events.push(shinobi_sim::events::NetEvent::PowerFx { tick: 0, what: shinobi_sim::power_pass::FX_TAG_BLAST, owner: 2, pos: shinobi_proto::quant::q_pos3([bx, by - 0.6, bz - 8.0]) });
                }
            }
            // `pedestals`: four tag pedestals (and one taken) light up in front of us
            "pedestals" => {
                if frame == 3 {
                    let q = shinobi_proto::quant::q_pos3;
                    for (id, dx, dz, kind) in [(101u32, -3.0f32, -7.0f32, 2u8), (102, 0.5, -9.0, 2), (103, 4.0, -7.5, 2), (104, 7.0, -11.0, 2)] {
                        v.events.push(shinobi_sim::events::NetEvent::PickupSpawned { tick: 0, id, pos: q([bx + dx, by + 0.2, bz + dz]), kind });
                    }
                }
                if frame == 4 {
                    v.events.push(shinobi_sim::events::NetEvent::PickupTaken { tick: 0, id: 104, player: 2 });
                }
            }
            "starkinds" => {
                v.discs.push(([bx - 1.0, by + 1.0, bz - 6.0], [0.0, 0.0, -1.0]));
                v.disc_owners.push(2);
                v.disc_kinds.push(shinobi_sim::combat::stars::StarKind::Standard);
                v.discs.push(([bx + 1.0, by + 1.0, bz - 6.0], [0.0, 0.0, -1.0]));
                v.disc_owners.push(2);
                v.disc_kinds.push(shinobi_sim::combat::stars::StarKind::Flash);
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

/// The five review pieces (floor, two walls, two ramps) in front of the camera spawn.
fn review_pieces(block_size: f32, base: [f32; 3]) -> Vec<shinobi_sim::blocks::BlockInfo> {
    use shinobi_sim::blocks::{BlockInfo, Piece, PieceKind, SCALE_ONE};
    let sz = block_size.max(0.5);
    let c = |x: i32, y: i32, z: i32| [(base[0] / sz).floor() as i32 + x * 2, (base[1] / sz).floor() as i32 + y, (base[2] / sz).floor() as i32 + z * 2];
    [
        (2, c(-2, 0, -2), Piece::FLOOR),
        (3, c(-1, 0, -3), Piece::WALL_X),
        (5, c(1, 0, -3), Piece::WALL_Z),
        (7, c(0, 0, -4), Piece { kind: PieceKind::Ramp, axis: 0 }),
        (9, c(2, 0, -2), Piece { kind: PieceKind::Ramp, axis: 1 }),
    ]
    .into_iter()
    .map(|(owner, cell, piece)| BlockInfo { owner, cell, piece, scale_q: SCALE_ONE })
    .collect()
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
        fx: 0,
    }
}

/// Scripted world, a pure function of the tick: us standing still, a runner, a dasher and a jumper.
fn offline_world(mut net: ResMut<Net>, base: Res<OfflineSpawn>, my_look: Res<crate::character_look::MyLook>) {
    net.0.advance_clock();
    let t = net.0.local_tick();
    let sec = (t / 60.0) as f32;
    let [bx, by, bz] = base.0;
    // others are placed relative to our spawn so every level gets the same composition
    let at = |dx: f32, dy: f32, dz: f32| [bx + dx, by + dy, bz + dz];
    // SHOT_FX=death (we die) / rdeath (player 4 dies) at tick 100
    let fx = std::env::var("SHOT_FX").unwrap_or_default();
    let has = |n: &str| fx.split(',').any(|i| i.split(':').next() == Some(n));
    let dead = |on: bool| if on && t >= 100.0 { shinobi_proto::snapshot::FLAG_DEAD } else { 0 };
    // SHOT_FX=rguard: a player 6 m ahead faces us with the shield up (yaw pi = faces +Z, toward the camera)
    let rguard = if has("rguard") {
        vec![(5, EntityState { yaw: 32768, fx: shinobi_proto::snapshot::FX_BLOCK, ..ent(at(1.5, 0.0, -6.0), [0.0; 3], MoveKind::Grounded) })]
    } else {
        Vec::new()
    };
    // SHOT_FX=ropen: the same player with the strike window open (the lock triangle pulses gold)
    let ropen = if has("ropen") {
        vec![(6, EntityState { yaw: 32768, fx: shinobi_proto::snapshot::FX_OPEN, ..ent(at(-1.5, 0.0, -5.0), [0.0; 3], MoveKind::Grounded) })]
    } else {
        Vec::new()
    };
    let snap = Snapshot {
        tick: t as u32,
        entities: vec![
            (1, EntityState { flags: dead(has("death")), ..ent(at(0.0, 0.0, 0.0), [0.0; 3], MoveKind::Grounded) }),
            (2, ent(at(4.0, 0.0, -3.0 - 9.0 * sec), [0.0, 0.0, -9.0], MoveKind::Grounded)),
            (3, ent(at(-3.5, 0.0, -6.0 - 14.0 * sec.min(1.2)), [0.0, 0.0, -14.0], MoveKind::Evading)),
            (4, EntityState { flags: dead(has("rdeath")), ..ent(at(8.0, 1.6 + 1.2 * (sec * 3.0).sin(), -14.0), [0.0; 3], MoveKind::Airborne) }),
        ],
    };
    let mut snap = snap;
    // SHOT_FX=lineup (distance from SHOT_LINE=metres, default 5): eight players facing us, one pose each:
    // idle, run, air, wall-run, dash, roll, block, strike. Used for the character-look review frames.
    if has("lineup") {
        use shinobi_proto::snapshot::{FLAG_ROLL, FX_BLOCK, FX_OPEN};
        let d: f32 = std::env::var("SHOT_LINE").ok().and_then(|v| v.parse().ok()).unwrap_or(5.0);
        let sp = d * 0.3;
        let rows: [(MoveKind, u8, u8, f32); 8] = [
            (MoveKind::Grounded, 0, 0, 0.0),
            (MoveKind::Grounded, 0, 0, 9.0),
            (MoveKind::Airborne, 0, 0, 5.0),
            (MoveKind::WallRun, 0, 0, 9.0),
            (MoveKind::Evading, 0, 0, 20.0),
            (MoveKind::Evading, FLAG_ROLL, 0, 8.0),
            (MoveKind::Grounded, 0, FX_BLOCK, 0.0),
            (MoveKind::Grounded, 0, FX_OPEN, 0.0),
        ];
        // SHOT_LINE_PICK=0,1,6 keeps only those poses (spaced evenly) for close-up review frames
        let pick: Vec<usize> = std::env::var("SHOT_LINE_PICK").ok().map(|v| v.split(',').filter_map(|n| n.parse().ok()).collect()).unwrap_or_else(|| (0..8).collect());
        for (slot, i) in pick.iter().copied().enumerate() {
            let (kind, flags, fxb, v) = rows[i];
            let x = (slot as f32 - (pick.len() as f32 - 1.0) * 0.5) * sp;
            let along = -d;
            let up = if kind == MoveKind::Airborne { 0.8 } else { 0.0 };
            snap.entities.push((20 + i as u16, EntityState { yaw: 32768, flags, fx: fxb, ..ent(at(x, up, along), [0.0, 0.0, v], kind) }));
        }
    }
    snap.entities.extend(rguard);
    snap.entities.extend(ropen);
    // SHOT_CHAR=file.json dresses us (first person) and every other player in that character file;
    // SHOT_CHAR=presets gives each other player a built-in look. Unset = untouched (goldens stay exact).
    match std::env::var("SHOT_CHAR").ok().as_deref() {
        Some("presets") => {
            for (id, _) in snap.entities.iter().filter(|(id, _)| *id != 1) {
                net.0.game.net.inject_look(*id, &shinobi_proto::character::preset(*id as usize));
            }
        }
        Some(_) => {
            if let Some(spec) = &my_look.0 {
                for (id, _) in snap.entities.iter().filter(|(id, _)| *id != 1) {
                    net.0.game.net.inject_look(*id, spec);
                }
            }
        }
        None => {}
    }
    // SHOT_FX=guard: our own shield up (first-person blade + shield + HUD ring)
    net.0.game.force_blocking(has("guard"));
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
