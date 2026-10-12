//! Stealth cloak (docs/melee.md "Cloak"): a short, energy-limited stealth window, the last piece of the combat
//! redesign. The arena is a bright Tron grid, so full invisibility would read as a bug; the cloak is a client-side
//! visual (a faint, flickering outline, trail and glow dimmed, no footstep dust: `character_look.rs` / `audio.rs`)
//! layered on top of this plain energy state machine, which both the server and the bots run.
//!
//! Hold the button: while held, a charge of `duration_s` drains to empty; releasing early, running out, attacking
//! (a katana swing, a star throw, a bow shot) or taking any damage ends it at once and starts `cooldown_s` before it
//! can be raised again. A katana hit landed while still cloaked (so: before the swing itself has had a chance to end
//! it) is a stealth kill: `combat.cloak.stealth_mult` scales the blow (see `pvp::apply_hits`, mirroring `exec_frac`).
//! Sprinting, dashing and the grapple stay exactly as loud and visible while cloaked (`cloak::loud`) — only a
//! walking or crouched cloak is quiet, so the cosmetic stealth never removes every tell.

/// `combat.cloak.*` (Balance).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloakCfg {
    /// Seconds of charge a fresh activation gets.
    pub duration_s: f32,
    /// Seconds before the cloak can be raised again after it ends, by any means (timeout, choice, an attack, a hit taken).
    pub cooldown_s: f32,
    /// Damage multiplier of a katana hit landed while the attacker is still cloaked (0 = off; the demo sets this high
    /// enough to be a guaranteed kill against full hp, like an execute).
    pub stealth_mult: f32,
}

impl Default for CloakCfg {
    fn default() -> Self {
        Self { duration_s: 6.0, cooldown_s: 4.0, stealth_mult: 5.0 }
    }
}

/// Per-player cloak state. Server-authoritative only (no OwnState / prediction: it changes no movement and a one-tick
/// mispredict of a purely cosmetic flag is invisible), stepped once per tick like `PowerState` / `BlockState`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct CloakState {
    pub active: bool,
    /// Seconds of charge left while active (0 while inactive).
    pub energy: f32,
    /// Seconds left before the cloak can be raised again.
    pub cooldown: f32,
}

impl CloakState {
    /// Advance one tick. `want`: the button is held. `can_act`: alive, in a live match, not staggered (same gate as
    /// the other weapons).
    pub fn update(&mut self, dt: f32, want: bool, can_act: bool, cfg: &CloakCfg) {
        self.cooldown = (self.cooldown - dt).max(0.0);
        if self.active {
            self.energy -= dt;
            // tiny epsilon: 60 ticks of 1/60 s does not sum back to exactly 1.0 in f32
            if self.energy <= 1e-4 || !want || !can_act {
                self.end(cfg);
            }
        } else if want && can_act && self.cooldown <= 0.0 && cfg.duration_s > 0.0 {
            self.active = true;
            self.energy = cfg.duration_s;
        }
    }

    /// Attacking, or taking a hit, while cloaked: ends it immediately and starts the cooldown. A no-op if not active
    /// (so the caller never needs to check first).
    pub fn brk(&mut self, cfg: &CloakCfg) {
        if self.active {
            self.end(cfg);
        }
    }

    fn end(&mut self, cfg: &CloakCfg) {
        self.active = false;
        self.energy = 0.0;
        self.cooldown = cfg.cooldown_s;
    }

    /// Reset to fresh (death, respawn): no lingering cloak or cooldown across a life.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// 0..=1 charge left, for the HUD meter.
    pub fn energy_frac(&self, cfg: &CloakCfg) -> f32 {
        if cfg.duration_s <= 0.0 {
            0.0
        } else {
            (self.energy / cfg.duration_s).clamp(0.0, 1.0)
        }
    }
}

/// A cloaked player moving at `speed` (m/s, horizontal) is still loud / visible: sprinting or dashing gives away a
/// cloak the same as it would an uncloaked player (docs/melee.md "Cloak" / "Noise"). `run_speed` is the character's
/// own ground run speed (`character::Cfg::speed` or similar); anything at or above it, plus any dash/evasion/grapple
/// in flight, is a tell. A slow walk or a crouch-slide stays quiet.
pub fn loud(speed: f32, run_speed: f32, dashing: bool, roped: bool) -> bool {
    dashing || roped || (run_speed > 0.0 && speed >= run_speed - 1e-3)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn run(s: &mut CloakState, cfg: &CloakCfg, n: u32, want: bool) {
        for _ in 0..n {
            s.update(DT, want, true, cfg);
        }
    }

    #[test]
    fn holding_the_button_activates_and_drains_to_expiry_then_waits_out_the_cooldown() {
        let cfg = CloakCfg { duration_s: 1.0, cooldown_s: 0.5, stealth_mult: 5.0 };
        let mut s = CloakState::default();
        run(&mut s, &cfg, 1, true);
        assert!(s.active && (s.energy - 1.0).abs() < 1e-4);
        // the activation tick itself does not drain (the charge is full the instant it comes up), so draining the
        // full 1 s of charge takes 60 more ticks of being held
        run(&mut s, &cfg, 60, true);
        assert!(!s.active, "charge ran out");
        assert!((s.cooldown - 0.5).abs() < 1e-3);
        // cannot re-raise mid-cooldown even if the button is held throughout
        run(&mut s, &cfg, 29, true);
        assert!(!s.active, "cooldown {}", s.cooldown);
        run(&mut s, &cfg, 1, true);
        assert!(s.active, "cooldown elapsed: raised again");
    }

    #[test]
    fn releasing_early_ends_it_and_still_pays_the_cooldown() {
        let cfg = CloakCfg::default();
        let mut s = CloakState::default();
        run(&mut s, &cfg, 1, true);
        assert!(s.active);
        run(&mut s, &cfg, 30, false);
        assert!(!s.active && s.cooldown > 0.0);
    }

    #[test]
    fn a_break_ends_an_active_cloak_and_does_nothing_if_already_down() {
        let cfg = CloakCfg::default();
        let mut s = CloakState::default();
        s.brk(&cfg);
        assert_eq!(s.cooldown, 0.0, "nothing to break");
        run(&mut s, &cfg, 1, true);
        assert!(s.active);
        s.brk(&cfg);
        assert!(!s.active && (s.cooldown - cfg.cooldown_s).abs() < 1e-4);
    }

    #[test]
    fn clear_drops_any_lingering_cloak_or_cooldown() {
        let cfg = CloakCfg::default();
        let mut s = CloakState::default();
        run(&mut s, &cfg, 1, true);
        s.brk(&cfg);
        assert!(s.cooldown > 0.0);
        s.clear();
        assert_eq!(s, CloakState::default());
    }

    #[test]
    fn loudness_tells() {
        assert!(!loud(2.0, 6.0, false, false), "a walk stays quiet");
        assert!(loud(6.0, 6.0, false, false), "sprinting is a tell");
        assert!(loud(0.0, 6.0, true, false), "a dash is always a tell");
        assert!(loud(0.0, 6.0, false, true), "a taut grapple is always a tell");
    }
}
