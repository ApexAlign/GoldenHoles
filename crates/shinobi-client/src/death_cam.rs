//! Death camera: what the local view does between the moment the server says we are dead and the respawn.
//!
//! The predictor keeps no authority while dead (the server ignores a dead player's intents and sends no movement
//! state), so the shell must not keep looking like a living, controllable player. This is a pure function of the
//! seconds since death: the eye drops and rolls as the body collapses, pulls back behind the body so it is in
//! frame while it de-rezzes, and the view slowly turns toward whoever did it (kill-cam). All cosmetic.

/// Seconds the collapse / turn takes to settle; the rest of the respawn delay holds the final pose.
pub const SETTLE_S: f32 = 1.1;
/// How far the screen is washed out by the time the sequence settles is a HUD matter; this is the camera only.
const DROP_M: f32 = 0.85;
const PULL_M: f32 = 1.6;
const ROLL_RAD: f32 = 0.55;
const FOV_ADD_DEG: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeathPose {
    pub eye: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    /// Added to the base FOV, degrees.
    pub fov_add: f32,
    /// View-frame translation shake (right, up), metres: a hard kick at the moment of death that dies out.
    pub shake: [f32; 2],
}

fn smooth(x: f32) -> f32 {
    let t = x.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn wrap(a: f32) -> f32 {
    (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

/// `t`: seconds since death. `pos`: body centre where we died. `yaw0` / `pitch0`: the aim at death. `killer`: the
/// killer's current centre if known. `eye_above`: eye height over the centre. `side`: +1 / -1, which way the view rolls.
pub fn death_pose(t: f32, pos: [f32; 3], yaw0: f32, pitch0: f32, killer: Option<[f32; 3]>, eye_above: f32, side: f32) -> DeathPose {
    let turn = smooth(t / SETTLE_S) * 0.85;
    let (yaw_t, pitch_t) = match killer {
        Some(k) => {
            let (dx, dy, dz) = (k[0] - pos[0], k[1] + 0.2 - (pos[1] + eye_above), k[2] - pos[2]);
            let h = (dx * dx + dz * dz).sqrt().max(1e-3);
            // yaw 0 faces -Z, positive turns toward -X (same convention as the aim)
            ((-dx).atan2(-dz), dy.atan2(h).clamp(-0.5, 0.6).max(0.12))
        }
        None => (yaw0 + 0.6 * side, 0.3),
    };
    let yaw = yaw0 + wrap(yaw_t - yaw0) * turn;
    let pitch = pitch0 + (pitch_t - pitch0) * turn;
    let roll = side * ROLL_RAD * smooth(t / 0.6);

    // collapse: quick drop, never below the floor (the capsule's feet are ~0.81 under the centre)
    let drop = DROP_M * smooth(t / 0.55);
    let pull = PULL_M * smooth(t / SETTLE_S);
    let (sy, cy) = yaw.sin_cos();
    let fwd_h = [-sy, -cy]; // horizontal forward for this yaw
    let y = (pos[1] + eye_above - drop).max(pos[1] - 0.55);
    let eye = [pos[0] - fwd_h[0] * pull, y, pos[2] - fwd_h[1] * pull];

    let k = (1.0 - t / 0.25).clamp(0.0, 1.0);
    let shake = [(t * 95.0).sin() * 0.05 * k * k, (t * 80.0).cos() * 0.04 * k * k];
    DeathPose { eye, yaw, pitch, roll, fov_add: FOV_ADD_DEG * smooth(t / 0.4), shake }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POS: [f32; 3] = [10.0, 1.0, -10.0];

    #[test]
    fn starts_exactly_where_the_player_was_looking() {
        let p = death_pose(0.0, POS, 1.0, 0.2, Some([20.0, 1.0, -10.0]), 0.7, 1.0);
        assert!((p.yaw - 1.0).abs() < 1e-4 && (p.pitch - 0.2).abs() < 1e-4 && p.roll == 0.0, "{p:?}");
        assert!((p.eye[1] - 1.7).abs() < 1e-4 && p.eye[0] == POS[0] && p.eye[2] == POS[2], "no pop at t = 0: {p:?}");
    }

    #[test]
    fn the_eye_drops_rolls_and_never_goes_under_the_floor() {
        let p = death_pose(2.0, POS, 0.0, 0.0, None, 0.7, 1.0);
        assert!(p.eye[1] < POS[1] && p.eye[1] >= POS[1] - 0.55 - 1e-4, "{p:?}");
        assert!((p.roll - ROLL_RAD).abs() < 1e-4 && (p.fov_add - FOV_ADD_DEG).abs() < 1e-3);
        assert!(death_pose(2.0, POS, 0.0, 0.0, None, 0.7, -1.0).roll < 0.0, "side flips the roll");
    }

    #[test]
    fn it_turns_toward_the_killer_the_short_way_and_pulls_back_to_frame_the_body() {
        // the killer stands at +X of us: aim yaw for "toward +X" is -PI/2 (positive yaw turns toward -X)
        let want = -std::f32::consts::FRAC_PI_2;
        let start = death_pose(0.0, POS, 3.0, 0.0, Some([30.0, 1.0, -10.0]), 0.7, 1.0);
        let end = death_pose(3.0, POS, 3.0, 0.0, Some([30.0, 1.0, -10.0]), 0.7, 1.0);
        assert!(wrap(end.yaw - want).abs() < wrap(start.yaw - want).abs() * 0.3, "closer to the killer: {} -> {}", start.yaw, end.yaw);
        // the camera sits behind the body relative to the view direction, so the body is between it and the killer
        let (dx, dz) = (POS[0] - end.eye[0], POS[2] - end.eye[2]);
        let fwd = [-end.yaw.sin(), -end.yaw.cos()];
        assert!((dx * fwd[0] + dz * fwd[1]) > 1.0, "the body is ahead of the camera: ({dx}, {dz}) vs {fwd:?}");
    }

    #[test]
    fn the_hit_shake_dies_out() {
        assert!(death_pose(0.05, POS, 0.0, 0.0, None, 0.7, 1.0).shake.iter().any(|s| s.abs() > 0.01));
        assert_eq!(death_pose(0.3, POS, 0.0, 0.0, None, 0.7, 1.0).shake, [0.0, 0.0]);
    }
}
