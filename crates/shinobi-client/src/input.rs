//! Input normalization: PC (keyboard + Pointer Lock mouse) and mobile (floating left stick + right relative
//! aim pad) both end up as the same `PlayerIntent` -> `PlayerIntentPacket`. Platform glue (DOM/winit/touch
//! APIs) feeds the plain structs below; nothing here touches a platform.
//!
//! Sample once per 60 Hz sim tick, not per render frame: edges are latched so a 144 Hz tap that falls
//! between two sim ticks is not lost.

use std::f32::consts::{FRAC_PI_2, TAU};

use shinobi_proto::intent::{Buttons, PlayerIntent};
use shinobi_proto::quant::{q_axis, q_pitch, q_yaw};

/// Client-owned aim. Yaw 0 faces -Z, positive turns toward -X (matches the server).
#[derive(Clone, Copy, Default, Debug)]
pub struct Aim {
    pub yaw: f32,
    pub pitch: f32,
}

impl Aim {
    /// `dx`/`dy` in device units (Pointer Lock `movementX/Y` or touch px), `rad_per_unit` = sensitivity.
    /// Right / down positive, like the DOM.
    pub fn look(&mut self, dx: f32, dy: f32, rad_per_unit: f32) {
        self.yaw = (self.yaw - dx * rad_per_unit).rem_euclid(TAU);
        let lim = 89f32.to_radians().min(FRAC_PI_2);
        self.pitch = (self.pitch - dy * rad_per_unit).clamp(-lim, lim);
    }
}

/// Button latch: `set` from events at any rate, `sample` once per sim tick.
#[derive(Clone, Copy, Default, Debug)]
pub struct ButtonLatch {
    held: u8,
    edges: u8,
}

impl ButtonLatch {
    /// `flag` is one of `Buttons::*`. Held-only flags (`JUMP_HELD`, `WALL_CLING`) pass through unchanged.
    pub fn set(&mut self, flag: u8, down: bool) {
        if down {
            if self.held & flag == 0 {
                self.edges |= flag;
            }
            self.held |= flag;
        } else {
            self.held &= !flag;
        }
    }

    /// Jump sets both the edge (`JUMP_PRESSED`) and the held flag from one physical button.
    pub fn set_jump(&mut self, down: bool) {
        self.set(Buttons::JUMP_HELD, down);
        if down && self.edges & Buttons::JUMP_HELD != 0 {
            self.edges = (self.edges & !Buttons::JUMP_HELD) | Buttons::JUMP_PRESSED;
        }
    }

    pub fn sample(&mut self) -> Buttons {
        let out = self.held | (self.edges & Buttons::EDGES);
        self.edges = 0;
        Buttons(out & Buttons::MASK)
    }
}

/// WASD -> view-frame stick (x strafe right, z forward), normalized so diagonals are not 1.41x.
pub fn keys_to_stick(w: bool, a: bool, s: bool, d: bool) -> (f32, f32) {
    let x = d as i8 - a as i8;
    let z = w as i8 - s as i8;
    let (x, z) = (x as f32, z as f32);
    let len = (x * x + z * z).sqrt();
    if len > 0.0 { (x / len, z / len) } else { (0.0, 0.0) }
}

#[derive(Clone, Copy, Debug)]
pub enum TouchEvent {
    Down { id: u32, x: f32, y: f32 },
    Move { id: u32, x: f32, y: f32 },
    Up { id: u32 },
}

/// Dual-quadrant controller: a touch that starts on the left half owns a floating stick anchored where it
/// landed; a touch that starts on the right half is a relative aim trackpad (whole right half, no fixed origin).
#[derive(Clone, Copy, Debug)]
pub struct TouchController {
    pub screen_w: f32,
    /// Stick travel (px) for full deflection.
    pub stick_radius: f32,
    /// Stick deadzone, fraction of radius.
    pub deadzone: f32,
    /// Mirror the halves: the floating stick owns the RIGHT half, the aim pad the left. Fingers already down keep
    /// their role when this flips (a gesture never changes meaning mid-drag).
    pub left_handed: bool,
    left: Option<(u32, [f32; 2], [f32; 2])>, // id, anchor, current
    right: Option<(u32, [f32; 2])>,          // id, last
    pending_look: [f32; 2],
}

impl TouchController {
    pub fn new(screen_w: f32, stick_radius: f32) -> Self {
        Self { screen_w, stick_radius, deadzone: 0.12, left_handed: false, left: None, right: None, pending_look: [0.0; 2] }
    }

    pub fn event(&mut self, e: TouchEvent) {
        match e {
            TouchEvent::Down { id, x, y } => {
                if (x < self.screen_w * 0.5) != self.left_handed {
                    if self.left.is_none() {
                        self.left = Some((id, [x, y], [x, y]));
                    }
                } else if self.right.is_none() {
                    self.right = Some((id, [x, y]));
                }
            }
            TouchEvent::Move { id, x, y } => {
                if let Some((lid, _, cur)) = &mut self.left {
                    if *lid == id {
                        *cur = [x, y];
                    }
                }
                if let Some((rid, last)) = &mut self.right {
                    if *rid == id {
                        self.pending_look[0] += x - last[0];
                        self.pending_look[1] += y - last[1];
                        *last = [x, y];
                    }
                }
            }
            TouchEvent::Up { id } => {
                if self.left.map_or(false, |l| l.0 == id) {
                    self.left = None;
                }
                if self.right.map_or(false, |r| r.0 == id) {
                    self.right = None;
                }
            }
        }
    }

    /// View-frame stick: x right, z forward (screen-up = forward). Zero inside the deadzone.
    pub fn stick(&self) -> (f32, f32) {
        let Some((_, a, c)) = self.left else { return (0.0, 0.0) };
        let (dx, dy) = ((c[0] - a[0]) / self.stick_radius, (a[1] - c[1]) / self.stick_radius);
        let len = (dx * dx + dy * dy).sqrt();
        if len < self.deadzone {
            return (0.0, 0.0);
        }
        let scaled = ((len - self.deadzone) / (1.0 - self.deadzone)).min(1.0);
        (dx / len * scaled, dy / len * scaled)
    }

    /// Floating stick for drawing: (anchor, current) in px while the left thumb is down.
    pub fn stick_view(&self) -> Option<([f32; 2], [f32; 2])> {
        self.left.map(|(_, a, c)| (a, c))
    }

    /// Right-half aim pad is currently held.
    pub fn aiming(&self) -> bool {
        self.right.is_some()
    }

    /// Accumulated aim-pad travel since last call (px); feed to `Aim::look`.
    pub fn take_look(&mut self) -> (f32, f32) {
        let l = self.pending_look;
        self.pending_look = [0.0; 2];
        (l[0], l[1])
    }
}

/// Final normalization step shared by both platforms.
pub fn build_intent(tick: u32, stick: (f32, f32), aim: &Aim, buttons: Buttons) -> PlayerIntent {
    PlayerIntent {
        tick,
        buttons,
        move_x: q_axis(stick.0),
        move_z: q_axis(stick.1),
        aim_yaw: q_yaw(aim.yaw),
        aim_pitch: q_pitch(aim.pitch),
        ultimate: false,
        build: false, build_wall: false, build_ramp: false,
        zip: false,
        block: false,
        mark: false,
        star: 0,
        draw: false,
        cloak: false,
        grind: false,
        zoom: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagonal_is_normalized_and_opposites_cancel() {
        let (x, z) = keys_to_stick(true, false, false, true);
        assert!(((x * x + z * z).sqrt() - 1.0).abs() < 1e-6);
        assert_eq!(keys_to_stick(true, false, true, false), (0.0, 0.0));
    }

    #[test]
    fn tap_between_sim_ticks_is_not_lost() {
        let mut b = ButtonLatch::default();
        b.set(Buttons::SLICE, true);
        b.set(Buttons::SLICE, false); // released before the next sim tick
        assert!(b.sample().has(Buttons::SLICE));
        assert!(!b.sample().has(Buttons::SLICE), "edge fires once");
    }

    #[test]
    fn jump_gives_edge_then_hold_and_cling_is_held() {
        let mut b = ButtonLatch::default();
        b.set_jump(true);
        b.set(Buttons::WALL_CLING, true);
        let s1 = b.sample();
        assert!(s1.has(Buttons::JUMP_PRESSED) && s1.has(Buttons::JUMP_HELD) && s1.has(Buttons::WALL_CLING));
        let s2 = b.sample();
        assert!(!s2.has(Buttons::JUMP_PRESSED) && s2.has(Buttons::JUMP_HELD) && s2.has(Buttons::WALL_CLING));
    }

    #[test]
    fn touch_quadrants_route_independently() {
        let mut t = TouchController::new(1000.0, 100.0);
        t.event(TouchEvent::Down { id: 1, x: 200.0, y: 500.0 });
        t.event(TouchEvent::Down { id: 2, x: 800.0, y: 500.0 });
        t.event(TouchEvent::Move { id: 1, x: 200.0, y: 400.0 }); // drag up 100px = full forward
        t.event(TouchEvent::Move { id: 2, x: 830.0, y: 490.0 });
        let (sx, sz) = t.stick();
        assert!(sx.abs() < 1e-6 && (sz - 1.0).abs() < 1e-6);
        assert_eq!(t.take_look(), (30.0, -10.0));
        assert_eq!(t.take_look(), (0.0, 0.0));
        t.event(TouchEvent::Up { id: 1 });
        assert_eq!(t.stick(), (0.0, 0.0));
    }

    #[test]
    fn mouse_right_turns_right_and_pitch_clamps() {
        let mut a = Aim::default();
        a.look(100.0, -1e6, 0.002);
        assert!(a.yaw > std::f32::consts::PI, "turning right wraps below 2pi (positive yaw = left)");
        assert!((a.pitch - 89f32.to_radians()).abs() < 1e-5);
    }

    #[test]
    fn pc_and_touch_produce_identical_intents() {
        let aim = Aim { yaw: 1.0, pitch: 0.3 };
        let pc = build_intent(7, keys_to_stick(true, false, false, false), &aim, Buttons(Buttons::FIRE));
        let mut t = TouchController::new(1000.0, 100.0);
        t.event(TouchEvent::Down { id: 1, x: 100.0, y: 300.0 });
        t.event(TouchEvent::Move { id: 1, x: 100.0, y: 100.0 });
        let touch = build_intent(7, t.stick(), &aim, Buttons(Buttons::FIRE));
        assert_eq!(pc, touch);
    }

    #[test]
    fn left_handed_swaps_which_half_owns_the_stick_and_the_pad() {
        for left_handed in [false, true] {
            let mut t = TouchController::new(1000.0, 100.0);
            t.left_handed = left_handed;
            let (stick_x, pad_x) = if left_handed { (800.0, 200.0) } else { (200.0, 800.0) };
            t.event(TouchEvent::Down { id: 1, x: stick_x, y: 500.0 });
            t.event(TouchEvent::Down { id: 2, x: pad_x, y: 500.0 });
            t.event(TouchEvent::Move { id: 1, x: stick_x, y: 400.0 }); // drag up 100 px = full forward
            t.event(TouchEvent::Move { id: 2, x: pad_x + 30.0, y: 490.0 });
            let (sx, sz) = t.stick();
            assert!(sx.abs() < 1e-6 && (sz - 1.0).abs() < 1e-6, "left_handed={left_handed}: stick ({sx}, {sz})");
            assert_eq!(t.take_look(), (30.0, -10.0), "left_handed={left_handed}");
        }
    }

    #[test]
    fn flipping_left_handed_mid_gesture_does_not_change_the_roles_of_fingers_already_down() {
        let mut t = TouchController::new(1000.0, 100.0);
        t.event(TouchEvent::Down { id: 1, x: 200.0, y: 500.0 }); // stick (right-handed layout)
        t.left_handed = true;
        t.event(TouchEvent::Move { id: 1, x: 200.0, y: 400.0 });
        assert!((t.stick().1 - 1.0).abs() < 1e-6, "the finger that started as the stick stays the stick");
        t.event(TouchEvent::Down { id: 2, x: 800.0, y: 500.0 }); // right half is now the stick side, but the slot is taken
        assert_eq!(t.take_look(), (0.0, 0.0), "no second stick is created");
        t.event(TouchEvent::Up { id: 1 });
        t.event(TouchEvent::Down { id: 3, x: 800.0, y: 500.0 });
        t.event(TouchEvent::Move { id: 3, x: 800.0, y: 400.0 });
        assert!((t.stick().1 - 1.0).abs() < 1e-6, "after the old stick lifts, the mirrored side owns it");
    }
}
