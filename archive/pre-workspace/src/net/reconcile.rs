//! Client prediction bookkeeping for the local player: keep predicted results per tick, compare with the
//! authoritative state, and either ignore, replay, or snap. Visual error is faded out instead of popped,
//! which is what keeps cellular jitter from reading as rubber-banding.

use std::collections::VecDeque;

use super::intent::PlayerIntent;

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
