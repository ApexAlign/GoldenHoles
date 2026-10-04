//! Bevy adapter for `shinobi_client::udp_game::UdpGame` (prediction + reconciliation + camera live there).
//! This file only schedules the 60 Hz fixed step and publishes the per-frame `RenderView`.

use std::net::SocketAddr;

use bevy::prelude::*;
use shinobi_client::client_game::{ClientGameCfg, InputFrame, RenderView};
use shinobi_client::display_clock::DisplayClock;
use shinobi_client::udp_game::UdpGame;
use shinobi_sim::level::LevelDesc;

#[derive(Resource)]
pub struct Net(pub UdpGame);

/// The level being played (also drawn by `play::setup`).
#[derive(Resource, Clone)]
pub struct LevelRes(pub LevelDesc);

/// Input accumulated between fixed steps: `look` and `buttons` are drained by each step, `stick` is a level.
#[derive(Resource, Default)]
pub struct PendingInput(pub InputFrame);

/// What to draw this frame (published by `publish_view`, which runs before every render-side system).
#[derive(Resource)]
pub struct ViewRes(pub RenderView);

pub struct NetPlugin {
    pub server: SocketAddr,
    /// No real session: the scene feeds `UdpGame` itself (deterministic regression shots).
    pub offline: bool,
    /// Same geometry the server runs (`--level`); prediction diverges at the first wall otherwise.
    pub level: LevelDesc,
}

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        let net = UdpGame::new(self.server, self.level.clone(), ClientGameCfg::default()).expect("bind UDP socket");
        let mut net = net;
        let view = ViewRes(net.view());
        app.insert_resource(LevelRes(self.level.clone()))
            .insert_resource(crate::hud_view::ServerLabel(self.server.to_string()))
            .insert_resource(Time::<Fixed>::from_hz(60.0))
            .insert_resource(Net(net))
            .insert_resource(view)
            .init_resource::<PendingInput>()
            .insert_resource(DisplayClockRes(
                if std::env::var_os("SHINOBI_RAW_CLOCK").is_some() { DisplayClock::raw() } else { DisplayClock::default() },
                0.0,
            ))
            .insert_resource(SubTick(!self.offline))
            .add_systems(First, publish_view);
        if !self.offline {
            app.add_systems(FixedUpdate, net_step);
        }
    }
}

/// Exactly one call per fixed run: `UdpGame` owns the tick clock (+1 per call).
fn net_step(mut net: ResMut<Net>, mut input: ResMut<PendingInput>) {
    FIXED_STEPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let frame = InputFrame {
        stick: input.0.stick,
        look: std::mem::take(&mut input.0.look),
        buttons: std::mem::take(&mut input.0.buttons),
    };
    net.0.fixed_step(&frame);
}

/// `ClientGame::view` takes the reliable events, so call it once per rendered frame.
fn publish_view(mut net: ResMut<Net>, mut view: ResMut<ViewRes>, fixed: Res<Time<Fixed>>, sub: Res<SubTick>, real: Res<Time<Real>>, mut clock: ResMut<DisplayClockRes>) {
    // draw between sim ticks (the fixed step is 60 Hz, the display is not); offline shots stay on exact ticks. The time to
    // draw is `DisplayClock`'s: the sim time with the frame-delta noise filtered out (SHINOBI_RAW_CLOCK=1 draws the raw sim time)
    view.0 = if sub.0 {
        let sim_now = net.0.local_tick() + fixed.overstep_fraction() as f64;
        let t = clock.0.advance(real.delta_secs_f64() * 60.0, sim_now);
        clock.1 = clock.0.lag(sim_now) as f32;
        net.0.view_at(t)
    } else {
        net.0.view()
    };
}

/// The display clock and its last lag behind the sim (ticks), for the PERF line.
#[derive(Resource)]
pub struct DisplayClockRes(pub DisplayClock, pub f32);

/// Fixed steps run so far (frame-time report in `play::update_title`).
pub static FIXED_STEPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Whether `publish_view` interpolates between fixed ticks.
#[derive(Resource)]
struct SubTick(bool);
