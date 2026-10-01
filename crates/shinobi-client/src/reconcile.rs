//! Client prediction bookkeeping for the local player: keep predicted results per tick, compare with the
//! authoritative state, and either ignore, replay, or snap. Visual error is faded out instead of popped,
//! which is what keeps cellular jitter from reading as rubber-banding.

use std::collections::VecDeque;

use shinobi_proto::intent::PlayerIntent;
use shinobi_proto::TICK_DT;
use shinobi_sim::character::{CharacterSystem, PhysicsWorld, PlayerInput};
use shinobi_sim::movement::step_player;
use shinobi_sim::own_state::OwnState;
use shinobi_sim::tick::to_player_input;

#[derive(Clone, Copy, Debug)]
struct Predicted {
    intent: PlayerIntent,
    pos_after: [f32; 3],
}

#[derive(Debug, PartialEq)]
pub enum Correction {
    /// Prediction matched within the deadband.
    None,
    /// Reset the character to `server_pos` (plus its velocity / move state from the snapshot) and re-run
    /// `CharacterSystem::update` over `replay` in order (ticks after the acked one).
    Replay { server_pos: [f32; 3], replay: Vec<PlayerIntent> },
}

pub struct Reconciler {
    history: VecDeque<Predicted>,
    /// Errors below this are ignored (quantisation noise).
    pub deadband: f32,
    /// Errors above this are snapped visually (teleports, respawns) instead of smoothed.
    pub snap_dist: f32,
    /// Render offset = simulated position + `visual_offset`; decays toward zero.
    pub visual_offset: [f32; 3],
    pub decay_per_sec: f32,
}

impl Default for Reconciler {
    fn default() -> Self {
        Self { history: VecDeque::new(), deadband: 0.02, snap_dist: 3.0, visual_offset: [0.0; 3], decay_per_sec: 10.0 }
    }
}

impl Reconciler {
    /// Call after simulating `intent` locally.
    pub fn record(&mut self, intent: PlayerIntent, pos_after: [f32; 3]) {
        self.history.push_back(Predicted { intent, pos_after });
        while self.history.len() > 128 {
            self.history.pop_front();
        }
    }

    /// `acked_tick`: newest local intent tick the server says it has simulated; `server_pos`: its result.
    /// `rendered_pos`: where the local player is currently drawn (for smoothing).
    pub fn on_server_state(&mut self, acked_tick: u32, server_pos: [f32; 3], rendered_pos: [f32; 3]) -> Correction {
        while self.history.front().map_or(false, |p| p.intent.tick < acked_tick) {
            self.history.pop_front();
        }
        let Some(front) = self.history.front().copied().filter(|p| p.intent.tick == acked_tick) else {
            return Correction::None; // nothing predicted for that tick (e.g. history trimmed)
        };
        self.history.pop_front();
        let d = [
            server_pos[0] - front.pos_after[0],
            server_pos[1] - front.pos_after[1],
            server_pos[2] - front.pos_after[2],
        ];
        let err = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if err <= self.deadband {
            return Correction::None;
        }
        let replay: Vec<_> = self.history.iter().map(|p| p.intent).collect();
        self.history.clear(); // caller re-records while replaying
        if err > self.snap_dist {
            self.visual_offset = [0.0; 3];
        } else {
            // pos_new = pos_old + d (before replay); absorb the jump into the offset so the render doesn't move
            self.visual_offset = [
                rendered_pos[0] - server_pos[0],
                rendered_pos[1] - server_pos[1],
                rendered_pos[2] - server_pos[2],
            ];
        }
        Correction::Replay { server_pos, replay }
    }

    /// Call every rendered frame.
    pub fn decay(&mut self, dt: f32) {
        let k = (-self.decay_per_sec * dt).exp();
        for v in &mut self.visual_offset {
            *v *= k;
        }
    }
}

/// `input_for` for callers whose only per-tick input is the stick and buttons.
pub fn plain_input(it: &PlayerIntent, _ch: &mut CharacterSystem) -> PlayerInput {
    to_player_input(it)
}

/// Authoritative replay: put the local character back to the server's state for the acked tick, then re-run every
/// buffered intent after it through the same `step_player` + `to_player_input` the server uses, re-recording each
/// result so the next ack compares against the corrected prediction. Returns the new predicted position.
///
/// `input_for`: per-tick intent -> `PlayerInput` (use `plain_input` when nothing but the stick matters).
/// `now_at_ack`: sim time after the acked tick (unused by movement). Intents are replayed with the
/// aim they were sent with; the server's slew clamp only differs from that for hostile or dropped-aim input.
pub fn apply_replay(
    rec: &mut Reconciler,
    ch: &mut CharacterSystem,
    world: &mut PhysicsWorld,
    server_pos: [f32; 3],
    own: &OwnState,
    replay: &[PlayerIntent],
    now_at_ack: f32,
    input_for: &mut dyn FnMut(&PlayerIntent, &mut CharacterSystem) -> PlayerInput,
) -> [f32; 3] {
    ch.restore_own(world, own);
    let mut now = now_at_ack;
    let mut pos = server_pos;
    for it in replay {
        // pre-step hook: turns the intent into this tick's PlayerInput (katana lunge, ...) and may advance
        // controller-side state it owns, exactly as it did when the tick was first predicted
        let input = input_for(it, ch);
        step_player(ch, &input, world, now, TICK_DT);
        now += TICK_DT;
        let t = world.bodies[ch.body].translation();
        pos = [t.x, t.y, t.z];
        rec.record(*it, pos);
    }
    pos
}

impl Reconciler {
    /// `on_server_state` + `apply_replay` in one call. `Some(new_pos)` when the prediction was corrected.
    /// `rendered_pos`: the position currently drawn (simulated + `visual_offset`).
    pub fn on_server_own_state(
        &mut self,
        acked_tick: u32,
        _server_pos: [f32; 3],
        own: &OwnState,
        rendered_pos: [f32; 3],
        ch: &mut CharacterSystem,
        world: &mut PhysicsWorld,
        input_for: &mut dyn FnMut(&PlayerIntent, &mut CharacterSystem) -> PlayerInput,
    ) -> Option<[f32; 3]> {
        // the blob carries the exact server position; the snapshot's quantised one is only a fallback source
        let server_pos = own.pos;
        match self.on_server_state(acked_tick, server_pos, rendered_pos) {
            Correction::None => None,
            Correction::Replay { server_pos, replay } => {
                let now = (acked_tick + 1) as f32 * TICK_DT;
                let new_pos = apply_replay(self, ch, world, server_pos, own, &replay, now, input_for);
                // `rendered_pos` is what is on screen now (simulated + offset). Keep the screen still: the new
                // simulated position is `new_pos`, so the offset is the difference. (`on_server_state` alone
                // computes it against the acked-tick position, which is only right if nothing is replayed.)
                let d = [rendered_pos[0] - new_pos[0], rendered_pos[1] - new_pos[1], rendered_pos[2] - new_pos[2]];
                let jump = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                self.visual_offset = if jump > self.snap_dist { [0.0; 3] } else { d };
                Some(new_pos)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn it(tick: u32) -> PlayerIntent {
        PlayerIntent { tick, ..Default::default() }
    }

    #[test]
    fn matching_prediction_is_ignored() {
        let mut r = Reconciler::default();
        for t in 0..5 {
            r.record(it(t), [t as f32, 0.0, 0.0]);
        }
        assert_eq!(r.on_server_state(2, [2.001, 0.0, 0.0], [4.0, 0.0, 0.0]), Correction::None);
    }

    #[test]
    fn mismatch_replays_later_inputs_and_smooths() {
        let mut r = Reconciler::default();
        for t in 0..5 {
            r.record(it(t), [t as f32, 0.0, 0.0]);
        }
        match r.on_server_state(2, [1.5, 0.0, 0.0], [4.0, 0.0, 0.0]) {
            Correction::Replay { server_pos, replay } => {
                assert_eq!(server_pos[0], 1.5);
                assert_eq!(replay.iter().map(|i| i.tick).collect::<Vec<_>>(), vec![3, 4]);
            }
            c => panic!("{c:?}"),
        }
        // screen must not jump: offset holds the difference until it decays
        assert!((r.visual_offset[0] - 2.5).abs() < 1e-5);
        r.decay(0.3);
        assert!(r.visual_offset[0] < 0.2);
    }
}
