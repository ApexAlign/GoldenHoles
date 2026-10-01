//! First-person camera + weapon feedback driven by character state.
//! Pure math (no Rapier types in the update path) so it is unit-testable.
//!
//! Conventions: `roll` > 0 leans the view clockwise (to the right).
//! Wall on the right => negative roll (lean away from the wall, Titanfall style).

use crate::character::CharacterSystem;
use rapier3d::prelude::{Real, Vector};

#[derive(Clone, Copy)]
pub struct FpsCameraCfg {
    pub wall_roll: f32,        // rad, 15 deg
    pub roll_smooth: f32,      // s, critically damped
    pub base_fov: f32,         // deg
    pub wall_fov_add: f32,     // deg while wall-running
    pub fov_smooth: f32,
    pub thrust_impulse: f32,   // m/s kick on camera Y offset at double jump
    pub thrust_fov_punch: f32, // deg added instantly at double jump
    pub spring_k: f32,         // thrust spring stiffness
    pub spring_zeta: f32,      // damping ratio
    pub recoil_recover: f32,   // s, normal weapon recoil recovery
    pub recoil_reset: f32,     // s, rapid recovery after double jump
    pub reset_window: f32,     // s the fast recovery stays active
    pub weapon_dip: f32,       // m weapon drops at double jump
}

impl Default for FpsCameraCfg {
    fn default() -> Self {
        Self {
            wall_roll: 15f32.to_radians(),
            roll_smooth: 0.09,
            base_fov: 90.0,
            wall_fov_add: 6.0,
            fov_smooth: 0.12,
            thrust_impulse: 3.6,
            thrust_fov_punch: 7.0,
            spring_k: 220.0,
            spring_zeta: 0.55,
            recoil_recover: 0.18,
            recoil_reset: 0.045,
            reset_window: 0.2,
            weapon_dip: 0.05,
        }
    }
}

/// Per-frame inputs the camera needs from gameplay.
#[derive(Clone, Copy, Default)]
pub struct CameraInput {
    /// Some(+1) wall on player's right, Some(-1) on left, None if not wall-running.
    pub wall_side: Option<f32>,
    /// Edge flag: double jump fired this tick.
    pub double_jumped: bool,
}

impl CameraInput {
    /// `view_right`: camera right vector in world space (from yaw).
    pub fn from_character(c: &CharacterSystem, view_right: Vector<Real>) -> Self {
        let wall_side = c.wall_run_normal().map(|n| {
            // normal points wall -> player, so wall is on the right if normal faces -right
            if n.dot(&view_right) < 0.0 { 1.0 } else { -1.0 }
        });
        Self { wall_side, double_jumped: c.double_jumped }
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct CameraPose {
    pub roll: f32,     // rad, apply to view matrix about forward axis
    pub y_offset: f32, // m, add to eye height
    pub fov: f32,      // deg
}

#[derive(Clone, Copy, Default, Debug)]
pub struct WeaponPose {
    pub pitch: f32, // rad, muzzle-up recoil
    pub back: f32,  // m, pushed toward camera
    pub y_dip: f32, // m, downward lag
}

pub struct FpsCamera {
    pub cfg: FpsCameraCfg,
    roll: f32,
    roll_v: f32,
    fov_extra: f32,
    fov_v: f32,
    y: f32,
    y_v: f32,
    w_pitch: f32,
    w_pitch_v: f32,
    w_back: f32,
    w_back_v: f32,
    w_dip: f32,
    w_dip_v: f32,
    reset_left: f32,
}

/// Critically damped smoothing (Unity SmoothDamp form).
fn smooth_damp(cur: f32, target: f32, vel: &mut f32, smooth: f32, dt: f32) -> f32 {
    let omega = 2.0 / smooth.max(1e-4);
    let x = omega * dt;
    let e = 1.0 / (1.0 + x + 0.48 * x * x + 0.235 * x * x * x);
    let change = cur - target;
    let temp = (*vel + omega * change) * dt;
    *vel = (*vel - omega * temp) * e;
    target + (change + temp) * e
}

impl FpsCamera {
    pub fn new(cfg: FpsCameraCfg) -> Self {
        Self {
            cfg,
            roll: 0.0, roll_v: 0.0,
            fov_extra: 0.0, fov_v: 0.0,
            y: 0.0, y_v: 0.0,
            w_pitch: 0.0, w_pitch_v: 0.0,
            w_back: 0.0, w_back_v: 0.0,
            w_dip: 0.0, w_dip_v: 0.0,
            reset_left: 0.0,
        }
    }

    /// Call when the weapon fires.
    pub fn fire_kick(&mut self, pitch: f32, back: f32) {
        self.w_pitch += pitch;
        self.w_back += back;
    }

    pub fn update(&mut self, dt: f32, input: CameraInput) -> (CameraPose, WeaponPose) {
        let c = self.cfg;

        // --- wall-run roll: +-15 deg away from wall, plus FOV widen ---
        let roll_target = input.wall_side.map_or(0.0, |s| -s * c.wall_roll);
        self.roll = smooth_damp(self.roll, roll_target, &mut self.roll_v, c.roll_smooth, dt);
        let fov_target = if input.wall_side.is_some() { c.wall_fov_add } else { 0.0 };
        self.fov_extra = smooth_damp(self.fov_extra, fov_target, &mut self.fov_v, c.fov_smooth, dt);

        // --- double jump: vertical thrust + FOV punch + rapid recoil reset ---
        if input.double_jumped {
            self.y_v += c.thrust_impulse;
            self.fov_extra += c.thrust_fov_punch;
            self.reset_left = c.reset_window;
            // kill residual recoil momentum; fast decay handles the rest
            self.w_pitch_v = 0.0;
            self.w_back_v = 0.0;
            self.w_dip_v -= c.weapon_dip * 30.0; // weapon lags down as camera thrusts up
        }

        // underdamped spring returns thrust offset to 0 (one small overshoot)
        let k = c.spring_k;
        let cdamp = 2.0 * c.spring_zeta * k.sqrt();
        self.y_v += (-k * self.y - cdamp * self.y_v) * dt;
        self.y += self.y_v * dt;

        // weapon recoil recovery: fast while reset window active
        let rs = if self.reset_left > 0.0 { c.recoil_reset } else { c.recoil_recover };
        self.reset_left = (self.reset_left - dt).max(0.0);
        self.w_pitch = smooth_damp(self.w_pitch, 0.0, &mut self.w_pitch_v, rs, dt);
        self.w_back = smooth_damp(self.w_back, 0.0, &mut self.w_back_v, rs, dt);
        self.w_dip = smooth_damp(self.w_dip, 0.0, &mut self.w_dip_v, 0.08, dt);

        (
            CameraPose { roll: self.roll, y_offset: self.y, fov: c.base_fov + self.fov_extra },
            WeaponPose { pitch: self.w_pitch, back: self.w_back, y_dip: self.w_dip },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn run(cam: &mut FpsCamera, secs: f32, inp: CameraInput) -> (CameraPose, WeaponPose) {
        let mut out = (CameraPose::default(), WeaponPose::default());
        for _ in 0..(secs / DT) as usize {
            out = cam.update(DT, inp);
        }
        out
    }

    #[test]
    fn wall_run_tilts_15_away_from_wall_and_recovers() {
        let mut cam = FpsCamera::new(FpsCameraCfg::default());
        let (p, _) = run(&mut cam, 0.6, CameraInput { wall_side: Some(1.0), double_jumped: false });
        assert!((p.roll + 15f32.to_radians()).abs() < 0.01, "roll {}", p.roll);
        let (p, _) = run(&mut cam, 0.6, CameraInput { wall_side: Some(-1.0), double_jumped: false });
        assert!((p.roll - 15f32.to_radians()).abs() < 0.01);
        let (p, _) = run(&mut cam, 0.8, CameraInput::default());
        assert!(p.roll.abs() < 0.005);
    }

    #[test]
    fn double_jump_thrusts_up_then_settles() {
        let mut cam = FpsCamera::new(FpsCameraCfg::default());
        let (p, _) = cam.update(DT, CameraInput { wall_side: None, double_jumped: true });
        assert!(p.y_offset > 0.0 || cam.y_v > 0.0);
        let mut peak = 0.0f32;
        for _ in 0..30 { peak = peak.max(cam.update(DT, CameraInput::default()).0.y_offset); }
        assert!(peak > 0.03, "peak {peak}");
        let (p, _) = run(&mut cam, 1.5, CameraInput::default());
        assert!(p.y_offset.abs() < 0.002);
        assert!((p.fov - 90.0).abs() < 0.2);
    }

    #[test]
    fn double_jump_resets_recoil_faster_than_normal() {
        let mut a = FpsCamera::new(FpsCameraCfg::default());
        let mut b = FpsCamera::new(FpsCameraCfg::default());
        a.fire_kick(0.1, 0.05);
        b.fire_kick(0.1, 0.05);
        let (_, wa) = {
            a.update(DT, CameraInput { wall_side: None, double_jumped: true });
            run(&mut a, 0.1, CameraInput::default())
        };
        let (_, wb) = {
            b.update(DT, CameraInput::default());
            run(&mut b, 0.1, CameraInput::default())
        };
        assert!(wa.pitch.abs() < wb.pitch.abs() * 0.5, "{} vs {}", wa.pitch, wb.pitch);
    }
}
