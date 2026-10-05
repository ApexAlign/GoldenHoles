//! Player settings reach the camera, look input and effects strengths.

use shinobi_client::camera::{CameraFeel, CameraInput, FpsCamera, FpsCameraCfg};
use shinobi_client::client_game::{ClientGame, ClientGameCfg, InputFrame};
use shinobi_client::input::TouchEvent;
use shinobi_client::settings::Settings;
use shinobi_sim::level::LevelDesc;

const DT: f32 = 1.0 / 60.0;

fn game() -> ClientGame {
    ClientGame::new(LevelDesc::standard(), ClientGameCfg::default())
}

fn cam(s: &Settings) -> FpsCamera {
    let mut c = FpsCamera::new(FpsCameraCfg { base_fov: s.clamped().fov_deg, ..FpsCameraCfg::default() });
    c.feel = s.feel();
    c
}

fn settle(cam: &mut FpsCamera, ticks: u32, input: CameraInput) -> (f32, f32, f32) {
    let mut last = (0.0, 0.0, 0.0);
    for _ in 0..ticks {
        let (p, _) = cam.update(DT, input);
        last = (p.roll, p.y_offset, p.fov);
    }
    last
}

#[test]
fn feel_maps_intensity_reduced_motion_and_warp() {
    assert_eq!(Settings::default().feel(), CameraFeel::default(), "defaults are full strength");
    let half = Settings { fx_intensity: 0.5, crt_warp: 0.5, ..Default::default() }.feel();
    assert_eq!((half.roll, half.thrust, half.fov_kick, half.shake), (0.5, 0.5, 0.5, 0.5));
    assert_eq!(half.warp, 0.25, "warp is the intensity times the CRT share");
    let rm = Settings { reduced_motion: true, fx_intensity: 1.0, ..Default::default() }.feel();
    assert_eq!(rm, CameraFeel::OFF, "reduced motion beats the intensity slider");
    let junk = Settings { fx_intensity: f32::NAN, crt_warp: 9.0, ..Default::default() }.feel();
    assert!(junk.shake == 1.0 && junk.warp == 1.0, "bad values are repaired, not propagated: {junk:?}");
}

#[test]
fn wall_run_tilt_scales_with_fx_intensity_and_vanishes_in_reduced_motion() {
    let wall = CameraInput { wall_side: Some(1.0), ..Default::default() };
    let full = settle(&mut cam(&Settings::default()), 60, wall).0;
    let half = settle(&mut cam(&Settings { fx_intensity: 0.5, ..Default::default() }), 60, wall).0;
    let rm = settle(&mut cam(&Settings { reduced_motion: true, ..Default::default() }), 60, wall).0;
    assert!((full + 8f32.to_radians()).abs() < 0.01, "full-strength tilt is 8 degrees: {full}");
    assert!((half - full * 0.5).abs() < 0.005, "half intensity, half tilt: {half} vs {full}");
    assert_eq!(rm, 0.0, "reduced motion: no roll");
}

#[test]
fn double_jump_thrust_and_fov_punch_scale_and_reduced_motion_removes_them() {
    let jump = CameraInput { double_jumped: true, ..Default::default() };
    let peak = |s: &Settings| {
        let mut c = cam(s);
        c.update(DT, jump);
        let (mut y, mut fov) = (0.0f32, 0.0f32);
        for _ in 0..30 {
            let (p, _) = c.update(DT, CameraInput::default());
            y = y.max(p.y_offset);
            fov = fov.max(p.fov - s.clamped().fov_deg);
        }
        (y, fov)
    };
    let (y1, f1) = peak(&Settings::default());
    let (yh, fh) = peak(&Settings { fx_intensity: 0.5, ..Default::default() });
    let (yr, fr) = peak(&Settings { reduced_motion: true, ..Default::default() });
    assert!(y1 > 0.015 && f1 > 1.2, "full strength: thrust {y1} m, FOV punch {f1} deg");
    assert!((yh / y1 - 0.5).abs() < 0.05 && (fh / f1 - 0.5).abs() < 0.1, "half: {yh}/{y1}, {fh}/{f1}");
    assert!(yr.abs() < 1e-6 && fr.abs() < 1e-3, "reduced motion: thrust {yr}, punch {fr}");
}

#[test]
fn base_fov_comes_from_settings() {
    let mut g = game();
    g.apply_settings(&Settings { fov_deg: 105.0, ..Default::default() });
    assert_eq!(g.camera.cfg.base_fov, 105.0);
    // out-of-range values are clamped by the settings, not trusted
    g.apply_settings(&Settings { fov_deg: 400.0, ..Default::default() });
    assert_eq!(g.camera.cfg.base_fov, 110.0, "FOV range is 80..110");
    let (p, _) = cam(&Settings { fov_deg: 95.0, ..Default::default() }).update(DT, CameraInput::default());
    assert!((p.fov - 95.0).abs() < 0.5);
}

#[test]
fn mouse_sensitivity_and_invert_y_apply_live() {
    let mut g = game();
    g.apply_settings(&Settings { mouse_sens: 0.004, ..Default::default() });
    g.look(-100.0, 0.0);
    assert!((g.aim.yaw - 0.4).abs() < 1e-5, "yaw {} for 100 units at 0.004 rad/unit", g.aim.yaw);
    let mut a = game();
    let mut b = game();
    a.apply_settings(&Settings::default());
    b.apply_settings(&Settings { invert_y: true, ..Default::default() });
    a.look(0.0, 20.0);
    b.look(0.0, 20.0);
    assert!(a.aim.pitch < 0.0 && b.aim.pitch > 0.0 && (a.aim.pitch + b.aim.pitch).abs() < 1e-6, "invert flips pitch only");
    // the frame's look (mouse delta accumulated per tick) goes through the same path
    let mut c = game();
    c.apply_settings(&Settings { invert_y: true, ..Default::default() });
    c.sim_tick(&InputFrame { look: (0.0, 20.0), ..Default::default() }, 0.0);
    assert!((c.aim.pitch - b.aim.pitch).abs() < 1e-6);
    // the live value also feeds the demo script's unit conversion
    assert_eq!(g.cfg.rad_per_unit, 0.004);
}

#[test]
fn touch_aim_pad_uses_its_own_sensitivity_and_stick_follows_screen_height() {
    let mut g = game();
    g.set_screen(1600.0, 800.0);
    g.apply_settings(&Settings { mouse_sens: 0.002, aim_pad_sens: 0.008, stick_radius_frac: 0.2, ..Default::default() });
    g.touch_event(TouchEvent::Down { id: 7, x: 1200.0, y: 400.0 }); // right half = aim pad
    g.touch_event(TouchEvent::Move { id: 7, x: 1150.0, y: 400.0 }); // 50 px left
    g.sim_tick(&InputFrame::default(), 0.0);
    assert!((g.aim.yaw - 50.0 * 0.008).abs() < 1e-5, "pad yaw {} (mouse sens would give {})", g.aim.yaw, 50.0 * 0.002);

    // left half: stick travel = 0.2 of the 800 px height = 160 px; 80 px drag is half deflection (past the deadzone)
    g.touch_event(TouchEvent::Down { id: 8, x: 200.0, y: 600.0 });
    g.touch_event(TouchEvent::Move { id: 8, x: 200.0, y: 520.0 });
    let intent = g.touch_stick_for_test();
    let (sx, sz) = intent;
    assert!(sx.abs() < 1e-6 && sz > 0.3 && sz < 0.7, "half-deflection forward, got ({sx}, {sz})");
}

#[test]
fn view_exposes_the_feel_for_the_shell_to_scale_shake_and_warp() {
    let mut g = game();
    g.apply_settings(&Settings { fx_intensity: 0.6, crt_warp: 0.5, ..Default::default() });
    let v = g.view(0.0);
    assert_eq!(v.feel, Settings { fx_intensity: 0.6, crt_warp: 0.5, ..Default::default() }.feel());
    g.apply_settings(&Settings { reduced_motion: true, ..Default::default() });
    assert_eq!(g.view(0.0).feel, CameraFeel::OFF);
}
