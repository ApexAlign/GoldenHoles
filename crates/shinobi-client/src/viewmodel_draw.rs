//! Draw data for the first-person katana: turns `viewmodel.rs` poses + arc segments into neon blade
//! geometry and instanced ribbons. Data only; drawn in the viewmodel pass (own depth buffer, own
//! projection) so it never clips into world geometry. View space: +X right, +Y up, -Z forward.

use shinobi_sim::rapier3d::na::Vector3;

use crate::emit::RibbonInstance;
use crate::fx::wrap_time;
use crate::hud::{Palette, Rgba};
use crate::viewmodel::KatanaViewModel;

/// Segment widths (m) for blade tip ribbon and the two inner ribbons.
const RIBBON_WIDTH: [f32; 3] = [0.028, 0.018, 0.012];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BladeDraw {
    pub hilt: [f32; 3],
    pub tip: [f32; 3],
    pub width: f32,
    /// HDR edge colour (bloom catches it).
    pub edge: [f32; 4],
    /// HDR body tint; `pulse` scales the animated grid on the blade.
    pub body: [f32; 4],
    pub grid_pulse: f32,
}

#[derive(Clone, Debug)]
pub struct KatanaDraw {
    pub blade: BladeDraw,
    /// View-space ribbons with alpha baked into `color.a` (life 1 s, spawn = now => no shader-side fade).
    pub ribbons: Vec<RibbonInstance>,
    /// Extra FOV degrees for the lunge (already in `viewmodel`; surfaced so the camera rig can add it).
    pub fov_kick_deg: f32,
}

fn hdr(c: Rgba, gain: f32) -> [f32; 4] {
    [c.0 * gain, c.1 * gain, c.2 * gain, 1.0]
}

fn arr(v: Vector3<f32>) -> [f32; 3] {
    [v.x, v.y, v.z]
}

/// `grid_pulse` from `FxOutput::grid_pulse` so the blade grid beats with the world grid.
pub fn katana_draw(vm: &KatanaViewModel, pal: &Palette, server_time: f64, grid_pulse: f32) -> KatanaDraw {
    let pose = vm.pose();
    let hilt = pose.translation;
    let tip = hilt + pose.rotation * Vector3::new(0.0, 0.0, -vm.cfg.blade_len);
    let blade = BladeDraw {
        hilt: arr(hilt),
        tip: arr(tip),
        width: 0.035,
        edge: hdr(pal.orange, 3.0),
        body: hdr(pal.cyan, 0.8 + 0.6 * grid_pulse),
        grid_pulse,
    };

    let t = wrap_time(server_time);
    let ribbons = vm
        .segments()
        .map(|s| {
            let w = RIBBON_WIDTH[(s.ribbon as usize).min(2)];
            let col = if s.ribbon == 0 { pal.orange } else { pal.magenta };
            RibbonInstance {
                p0: [s.a.x, s.a.y, s.a.z, w],
                p1: [s.b.x, s.b.y, s.b.z, w],
                color: [col.0 * 1.5, col.1 * 1.5, col.2 * 1.5, s.alpha],
                params: [t, 1.0, 0.0, 0.0],
            }
        })
        .collect();

    KatanaDraw { blade, ribbons, fov_kick_deg: vm.fov_kick_deg() }
}

/// Seconds for the whole throw cycle: fly out, hand empty, shuriken returns.
pub const THROW_CYCLE: f32 = 0.45;
const THROW_FLY: f32 = 0.18;
const THROW_HIDDEN: f32 = 0.30;
const SPIN_IDLE: f32 = 5.0; // rad/s

/// Seconds a hit confirmation pulses the held weapons.
pub const HIT_PULSE_SECS: f32 = 0.22;

/// Hit confirmation on the held weapons: jolts them forward and flares their brightness. `amount()` is 1 on the
/// hit, easing to 0 over `HIT_PULSE_SECS`; a kill pulses harder (`strength` 1.6).
#[derive(Clone, Copy, Debug, Default)]
pub struct HitPulse {
    t: f32,
    strength: f32,
}

impl HitPulse {
    pub fn on_hit(&mut self, kill: bool) {
        self.t = 0.0;
        self.strength = if kill { 1.6 } else { 1.0 };
    }

    pub fn update(&mut self, dt: f32) {
        self.t = (self.t + dt).min(HIT_PULSE_SECS);
    }

    pub fn amount(&self) -> f32 {
        if self.t >= HIT_PULSE_SECS {
            return 0.0;
        }
        let u = 1.0 - self.t / HIT_PULSE_SECS;
        self.strength * u * u
    }
}

/// Held shuriken (left hand, view space). Pure state machine; `FIRE` calls `throw_now`.
#[derive(Clone, Copy, Debug)]
pub struct ShurikenViewModel {
    spin: f32,
    t: f32, // seconds into the throw cycle; >= THROW_CYCLE = idle
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShurikenDraw {
    /// View-space centre (m).
    pub pos: [f32; 3],
    /// Tilt of the disc towards the camera (rad about view X).
    pub tilt: f32,
    pub spin: f32,
    /// 0 = hidden (hand empty), 1 = full size.
    pub scale: f32,
}

impl Default for ShurikenViewModel {
    fn default() -> Self {
        Self { spin: 0.0, t: THROW_CYCLE }
    }
}

impl ShurikenViewModel {
    pub const REST: [f32; 3] = [-0.24, -0.2, -0.5];

    pub fn throw_now(&mut self) {
        self.t = 0.0;
    }

    pub fn update(&mut self, dt: f32) {
        // spins faster while being thrown
        let rate = if self.t < THROW_FLY { SPIN_IDLE * 5.0 } else { SPIN_IDLE };
        self.spin = (self.spin + rate * dt) % std::f32::consts::TAU;
        self.t = (self.t + dt).min(THROW_CYCLE);
    }

    pub fn draw(&self) -> ShurikenDraw {
        let r = Self::REST;
        let (offset, scale) = if self.t < THROW_FLY {
            let p = self.t / THROW_FLY;
            // wind-up: the star is cocked back toward the camera for the first quarter, then snaps forward
            let wind = (1.0 - p / 0.25).max(0.0);
            ([0.25 * p - 0.05 * wind, 0.10 * p + 0.03 * wind, -2.2 * p * p + 0.09 * wind], 1.0 - 0.4 * p)
        } else if self.t < THROW_HIDDEN {
            ([0.0; 3], 0.0)
        } else if self.t < THROW_CYCLE {
            let p = (self.t - THROW_HIDDEN) / (THROW_CYCLE - THROW_HIDDEN);
            ([0.0; 3], p * p * (3.0 - 2.0 * p))
        } else {
            ([0.0; 3], 1.0)
        };
        ShurikenDraw { pos: [r[0] + offset[0], r[1] + offset[1], r[2] + offset[2]], tilt: -0.9, spin: self.spin, scale }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewmodel::ViewCfg;

    const DT: f32 = 1.0 / 240.0;

    #[test]
    fn idle_blade_length_matches_cfg_and_is_hdr() {
        let vm = KatanaViewModel::new(ViewCfg::default());
        let d = katana_draw(&vm, &Palette::TRON, 0.0, 1.0);
        let len = ((d.blade.tip[0] - d.blade.hilt[0]).powi(2)
            + (d.blade.tip[1] - d.blade.hilt[1]).powi(2)
            + (d.blade.tip[2] - d.blade.hilt[2]).powi(2))
        .sqrt();
        assert!((len - vm.cfg.blade_len).abs() < 1e-4);
        assert!(d.blade.edge[0] > 1.0, "edge must exceed the bloom threshold");
        assert!(d.ribbons.is_empty() && d.fov_kick_deg == 0.0);
    }

    #[test]
    fn swing_emits_fading_ribbons_across_the_view() {
        let mut vm = KatanaViewModel::new(ViewCfg::default());
        vm.start_swing(false, true);
        for _ in 0..24 {
            vm.update(DT);
        }
        let d = katana_draw(&vm, &Palette::TRON, 2.0, 0.5);
        assert!(!d.ribbons.is_empty());
        assert!(d.ribbons.iter().all(|r| r.color[3] > 0.0 && r.color[3] <= 1.0));
        assert!(d.ribbons.iter().all(|r| r.p0[2] < 0.0), "in front of the camera (-Z)");
        assert!(d.fov_kick_deg > 0.0, "lunge kicks FOV");
        // later the trail fully expires
        for _ in 0..(0.5 / DT) as usize {
            vm.update(DT);
        }
        assert!(katana_draw(&vm, &Palette::TRON, 3.0, 0.5).ribbons.is_empty());
    }

    #[test]
    fn grid_pulse_modulates_blade_body() {
        let vm = KatanaViewModel::new(ViewCfg::default());
        let lo = katana_draw(&vm, &Palette::TRON, 0.0, 0.0).blade.body[2];
        let hi = katana_draw(&vm, &Palette::TRON, 0.0, 1.0).blade.body[2];
        assert!(hi > lo);
    }

    #[test]
    fn shuriken_idle_throw_hidden_return_cycle() {
        let mut sh = ShurikenViewModel::default();
        assert_eq!(sh.draw().scale, 1.0);
        assert_eq!(sh.draw().pos, ShurikenViewModel::REST);

        sh.throw_now();
        for _ in 0..30 {
            sh.update(0.005); // 0.15 s in: flying
        }
        let fly = sh.draw();
        assert!(fly.pos[2] < ShurikenViewModel::REST[2] - 0.5, "flies away from the camera: {fly:?}");
        assert!(fly.scale < 1.0 && fly.scale > 0.0);

        for _ in 0..30 {
            sh.update(0.005); // 0.30 s: hand empty
        }
        assert_eq!(sh.draw().scale, 0.0);
        for _ in 0..10 {
            sh.update(0.005); // 0.35 s: coming back
        }
        let back = sh.draw();
        assert!(back.scale > 0.0 && back.scale < 1.0 && back.pos == ShurikenViewModel::REST);
        for _ in 0..40 {
            sh.update(0.005);
        }
        assert_eq!(sh.draw().scale, 1.0, "cycle ends back at rest");
    }

    #[test]
    fn hit_pulse_peaks_on_hit_and_decays_to_zero() {
        let mut p = HitPulse::default();
        assert_eq!(p.amount(), 0.0);
        p.on_hit(false);
        assert_eq!(p.amount(), 1.0);
        p.update(HIT_PULSE_SECS * 0.5);
        assert!(p.amount() > 0.0 && p.amount() < 1.0);
        p.update(HIT_PULSE_SECS);
        assert_eq!(p.amount(), 0.0);
        p.on_hit(true);
        assert!(p.amount() > 1.0, "a kill pulses harder");
    }

    #[test]
    fn throw_winds_up_toward_the_camera_before_snapping_forward() {
        let mut sh = ShurikenViewModel::default();
        sh.throw_now();
        sh.update(0.005);
        assert!(sh.draw().pos[2] > ShurikenViewModel::REST[2], "cocked back first");
    }

    #[test]
    fn shuriken_keeps_spinning_and_wraps() {
        let mut sh = ShurikenViewModel::default();
        let mut last = 0.0;
        let mut wrapped = false;
        for _ in 0..600 {
            sh.update(0.01);
            let sp = sh.draw().spin;
            wrapped |= sp < last;
            last = sp;
            assert!((0.0..std::f32::consts::TAU).contains(&sp));
        }
        assert!(wrapped, "spin angle wraps instead of growing unbounded");
    }
}
