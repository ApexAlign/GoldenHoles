//! Cosmetic camera feel: landing dip, dash-end settle, slide lower + roll, optional bob, FX scaling.
//! Everything scales with `Settings::feel`, is zero under reduced motion, and never touches the simulation.

use shinobi_client::camera::{CameraFeel, CameraInput, FpsCamera, FpsCameraCfg};
use shinobi_client::fx::{FxCfg, FxInput, FxState};
use shinobi_client::settings::Settings;

const DT: f32 = 1.0 / 60.0;

fn cam(s: &Settings) -> FpsCamera {
    let mut c = FpsCamera::new(FpsCameraCfg::default());
    c.feel = s.feel();
    c
}

/// (lowest eye offset, lowest FOV delta) over a second after one input, then the settled values.
fn after(cam: &mut FpsCamera, first: CameraInput) -> ((f32, f32), (f32, f32)) {
    let base = cam.cfg.base_fov;
    let (p, _) = cam.update(DT, first);
    let (mut y, mut f) = (p.y_offset, p.fov - base);
    for _ in 0..60 {
        let (p, _) = cam.update(DT, CameraInput::default());
        y = y.min(p.y_offset);
        f = f.min(p.fov - base);
    }
    for _ in 0..120 {
        cam.update(DT, CameraInput::default());
    }
    let (p, _) = cam.update(DT, CameraInput::default());
    ((y, f), (p.y_offset, p.fov - base))
}

fn landing(speed: f32) -> CameraInput {
    CameraInput { landed_speed: speed, ..Default::default() }
}

#[test]
fn hard_landing_dips_the_eye_and_narrows_fov_in_proportion_then_settles() {
    let ((y_soft, f_soft), _) = after(&mut cam(&Settings::default()), landing(5.0));
    assert!(y_soft.abs() < 1e-6 && f_soft.abs() < 1e-3, "below the threshold nothing happens: {y_soft}, {f_soft}");
    let ((y_mid, f_mid), _) = after(&mut cam(&Settings::default()), landing(12.0));
    let ((y_hard, f_hard), (y_end, f_end)) = after(&mut cam(&Settings::default()), landing(18.0));
    assert!(y_mid < -0.03 && y_hard < y_mid, "dip grows with fall speed: {y_mid} then {y_hard}");
    assert!(f_mid < -0.5 && f_hard < f_mid, "FOV narrows with fall speed: {f_mid} then {f_hard}");
    assert!(y_hard > -0.4, "a dip, not a crash: {y_hard}");
    assert!(y_end.abs() < 0.002 && f_end.abs() < 0.2, "settles back: {y_end}, {f_end}");
    // faster than full strength does not keep growing
    let ((y_fast, _), _) = after(&mut cam(&Settings::default()), landing(40.0));
    assert!((y_fast - y_hard).abs() < 1e-5);
}

#[test]
fn dash_end_undershoots_fov_and_settles() {
    let ((_, f), (_, f_end)) = after(&mut cam(&Settings::default()), CameraInput { dash_ended: true, ..Default::default() });
    assert!(f < -1.5 && f > -3.5, "undershoot {f}");
    assert!(f_end.abs() < 0.2, "settled {f_end}");
}

#[test]
fn slide_lowers_the_eye_and_leans_into_the_slide_then_recovers() {
    let mut c = cam(&Settings::default());
    let slide = CameraInput { sliding: Some(1.0), ..Default::default() };
    let (mut p, _) = c.update(DT, slide);
    for _ in 0..60 {
        p = c.update(DT, slide).0;
    }
    assert!((p.y_offset + 0.35).abs() < 0.01, "eye lowered by the slide drop: {}", p.y_offset);
    assert!((p.roll - 4f32.to_radians()).abs() < 0.005, "leans right when sliding right: {}", p.roll);
    let left = {
        let mut c = cam(&Settings::default());
        let s = CameraInput { sliding: Some(-1.0), ..Default::default() };
        let mut p = c.update(DT, s).0;
        for _ in 0..60 {
            p = c.update(DT, s).0;
        }
        p.roll
    };
    assert!((left + 4f32.to_radians()).abs() < 0.005, "and left when sliding left: {left}");
    for _ in 0..90 {
        p = c.update(DT, CameraInput::default()).0;
    }
    assert!(p.y_offset.abs() < 0.005 && p.roll.abs() < 0.003, "back to normal after the slide");
    // straight slide: lower but level
    let mut c = cam(&Settings::default());
    for _ in 0..60 {
        p = c.update(DT, CameraInput { sliding: Some(0.0), ..Default::default() }).0;
    }
    assert!(p.roll.abs() < 1e-4 && p.y_offset < -0.3);
}

#[test]
fn sprint_bob_is_off_by_default_and_bounded_when_enabled() {
    let run = CameraInput { ground_speed: 9.0, ..Default::default() };
    let peak = |cfg: FpsCameraCfg, feel: CameraFeel, input: CameraInput| {
        let mut c = FpsCamera::new(cfg);
        c.feel = feel;
        let mut hi = 0.0f32;
        for _ in 0..240 {
            hi = hi.max(c.update(DT, input).0.y_offset.abs());
        }
        hi
    };
    assert_eq!(peak(FpsCameraCfg::default(), CameraFeel::default(), run), 0.0, "off by default");
    let on = FpsCameraCfg { bob_amp: 0.018, ..Default::default() };
    let p = peak(on, CameraFeel::default(), run);
    assert!(p > 0.012 && p <= 0.0181, "bob amplitude {p}");
    assert!((peak(on, Settings { fx_intensity: 0.5, ..Default::default() }.feel(), run) / p - 0.5).abs() < 0.08, "scaled by intensity");
    assert_eq!(peak(on, CameraFeel::OFF, run), 0.0, "reduced motion: none");
    assert_eq!(peak(on, CameraFeel::default(), CameraInput { ground_speed: 2.0, ..Default::default() }), 0.0, "no bob when slow");
    assert_eq!(peak(on, CameraFeel::default(), CameraInput::default()), 0.0, "no bob in the air");
}

#[test]
fn every_new_effect_scales_with_intensity_and_vanishes_in_reduced_motion() {
    let all = |s: &Settings| {
        let mut c = cam(s);
        let (a, b) = after(&mut c, landing(18.0));
        let (d, e) = after(&mut c, CameraInput { dash_ended: true, ..Default::default() });
        let mut c = cam(s);
        let mut p = c.update(DT, CameraInput { sliding: Some(1.0), ..Default::default() }).0;
        for _ in 0..60 {
            p = c.update(DT, CameraInput { sliding: Some(1.0), ..Default::default() }).0;
        }
        (a.0, a.1, d.1, p.y_offset, p.roll, b.0 + e.0)
    };
    let full = all(&Settings::default());
    let half = all(&Settings { fx_intensity: 0.5, ..Default::default() });
    let rm = all(&Settings { reduced_motion: true, ..Default::default() });
    for (name, f, h) in [("dip", full.0, half.0), ("landing fov", full.1, half.1), ("slide drop", full.3, half.3), ("slide roll", full.4, half.4)] {
        assert!(f != 0.0 && (h / f - 0.5).abs() < 0.08, "{name}: full {f}, half {h}");
    }
    assert!(full.2 != 0.0 && (half.2 / full.2 - 0.5).abs() < 0.12, "dash-end FOV: {} vs {}", full.2, half.2);
    assert_eq!((rm.0, rm.1, rm.2, rm.3, rm.4), (0.0, 0.0, 0.0, 0.0, 0.0), "reduced motion zeroes everything: {rm:?}");
}

#[test]
fn apply_fx_scales_shell_effects_and_lunge_fov_kick() {
    // drive the real FxState through a dash (the katana lunge is a dash) and a double jump
    let mut fx = FxState::new(FxCfg::default());
    let mut out = fx.update(DT, FxInput { double_jumped: true, dash_active: true, ..Default::default() });
    for _ in 0..12 {
        out = fx.update(DT, FxInput { dash_active: true, ..Default::default() });
    }
    assert!(out.fov_add_deg > 5.0 && out.fov_warp > 0.05, "the lunge FOV kick exists: {out:?}");

    let half = Settings { fx_intensity: 0.5, ..Default::default() }.feel().apply_fx(&out);
    assert!((half.fov_add_deg - out.fov_add_deg * 0.5).abs() < 1e-5, "lunge FOV kick scaled by intensity");
    assert!((half.fov_warp - out.fov_warp * 0.5).abs() < 1e-5);
    assert!((half.pitch_kick_deg - out.pitch_kick_deg * 0.5).abs() < 1e-5 && (half.vignette - out.vignette * 0.5).abs() < 1e-5);

    let rm = Settings { reduced_motion: true, ..Default::default() }.feel().apply_fx(&out);
    assert_eq!((rm.fov_add_deg, rm.fov_warp, rm.pitch_kick_deg, rm.yaw_kick_deg, rm.vignette), (0.0, 0.0, 0.0, 0.0, 0.0));
    // decoration passes through untouched
    assert_eq!(rm.grid_pulse, out.grid_pulse);
    assert_eq!(rm.slash.active, out.slash.active);
    // CRT warp share
    let w = Settings { crt_warp: 0.5, ..Default::default() }.feel().apply_fx(&out);
    assert!((w.fov_warp - out.fov_warp * 0.5).abs() < 1e-5 && w.fov_add_deg == out.fov_add_deg);
}
