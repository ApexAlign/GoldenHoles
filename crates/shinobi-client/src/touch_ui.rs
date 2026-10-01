//! Touch overlay: on-screen buttons (hit-testing, multi-touch) plus the draw list for the floating stick,
//! aim-pad hint and buttons. Renderer-agnostic like `hud`: it emits `hud::Prim` in normalized screen space.
//!
//! Routing: feed every `TouchEvent` to `TouchUi::event` first. `true` = a button owns that finger, do NOT
//! forward it to `TouchController` (otherwise a button press on the right half would also steer the aim pad).
//! `false` = forward it to the controller. Buttons write straight into the `ButtonLatch` (`input.rs`), so the
//! per-tick sampling and "tap between ticks is not lost" behaviour are shared with the keyboard path.

use shinobi_proto::intent::Buttons;

use crate::hud::{Align, Palette, Prim, Rgba};
use crate::input::{ButtonLatch, TouchEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchButton {
    Jump,
    /// Neon shuriken throw (`Buttons::THROW_SHURIKEN`).
    Shuriken,
    /// Katana dash-slice (`Buttons::KATANA_DASH_SLICE`).
    Slice,
    /// Held while touched: press into the wall (`Buttons::WALL_CLING`).
    WallCling,
    /// Evasive dash (`Buttons::DASH`).
    Dash,
}

impl TouchButton {
    pub const ALL: [TouchButton; 5] = [Self::Jump, Self::Shuriken, Self::Slice, Self::WallCling, Self::Dash];

    fn label(self) -> &'static str {
        match self {
            Self::Jump => "JUMP",
            Self::Shuriken => "STAR",
            Self::Slice => "SLICE",
            Self::WallCling => "WALL",
            Self::Dash => "DASH",
        }
    }

    fn press(self, latch: &mut ButtonLatch, down: bool) {
        match self {
            Self::Jump => latch.set_jump(down),
            Self::Shuriken => latch.set(Buttons::THROW_SHURIKEN, down),
            Self::Slice => latch.set(Buttons::KATANA_DASH_SLICE, down),
            Self::WallCling => latch.set(Buttons::WALL_CLING, down),
            Self::Dash => latch.set(Buttons::DASH, down),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ButtonSlot {
    pub button: TouchButton,
    pub center: [f32; 2], // px
    pub radius: f32,      // px
}

/// Thumb-arc cluster in the bottom-right, sized from screen height so it scales across phones.
/// Offsets are (from-right, from-bottom, radius) in units of screen height.
#[derive(Clone, Debug)]
pub struct Layout {
    pub w: f32,
    pub h: f32,
    pub slots: [ButtonSlot; 5],
    /// Default resting spot of the stick hint (the stick itself floats to wherever the thumb lands).
    pub stick_hint: [f32; 2],
    pub stick_radius: f32,
    /// Mirrored layout: buttons + aim pad on the left, stick on the right. `TouchController` must swap
    /// its halves to match (it hard-codes left = stick today).
    pub left_handed: bool,
}

/// Extra px-fraction around a button that still counts as a hit (fat thumbs).
const HIT_SLOP: f32 = 1.15;

impl Layout {
    pub fn new(w: f32, h: f32) -> Self {
        Self::build(w, h, false, 0.13)
    }

    /// `stick_radius_frac`: stick travel as a fraction of screen height.
    pub fn build(w: f32, h: f32, left_handed: bool, stick_radius_frac: f32) -> Self {
        let mx = |x: f32| if left_handed { w - x } else { x };
        let slot = |button, right: f32, bottom: f32, r: f32| ButtonSlot {
            button,
            center: [mx(w - right * h), h - bottom * h],
            radius: r * h,
        };
        Self {
            left_handed,
            w,
            h,
            slots: [
                slot(TouchButton::Jump, 0.17, 0.17, 0.095),
                slot(TouchButton::Shuriken, 0.40, 0.12, 0.080),
                slot(TouchButton::Slice, 0.14, 0.41, 0.080),
                slot(TouchButton::Dash, 0.38, 0.35, 0.070),
                slot(TouchButton::WallCling, 0.15, 0.63, 0.065),
            ],
            stick_hint: [mx(0.2 * h), h - 0.28 * h],
            stick_radius: stick_radius_frac * h,
        }
    }

    /// Nearest button whose (slop-expanded) disc contains the point.
    pub fn hit(&self, x: f32, y: f32) -> Option<TouchButton> {
        self.slots
            .iter()
            .filter_map(|s| {
                let d = ((x - s.center[0]).powi(2) + (y - s.center[1]).powi(2)).sqrt();
                (d <= s.radius * HIT_SLOP).then_some((d / s.radius, s.button))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, b)| b)
    }

    pub fn slot(&self, b: TouchButton) -> &ButtonSlot {
        self.slots.iter().find(|s| s.button == b).expect("every button has a slot")
    }
}

/// Floating stick as the controller sees it (px). Needs read access to `TouchController`'s left touch;
/// until it exposes an accessor the platform glue can mirror the `Down`/`Move` positions it already has.
#[derive(Clone, Copy, Debug)]
pub struct StickView {
    pub anchor: [f32; 2],
    pub current: [f32; 2],
}

/// Per-frame overlay inputs.
#[derive(Clone, Copy, Debug, Default)]
pub struct TouchView {
    pub stick: Option<StickView>,
    /// Right-half aim pad currently held (hide the hint).
    pub aiming: bool,
    /// 0..=1 cooldown fill (1 = ready). Slice shares the dash cooldown.
    pub dash_ready: f32,
    pub shuriken_ready: f32,
}

pub struct TouchUi {
    pub layout: Layout,
    active: Vec<(u32, TouchButton)>, // finger id -> button it owns
}

impl TouchUi {
    pub fn new(w: f32, h: f32) -> Self {
        Self { layout: Layout::new(w, h), active: Vec::with_capacity(5) }
    }

    pub fn resize(&mut self, w: f32, h: f32, latch: &mut ButtonLatch) {
        self.release_all(latch);
        self.layout = Layout::build(w, h, self.layout.left_handed, self.layout.stick_radius / self.layout.h);
    }

    /// Re-layout from settings (handedness, stick size). Releases held buttons since they move.
    pub fn apply_settings(&mut self, s: &crate::settings::Settings, latch: &mut ButtonLatch) {
        self.release_all(latch);
        self.layout = Layout::build(self.layout.w, self.layout.h, s.left_handed, s.stick_radius_frac);
    }

    pub fn is_down(&self, b: TouchButton) -> bool {
        self.active.iter().any(|(_, a)| *a == b)
    }

    /// Returns true when the event belongs to a button (don't forward to `TouchController`).
    pub fn event(&mut self, e: TouchEvent, latch: &mut ButtonLatch) -> bool {
        match e {
            TouchEvent::Down { id, x, y } => {
                let Some(b) = self.layout.hit(x, y) else { return false };
                // A second finger on an already-held button is swallowed, not turned into aim/stick input.
                if !self.is_down(b) {
                    self.active.push((id, b));
                    b.press(latch, true);
                }
                true
            }
            // Sliding off a button keeps it held (thumbs drift); only lifting releases.
            TouchEvent::Move { id, .. } => self.active.iter().any(|(i, _)| *i == id),
            TouchEvent::Up { id } => {
                let Some(pos) = self.active.iter().position(|(i, _)| *i == id) else { return false };
                let (_, b) = self.active.swap_remove(pos);
                b.press(latch, false);
                true
            }
        }
    }

    /// Focus loss / resize / touchcancel: never leave a button stuck down.
    pub fn release_all(&mut self, latch: &mut ButtonLatch) {
        for (_, b) in self.active.drain(..) {
            b.press(latch, false);
        }
    }

    pub fn draw(&self, v: &TouchView, p: &Palette) -> Vec<Prim> {
        let l = &self.layout;
        let n = |x: f32, y: f32| (x / l.w, y / l.h);
        let rn = |r: f32| r / l.h;
        let mut out = Vec::with_capacity(32);

        // Stick: base ring + knob. Idle = faint hint at the resting spot; active = at the anchor, knob clamped.
        let (anchor, knob, alpha) = match v.stick {
            Some(s) => {
                let (dx, dy) = (s.current[0] - s.anchor[0], s.current[1] - s.anchor[1]);
                let len = (dx * dx + dy * dy).sqrt();
                let k = if len > l.stick_radius { l.stick_radius / len } else { 1.0 };
                (s.anchor, [s.anchor[0] + dx * k, s.anchor[1] + dy * k], 0.8)
            }
            None => (l.stick_hint, l.stick_hint, 0.22),
        };
        out.push(Prim::Ring { c: n(anchor[0], anchor[1]), r: rn(l.stick_radius), color: p.cyan.a(alpha * 0.6), fill: false, glow: 0.006 });
        out.push(Prim::Ring { c: n(anchor[0], anchor[1]), r: rn(l.stick_radius * 0.25), color: p.cyan.a(alpha * 0.25), fill: false, glow: 0.0 });
        out.push(Prim::Ring { c: n(knob[0], knob[1]), r: rn(l.stick_radius * 0.38), color: p.cyan.a(alpha), fill: true, glow: 0.01 });

        // Aim-pad hint: corner brackets around the open right-half area, hidden while a finger is on it.
        if !v.aiming {
            let (x0, y0, x1, y1, t) = if l.left_handed { (0.03, 0.22, 0.44, 0.56, 0.03) } else { (0.56, 0.22, 0.97, 0.56, 0.03) };
            let c = p.magenta.a(0.25);
            for (cx, cy, sx, sy) in [(x0, y0, 1.0, 1.0), (x1, y0, -1.0, 1.0), (x0, y1, 1.0, -1.0), (x1, y1, -1.0, -1.0)] {
                out.push(Prim::Line { a: (cx, cy), b: (cx + sx * t, cy), color: c, glow: 0.003 });
                out.push(Prim::Line { a: (cx, cy), b: (cx, cy + sy * t * 1.7), color: c, glow: 0.003 });
            }
            out.push(Prim::Text { pos: ((x0 + x1) * 0.5, (y0 + y1) * 0.5), text: "DRAG TO AIM".into(), size: 0.03, color: p.magenta.a(0.3), align: Align::Center });
        }

        // Buttons: ring + label; cooldown buttons go orange and show a fill arc-proxy (inner disc scales).
        for s in &l.slots {
            let down = self.is_down(s.button);
            let ready = match s.button {
                TouchButton::Shuriken => v.shuriken_ready,
                TouchButton::Slice | TouchButton::Dash => v.dash_ready,
                _ => 1.0,
            }
            .clamp(0.0, 1.0);
            let base: Rgba = match s.button {
                TouchButton::Slice => p.orange,
                TouchButton::Dash => p.magenta,
                _ => p.cyan,
            };
            let col = if ready < 1.0 { p.orange } else { base };
            let c = n(s.center[0], s.center[1]);
            out.push(Prim::Ring { c, r: rn(s.radius), color: col.a(if down { 0.95 } else { 0.4 }), fill: false, glow: if down { 0.012 } else { 0.005 } });
            if down {
                out.push(Prim::Ring { c, r: rn(s.radius * 0.92), color: col.a(0.3), fill: true, glow: 0.0 });
            } else if ready < 1.0 {
                out.push(Prim::Ring { c, r: rn(s.radius * 0.8 * ready), color: col.a(0.25), fill: true, glow: 0.0 });
            }
            out.push(Prim::Text { pos: (c.0, c.1 + 0.012), text: s.button.label().into(), size: 0.026, color: p.text.a(if down { 1.0 } else { 0.7 }), align: Align::Center });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::TouchController;

    fn ui() -> TouchUi {
        TouchUi::new(1600.0, 800.0)
    }

    fn center(u: &TouchUi, b: TouchButton) -> (f32, f32) {
        let c = u.layout.slot(b).center;
        (c[0], c[1])
    }

    #[test]
    fn every_button_hits_at_its_center_and_sits_in_the_right_half_on_screen() {
        let u = ui();
        for s in &u.layout.slots {
            assert_eq!(u.layout.hit(s.center[0], s.center[1]), Some(s.button));
            assert!(s.center[0] - s.radius > u.layout.w * 0.5, "{:?} leaks into the stick half", s.button);
            assert!(s.center[0] + s.radius <= u.layout.w && s.center[1] + s.radius <= u.layout.h);
            assert!(s.center[1] - s.radius >= 0.0);
        }
    }

    #[test]
    fn buttons_do_not_overlap_so_nearest_wins_only_in_the_slop_ring() {
        let l = Layout::new(1600.0, 800.0);
        for (i, a) in l.slots.iter().enumerate() {
            for b in &l.slots[i + 1..] {
                let d = ((a.center[0] - b.center[0]).powi(2) + (a.center[1] - b.center[1]).powi(2)).sqrt();
                assert!(d >= a.radius + b.radius, "{:?} overlaps {:?}", a.button, b.button);
            }
        }
        // empty space: middle of the aim pad and the left half hit nothing
        assert_eq!(l.hit(1100.0, 300.0), None);
        assert_eq!(l.hit(200.0, 500.0), None);
    }

    #[test]
    fn taps_latch_the_right_buttons() {
        let (mut u, mut l) = (ui(), ButtonLatch::default());
        let cases = [
            (TouchButton::Shuriken, Buttons::FIRE),
            (TouchButton::Slice, Buttons::SLICE),
            (TouchButton::Dash, Buttons::DASH),
        ];
        for (i, (b, flag)) in cases.into_iter().enumerate() {
            let (x, y) = center(&u, b);
            assert!(u.event(TouchEvent::Down { id: i as u32, x, y }, &mut l));
            assert!(u.event(TouchEvent::Up { id: i as u32 }, &mut l)); // released before the sim tick
            assert!(l.sample().has(flag), "{b:?}");
            assert!(!l.sample().has(flag), "edge fires once");
        }
    }

    #[test]
    fn jump_edge_then_hold_and_wall_cling_is_held_until_lift() {
        let (mut u, mut l) = (ui(), ButtonLatch::default());
        let (jx, jy) = center(&u, TouchButton::Jump);
        let (cx, cy) = center(&u, TouchButton::WallCling);
        u.event(TouchEvent::Down { id: 1, x: jx, y: jy }, &mut l);
        u.event(TouchEvent::Down { id: 2, x: cx, y: cy }, &mut l);
        let s1 = l.sample();
        assert!(s1.has(Buttons::JUMP_PRESSED) && s1.has(Buttons::JUMP_HELD) && s1.has(Buttons::WALL_CLING));
        let s2 = l.sample();
        assert!(!s2.has(Buttons::JUMP_PRESSED) && s2.has(Buttons::JUMP_HELD) && s2.has(Buttons::WALL_CLING));
        u.event(TouchEvent::Up { id: 2 }, &mut l);
        let s3 = l.sample();
        assert!(s3.has(Buttons::JUMP_HELD) && !s3.has(Buttons::WALL_CLING));
    }

    #[test]
    fn multitouch_stick_aim_and_buttons_all_work_at_once() {
        let (mut u, mut l) = (ui(), ButtonLatch::default());
        let mut tc = TouchController::new(1600.0, 100.0);
        let (jx, jy) = center(&u, TouchButton::Jump);
        let (sx, sy) = center(&u, TouchButton::Shuriken);
        // id1 stick (left), id2 aim (right, open area), id3 jump, id4 shuriken: one gesture stream.
        let downs = [(1, 200.0, 500.0), (2, 1100.0, 300.0), (3, jx, jy), (4, sx, sy)];
        for (id, x, y) in downs {
            let e = TouchEvent::Down { id, x, y };
            if !u.event(e, &mut l) {
                tc.event(e);
            }
        }
        for e in [
            TouchEvent::Move { id: 1, x: 200.0, y: 400.0 },
            TouchEvent::Move { id: 2, x: 1130.0, y: 290.0 },
            TouchEvent::Move { id: 3, x: jx + 5.0, y: jy - 5.0 }, // jittering thumb must not steer the aim pad
        ] {
            if !u.event(e, &mut l) {
                tc.event(e);
            }
        }
        assert_eq!(tc.stick(), (0.0, 1.0));
        assert_eq!(tc.take_look(), (30.0, -10.0), "button finger's moves never reach the aim pad");
        let b = l.sample();
        assert!(b.has(Buttons::JUMP_HELD) && b.has(Buttons::FIRE));
    }

    #[test]
    fn second_finger_on_a_held_button_is_swallowed_and_does_not_release_it() {
        let (mut u, mut l) = (ui(), ButtonLatch::default());
        let (x, y) = center(&u, TouchButton::WallCling);
        assert!(u.event(TouchEvent::Down { id: 1, x, y }, &mut l));
        assert!(u.event(TouchEvent::Down { id: 2, x, y }, &mut l), "swallowed, not forwarded as aim");
        assert!(!u.event(TouchEvent::Up { id: 2 }, &mut l), "the ignored finger owns nothing");
        assert!(l.sample().has(Buttons::WALL_CLING));
        u.event(TouchEvent::Up { id: 1 }, &mut l);
        assert!(!l.sample().has(Buttons::WALL_CLING));
    }

    #[test]
    fn non_button_touches_are_left_for_the_controller_and_release_all_unsticks() {
        let (mut u, mut l) = (ui(), ButtonLatch::default());
        assert!(!u.event(TouchEvent::Down { id: 9, x: 100.0, y: 100.0 }, &mut l));
        assert!(!u.event(TouchEvent::Move { id: 9, x: 120.0, y: 100.0 }, &mut l));
        assert!(!u.event(TouchEvent::Up { id: 9 }, &mut l));
        let (x, y) = center(&u, TouchButton::WallCling);
        u.event(TouchEvent::Down { id: 1, x, y }, &mut l);
        u.release_all(&mut l);
        assert!(!l.sample().has(Buttons::WALL_CLING) && !u.is_down(TouchButton::WallCling));
    }

    #[test]
    fn overlay_knob_is_clamped_to_the_base_and_hint_hides_while_aiming() {
        let u = ui();
        let p = Palette::TRON;
        let far = TouchView { stick: Some(StickView { anchor: [200.0, 600.0], current: [900.0, 600.0] }), ..Default::default() };
        let prims = u.draw(&far, &p);
        let knob = prims.iter().find_map(|q| match q { Prim::Ring { c, fill: true, .. } => Some(*c), _ => None }).unwrap();
        let expect_x = (200.0 + u.layout.stick_radius) / u.layout.w;
        assert!((knob.0 - expect_x).abs() < 1e-5 && (knob.1 - 600.0 / u.layout.h).abs() < 1e-5);

        let has_hint = |v: &TouchView| u.draw(v, &p).iter().any(|q| matches!(q, Prim::Text { text, .. } if text == "DRAG TO AIM"));
        assert!(has_hint(&TouchView::default()));
        assert!(!has_hint(&TouchView { aiming: true, ..Default::default() }));
    }

    #[test]
    fn pressed_and_cooling_buttons_render_differently_and_all_prims_are_on_screen() {
        let (mut u, mut l) = (ui(), ButtonLatch::default());
        let p = Palette::TRON;
        let idle = u.draw(&TouchView { dash_ready: 1.0, shuriken_ready: 1.0, ..Default::default() }, &p);
        let (x, y) = center(&u, TouchButton::Dash);
        u.event(TouchEvent::Down { id: 1, x, y }, &mut l);
        let pressed = u.draw(&TouchView { dash_ready: 0.4, shuriken_ready: 1.0, ..Default::default() }, &p);
        assert!(pressed.len() > idle.len());
        for q in &pressed {
            if let Some(b) = crate::hud::bounds(q) {
                assert!(b.x >= -0.01 && b.y >= -0.01 && b.x + b.w <= 1.01 && b.y + b.h <= 1.01, "off-screen: {b:?}");
            }
        }
    }

    #[test]
    fn left_handed_layout_mirrors_buttons_into_the_left_half() {
        let mut u = ui();
        let mut l = ButtonLatch::default();
        let mut s = crate::settings::Settings::default();
        s.left_handed = true;
        s.stick_radius_frac = 0.2;
        u.apply_settings(&s, &mut l);
        assert_eq!(u.layout.stick_radius, 0.2 * 800.0);
        for sl in &u.layout.slots {
            assert!(sl.center[0] + sl.radius < u.layout.w * 0.5, "{:?} not mirrored", sl.button);
            assert_eq!(u.layout.hit(sl.center[0], sl.center[1]), Some(sl.button));
        }
        assert!(u.layout.stick_hint[0] > u.layout.w * 0.5);
        u.resize(1200.0, 600.0, &mut l);
        assert!(u.layout.left_handed && (u.layout.stick_radius - 120.0).abs() < 1e-3, "resize keeps settings");
    }
}
