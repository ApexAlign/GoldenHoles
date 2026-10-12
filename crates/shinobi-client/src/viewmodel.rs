//! First-person katana view model. Engine-agnostic, client-only cosmetics (like `FpsCamera`).
//!
//! View space: +X right, +Y up, -Z forward; units are metres at the view-model depth.
//! The renderer draws the blade at `ViewModelPose` and the `segments()` as additive ribbons,
//! both on an overlay pass (own depth buffer) so nothing clips into world geometry.
//! Add `fov_kick_deg()` to the camera pose FOV for the dash feel.
use std::collections::VecDeque;
use std::f32::consts::PI;

use shinobi_sim::rapier3d::na::{Matrix3, Rotation3, UnitQuaternion, Vector3};

use shinobi_sim::combat::CombatEvent;

const RIBBONS: usize = 3;
/// Fixed sampling step so trail density is independent of frame rate.
const SAMPLE_DT: f32 = 1.0 / 240.0;
const MAX_SEGMENTS: usize = 768;

#[derive(Clone, Copy, Debug)]
pub struct ViewCfg {
    pub fov_deg: f32,
    pub aspect: f32,
    /// Depth (metres) of the plane the blade tip sweeps across.
    pub depth: f32,
    /// How far past the screen edge the arc starts/ends (1.0 = exactly the edge).
    pub overshoot: f32,
    pub blade_len: f32,
    pub swing_secs: f32,
    pub recover_secs: f32,
    pub trail_secs: f32,
    pub fov_kick_deg: f32,
    pub fov_kick_secs: f32,
}

impl Default for ViewCfg {
    fn default() -> Self {
        Self {
            fov_deg: 90.0, // CameraCfg::base_fov_deg
            aspect: 16.0 / 9.0,
            depth: 0.8,
            overshoot: 1.25,
            blade_len: 0.9,
            swing_secs: 0.16,
            recover_secs: 0.12,
            trail_secs: 0.18,
            fov_kick_deg: 10.0,
            fov_kick_secs: 0.3,
        }
    }
}

impl ViewCfg {
    /// Visible half-width/half-height at `depth`: `d * tan(fov/2) * aspect`, `d * tan(fov/2)`.
    /// `fov_deg` is vertical; pass the camera's current value when it changes.
    pub fn half_extents(&self) -> (f32, f32) {
        let hh = self.depth * (self.fov_deg.to_radians() * 0.5).tan();
        (hh * self.aspect, hh)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ViewModelPose {
    pub translation: Vector3<f32>,
    /// Blade extends along local -Z.
    pub rotation: UnitQuaternion<f32>,
}

impl ViewModelPose {
    fn rest() -> Self {
        Self {
            // chudan-ish guard: hand low right, blade rising across the lower right toward the centre, edge up
            translation: Vector3::new(0.22, -0.20, -0.46),
            rotation: UnitQuaternion::from_euler_angles(0.42, 0.50, 0.60),
        }
    }

    fn lerp(&self, o: &Self, t: f32) -> Self {
        Self {
            translation: self.translation.lerp(&o.translation, t),
            rotation: self.rotation.slerp(&o.rotation, t),
        }
    }
}

/// One ribbon piece in view space. `ribbon` 0 = blade tip, 1/2 = toward the hilt.
#[derive(Clone, Copy, Debug)]
pub struct ArcSegment {
    pub a: Vector3<f32>,
    pub b: Vector3<f32>,
    pub ribbon: u8,
    /// 1 -> 0 over `trail_secs`.
    pub alpha: f32,
}

#[derive(Clone, Copy)]
struct Raw {
    a: Vector3<f32>,
    b: Vector3<f32>,
    ribbon: u8,
    born: f32,
}

enum State {
    Idle,
    Swing { t: f32, sign: f32 },
    Recover { t: f32, from: ViewModelPose },
}

pub struct KatanaViewModel {
    pub cfg: ViewCfg,
    state: State,
    pose: ViewModelPose,
    segs: VecDeque<Raw>,
    last: [Option<Vector3<f32>>; RIBBONS],
    clock: f32,
    kick_t: f32,
}

impl KatanaViewModel {
    pub fn new(cfg: ViewCfg) -> Self {
        Self {
            cfg,
            state: State::Idle,
            pose: ViewModelPose::rest(),
            segs: VecDeque::new(),
            last: [None; RIBBONS],
            clock: 0.0,
            kick_t: f32::MAX,
        }
    }

    /// Feed sim events; slashes start a swing, lunges add the FOV kick.
    pub fn on_event(&mut self, e: &CombatEvent) {
        if let CombatEvent::Slash { flip, lunge, .. } = *e {
            self.start_swing(flip, lunge);
        }
    }

    pub fn start_swing(&mut self, flip: bool, lunge: bool) {
        self.state = State::Swing { t: 0.0, sign: if flip { 1.0 } else { -1.0 } };
        self.last = [None; RIBBONS];
        if lunge {
            self.kick_t = 0.0;
        }
    }

    /// Additive FOV degrees for the dash. Zero when idle.
    pub fn fov_kick_deg(&self) -> f32 {
        let u = self.kick_t / self.cfg.fov_kick_secs;
        if u >= 1.0 {
            0.0
        } else {
            self.cfg.fov_kick_deg * (PI * u.sqrt()).sin()
        }
    }

    pub fn pose(&self) -> ViewModelPose {
        self.pose
    }

    pub fn segments(&self) -> impl Iterator<Item = ArcSegment> + '_ {
        let life = self.cfg.trail_secs;
        self.segs.iter().filter_map(move |s| {
            let age = self.clock - s.born;
            (age < life).then(|| ArcSegment { a: s.a, b: s.b, ribbon: s.ribbon, alpha: 1.0 - age / life })
        })
    }

    /// Tip position (view space) for swing progress `u` in [0, 1].
    fn tip_path(&self, u: f32, sign: f32) -> Vector3<f32> {
        let c = &self.cfg;
        let (hw, hh) = c.half_extents();
        let e = 1.0 - (1.0 - u).powi(3); // ease-out: explosive start
        let start = Vector3::new(sign * hw * c.overshoot, -hh * 0.9, -c.depth);
        let end = Vector3::new(-sign * hw * c.overshoot, hh * 0.9, -c.depth);
        // bows toward the camera mid-swing (bigger on screen, never smaller than the edge points) and sags
        // slightly below the straight diagonal so the cut reads as a drawn katana arc, not a ruler line
        start.lerp(&end, e) + Vector3::new(0.0, -0.14 * hh * (PI * e).sin(), 0.25 * c.depth * (PI * e).sin())
    }

    /// Blade pose + ribbon points for progress `u`.
    fn sample(&self, u: f32, sign: f32) -> (ViewModelPose, [Vector3<f32>; RIBBONS]) {
        let tip = self.tip_path(u, sign);
        let t = self.tip_path((u + 0.02).min(1.0), sign) - self.tip_path((u - 0.02).max(0.0), sign);
        // blade stays perpendicular to travel, edge leading
        let n = Vector3::new(t.y, -t.x, 0.0) * sign;
        let n = if n.norm() > 1e-6 { n.normalize() } else { Vector3::y() };
        let len = self.cfg.blade_len;
        let hand = tip - n * len;
        // roll so the cutting edge (local +X) leads the swing; the blade's local -Z runs along `n`
        let tdir = t.normalize();
        let xa = (tdir - n * tdir.dot(&n)).try_normalize(1e-6).unwrap_or_else(Vector3::x);
        let za = -n;
        let ya = za.cross(&xa);
        let rotation = UnitQuaternion::from_rotation_matrix(&Rotation3::from_matrix_unchecked(Matrix3::from_columns(&[xa, ya, za])));
        (
            ViewModelPose { translation: hand, rotation },
            [tip, hand + n * (len * 0.6), hand + n * (len * 0.3)],
        )
    }

    pub fn update(&mut self, dt: f32) -> ViewModelPose {
        self.clock += dt;
        self.kick_t += dt;

        match self.state {
            State::Idle => {}
            State::Swing { mut t, sign } => {
                let dur = self.cfg.swing_secs;
                let mut left = dt;
                let mut done = false;
                while left > 0.0 && !done {
                    let step = left.min(SAMPLE_DT);
                    t += step;
                    left -= step;
                    let u = (t / dur).min(1.0);
                    let (pose, pts) = self.sample(u, sign);
                    self.pose = pose;
                    for (i, p) in pts.iter().enumerate() {
                        if let Some(prev) = self.last[i] {
                            if (p - prev).norm() > 1e-5 {
                                self.segs.push_back(Raw { a: prev, b: *p, ribbon: i as u8, born: self.clock });
                            }
                        }
                        self.last[i] = Some(*p);
                    }
                    done = t >= dur;
                }
                self.state = if done { State::Recover { t: 0.0, from: self.pose } } else { State::Swing { t, sign } };
            }
            State::Recover { t, from } => {
                let t = t + dt;
                let u = (t / self.cfg.recover_secs).min(1.0);
                self.pose = from.lerp(&ViewModelPose::rest(), u * u * (3.0 - 2.0 * u));
                self.state = if u >= 1.0 { State::Idle } else { State::Recover { t, from } };
            }
        }

        let life = self.cfg.trail_secs;
        while self.segs.front().map_or(false, |s| self.clock - s.born >= life) || self.segs.len() > MAX_SEGMENTS {
            self.segs.pop_front();
        }
        self.pose
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj_x(cfg: &ViewCfg, v: Vector3<f32>) -> f32 {
        // normalised device x: -1..1 spans the screen
        let half_w = (cfg.fov_deg.to_radians() * 0.5).tan() * cfg.aspect;
        v.x / -v.z / half_w
    }

    fn swing_tip_xs(cfg: ViewCfg, flip: bool) -> (f32, f32) {
        let mut vm = KatanaViewModel::new(cfg);
        vm.start_swing(flip, true);
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for _ in 0..30 {
            vm.update(1.0 / 120.0);
            for s in vm.segments().filter(|s| s.ribbon == 0) {
                for p in [s.a, s.b] {
                    let x = proj_x(&cfg, p);
                    lo = lo.min(x);
                    hi = hi.max(x);
                }
            }
        }
        (lo, hi)
    }

    #[test]
    fn arc_clears_the_whole_screen_at_several_fovs_and_aspects() {
        for (fov, aspect) in [(90.0, 16.0 / 9.0), (60.0, 16.0 / 9.0), (110.0, 21.0 / 9.0), (90.0, 4.0 / 3.0)] {
            let cfg = ViewCfg { fov_deg: fov, aspect, ..ViewCfg::default() };
            for flip in [false, true] {
                let (lo, hi) = swing_tip_xs(cfg, flip);
                assert!(lo <= -1.0 && hi >= 1.0, "fov {fov} aspect {aspect} flip {flip}: x in [{lo}, {hi}]");
            }
        }
    }

    #[test]
    fn trail_fades_and_swing_returns_to_rest() {
        let cfg = ViewCfg::default();
        let mut vm = KatanaViewModel::new(cfg);
        vm.start_swing(true, true);
        for _ in 0..240 {
            vm.update(1.0 / 120.0);
        }
        assert_eq!(vm.segments().count(), 0);
        let rest = ViewModelPose::rest();
        assert!((vm.pose().translation - rest.translation).norm() < 1e-4);
        assert!(vm.fov_kick_deg() == 0.0);
    }

    #[test]
    fn fov_kick_only_on_lunge() {
        let mut vm = KatanaViewModel::new(ViewCfg::default());
        vm.start_swing(true, false);
        vm.update(0.05);
        assert_eq!(vm.fov_kick_deg(), 0.0);
        vm.start_swing(true, true);
        vm.update(0.05);
        assert!(vm.fov_kick_deg() > 1.0);
    }
}
