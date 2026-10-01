//! Shell-side touch controller: floating stick + relative aim pad (`TouchController`) and on-screen buttons
//! (`TouchUi`) behind one API. Renderer-agnostic. The shell owns this (not `ClientGame`) so it can draw the
//! overlay from the same state it feeds the game:
//!
//!   touch event  -> `event()`   routes to a button or to the stick/aim controller, emits `ButtonEvent`s
//!   each tick    -> `fill()`    puts the stick and accumulated look into the `InputFrame`
//!   each frame   -> `draw()`    `hud::Prim` list for the overlay (stick base/knob, aim hint, buttons)

use crate::client_game::{Action, ButtonEvent, InputFrame};
use crate::hud::{Palette, Prim};
use crate::input::{TouchController, TouchEvent};
use crate::touch_ui::{StickView, TouchButton, TouchUi, TouchView};

pub struct TouchSession {
    pub ui: TouchUi,
    pub ctl: TouchController,
    /// Which on-screen buttons we have already reported as held (to emit only changes).
    held: [bool; 5],
}

fn action(b: TouchButton) -> Action {
    match b {
        TouchButton::Jump => Action::Jump,
        TouchButton::Shuriken => Action::Fire,
        TouchButton::Slice => Action::Slice,
        TouchButton::WallCling => Action::Cling,
        TouchButton::Dash => Action::Dash,
    }
}

impl TouchSession {
    pub fn new(w: f32, h: f32) -> Self {
        let ui = TouchUi::new(w, h);
        let mut ctl = TouchController::new(w, ui.layout.stick_radius);
        ctl.screen_w = w;
        Self { ui, ctl, held: [false; 5] }
    }

    /// Route one finger event. Button state changes are appended to `out` as press/release `ButtonEvent`s.
    pub fn event(&mut self, e: TouchEvent, out: &mut Vec<ButtonEvent>) {
        let mut scratch = crate::input::ButtonLatch::default(); // TouchUi wants a latch; we read its state instead
        if !self.ui.event(e, &mut scratch) {
            self.ctl.event(e);
        }
        self.sync_buttons(out);
    }

    /// Focus loss / `Canceled` touches: never leave a button or the stick stuck.
    pub fn cancel_all(&mut self, out: &mut Vec<ButtonEvent>) {
        let mut scratch = crate::input::ButtonLatch::default();
        self.ui.release_all(&mut scratch);
        self.ctl = TouchController::new(self.ctl.screen_w, self.ctl.stick_radius);
        self.sync_buttons(out);
    }

    pub fn resize(&mut self, w: f32, h: f32, out: &mut Vec<ButtonEvent>) {
        let mut scratch = crate::input::ButtonLatch::default();
        self.ui.resize(w, h, &mut scratch);
        self.ctl = TouchController::new(w, self.ui.layout.stick_radius);
        self.sync_buttons(out);
    }

    fn sync_buttons(&mut self, out: &mut Vec<ButtonEvent>) {
        for (i, b) in TouchButton::ALL.iter().enumerate() {
            let now = self.ui.is_down(*b);
            if now != self.held[i] {
                self.held[i] = now;
                out.push(ButtonEvent { action: action(*b), down: now });
            }
        }
    }

    /// Stick (level) and look (accumulated since last call) for the next sim tick.
    pub fn fill(&mut self, f: &mut InputFrame) {
        let s = self.ctl.stick();
        if s != (0.0, 0.0) {
            f.stick = s;
        }
        let l = self.ctl.take_look();
        f.look.0 += l.0;
        f.look.1 += l.1;
    }

    pub fn view(&self, dash_ready: f32, shuriken_ready: f32) -> TouchView {
        TouchView {
            stick: self.ctl.stick_view().map(|(anchor, current)| StickView { anchor, current }),
            aiming: self.ctl.aiming(),
            dash_ready,
            shuriken_ready,
        }
    }

    pub fn draw(&self, dash_ready: f32, shuriken_ready: f32, p: &Palette) -> Vec<Prim> {
        self.ui.draw(&self.view(dash_ready, shuriken_ready), p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::TouchEvent::{Down, Move, Up};

    const W: f32 = 960.0;
    const H: f32 = 540.0;

    fn center(s: &TouchSession, b: TouchButton) -> (f32, f32) {
        let c = s.ui.layout.slot(b).center;
        (c[0], c[1])
    }

    #[test]
    fn button_press_and_release_become_game_actions() {
        let mut s = TouchSession::new(W, H);
        let mut out = Vec::new();
        let (x, y) = center(&s, TouchButton::Jump);
        s.event(Down { id: 1, x, y }, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].action, out[0].down), (Action::Jump, true));
        s.event(Up { id: 1 }, &mut out);
        assert_eq!((out[1].action, out[1].down), (Action::Jump, false));

        out.clear();
        let (x, y) = center(&s, TouchButton::Shuriken);
        s.event(Down { id: 2, x, y }, &mut out);
        assert_eq!(out[0].action, Action::Fire);
    }

    #[test]
    fn button_finger_does_not_steer_the_aim_pad() {
        let mut s = TouchSession::new(W, H);
        let mut out = Vec::new();
        let (x, y) = center(&s, TouchButton::Slice); // right half
        s.event(Down { id: 1, x, y }, &mut out);
        s.event(Move { id: 1, x: x - 40.0, y: y - 40.0 }, &mut out);
        let mut f = InputFrame::default();
        s.fill(&mut f);
        assert_eq!(f.look, (0.0, 0.0), "button-owned finger must not turn the camera");
    }

    #[test]
    fn left_thumb_drives_stick_right_thumb_drives_look() {
        let mut s = TouchSession::new(W, H);
        let mut out = Vec::new();
        s.event(Down { id: 1, x: 150.0, y: 400.0 }, &mut out);
        s.event(Move { id: 1, x: 150.0, y: 400.0 - s.ctl.stick_radius }, &mut out); // full forward
        s.event(Down { id: 2, x: 600.0, y: 100.0 }, &mut out); // right half, away from buttons
        s.event(Move { id: 2, x: 630.0, y: 90.0 }, &mut out);
        assert!(out.is_empty(), "no buttons involved");
        let mut f = InputFrame::default();
        s.fill(&mut f);
        assert!(f.stick.1 > 0.9 && f.stick.0.abs() < 0.05, "forward: {:?}", f.stick);
        assert_eq!(f.look, (30.0, -10.0));
        let mut f2 = InputFrame::default();
        s.fill(&mut f2);
        assert_eq!(f2.look, (0.0, 0.0), "look is drained once");
        let v = s.view(1.0, 1.0);
        assert!(v.stick.is_some() && v.aiming);
    }

    #[test]
    fn cancel_and_resize_release_everything() {
        let mut s = TouchSession::new(W, H);
        let mut out = Vec::new();
        let (x, y) = center(&s, TouchButton::WallCling);
        s.event(Down { id: 1, x, y }, &mut out);
        s.event(Down { id: 2, x: 100.0, y: 400.0 }, &mut out);
        out.clear();
        s.cancel_all(&mut out);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].action, out[0].down), (Action::Cling, false));
        assert!(s.view(1.0, 1.0).stick.is_none());

        let (x, y) = center(&s, TouchButton::Dash);
        s.event(Down { id: 3, x, y }, &mut out);
        out.clear();
        s.resize(1280.0, 720.0, &mut out);
        assert_eq!((out[0].action, out[0].down), (Action::Dash, false));
        assert_eq!(s.ctl.screen_w, 1280.0);
    }

    #[test]
    fn overlay_draws_buttons_and_the_knob_follows_the_thumb() {
        let mut s = TouchSession::new(W, H);
        let knob = |prims: &[Prim]| {
            prims.iter().find_map(|p| match p {
                Prim::Ring { c, fill: true, .. } => Some(*c), // first filled ring = stick knob
                _ => None,
            })
        };
        let idle = s.draw(1.0, 1.0, &Palette::TRON);
        assert!(idle.len() > 15, "5 buttons + stick hint + aim hint: {}", idle.len());
        let mut out = Vec::new();
        s.event(Down { id: 1, x: 150.0, y: 400.0 }, &mut out);
        s.event(Move { id: 1, x: 190.0, y: 380.0 }, &mut out);
        let held = s.draw(1.0, 1.0, &Palette::TRON);
        let (k0, k1) = (knob(&idle).unwrap(), knob(&held).unwrap());
        assert!(k1.0 > k0.0 - 1.0 && k0 != k1, "knob moved to the thumb: {k0:?} -> {k1:?}");
        assert!((k1.0 - 190.0 / W).abs() < 1e-4 && (k1.1 - 380.0 / H).abs() < 1e-4, "{k1:?}");
    }
}
