//! Draw data for other players: a low-effort neon silhouette from interpolated transform + `MoveKind`.
//! Data only. The silhouette is a handful of boxes (torso, head, legs, arm+katana) so a firefight with
//! many ninjas stays cheap on mobile; high-poly art is optional and client-side.
//!
//! Model space: origin at the feet, +Y up, facing -Z; `NinjaDraw` carries the world transform.

use shinobi_proto::snapshot::{EntityState, MoveKind, FLAG_DEAD, FLAG_INVULN, FLAG_ROLL};

use crate::hud::{Palette, Rgba};
use crate::interp::Sample;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NinjaPart {
    pub name: &'static str,
    pub offset: [f32; 3], // model space centre
    pub half: [f32; 3],   // box half extents
    pub pitch: f32,       // rad about model X
}

#[derive(Clone, Debug)]
pub struct NinjaDraw {
    /// Feet position in world space.
    pub pos: [f32; 3],
    pub yaw: f32,
    /// Extra body roll (rad) about the facing axis.
    pub roll: f32,
    pub parts: Vec<NinjaPart>,
    /// HDR outline/body colour, alpha carries invuln flicker.
    pub color: [f32; 4],
    /// 0..1 how hard the suit grid glows (scales with speed).
    pub glow: f32,
    /// Motion-smear length in metres along facing (dash/roll), 0 otherwise.
    pub smear: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct NinjaCfg {
    /// Distance from the interpolated `Sample::pos` down to the feet.
    pub feet_below_pos: f32,
    pub height: f32,
}

impl Default for NinjaCfg {
    fn default() -> Self {
        Self { feet_below_pos: 0.9, height: 1.8 }
    }
}

fn speed(st: &EntityState) -> f32 {
    let v = |i: usize| st.vel[i] as f32 / 64.0;
    (v(0) * v(0) + v(1) * v(1) + v(2) * v(2)).sqrt()
}

fn hdr(c: Rgba, gain: f32, a: f32) -> [f32; 4] {
    [c.0 * gain, c.1 * gain, c.2 * gain, a]
}

/// `friendly` picks cyan vs magenta; `t` is server seconds for the invuln flicker. Dead players draw nothing.
pub fn ninja_draw(
    s: &Sample,
    st: &EntityState,
    pal: &Palette,
    cfg: &NinjaCfg,
    friendly: bool,
    t: f32,
) -> Option<NinjaDraw> {
    if st.flags & FLAG_DEAD != 0 {
        return None;
    }
    let h = cfg.height;
    let spd = speed(st);
    let mut roll = 0.0;
    let mut smear = 0.0;
    let mut lean = (spd / 10.0).clamp(0.0, 1.0) * 0.3; // running forward lean
    let mut tuck = 0.0f32; // 0 upright legs .. 1 fully tucked
    let mut squash = 1.0f32;

    match st.kind {
        MoveKind::Grounded => {}
        MoveKind::Airborne => {
            tuck = 0.6;
            lean *= 0.5;
        }
        MoveKind::WallRun => {
            roll = 0.35; // side unknown from snapshots; renderer may mirror by wall normal
            lean = 0.4;
        }
        MoveKind::Evading => {
            if st.flags & FLAG_ROLL != 0 {
                squash = 0.55;
                tuck = 1.0;
                lean = 0.9;
            } else {
                lean = 1.1; // dash: body horizontal-ish
                smear = 2.5;
            }
        }
    }

    let hh = h * squash;
    let leg_h = hh * 0.45 * (1.0 - 0.45 * tuck);
    let parts = vec![
        NinjaPart { name: "legs", offset: [0.0, leg_h * 0.5, 0.0], half: [0.17, leg_h * 0.5, 0.11], pitch: -0.3 * tuck },
        NinjaPart { name: "torso", offset: [0.0, leg_h + hh * 0.22, 0.0], half: [0.22, hh * 0.22, 0.12], pitch: -lean },
        NinjaPart { name: "head", offset: [0.0, leg_h + hh * 0.52, -lean * 0.12], half: [0.11, 0.12, 0.11], pitch: -lean },
        // katana held at the hip, blade trailing back when moving fast
        NinjaPart { name: "katana", offset: [0.28, leg_h + hh * 0.3, 0.25], half: [0.02, 0.02, 0.55], pitch: 0.5 + lean * 0.4 },
    ];

    let base = if friendly { pal.cyan } else { pal.magenta };
    let invuln = st.flags & FLAG_INVULN != 0;
    let a = if invuln { 0.45 + 0.35 * (t * 24.0).sin().abs() } else { 1.0 };

    Some(NinjaDraw {
        pos: [s.pos[0], s.pos[1] - cfg.feet_below_pos, s.pos[2]],
        yaw: s.yaw,
        roll,
        parts,
        color: hdr(base, 2.2, a),
        glow: (0.4 + spd / 20.0).clamp(0.4, 1.0),
        smear,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Sample {
        Sample { pos: [4.0, 1.9, -2.0], yaw: 1.0, extrapolated: false }
    }
    fn st(kind: MoveKind, flags: u8, vx: i16) -> EntityState {
        EntityState { kind, flags, vel: [vx, 0, 0], ..Default::default() }
    }
    fn draw(kind: MoveKind, flags: u8, vx: i16) -> NinjaDraw {
        ninja_draw(&sample(), &st(kind, flags, vx), &Palette::TRON, &NinjaCfg::default(), false, 0.0).unwrap()
    }
    fn part<'a>(d: &'a NinjaDraw, n: &str) -> &'a NinjaPart {
        d.parts.iter().find(|p| p.name == n).unwrap()
    }

    #[test]
    fn dead_draws_nothing_and_feet_sit_below_pos() {
        assert!(ninja_draw(&sample(), &st(MoveKind::Grounded, FLAG_DEAD, 0), &Palette::TRON, &NinjaCfg::default(), true, 0.0).is_none());
        let d = draw(MoveKind::Grounded, 0, 0);
        assert!((d.pos[1] - 1.0).abs() < 1e-5);
        assert_eq!(d.parts.len(), 4, "cheap: four boxes");
    }

    #[test]
    fn running_leans_forward_with_speed() {
        let still = draw(MoveKind::Grounded, 0, 0);
        let fast = draw(MoveKind::Grounded, 0, 64 * 10);
        assert!(part(&fast, "torso").pitch < part(&still, "torso").pitch);
        assert!(fast.glow > still.glow);
    }

    #[test]
    fn airborne_tucks_legs_wallrun_rolls_dash_smears_roll_squashes() {
        let g = draw(MoveKind::Grounded, 0, 0);
        let air = draw(MoveKind::Airborne, 0, 0);
        assert!(part(&air, "legs").half[1] < part(&g, "legs").half[1]);
        assert!(draw(MoveKind::WallRun, 0, 0).roll.abs() > 0.2);
        let dash = draw(MoveKind::Evading, 0, 64 * 30);
        assert!(dash.smear > 0.0 && dash.roll == 0.0);
        let roll = draw(MoveKind::Evading, FLAG_ROLL, 64 * 10);
        assert!(roll.smear == 0.0 && part(&roll, "torso").half[1] < part(&g, "torso").half[1]);
    }

    #[test]
    fn team_colour_and_invuln_flicker() {
        let e = draw(MoveKind::Grounded, 0, 0);
        let f = ninja_draw(&sample(), &st(MoveKind::Grounded, 0, 0), &Palette::TRON, &NinjaCfg::default(), true, 0.0).unwrap();
        assert!(e.color[0] > e.color[2] * 0.5 && f.color[2] > f.color[0], "enemy magenta, friendly cyan");
        assert!(e.color[0] > 1.0, "HDR");
        let inv = draw(MoveKind::Grounded, FLAG_INVULN, 0);
        assert!(inv.color[3] < 1.0 && inv.color[3] > 0.0);
    }
}
