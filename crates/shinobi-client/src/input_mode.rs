//! Input modes for the interactive HUD: what the game may read while a menu-ish overlay is up.
//!
//! Pure logic (the shell maps keys to `ModeInput` once per frame and applies `ModeOut`). Layered on top of
//! `pointer_lock::PointerLock` (Esc / focus / click-to-grab stay there): this decides, while the pointer is locked,
//! whether the cursor is freed and which of move / look / actions still reach the game.
//!
//! | mode        | opens                                   | move | look | actions | cursor |
//! |-------------|------------------------------------------|------|------|---------|--------|
//! | Play        | default                                  | yes  | yes  | yes     | grabbed|
//! | Scoreboard  | hold Tab                                 | yes  | no   | no      | free   |
//! | BuildRadial | hold a build key >= `radial_hold_secs`   | yes  | no   | no      | free   |
//! | Console     | backquote (toggle)                       | no   | no   | no      | free   |
//!
//! Priority Console > BuildRadial > Scoreboard. Entering a mode that blocks actions releases every held button (a held
//! fire / slice / grapple must not stay latched under a menu), and Console also zeroes the stick, so movement keys typed
//! into the console never leak into the game. Esc closes Console / BuildRadial first (the shell must not forward that Esc
//! to the pointer lock), a second Esc is the pause menu as before.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Play,
    Scoreboard,
    BuildRadial,
    Console,
}

/// What the game may read in a mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeFlags {
    pub allow_move: bool,
    pub allow_look: bool,
    /// Buttons: jump / dash / slice / fire / grapple / build ...
    pub allow_actions: bool,
    /// The OS cursor is released (visible, UI hover / click) even though the pointer lock state is "locked".
    pub cursor_free: bool,
}

impl Mode {
    pub fn flags(self) -> ModeFlags {
        match self {
            Mode::Play => ModeFlags { allow_move: true, allow_look: true, allow_actions: true, cursor_free: false },
            Mode::Scoreboard | Mode::BuildRadial => ModeFlags { allow_move: true, allow_look: false, allow_actions: false, cursor_free: true },
            Mode::Console => ModeFlags { allow_move: false, allow_look: false, allow_actions: false, cursor_free: true },
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ModeCfg {
    /// Hold a build key this long to open the radial menu (a tap still prints immediately).
    pub radial_hold_secs: f32,
    /// Off until the HUD has a radial menu to show.
    pub radial_enabled: bool,
}

impl Default for ModeCfg {
    fn default() -> Self {
        Self { radial_hold_secs: 0.35, radial_enabled: false }
    }
}

impl ModeCfg {
    /// A build key was just released after `held_secs` (its hold time as of the previous frame): does the release print
    /// (a tap), or was it a radial hold whose release belongs to the menu? Build keys print on release so a hold can
    /// replace the print; the delivered `down`+`up` pair is exactly what a press used to send, so Combat's
    /// double-tap-to-cycle window (press to press, in ticks) is unchanged, only shifted by the tap length.
    pub fn build_release_prints(&self, held_secs: f32) -> bool {
        !(self.radial_enabled && held_secs >= self.radial_hold_secs)
    }
}

/// One frame of raw key state, already filtered to "the window has focus".
#[derive(Clone, Copy, Debug, Default)]
pub struct ModeInput {
    /// The pointer is grabbed (not paused / unfocused). Unlocked = the pause menu owns the screen: Play with a free cursor.
    pub locked: bool,
    pub tab_held: bool,
    /// Longest time (s) a build key has been held continuously; 0 when none is down.
    pub build_held_secs: f32,
    pub backquote_pressed: bool,
    pub escape_pressed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeOut {
    pub mode: Mode,
    pub flags: ModeFlags,
    pub changed: bool,
    /// Release every held button now (and zero the stick when `!flags.allow_move`).
    pub release_buttons: bool,
    /// This frame's Esc closed Console / BuildRadial: do not forward it to the pointer lock.
    pub consumed_escape: bool,
    /// The radial menu just closed by letting go of the build key: commit the highlighted piece.
    pub radial_commit: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ModeMachine {
    pub cfg: ModeCfg,
    mode: Mode,
    console: bool,
}

impl ModeMachine {
    pub fn new(cfg: ModeCfg) -> Self {
        Self { cfg, mode: Mode::Play, console: false }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn update(&mut self, inp: &ModeInput) -> ModeOut {
        let prev = self.mode;
        let mut consumed_escape = false;
        if !inp.locked {
            // pause menu / unfocused: nothing of ours is active (the console closes with the lock)
            self.console = false;
        } else {
            if inp.backquote_pressed {
                self.console = !self.console;
            }
            if inp.escape_pressed && (self.console || prev == Mode::BuildRadial) {
                self.console = false;
                consumed_escape = true;
            }
        }
        let radial = self.cfg.radial_enabled && inp.locked && inp.build_held_secs >= self.cfg.radial_hold_secs && !consumed_escape;
        self.mode = if !inp.locked {
            Mode::Play
        } else if self.console {
            Mode::Console
        } else if radial {
            Mode::BuildRadial
        } else if inp.tab_held {
            Mode::Scoreboard
        } else {
            Mode::Play
        };
        let flags = self.mode.flags();
        let changed = self.mode != prev;
        ModeOut {
            mode: self.mode,
            flags,
            changed,
            release_buttons: changed && !flags.allow_actions,
            consumed_escape,
            radial_commit: prev == Mode::BuildRadial && self.mode != Mode::BuildRadial && !consumed_escape && inp.locked && inp.build_held_secs == 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn play() -> ModeInput {
        ModeInput { locked: true, ..Default::default() }
    }

    #[test]
    fn tab_scoreboard_keeps_movement_but_frees_the_cursor_and_blocks_look_and_actions() {
        let mut m = ModeMachine::default();
        assert_eq!(m.update(&play()).mode, Mode::Play);
        let o = m.update(&ModeInput { tab_held: true, ..play() });
        assert_eq!(o.mode, Mode::Scoreboard);
        assert!(o.changed && o.release_buttons, "held fire / slice must not stay latched under the board");
        assert!(o.flags.allow_move && !o.flags.allow_look && !o.flags.allow_actions && o.flags.cursor_free);
        let o = m.update(&ModeInput { tab_held: true, ..play() });
        assert!(!o.changed && !o.release_buttons);
        let o = m.update(&play());
        assert_eq!(o.mode, Mode::Play);
        assert!(!o.flags.cursor_free && o.flags.allow_look && o.flags.allow_actions);
    }

    #[test]
    fn console_toggles_blocks_everything_and_esc_closes_it_first() {
        let mut m = ModeMachine::default();
        let o = m.update(&ModeInput { backquote_pressed: true, ..play() });
        assert_eq!(o.mode, Mode::Console);
        assert!(!o.flags.allow_move && !o.flags.allow_look && !o.flags.allow_actions && o.flags.cursor_free);
        assert!(o.release_buttons, "movement keys typed into the console must not leak into the game");
        // holding Tab under the console does not open the board
        assert_eq!(m.update(&ModeInput { tab_held: true, ..play() }).mode, Mode::Console);
        let o = m.update(&ModeInput { escape_pressed: true, ..play() });
        assert_eq!(o.mode, Mode::Play);
        assert!(o.consumed_escape, "the first Esc only closes the console");
        // a second Esc is not ours
        let o = m.update(&ModeInput { escape_pressed: true, ..play() });
        assert!(!o.consumed_escape);
        // backquote toggles
        assert_eq!(m.update(&ModeInput { backquote_pressed: true, ..play() }).mode, Mode::Console);
        assert_eq!(m.update(&ModeInput { backquote_pressed: true, ..play() }).mode, Mode::Play);
    }

    #[test]
    fn radial_opens_on_a_hold_only_when_enabled_and_commits_on_release() {
        let mut off = ModeMachine::default();
        assert_eq!(off.update(&ModeInput { build_held_secs: 2.0, ..play() }).mode, Mode::Play, "disabled by default: a hold is just a held key");
        let mut m = ModeMachine::new(ModeCfg { radial_enabled: true, ..Default::default() });
        assert_eq!(m.update(&ModeInput { build_held_secs: 0.2, ..play() }).mode, Mode::Play, "a tap / short hold prints, no menu");
        let o = m.update(&ModeInput { build_held_secs: 0.4, ..play() });
        assert_eq!(o.mode, Mode::BuildRadial);
        assert!(o.release_buttons && o.flags.cursor_free && !o.flags.allow_look);
        let o = m.update(&ModeInput { build_held_secs: 0.0, ..play() });
        assert_eq!(o.mode, Mode::Play);
        assert!(o.radial_commit, "letting go commits the highlighted piece");
        // Esc cancels instead of committing
        m.update(&ModeInput { build_held_secs: 0.5, ..play() });
        let o = m.update(&ModeInput { build_held_secs: 0.5, escape_pressed: true, ..play() });
        assert!(o.consumed_escape && !o.radial_commit && o.mode == Mode::Play);
    }

    #[test]
    fn priority_console_over_radial_over_scoreboard_and_pause_resets_everything() {
        let mut m = ModeMachine::new(ModeCfg { radial_enabled: true, ..Default::default() });
        let held = ModeInput { tab_held: true, build_held_secs: 1.0, ..play() };
        assert_eq!(m.update(&held).mode, Mode::BuildRadial);
        assert_eq!(m.update(&ModeInput { backquote_pressed: true, ..held }).mode, Mode::Console);
        // the pointer unlocks (Esc pause menu, focus loss): back to plain Play, console closed
        let paused = ModeInput { locked: false, ..held };
        assert_eq!(m.update(&paused).mode, Mode::Play);
        assert_eq!(m.update(&held).mode, Mode::BuildRadial, "the console did not survive the pause");
    }

    #[test]
    fn build_release_prints_for_taps_not_for_radial_holds() {
        let on = ModeCfg { radial_enabled: true, ..Default::default() };
        assert!(on.build_release_prints(0.05), "a quick tap prints");
        assert!(on.build_release_prints(0.34), "just under the hold threshold still prints");
        assert!(!on.build_release_prints(0.35), "a radial hold prints only through the menu");
        let off = ModeCfg::default();
        assert!(off.build_release_prints(5.0), "radial disabled: every release prints, however long");
    }
}
