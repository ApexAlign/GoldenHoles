//! `--headless`: same `NetPlugin` and `Time<Fixed>` scheduling as the windowed build, but MinimalPlugins
//! and a scripted "walk forward" stick. Proves handshake, intent stamping, prediction and snapshot decode
//! against a live server without a window or GPU. Exit code 1 if it never connects or never sees the world.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bevy::app::ScheduleRunnerPlugin;
use bevy::prelude::*;

use crate::net::{Net, NetPlugin, PendingInput, ViewRes};

#[derive(Resource)]
struct Run {
    secs: f32,
    last_log: f32,
    first_pos: Option<[f32; 3]>,
    max_remotes: usize,
}

static PASSED: AtomicBool = AtomicBool::new(false);

pub fn run(server: SocketAddr, secs: f32, demo: bool, level: shinobi_sim::level::LevelDesc) {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(1.0 / 120.0))))
        .add_plugins(NetPlugin { server, offline: false, level })
        .insert_resource(Run { secs, last_log: 0.0, first_pos: None, max_remotes: 0 })
        .add_systems(Update, report);
    if demo {
        app.add_plugins(crate::demo::DemoPlugin);
    } else {
        app.add_systems(Startup, |mut i: ResMut<PendingInput>| i.0.stick = (0.0, 1.0));
    }
    let exit = app.run();
    // the World is consumed by run(); the verdict was stored by `report` before it asked to exit
    if !PASSED.load(Ordering::SeqCst) || matches!(exit, AppExit::Error(_)) {
        std::process::exit(1);
    }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn report(time: Res<Time>, net: Res<Net>, view: Res<ViewRes>, mut run: ResMut<Run>, mut exit: MessageWriter<AppExit>) {
    let t = time.elapsed_secs();
    let s = &net.0;
    let v = &view.0;
    run.max_remotes = run.max_remotes.max(v.remotes.len());
    if run.first_pos.is_none() {
        run.first_pos = v.local_pos;
    }
    if t - run.last_log >= 1.0 {
        run.last_log = t;
        println!(
            "t={:.1}s connected={} id={:?} rx={} tx={} predicting={} corr={} remotes={} pos={:?} speed={:.1} wall_run={} roll={:.2} hp={:?}",
            t, s.connected(), s.own_id(), s.stats.rx_datagrams, s.stats.tx_packets,
            s.game.predicting(), s.game.corrections, v.remotes.len(), v.local_pos,
            v.fx.speed, v.fx.wall_running, v.camera.roll, v.hp
        );
    }
    if t >= run.secs {
        let moved = run.first_pos.zip(v.local_pos).map(|(a, b)| dist(a, b)).unwrap_or(0.0);
        println!(
            "headless summary: connected={} id={:?} rx={} ignored={} bad={} tx_hello={} tx_packets={} predicting={} corrections={} remotes_max={} moved={:.2}m",
            s.connected(), s.own_id(), s.stats.rx_datagrams, s.stats.rx_ignored, s.game.net.bad_datagrams,
            s.stats.tx_hello, s.stats.tx_packets, s.game.predicting(), s.game.corrections, run.max_remotes, moved
        );
        PASSED.store(
            s.connected() && s.stats.tx_packets > 0 && s.game.net.bad_datagrams == 0 && s.game.predicting() && moved > 1.0,
            Ordering::SeqCst,
        );
        exit.write(AppExit::Success);
    }
}
