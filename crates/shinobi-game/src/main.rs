//! Playable client shell. Bevy owns window/render; all game logic comes from `shinobi-client`
//! (input, udp_session/netclient, camera, fx, emit, remote, trail) and `shinobi-sim`.
//! This crate only adapts them to Bevy.
//!
//!   shinobi-game [--server 127.0.0.1:7777]              windowed
//!   shinobi-game --shot out.ppm [--secs 3] [--idle]     offscreen render against a live server, no window
//!   shinobi-game --shot-offline out.ppm [--secs 2]      deterministic offscreen render, no network (golden test)
//!   options: --tier high|medium|low   --touch (show touch overlay)   --scale F (pin dynamic resolution)
//!   shinobi-game --record out.mp4 [--secs 20] [--fps 30] [--demo] --server ADDR   offscreen 1280x720 clip via ffmpeg
//!   shinobi-game --headless [--secs 5] [--server ADDR]  network-only smoke test (no window/GPU)

mod signal;
mod crt;
mod demo;
mod audio;
mod blocks;
mod character_look;
mod death;
mod discs;
mod env_look;
mod impacts;
mod lock;
mod rope;
mod synth;
mod mixer;
mod team;
mod grid;
mod hud_view;
mod mobile;
mod headless;
mod net;
mod play;
mod record;
mod shot;
mod sky;
mod star_fx;
mod vm;

use std::net::SocketAddr;

use bevy::prelude::*;
use shinobi_client::quality::{Quality, Tier};

fn arg_value(args: &[String], key: &str) -> Option<String> {
    args.iter().position(|a| a == key).and_then(|i| args.get(i + 1).cloned())
}

/// `--level PATH` wins, else the server's default (`levels/ffa.lvl` when found), else the built-in level.
fn load_level(path: Option<String>) -> shinobi_sim::level::LevelDesc {
    if path.as_deref() == Some("builtin") {
        eprintln!("level: built-in (forced)");
        return shinobi_sim::level::LevelDesc::standard();
    }
    let path = path.map(std::path::PathBuf::from).or_else(shinobi_sim::level::default_level_path);
    match path {
        None => {
            eprintln!("level: built-in");
            shinobi_sim::level::LevelDesc::standard()
        }
        Some(p) => {
            let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("level {}: {e}", p.display()));
            if let Ok(f) = shinobi_sim::level::parse_level_file(&text) {
                hud_view::set_level_pickups(f.pickups.iter().map(|p| p.pos).collect());
            }
            let l = shinobi_sim::level::LevelDesc::from_text(&text).unwrap_or_else(|e| panic!("level {}: {e}", p.display()));
            eprintln!("level: {} ({} boxes, {} spawns)", p.display(), l.boxes.len(), l.spawns.len());
            l
        }
    }
}

fn present_mode(s: Option<&str>) -> bevy::window::PresentMode {
    use bevy::window::PresentMode;
    match s.map(str::to_ascii_lowercase).as_deref() {
        Some("mailbox") => PresentMode::Mailbox,
        Some("immediate") | Some("novsync") => PresentMode::AutoNoVsync,
        Some("relaxed") | Some("fiforelaxed") => PresentMode::FifoRelaxed,
        Some("fifo") => PresentMode::Fifo,
        _ => PresentMode::AutoVsync,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let server: SocketAddr = arg_value(&args, "--server")
        .unwrap_or_else(|| "127.0.0.1:7777".into())
        .parse()
        .expect("--server ADDR:PORT");

    let flag = |k: &str| args.iter().any(|a| a == k);
    // `--tier high|medium|low` pins the quality tier; without it (or with `--tier auto`) the live game picks one from
    // the GPU class and steps down when frames run long. Shots always use a fixed tier (default high).
    let tier_arg = arg_value(&args, "--tier").filter(|t| !t.eq_ignore_ascii_case("auto"));
    let tier = tier_arg.as_deref().map(|t| Tier::parse(t).expect("--tier high|medium|low|auto")).unwrap_or_default();
    let auto_tier = tier_arg.is_none();
    let opts = shot::ShotOpts {
        // offline review shots are idle (rest pose) unless `--action` asks for the scripted swing + throw (goldens use it)
        idle: flag("--idle") || (flag("--shot-offline") && !flag("--action")),
        touch: flag("--touch"),
        quality: Quality::from_tier(tier),
        scale: arg_value(&args, "--scale").and_then(|s| s.parse().ok()),
        level: load_level(arg_value(&args, "--level")),
        spawn: arg_value(&args, "--spawn").and_then(|s| s.parse().ok()).unwrap_or(0),
    };

    if flag("--headless") {
        let secs = arg_value(&args, "--secs").and_then(|s| s.parse().ok()).unwrap_or(5.0);
        headless::run(server, secs, flag("--demo"), opts.level.clone());
        return;
    }

    if let Some(out) = arg_value(&args, "--record") {
        let ro = record::RecordOpts {
            out,
            secs: arg_value(&args, "--secs").and_then(|s| s.parse().ok()).unwrap_or(20.0),
            fps: arg_value(&args, "--fps").and_then(|s| s.parse().ok()).unwrap_or(30),
            size: (1280, 720),
            demo: flag("--demo"),
        };
        record::run(server, ro, opts);
        return;
    }

    if let Some(out) = arg_value(&args, "--shot-offline") {
        let secs = arg_value(&args, "--secs").and_then(|s| s.parse().ok()).unwrap_or(2.0);
        shot::run_offline(secs, out, opts);
        return;
    }

    if let Some(out) = arg_value(&args, "--shot") {
        let secs = arg_value(&args, "--secs").and_then(|s| s.parse().ok()).unwrap_or(3.0);
        shot::run(server, secs, out, opts);
        return;
    }

    let mut app = App::new();
    if let Some(f) = opts.scale {
        app.insert_resource(play::ForcedScale(f));
    }
    // profiling aid: SHINOBI_DISABLE=sky,hud,vm,crt,fx (comma list) leaves those plugins out to find what a frame costs
    let disabled = std::env::var("SHINOBI_DISABLE").unwrap_or_default();
    let off = |n: &str| disabled.split(',').any(|d| d.trim() == n);
    // window system: `--x11` forces X11/XWayland even when a Wayland session is available (winit's own switch)
    if flag("--x11") {
        std::env::set_var("WINIT_UNIX_BACKEND", "x11");
    }
    // `--present auto|mailbox|immediate|relaxed` (or SHINOBI_PRESENT): swap-chain mode. Default `auto` = vsync (Fifo),
    // the smoothest and coolest; `mailbox` / `immediate` trade tearing / heat for lower latency.
    let present = present_mode(arg_value(&args, "--present").or_else(|| std::env::var("SHINOBI_PRESENT").ok()).as_deref());
    // SHINOBI_GPU=igpu|dgpu|auto: adapter power preference (WGPU_ADAPTER_NAME=<substring> still picks by name)
    let power = match std::env::var("SHINOBI_GPU").ok().as_deref() {
        Some("igpu") | Some("low") => bevy::render::settings::PowerPreference::LowPower,
        Some("dgpu") | Some("high") => bevy::render::settings::PowerPreference::HighPerformance,
        _ => bevy::render::settings::PowerPreference::default(),
    };
    eprintln!("WINDOW: present mode {present:?}, power preference {power:?}");
    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window { title: shinobi_client::overlay::window_title(false).into(), present_mode: present, ..default() }),
                ..default()
            })
            .set(bevy::render::RenderPlugin {
                render_creation: bevy::render::settings::WgpuSettings { power_preference: power, ..default() }.into(),
                ..default()
            }),
    )
    .add_plugins(net::NetPlugin { server, offline: false, level: opts.level.clone() })
    .add_plugins(mobile::MobilePlugin { quality: opts.quality, auto_tier, touch_overlay: opts.touch, screen: (1280.0, 720.0) })
    .add_plugins(grid::GridPlugin);
    if !off("crt") {
        app.add_plugins(crt::CrtPlugin);
    }
    if !off("sky") {
        app.add_plugins(sky::SkyPlugin);
    }
    app.add_plugins(signal::SignalExitPlugin);
    app.add_plugins(play::PlayPlugin);
    app.add_plugins(env_look::EnvLookPlugin);
    if !off("vm") {
        app.add_plugins(vm::VmPlugin);
    }
    app.add_plugins((crate::team::TeamPlugin, crate::audio::AudioFxPlugin));
    if !off("fx") {
        app.add_plugins((crate::discs::DiscPlugin, crate::impacts::ImpactPlugin, crate::rope::RopePlugin, crate::blocks::BlocksPlugin, crate::lock::LockPlugin, crate::death::DeathFxPlugin, crate::star_fx::StarFxPlugin));
    }
    if !off("hud") {
        app.add_plugins(hud_view::HudPlugin);
    }
    if flag("--demo") {
        app.add_plugins(demo::DemoPlugin);
    }
    app.run();
}
