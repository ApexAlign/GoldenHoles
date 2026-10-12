//! Gamepad mapping, platform-free: the shell hands a `PadSnapshot` (sticks, analog triggers, buttons) once per rendered
//! frame and gets back a move stick, a look turn for this frame and the button edges, in the same shapes keyboard + mouse
//! produce (`InputFrame.stick`, `Settings::look`, `ButtonEvent`), so a pad plays exactly like the PC controls.
//!
//! Layout (Xbox naming): left stick move, right stick look, RT throw shuriken, LT katana slash, A jump, B slide,
//! X grapple, Y roll, LB dash, RB print floor, D-pad right print wall, stick forward into a wall = wall cling.

use crate::client_game::{Action, ButtonEvent};
use crate::settings::MOUSE_SENS;

/// Buttons the mapper reads; the shell translates its pad API into these.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pad {
    South,
    East,
    West,
    North,
    LeftBumper,
    RightBumper,
    DPadRight,
    Start,
    DPadUp,
    DPadLeft,
}
const NPAD: usize = 10;

#[derive(Clone, Copy, Default, Debug)]
pub struct PadSnapshot {
    /// Stick axes, x right / y up, each -1..1.
    pub left: (f32, f32),
    pub right: (f32, f32),
    /// Analog triggers 0..1.
    pub lt: f32,
    pub rt: f32,
    held: [bool; NPAD],
}

impl PadSnapshot {
    pub fn set(&mut self, b: Pad, down: bool) {
        self.held[b as usize] = down;
    }
    pub fn held(&self, b: Pad) -> bool {
        self.held[b as usize]
    }
}

/// What one frame of pad input becomes.
#[derive(Clone, Default, Debug)]
pub struct PadOut {
    /// View-frame move stick (x strafe right, z forward), deadzone-free and length <= 1.
    pub stick: (f32, f32),
    /// Look in the units `Settings::look` takes for the mouse (so the sensitivity slider and invert-Y apply to the pad).
    pub look: (f32, f32),
    pub events: Vec<ButtonEvent>,
    /// Start (or any press) while the pointer is free: the shell treats it as "click to play".
    pub start_pressed: bool,
    /// Any stick / button activity this frame (for "a pad is in use" switching).
    pub active: bool,
}

pub const DEADZONE: f32 = 0.18;
/// Right-stick turn rate at full deflection, rad/s, at the default mouse sensitivity.
pub const TURN_RATE: f32 = 3.6;
/// Extra turn rate after holding the stick near its limit (the aim "accelerates" for fast 180s, then eases back).
pub const TURN_BOOST: f32 = 0.6;
const BOOST_RAMP_SECS: f32 = 0.35;
const PRESS: f32 = 0.5;
const RELEASE: f32 = 0.35;

#[derive(Clone, Copy, Default, Debug)]
pub struct PadMapper {
    prev: [bool; NPAD],
    lt_down: bool,
    rt_down: bool,
    cling: bool,
    /// Seconds the right stick has sat above 90 % deflection.
    pegged: f32,
}

/// Radial deadzone with rescale: 0 inside `dz`, then 0..1 out to the rim, direction kept (no axis snapping).
pub fn radial(v: (f32, f32), dz: f32) -> (f32, f32) {
    let m = (v.0 * v.0 + v.1 * v.1).sqrt();
    if !(m.is_finite()) || m <= dz {
        return (0.0, 0.0);
    }
    let s = ((m - dz) / (1.0 - dz)).min(1.0) / m;
    (v.0 * s, v.1 * s)
}

/// Look response: gentle near the centre for fine aim, full at the rim (`m` = rescaled magnitude 0..1).
pub fn look_curve(m: f32) -> f32 {
    m * m * 0.6 + m * 0.4
}

fn edge(out: &mut Vec<ButtonEvent>, was: bool, now: bool, action: Action) {
    if now != was {
        out.push(ButtonEvent { action, down: now });
    }
}

impl PadMapper {
    /// One rendered frame. `mouse_sens` is the settings' mouse sensitivity (rad/count) so the slider scales pad turning
    /// too; `dt` the real frame time in seconds.
    pub fn update(&mut self, s: &PadSnapshot, dt: f32, mouse_sens: f32) -> PadOut {
        let mut out = PadOut::default();
        let mv = radial(s.left, DEADZONE);
        out.stick = mv;

        // look: curve on the magnitude, boost after holding the rim, units chosen so Settings::look yields rad/s * dt
        let r = radial(s.right, DEADZONE);
        let m = (r.0 * r.0 + r.1 * r.1).sqrt();
        self.pegged = if m > 0.9 { (self.pegged + dt).min(BOOST_RAMP_SECS) } else { 0.0 };
        if m > 0.0 {
            let boost = 1.0 + TURN_BOOST * self.pegged / BOOST_RAMP_SECS;
            let rate = TURN_RATE * look_curve(m) * boost * (mouse_sens / MOUSE_SENS.2); // rad/s along the stick direction
            let (dx, dy) = (r.0 / m, -r.1 / m); // stick up = look up = negative "down"
            let per_unit = mouse_sens.max(1e-6);
            out.look = (dx * rate * dt / per_unit, dy * rate * dt / per_unit);
        }

        // triggers -> buttons with hysteresis
        let rt = if self.rt_down { s.rt > RELEASE } else { s.rt > PRESS };
        let lt = if self.lt_down { s.lt > RELEASE } else { s.lt > PRESS };
        edge(&mut out.events, self.rt_down, rt, Action::Fire);
        edge(&mut out.events, self.lt_down, lt, Action::Slice);
        self.rt_down = rt;
        self.lt_down = lt;

        // forward into a wall: cling while the stick is pushed forward (the server validates the contact)
        let cling = mv.1 > 0.6;
        edge(&mut out.events, self.cling, cling, Action::Cling);
        self.cling = cling;

        let now = s.held;
        for (b, held) in now.iter().enumerate() {
            let was = self.prev[b];
            let pad = [Pad::South, Pad::East, Pad::West, Pad::North, Pad::LeftBumper, Pad::RightBumper, Pad::DPadRight, Pad::Start, Pad::DPadUp, Pad::DPadLeft][b];
            let action = match pad {
                Pad::South => Some(Action::Jump),
                Pad::East => Some(Action::Slide),
                Pad::West => Some(Action::Grapple),
                Pad::North => Some(Action::Roll),
                Pad::LeftBumper => Some(Action::Dash),
                Pad::RightBumper => Some(Action::Block), // hold the right bumper to guard / parry
                Pad::DPadRight => Some(Action::BuildFloor),
                Pad::DPadUp => Some(Action::Katana), // weapon slots: up = 1 katana, left = 2 bow
                Pad::DPadLeft => Some(Action::Bow),
                Pad::Start => None,
            };
            if let Some(a) = action {
                edge(&mut out.events, was, *held, a);
            }
            if pad == Pad::Start && *held && !was {
                out.start_pressed = true;
            }
        }
        self.prev = now;
        out.active = mv != (0.0, 0.0) || m > 0.0 || s.lt > 0.05 || s.rt > 0.05 || now.iter().any(|b| *b);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> PadSnapshot {
        PadSnapshot::default()
    }

    #[test]
    fn deadzone_swallows_drift_and_the_rim_reaches_one_without_axis_snapping() {
        assert_eq!(radial((0.1, 0.1), DEADZONE), (0.0, 0.0));
        let (x, y) = radial((1.0, 0.0), DEADZONE);
        assert!((x - 1.0).abs() < 1e-6 && y == 0.0);
        let (x, y) = radial((0.5, 0.5), DEADZONE);
        assert!((x - y).abs() < 1e-6 && x > 0.0, "diagonal stays diagonal");
        let (x, y) = radial((1.0, 1.0), DEADZONE); // square-gate corner: clamped to the unit circle
        assert!(((x * x + y * y).sqrt() - 1.0).abs() < 1e-5);
        assert_eq!(radial((f32::NAN, 0.0), DEADZONE), (0.0, 0.0));
    }

    #[test]
    fn left_stick_is_the_move_stick_forward_is_up() {
        let mut m = PadMapper::default();
        let mut s = snap();
        s.left = (0.0, 1.0);
        let o = m.update(&s, 1.0 / 60.0, MOUSE_SENS.2);
        assert!(o.stick.1 > 0.99 && o.stick.0 == 0.0);
        s.left = (1.0, 0.0);
        assert!(m.update(&s, 1.0 / 60.0, MOUSE_SENS.2).stick.0 > 0.99);
    }

    #[test]
    fn right_stick_turns_at_the_rate_and_scales_with_the_sensitivity_slider() {
        let mut m = PadMapper::default();
        let mut s = snap();
        s.right = (1.0, 0.0);
        let dt = 1.0 / 60.0;
        let o = m.update(&s, dt, MOUSE_SENS.2);
        // Settings::look turns by units * mouse_sens: TURN_RATE * dt (plus the first frame's sliver of boost)
        let turned = o.look.0 * MOUSE_SENS.2;
        let expect = TURN_RATE * dt * look_curve(1.0) * (1.0 + TURN_BOOST * dt / BOOST_RAMP_SECS);
        assert!((turned - expect).abs() < 1e-5, "{turned} vs {expect}");
        assert_eq!(o.look.1, 0.0);
        // doubling the sensitivity doubles the angle turned per frame for the same stick
        let mut m2 = PadMapper::default();
        let o2 = m2.update(&s, dt, MOUSE_SENS.2 * 2.0);
        assert!((o2.look.0 * MOUSE_SENS.2 * 2.0 / (turned) - 2.0).abs() < 1e-3);
        // stick up looks up (negative dy: Aim::look subtracts dy)
        s.right = (0.0, 1.0);
        assert!(PadMapper::default().update(&s, dt, MOUSE_SENS.2).look.1 < 0.0);
    }

    #[test]
    fn small_deflection_is_fine_aim_and_holding_the_rim_accelerates_the_turn() {
        let dt = 1.0 / 60.0;
        let mut m = PadMapper::default();
        let mut s = snap();
        s.right = (0.45, 0.0);
        let gentle = m.update(&s, dt, MOUSE_SENS.2).look.0;
        s.right = (1.0, 0.0);
        let first = PadMapper::default().update(&s, dt, MOUSE_SENS.2).look.0;
        assert!(gentle < first * 0.3, "half stick is much slower than the rim: {gentle} vs {first}");
        let mut m = PadMapper::default();
        let mut last = 0.0;
        for _ in 0..30 {
            last = m.update(&s, dt, MOUSE_SENS.2).look.0;
        }
        assert!(last > first * 1.5, "boost after holding the rim: {first} -> {last}");
        s.right = (0.0, 0.0);
        m.update(&s, dt, MOUSE_SENS.2);
        s.right = (1.0, 0.0);
        let again = m.update(&s, dt, MOUSE_SENS.2).look.0;
        assert!(again < last, "releasing the stick resets the boost");
    }

    #[test]
    fn buttons_map_to_the_pc_actions_with_press_and_release_edges() {
        let mut m = PadMapper::default();
        let mut s = snap();
        let table = [
            (Pad::South, Action::Jump),
            (Pad::East, Action::Slide),
            (Pad::West, Action::Grapple),
            (Pad::North, Action::Roll),
            (Pad::LeftBumper, Action::Dash),
            (Pad::RightBumper, Action::Block),
            (Pad::DPadRight, Action::BuildFloor),
            (Pad::DPadUp, Action::Katana),
            (Pad::DPadLeft, Action::Bow),
        ];
        for (b, a) in table {
            s.set(b, true);
            let o = m.update(&s, 0.016, MOUSE_SENS.2);
            assert_eq!(o.events, vec![ButtonEvent { action: a, down: true }], "{b:?} press");
            assert!(m.update(&s, 0.016, MOUSE_SENS.2).events.is_empty(), "{b:?} held = no repeat");
            s.set(b, false);
            assert_eq!(m.update(&s, 0.016, MOUSE_SENS.2).events, vec![ButtonEvent { action: a, down: false }], "{b:?} release");
        }
    }

    #[test]
    fn triggers_throw_and_slash_with_hysteresis_and_start_means_click_to_play() {
        let mut m = PadMapper::default();
        let mut s = snap();
        s.rt = 0.4;
        assert!(m.update(&s, 0.016, MOUSE_SENS.2).events.is_empty(), "below the press threshold");
        s.rt = 0.6;
        assert_eq!(m.update(&s, 0.016, MOUSE_SENS.2).events, vec![ButtonEvent { action: Action::Fire, down: true }]);
        s.rt = 0.4; // still held: hysteresis, no flutter around 0.5
        assert!(m.update(&s, 0.016, MOUSE_SENS.2).events.is_empty());
        s.rt = 0.2;
        assert_eq!(m.update(&s, 0.016, MOUSE_SENS.2).events, vec![ButtonEvent { action: Action::Fire, down: false }]);
        s.lt = 1.0;
        assert_eq!(m.update(&s, 0.016, MOUSE_SENS.2).events, vec![ButtonEvent { action: Action::Slice, down: true }]);
        s.set(Pad::Start, true);
        assert!(m.update(&s, 0.016, MOUSE_SENS.2).start_pressed);
        assert!(!m.update(&s, 0.016, MOUSE_SENS.2).start_pressed, "edge only");
    }

    #[test]
    fn pushing_the_stick_forward_clings_and_idle_pad_is_inactive() {
        let mut m = PadMapper::default();
        let mut s = snap();
        assert!(!m.update(&s, 0.016, MOUSE_SENS.2).active);
        s.left = (0.0, 1.0);
        let o = m.update(&s, 0.016, MOUSE_SENS.2);
        assert!(o.active && o.events.contains(&ButtonEvent { action: Action::Cling, down: true }));
        s.left = (0.0, 0.0);
        assert!(m.update(&s, 0.016, MOUSE_SENS.2).events.contains(&ButtonEvent { action: Action::Cling, down: false }));
    }

    #[test]
    fn hostile_values_stay_finite() {
        let mut m = PadMapper::default();
        let mut s = snap();
        s.left = (f32::INFINITY, f32::NAN);
        s.right = (f32::NAN, 5.0);
        s.rt = f32::NAN;
        let o = m.update(&s, 0.016, MOUSE_SENS.2);
        assert!(o.stick.0.is_finite() && o.stick.1.is_finite() && o.look.0.is_finite() && o.look.1.is_finite());
    }
}
