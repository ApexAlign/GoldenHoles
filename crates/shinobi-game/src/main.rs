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

mod crt;
mod demo;
mod blocks;
mod discs;
mod impacts;
mod lock;
mod rope;
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
mod vm;
mod walls;

use std::net::SocketAddr;

use bevy::prelude::*;
use shinobi_client::quality::{Quality, Tier};

fn arg_value(args: &[String], key: &str) -> Option<String> {
    args.iter().position(|a| a == key).and_then(|i| args.get(i + 1).cloned())
}

/// `--level PATH` wins, else the server's default (`levels/arena.lvl` when found), else the built-in level.
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
            let l = shinobi_sim::level::LevelDesc::from_text(&text).unwrap_or_else(|e| panic!("level {}: {e}", p.display()));
            eprintln!("level: {} ({} boxes, {} spawns)", p.display(), l.boxes.len(), l.spawns.len());
            l
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let server: SocketAddr = arg_value(&args, "--server")
        .unwrap_or_else(|| "127.0.0.1:7777".into())
        .parse()
        .expect("--server ADDR:PORT");

    let flag = |k: &str| args.iter().any(|a| a == k);
    let tier = arg_value(&args, "--tier").map(|t| Tier::parse(&t).expect("--tier high|medium|low")).unwrap_or_default();
    let opts = shot::ShotOpts {
        idle: flag("--idle"),
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
    app
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window { title: shinobi_client::overlay::window_title(false).into(), ..default() }),
            ..default()
        }))
        .add_plugins(net::NetPlugin { server, offline: false, level: opts.level.clone() })
        .add_plugins(mobile::MobilePlugin { quality: opts.quality, touch_overlay: opts.touch, screen: (1280.0, 720.0) })
        .add_plugins(grid::GridPlugin)
        .add_plugins(crt::CrtPlugin)
        .add_plugins(sky::SkyPlugin)
        .add_plugins(play::PlayPlugin)
        .add_plugins((vm::VmPlugin, crate::discs::DiscPlugin, crate::impacts::ImpactPlugin, crate::rope::RopePlugin, crate::team::TeamPlugin, crate::blocks::BlocksPlugin, crate::lock::LockPlugin))
        .add_plugins(hud_view::HudPlugin)
        .add_plugins(walls::WallsPlugin);
    if flag("--demo") {
        app.add_plugins(demo::DemoPlugin);
    }
    app.run();
}
