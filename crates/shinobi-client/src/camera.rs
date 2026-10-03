//! First-person camera + weapon feedback driven by character state.
//! Pure math (no Rapier types in the update path) so it is unit-testable.
//!
//! Conventions: `roll` > 0 leans the view clockwise (to the right).
//! Wall on the right => negative roll (lean away from the wall, Titanfall style).

use crate::input::Aim;
use shinobi_sim::character::CharacterSystem;
use shinobi_sim::rapier3d::prelude::{Real, Vector};

/// Player-facing strength of the camera feedback, each 0..=1 (1 = as designed). Derived from `Settings::feel()`:
/// the FX-intensity slider scales everything, reduced-motion zeroes the vestibular ones (roll, thrust, FOV kicks,
/// shake, lens warp). `shake` and `warp` are applied by the shell to `FxState` output (see `RenderView::feel`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraFeel {
    pub roll: f32,
    pub thrust: f32,
    pub fov_kick: f32,
    pub shake: f32,
    pub warp: f32,
}

impl Default for CameraFeel {
    fn default() -> Self {
        Self { roll: 1.0, thrust: 1.0, fov_kick: 1.0, shake: 1.0, warp: 1.0 }
    }
}

impl CameraFeel {
    pub const OFF: Self = Self { roll: 0.0, thrust: 0.0, fov_kick: 0.0, shake: 0.0, warp: 0.0 };

    /// The shell's screen effects scaled by the player's strengths: look kick and vignette squeeze by `shake`,
    /// the dash/lunge FOV add by `fov_kick`, the lens warp by `warp`. The slash sweep and the grid pulse are
    /// decoration, not motion, and pass through. Always run `FxState` output through this.
    pub fn apply_fx(&self, o: &crate::fx::FxOutput) -> crate::fx::FxOutput {
        crate::fx::FxOutput {
            pitch_kick_deg: o.pitch_kick_deg * self.shake,
            yaw_kick_deg: o.yaw_kick_deg * self.shake,
            fov_add_deg: o.fov_add_deg * self.fov_kick,
            fov_warp: o.fov_warp * self.warp,
            vignette: o.vignette * self.shake,
            ..*o
        }
    }
}

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
    pub landing_min_speed: f32,  // m/s fall speed below which a landing is silent
    pub landing_full_speed: f32, // m/s fall speed of a full-strength landing
    pub landing_dip: f32,        // m/s downward kick on the eye at a full landing
    pub landing_fov_settle: f32, // deg the FOV narrows at a full landing
    pub dash_end_fov_settle: f32, // deg the FOV undershoots when a dash ends
    pub slide_drop: f32,         // m the eye lowers while sliding
    pub slide_roll: f32,         // rad of roll into the slide's lateral direction
    pub slide_smooth: f32,       // s
    /// Metres of vertical sprint bob. 0 = off, which is the default (some players get sick from it).
    pub bob_amp: f32,
    pub bob_hz: f32,             // cycles/s at `bob_ref_speed`
    pub bob_ref_speed: f32,
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
            landing_min_speed: 6.0,
            landing_full_speed: 18.0,
            landing_dip: 2.4,
            landing_fov_settle: 3.0,
            dash_end_fov_settle: 2.5,
            slide_drop: 0.35,
            slide_roll: 4f32.to_radians(),
            slide_smooth: 0.08,
            bob_amp: 0.0,
            bob_hz: 2.2,
            bob_ref_speed: 9.0,
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
    /// Fall speed (m/s, positive) on a tick that landed; 0 otherwise. Below `landing_min_speed` nothing happens.
    pub landed_speed: f32,
    /// Edge flag: a dash/roll ended this tick.
    pub dash_ended: bool,
    /// Sliding: lateral component of the slide direction in the view frame (-1 left .. +1 right).
    pub sliding: Option<f32>,
    /// Grounded horizontal speed (m/s) for the optional sprint bob; 0 when airborne.
    pub ground_speed: f32,
}

/// World-space camera right for `aim`. Yaw 0 faces -Z, positive yaw turns left (toward -X), so
/// right = (cos yaw, 0, -sin yaw). Same basis the server uses in `PlayerIntent::world_move`.
pub fn view_right(aim: &Aim) -> Vector<Real> {
    let (s, c) = aim.yaw.sin_cos();
    Vector::new(c, 0.0, -s)
}

/// World-space look direction. Pitch is +up.
pub fn view_forward(aim: &Aim) -> Vector<Real> {
    let (s, c) = aim.yaw.sin_cos();
    let (sp, cp) = aim.pitch.sin_cos();
    Vector::new(-s * cp, sp, -c * cp)
}

impl CameraInput {
    /// Wall side is judged in the *view* frame from `aim`, so looking back along the wall flips the tilt.
    pub fn from_character(c: &CharacterSystem, aim: &Aim) -> Self {
        let right = view_right(aim);
        let wall_side = c.wall_run_normal().map(|n| {
            // normal points wall -> player, so the wall is on the right if the normal faces -right
            if n.dot(&right) < 0.0 { 1.0 } else { -1.0 }
        });
        let v = c.vel;
        let hs = (v.x * v.x + v.z * v.z).sqrt();
        let sliding = c.sliding().then(|| if hs > 1e-3 { ((v.x * right.x + v.z * right.z) / hs).clamp(-1.0, 1.0) } else { 0.0 });
        let ground_speed = if c.state == shinobi_sim::character::MoveState::Grounded { hs } else { 0.0 };
        Self { wall_side, double_jumped: c.double_jumped, landed_speed: 0.0, dash_ended: false, sliding, ground_speed }
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct CameraPose {
    pub yaw: f32,      // rad, from `Aim` (0 = -Z, + = left)
    pub pitch: f32,    // rad, from `Aim` (+ = up)
    pub roll: f32,     // rad, apply to view matrix about forward axis
    pub y_offset: f32, // m, add to eye height
    /// m, eye shift in the VIEW frame (right, up): the hit-stop shake and the directional kick of a katana hit. A
    /// translation only: it never rotates the view, so the crosshair stays on the aim that goes out in intents.
    pub offset: [f32; 2],
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
    pub feel: CameraFeel,
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
    hit: HitReact,
    slide_y: f32,
    slide_y_v: f32,
    bob_phase: f32,
    bob_amt: f32,
    bob_amt_v: f32,
}

/// A katana hit reaching the camera: `stop` seconds of tight shake (the hit-stop), then a spring-back. All of it is
/// translation, roll and FOV; none of it touches yaw / pitch.
#[derive(Clone, Copy, Default, Debug)]
struct HitReact {
    /// Seconds since the hit; `>= total` = finished.
    age: f32,
    stop: f32,
    total: f32,
    /// View-frame kick direction (right, up), unit-ish, away from where the blade went.
    dir: [f32; 2],
    kick: f32,
    roll: f32,
    fov: f32,
    active: bool,
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
            feel: CameraFeel::default(),
            roll: 0.0, roll_v: 0.0,
            fov_extra: 0.0, fov_v: 0.0,
            y: 0.0, y_v: 0.0,
            w_pitch: 0.0, w_pitch_v: 0.0,
            w_back: 0.0, w_back_v: 0.0,
            w_dip: 0.0, w_dip_v: 0.0,
            reset_left: 0.0,
            hit: HitReact::default(),
            slide_y: 0.0, slide_y_v: 0.0,
            bob_phase: 0.0, bob_amt: 0.0, bob_amt_v: 0.0,
        }
    }

    /// A katana hit landed. `slash_right` = -1..1, how far to the right (in the view frame) the blade travelled; the
    /// camera nudges the other way. `stop` = the hit-stop seconds (shake for that long), `killed` = a bigger kick.
    /// Scaled by `feel.shake` (zero under reduced motion); the strongest request in a window wins.
    pub fn hit_react(&mut self, slash_right: f32, killed: bool, stop: f32) {
        let k = self.feel.shake;
        if k <= 0.0 {
            return;
        }
        let (kick, roll, fov) = if killed { (0.07, 0.07, 5.0) } else { (0.035, 0.035, 2.5) };
        let incoming = kick * k;
        if self.hit.active && self.hit.kick >= incoming {
            return;
        }
        let sx = -slash_right.clamp(-1.0, 1.0);
        self.hit = HitReact { age: 0.0, stop, total: stop + 0.18, dir: [0.7 * sx, -0.5], kick: incoming, roll: roll * k * if sx >= 0.0 { 1.0 } else { -1.0 }, fov: fov * k, active: true };
    }

    /// Call when the weapon fires.
    pub fn fire_kick(&mut self, pitch: f32, back: f32) {
        self.w_pitch += pitch;
        self.w_back += back;
    }

    /// Per-frame entry point: aim from the input pipeline, feedback from the character.
    pub fn frame(&mut self, dt: f32, c: &CharacterSystem, aim: &Aim) -> (CameraPose, WeaponPose) {
        self.update_aim(dt, CameraInput::from_character(c, aim), aim)
    }

    /// `update` with the pose's look angles filled from `aim`.
    pub fn update_aim(&mut self, dt: f32, input: CameraInput, aim: &Aim) -> (CameraPose, WeaponPose) {
        let (mut pose, weapon) = self.update(dt, input);
        pose.yaw = aim.yaw;
        pose.pitch = aim.pitch;
        (pose, weapon)
    }

    pub fn update(&mut self, dt: f32, input: CameraInput) -> (CameraPose, WeaponPose) {
        let c = self.cfg;

        // --- wall-run roll: +-15 deg away from wall, plus FOV widen ---
        let slide_roll = input.sliding.map_or(0.0, |lat| lat * c.slide_roll * self.feel.roll);
        let roll_target = input.wall_side.map_or(0.0, |s| -s * c.wall_roll * self.feel.roll) + slide_roll;
        self.roll = smooth_damp(self.roll, roll_target, &mut self.roll_v, c.roll_smooth, dt);
        let fov_target = if input.wall_side.is_some() { c.wall_fov_add * self.feel.fov_kick } else { 0.0 };
        self.fov_extra = smooth_damp(self.fov_extra, fov_target, &mut self.fov_v, c.fov_smooth, dt);

        // --- double jump: vertical thrust + FOV punch + rapid recoil reset ---
        if input.double_jumped {
            self.y_v += c.thrust_impulse * self.feel.thrust;
            self.fov_extra += c.thrust_fov_punch * self.feel.fov_kick;
            self.reset_left = c.reset_window;
            // kill residual recoil momentum; fast decay handles the rest
            self.w_pitch_v = 0.0;
            self.w_back_v = 0.0;
            self.w_dip_v -= c.weapon_dip * 30.0; // weapon lags down as camera thrusts up
        }

        // --- hard landing: the eye dips and the FOV narrows, both scaling with fall speed ---
        if input.landed_speed > c.landing_min_speed {
            let k = ((input.landed_speed - c.landing_min_speed) / (c.landing_full_speed - c.landing_min_speed).max(1e-3)).clamp(0.0, 1.0);
            self.y_v -= c.landing_dip * k * self.feel.thrust;
            self.fov_extra -= c.landing_fov_settle * k * self.feel.fov_kick;
        }
        // --- dash end: FOV undershoots then settles back ---
        if input.dash_ended {
            self.fov_extra -= c.dash_end_fov_settle * self.feel.fov_kick;
        }
        // --- slide: eye lowers ---
        // not scaled by `feel.thrust`: the shot origin drops by the same amount (`Cfg::slide_eye_drop`), so the
        // crosshair must too
        let slide_target = if input.sliding.is_some() { -c.slide_drop } else { 0.0 };
        self.slide_y = smooth_damp(self.slide_y, slide_target, &mut self.slide_y_v, c.slide_smooth, dt);
        // --- optional sprint bob (off unless `bob_amp` > 0) ---
        let bob_target = if c.bob_amp > 0.0 && input.ground_speed > 3.0 {
            c.bob_amp * (input.ground_speed / c.bob_ref_speed).min(1.0) * self.feel.thrust
        } else {
            0.0
        };
        self.bob_amt = smooth_damp(self.bob_amt, bob_target, &mut self.bob_amt_v, 0.12, dt);
        if self.bob_amt > 1e-5 {
            self.bob_phase = (self.bob_phase + std::f32::consts::TAU * c.bob_hz * dt * (input.ground_speed / c.bob_ref_speed)).rem_euclid(std::f32::consts::TAU);
        }
        let bob = self.bob_amt * self.bob_phase.sin();

        // --- katana hit: shake during the hit-stop, then the kick springs back (translation / roll / FOV only) ---
        let mut offset = [0.0f32; 2];
        let mut hit_roll = 0.0;
        let mut hit_fov = 0.0;
        if self.hit.active {
            self.hit.age += dt;
            let h = self.hit;
            if h.age >= h.total {
                self.hit.active = false;
            } else {
                let env = if h.age < h.stop { 1.0 } else { 1.0 - (h.age - h.stop) / (h.total - h.stop).max(1e-3) };
                let env = env.clamp(0.0, 1.0);
                let shake = if h.age < h.stop { (h.age * 220.0).sin() * 0.35 } else { 0.0 };
                offset = [h.dir[0] * h.kick * env + shake * h.kick, h.dir[1] * h.kick * env - shake * h.kick * 0.6];
                hit_roll = h.roll * env;
                hit_fov = -h.fov * env;
            }
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
            CameraPose { yaw: 0.0, pitch: 0.0, roll: self.roll + hit_roll, y_offset: self.y + self.slide_y + bob, offset, fov: c.base_fov + self.fov_extra + hit_fov },
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
        let (p, _) = run(&mut cam, 0.6, CameraInput { wall_side: Some(1.0), ..Default::default() });
        assert!((p.roll + 15f32.to_radians()).abs() < 0.01, "roll {}", p.roll);
        let (p, _) = run(&mut cam, 0.6, CameraInput { wall_side: Some(-1.0), ..Default::default() });
        assert!((p.roll - 15f32.to_radians()).abs() < 0.01);
        let (p, _) = run(&mut cam, 0.8, CameraInput::default());
        assert!(p.roll.abs() < 0.005);
    }

    #[test]
    fn double_jump_thrusts_up_then_settles() {
        let mut cam = FpsCamera::new(FpsCameraCfg::default());
        let (p, _) = cam.update(DT, CameraInput { double_jumped: true, ..Default::default() });
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
            a.update(DT, CameraInput { double_jumped: true, ..Default::default() });
            run(&mut a, 0.1, CameraInput::default())
        };
        let (_, wb) = {
            b.update(DT, CameraInput::default());
            run(&mut b, 0.1, CameraInput::default())
        };
        assert!(wa.pitch.abs() < wb.pitch.abs() * 0.5, "{} vs {}", wa.pitch, wb.pitch);
    }

    #[test]
    fn view_basis_matches_aim_conventions() {
        let close = |a: Vector<Real>, b: [f32; 3]| (a - Vector::new(b[0], b[1], b[2])).norm() < 1e-5;
        let a0 = Aim { yaw: 0.0, pitch: 0.0 };
        assert!(close(view_forward(&a0), [0.0, 0.0, -1.0]), "yaw 0 faces -Z");
        assert!(close(view_right(&a0), [1.0, 0.0, 0.0]));
        let left = Aim { yaw: std::f32::consts::FRAC_PI_2, pitch: 0.0 };
        assert!(close(view_forward(&left), [-1.0, 0.0, 0.0]), "positive yaw turns left");
        assert!(close(view_right(&left), [0.0, 0.0, -1.0]));
        let up = Aim { yaw: 0.0, pitch: 0.5 };
        assert!(view_forward(&up).y > 0.4, "+pitch looks up");
        // forward is unit length and always perpendicular to right
        let a = Aim { yaw: 1.3, pitch: -0.7 };
        assert!((view_forward(&a).norm() - 1.0).abs() < 1e-5);
        assert!(view_forward(&a).dot(&view_right(&a)).abs() < 1e-5);
    }

    #[test]
    fn mouse_motion_drives_pose_angles() {
        let mut aim = Aim::default();
        aim.look(-100.0, -50.0, 0.002); // mouse left + up
        assert!(aim.yaw > 0.0 && aim.pitch > 0.0);
        let mut cam = FpsCamera::new(FpsCameraCfg::default());
        let (p, _) = cam.update_aim(DT, CameraInput::default(), &aim);
        assert_eq!((p.yaw, p.pitch), (aim.yaw, aim.pitch));
    }

    #[test]
    fn a_katana_hit_shakes_then_springs_back_and_never_rotates_the_view() {
        let mut cam = FpsCamera::new(FpsCameraCfg::default());
        let aim = Aim { yaw: 0.7, pitch: -0.2 };
        cam.hit_react(1.0, false, 0.06);
        let mut peak = 0.0f32;
        let mut last = CameraPose::default();
        for _ in 0..30 {
            let (p, _) = cam.update_aim(DT, CameraInput::default(), &aim);
            peak = peak.max(p.offset[0].abs().max(p.offset[1].abs()));
            assert_eq!((p.yaw, p.pitch), (aim.yaw, aim.pitch), "the hit must not move the crosshair");
            last = p;
        }
        assert!(peak > 0.02 && peak < 0.1, "peak kick {peak} m");
        assert!(last.offset == [0.0, 0.0] && last.roll.abs() < 1e-4, "back to rest after 0.5 s: {last:?}");
    }

    #[test]
    fn a_kill_kicks_harder_and_reduced_motion_removes_the_hit_reaction() {
        let peak = |killed: bool, feel: CameraFeel| {
            let mut cam = FpsCamera::new(FpsCameraCfg::default());
            cam.feel = feel;
            cam.hit_react(0.0, killed, 0.08);
            let mut m = 0.0f32;
            let mut roll = 0.0f32;
            for _ in 0..30 {
                let (p, _) = cam.update(DT, CameraInput::default());
                m = m.max(p.offset[0].abs().max(p.offset[1].abs()));
                roll = roll.max(p.roll.abs());
            }
            (m, roll)
        };
        let (hit, kill) = (peak(false, CameraFeel::default()), peak(true, CameraFeel::default()));
        assert!(kill.0 > hit.0 * 1.5 && kill.1 > hit.1 * 1.5, "kill {kill:?} vs hit {hit:?}");
        assert_eq!(peak(true, CameraFeel::OFF), (0.0, 0.0), "reduced motion: nothing");
    }

    #[test]
    fn the_kick_goes_against_the_blade_direction() {
        let first = |right: f32| {
            let mut cam = FpsCamera::new(FpsCameraCfg::default());
            cam.hit_react(right, false, 0.0);
            cam.update(DT, CameraInput::default()).0.offset[0]
        };
        assert!(first(1.0) < 0.0 && first(-1.0) > 0.0, "kick opposes the slash: {} / {}", first(1.0), first(-1.0));
    }
}
