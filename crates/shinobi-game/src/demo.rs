//! `--demo`: drives the player with `shinobi_client::demo_script` (closed loop: it reads our predicted position
//! and the opponents') instead of the keyboard. Starts a moment after the connection is up and plays the film once.

use bevy::prelude::*;
use shinobi_client::client_game::ClientGameCfg;
use shinobi_client::demo_script::{DemoScript, World};
use shinobi_sim::level::LevelDesc;

use crate::net::{LevelRes, PendingInput, ViewRes};

/// Seconds after connecting before the script starts.
const LEAD_IN_S: f32 = 0.3;

#[derive(Resource)]
pub struct DemoDrive {
    script: DemoScript,
    start: Option<f32>,
    last: f32,
    finished_at: Option<f32>,
}

impl DemoDrive {
    pub fn new(level: &LevelDesc) -> Self {
        Self { script: DemoScript::arena(level), start: None, last: 0.0, finished_at: None }
    }

    /// Seconds since the script ended (`None` while it is still running).
    pub fn since_finished(&self, now: f32) -> Option<f32> {
        self.finished_at.map(|t| now - t)
    }
}

pub fn drive(time: Res<Time>, view: Res<ViewRes>, mut d: ResMut<DemoDrive>, mut pending: ResMut<PendingInput>) {
    let now = time.elapsed_secs();
    let v = &view.0;
    if d.start.is_none() {
        if !v.connected || v.local_pos.is_none() {
            return;
        }
        d.start = Some(now + LEAD_IN_S);
        d.last = now;
    }
    let t = now - d.start.unwrap();
    let dt = (now - d.last).max(0.0);
    d.last = now;
    let Some(pos) = v.local_pos else { return };
    if t < 0.0 {
        return;
    }
    let world = World { pos, yaw: v.aim.yaw, remotes: v.remotes.iter().map(|(_, s)| s.pos).collect() };
    let out = d.script.step(t, dt, ClientGameCfg::DEFAULT_RAD_PER_UNIT, &world);
    let f = &mut pending.0;
    f.stick = out.stick;
    f.look.0 += out.look.0;
    f.look.1 += out.look.1;
    f.buttons.extend(out.events);
    if d.finished_at.is_none() && d.script.finished() {
        d.finished_at = Some(now);
    }
}

pub struct DemoPlugin;

impl Plugin for DemoPlugin {
    fn build(&self, app: &mut App) {
        let level = app.world().get_resource::<LevelRes>().map(|l| l.0.clone()).unwrap_or_else(LevelDesc::standard);
        app.insert_resource(DemoDrive::new(&level)).add_systems(Update, drive);
    }
}
