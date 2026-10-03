//! Pointer-lock state machine for desktop shells (grab on click, Esc releases, focus loss releases) plus a
//! held-button tracker so a release can also let go of every button the player was holding.
//! Pure logic: the shell maps its window events to `LockEvent`s and applies `LockOut` to the OS cursor.
//!
//! Rules:
//! * not locked + left click      -> lock; that click only grabs (`swallow_click`), it must not slash
//! * locked + Esc                 -> release, cursor visible, all held buttons released
//! * focus lost (locked or not)   -> release + clear buttons; regaining focus does NOT re-grab (needs a click)

use crate::client_game::{Action, ButtonEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockEvent {
    LeftClick,
    Escape,
    FocusLost,
    FocusGained,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LockOut {
    /// The locked state changed: apply `PointerLock::cursor()` to the window.
    pub changed: bool,
    /// Release every held button (and zero the stick).
    pub release_buttons: bool,
    /// The click that caused the grab must not be forwarded as a game input.
    pub swallow_click: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PointerLock {
    locked: bool,
}

/// What the OS cursor should look like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorState {
    pub grab: bool,
    pub visible: bool,
}

impl PointerLock {
    pub fn locked(&self) -> bool {
        self.locked
    }

    pub fn cursor(&self) -> CursorState {
        CursorState { grab: self.locked, visible: !self.locked }
    }

    pub fn on(&mut self, e: LockEvent) -> LockOut {
        match (e, self.locked) {
            (LockEvent::LeftClick, false) => {
                self.locked = true;
                LockOut { changed: true, release_buttons: false, swallow_click: true }
            }
            (LockEvent::Escape, true) | (LockEvent::FocusLost, true) => {
                self.locked = false;
                LockOut { changed: true, release_buttons: true, swallow_click: false }
            }
            // focus loss while unlocked still must not leave a key "stuck" in the latch
            (LockEvent::FocusLost, false) => LockOut { release_buttons: true, ..Default::default() },
            _ => LockOut::default(),
        }
    }
}

/// Which actions are currently held, so `release_all` emits exactly the missing key-ups.
#[derive(Clone, Copy, Debug, Default)]
pub struct ButtonTracker {
    held: u16,
}

fn bit(a: Action) -> u16 {
    1 << match a {
        Action::Jump => 0,
        Action::Dash => 1,
        Action::Roll => 2,
        Action::Slice => 3,
        Action::Fire => 4,
        Action::Cling => 5,
        Action::Slide => 6,
        Action::Grapple => 7,
        Action::BuildFloor => 8,
        Action::BuildWall => 9,
    }
}

const ALL: [Action; 10] = [Action::Jump, Action::Dash, Action::Roll, Action::Slice, Action::Fire, Action::Cling, Action::Slide, Action::Grapple, Action::BuildFloor, Action::BuildWall];

impl ButtonTracker {
    pub fn apply(&mut self, e: &ButtonEvent) {
        if e.down {
            self.held |= bit(e.action);
        } else {
            self.held &= !bit(e.action);
        }
    }

    pub fn is_held(&self, a: Action) -> bool {
        self.held & bit(a) != 0
    }

    /// Key-up events for everything still held; the tracker is empty afterwards.
    pub fn release_all(&mut self, out: &mut Vec<ButtonEvent>) {
        for a in ALL {
            if self.is_held(a) {
                out.push(ButtonEvent { action: a, down: false });
            }
        }
        self.held = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_grabs_and_is_swallowed_second_click_is_a_game_input() {
        let mut p = PointerLock::default();
        assert_eq!(p.cursor(), CursorState { grab: false, visible: true });
        let o = p.on(LockEvent::LeftClick);
        assert!(o.changed && o.swallow_click && !o.release_buttons);
        assert!(p.locked());
        assert_eq!(p.cursor(), CursorState { grab: true, visible: false });
        assert_eq!(p.on(LockEvent::LeftClick), LockOut::default(), "already locked: click is gameplay");
    }

    #[test]
    fn escape_releases_cursor_and_buttons_only_when_locked() {
        let mut p = PointerLock::default();
        assert_eq!(p.on(LockEvent::Escape), LockOut::default(), "Esc while free does nothing");
        p.on(LockEvent::LeftClick);
        let o = p.on(LockEvent::Escape);
        assert!(o.changed && o.release_buttons && !o.swallow_click);
        assert!(!p.locked());
        assert!(p.cursor().visible && !p.cursor().grab);
    }

    #[test]
    fn focus_loss_releases_regain_does_not_regrab() {
        let mut p = PointerLock::default();
        p.on(LockEvent::LeftClick);
        let o = p.on(LockEvent::FocusLost);
        assert!(o.changed && o.release_buttons);
        assert_eq!(p.on(LockEvent::FocusGained), LockOut::default());
        assert!(!p.locked(), "needs a fresh click to play again");
        // alt-tab while already free still clears stuck keys
        let o = p.on(LockEvent::FocusLost);
        assert!(!o.changed && o.release_buttons);
    }

    #[test]
    fn full_session_sequence() {
        let mut p = PointerLock::default();
        let mut t = ButtonTracker::default();
        p.on(LockEvent::LeftClick);
        let down = |a| ButtonEvent { action: a, down: true };
        t.apply(&down(Action::Cling));
        t.apply(&down(Action::Slide));
        t.apply(&ButtonEvent { action: Action::Slide, down: false });
        let o = p.on(LockEvent::FocusLost);
        let mut out = Vec::new();
        if o.release_buttons {
            t.release_all(&mut out);
        }
        assert_eq!(out.len(), 1, "only the still-held Cling is released");
        assert_eq!((out[0].action, out[0].down), (Action::Cling, false));
        let mut again = Vec::new();
        t.release_all(&mut again);
        assert!(again.is_empty(), "tracker empty after release");
    }
}
