//! Ctrl+C / SIGTERM close the game the same way the window's close button does: an `AppExit` from a system, so the
//! window, GPU and network sockets drop normally and the process exits 0 (scripts/play.sh stops it with SIGINT).

use bevy::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);

pub struct SignalExitPlugin;

impl Plugin for SignalExitPlugin {
    fn build(&self, app: &mut App) {
        // a second signal while the first is still being handled (a hung frame) hard-exits instead of waiting
        let handler = ctrlc::set_handler(|| {
            if STOP.swap(true, Ordering::SeqCst) {
                std::process::exit(130);
            }
        });
        if let Err(e) = handler {
            eprintln!("signal handler not installed ({e}); Ctrl+C will kill the process the hard way");
        }
        app.add_systems(First, exit_on_signal);
    }
}

fn exit_on_signal(mut exit: MessageWriter<AppExit>) {
    if STOP.load(Ordering::SeqCst) {
        exit.write(AppExit::Success);
    }
}
