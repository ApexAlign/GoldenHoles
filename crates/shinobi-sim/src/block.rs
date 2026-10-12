//! Block / parry shield and the strike-timing window.
//!
//! **Block** (held button): the aim is locked where it was when the hold began (the server ignores any other aim,
//! the client ignores look), movement is untouched. Damage from the front arc is cut to `front_mult`; none from
//! behind. A block raised just before a hit lands (`parry_ticks`) is a **parry**: the hit is negated, the attacker
//! is staggered (cannot act) and wide open to a counter. The guard is not free: it drains while held, each blocked
//! hit costs `hit_cost`, and an emptied guard is broken (no block until `rearm_ticks` pass).
//!
//! **Strike window**: a player is *open* right after their own swing (recovering) and while staggered after a
//! parry. A hit inside the window deals `bonus_mult` x damage. The window is public (`FX_OPEN` in the snapshot) so
//! the targeting triangle can show when to strike.
//!
//! Everything here is a pure function of ticks, so the server alone decides; clients only see the `FX_*` bits.

use shinobi_proto::snapshot::{FX_BLOCK, FX_OPEN, FX_STAGGER};

/// Tunables in ticks (built from `balance::GuardBalance`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockCfg {
    /// Half-angle of the protected frontal arc, radians.
    pub front_rad: f32,
    /// Damage taken from the front while blocking (0 = immune).
    pub front_mult: f32,
    /// A block raised this recently when a hit lands is a parry.
    pub parry_ticks: u32,
    /// Continuous hold before the guard is spent.
    pub max_hold_ticks: u32,
    /// Ticks to refill an empty guard while not blocking.
    pub regen_ticks: u32,
    /// Guard fraction (0..1) a blocked (not parried) hit costs.
    pub hit_cost: f32,
    /// Once the guard is spent, no block until this many ticks have passed.
    pub rearm_ticks: u32,
    /// A block lowered without having absorbed a hit cannot be re-raised for this long (anti spam-raise; 0 = off).
    pub lockout_ticks: u32,
    /// A parried melee attacker cannot act this long.
    pub stagger_ticks: u32,
    /// The strike window after one's own swing.
    pub swing_open_ticks: u32,
    /// The counter window after being parried.
    pub counter_open_ticks: u32,
    /// Damage multiplier inside the window (1 = no bonus).
    pub bonus_mult: f32,
    /// A guard broken by a hit staggers its owner this long (they cannot act), then...
    pub break_stagger_ticks: u32,
    /// ...for `mercy_ticks` after any guard break, damage taken is scaled by `mercy_mult`: a broken guard must not
    /// hand the attacker a free combo.
    pub mercy_ticks: u32,
    pub mercy_mult: f32,
}

impl Default for BlockCfg {
    fn default() -> Self {
        crate::balance::GuardBalance::default().cfg()
    }
}

/// Per-player block + window state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockState {
    /// The shield is up (after the server's own checks, not the raw button).
    pub held: bool,
    /// Tick the current hold began.
    pub since: u32,
    /// Guard left, 0..=1.
    pub energy: f32,
    rearm_until: u32,
    /// This hold has absorbed at least one hit (block or parry): lowering it is then free.
    used: bool,
    /// Cannot act until this tick (parried).
    pub stagger_until: u32,
    /// Strike window open until this tick.
    pub open_until: u32,
    /// Damage is scaled down (`BlockCfg::mercy_mult`) until this tick: the grace after a guard break.
    pub mercy_until: u32,
}

impl Default for BlockState {
    fn default() -> Self {
        Self { held: false, since: 0, energy: 1.0, rearm_until: 0, used: false, stagger_until: 0, open_until: 0, mercy_until: 0 }
    }
}

impl BlockState {
    /// Advance one tick. `want`: the button is down; `can`: alive and the match is live.
    pub fn update(&mut self, want: bool, tick: u32, cfg: &BlockCfg, can: bool) {
        let usable = can && tick >= self.stagger_until && tick >= self.rearm_until;
        if want && usable && self.energy > 0.0 {
            if !self.held {
                self.held = true;
                self.since = tick;
                self.used = false;
            }
            self.energy -= 1.0 / cfg.max_hold_ticks.max(1) as f32;
            if self.energy <= 0.0 {
                self.break_guard(tick, cfg);
            }
        } else {
            // lowered by choice without the shield ever taking a hit: spam-raise is punished with a lockout
            if self.held && !want && !self.used && can {
                self.rearm_until = self.rearm_until.max(tick + cfg.lockout_ticks);
            }
            self.held = false;
            if tick >= self.rearm_until {
                self.energy = (self.energy + 1.0 / cfg.regen_ticks.max(1) as f32).min(1.0);
            }
        }
    }

    /// The guard is spent: down, and no re-raise for `rearm_ticks`.
    pub fn break_guard(&mut self, tick: u32, cfg: &BlockCfg) {
        self.held = false;
        self.energy = 0.0;
        self.rearm_until = tick + cfg.rearm_ticks;
        self.mercy_until = tick + cfg.mercy_ticks;
    }

    /// The shield took a hit (blocked or parried): lowering it is no longer a wasted raise.
    pub fn mark_used(&mut self) {
        self.used = true;
    }

    /// Inside the grace after a guard break.
    pub fn mercy(&self, tick: u32) -> bool {
        tick < self.mercy_until
    }

    /// First tick of a hold: the aim is accepted here and frozen afterwards.
    pub fn just_raised(&self, tick: u32) -> bool {
        self.held && self.since == tick
    }

    pub fn parrying(&self, tick: u32, cfg: &BlockCfg) -> bool {
        self.held && tick.wrapping_sub(self.since) < cfg.parry_ticks
    }

    pub fn staggered(&self, tick: u32) -> bool {
        tick < self.stagger_until
    }

    pub fn open(&self, tick: u32) -> bool {
        tick < self.open_until
    }

    /// Start (or extend) a strike window.
    pub fn open_for(&mut self, tick: u32, ticks: u32) {
        self.open_until = self.open_until.max(tick + ticks);
    }

    /// Back to a clean slate (respawn).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `FX_*` bits for the snapshot.
    pub fn fx_bits(&self, tick: u32) -> u8 {
        (if self.held { FX_BLOCK } else { 0 }) | (if self.open(tick) { FX_OPEN } else { 0 }) | (if self.staggered(tick) { FX_STAGGER } else { 0 })
    }
}

/// What a victim's guard does to one incoming hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guard {
    /// No guard applies (down, or the attacker is behind the arc).
    None,
    /// Blocked: damage cut to `front_mult`.
    Blocked,
    /// Parried: damage negated.
    Parried,
}

/// Does `victim`'s shield cover a hit from `attacker`? `yaw` is the victim's facing (0 faces -Z, positive turns
/// toward -X, the sim's convention); positions are world metres (only x/z matter).
pub fn guard_vs(st: &BlockState, yaw: f32, victim: [f32; 3], attacker: [f32; 3], tick: u32, cfg: &BlockCfg) -> Guard {
    if !st.held {
        return Guard::None;
    }
    let (dx, dz) = (attacker[0] - victim[0], attacker[2] - victim[2]);
    let len = libm::sqrtf(dx * dx + dz * dz);
    if len < 1e-3 {
        return Guard::None; // on top of each other: no meaningful front
    }
    let fwd = (-libm::sinf(yaw), -libm::cosf(yaw));
    let cos = (dx * fwd.0 + dz * fwd.1) / len;
    if cos < libm::cosf(cfg.front_rad) {
        return Guard::None;
    }
    if st.parrying(tick, cfg) {
        Guard::Parried
    } else {
        Guard::Blocked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BlockCfg {
        BlockCfg { max_hold_ticks: 120, regen_ticks: 120, rearm_ticks: 60, parry_ticks: 12, ..BlockCfg::default() }
    }

    #[test]
    fn the_guard_drains_breaks_rearms_and_refills() {
        let c = cfg();
        let mut b = BlockState::default();
        for t in 0..200 {
            b.update(true, t, &c, true);
            if t == 10 {
                assert!(b.held && b.energy < 1.0);
            }
        }
        assert!(!b.held, "held through the whole guard: it is spent and down");
        assert!(b.energy <= 0.0);
        // while re-arming a held button does nothing
        let broke_at = 120;
        b.update(true, broke_at + 30, &c, true);
        assert!(!b.held, "no re-raise inside the re-arm time");
        // after it the button works again (energy refills from empty)
        for t in 200..400 {
            b.update(false, t, &c, true);
        }
        assert!(b.energy > 0.9);
        b.update(true, 400, &c, true);
        assert!(b.held && b.just_raised(400));
    }

    #[test]
    fn only_the_front_arc_is_covered_and_a_fresh_block_parries() {
        let c = BlockCfg { front_rad: 70f32.to_radians(), ..cfg() };
        let mut b = BlockState::default();
        b.update(true, 100, &c, true);
        // victim faces -Z (yaw 0) at the origin
        let front = [0.0, 0.0, -5.0];
        let behind = [0.0, 0.0, 5.0];
        let side = [5.0, 0.0, 0.0];
        assert_eq!(guard_vs(&b, 0.0, [0.0; 3], front, 100, &c), Guard::Parried, "raised this tick: a parry");
        assert_eq!(guard_vs(&b, 0.0, [0.0; 3], front, 100 + c.parry_ticks, &c), Guard::Blocked, "late: a plain block");
        assert_eq!(guard_vs(&b, 0.0, [0.0; 3], behind, 100, &c), Guard::None, "nothing from behind");
        assert_eq!(guard_vs(&b, 0.0, [0.0; 3], side, 100, &c), Guard::None, "90 degrees is outside the 70 degree arc");
        // turned toward +X the side hit is the front one
        assert_ne!(guard_vs(&b, -std::f32::consts::FRAC_PI_2, [0.0; 3], side, 100, &c), Guard::None);
        let down = BlockState::default();
        assert_eq!(guard_vs(&down, 0.0, [0.0; 3], front, 100, &c), Guard::None, "no shield, no guard");
    }

    #[test]
    fn stagger_blocks_blocking_and_the_window_reports_in_the_snapshot_bits() {
        let c = cfg();
        let mut b = BlockState::default();
        b.stagger_until = 50;
        b.open_for(10, 30);
        b.update(true, 20, &c, true);
        assert!(!b.held, "a staggered player cannot raise a guard");
        assert_eq!(b.fx_bits(20), FX_OPEN | FX_STAGGER);
        assert_eq!(b.fx_bits(45), FX_STAGGER, "window over, still staggered");
        b.update(true, 60, &c, true);
        assert_eq!(b.fx_bits(60), FX_BLOCK);
    }

    #[test]
    fn a_raise_that_absorbs_nothing_costs_a_lockout_but_a_used_one_does_not() {
        let c = BlockCfg { lockout_ticks: 60, ..cfg() };
        // spam-raise: up for a few ticks, nothing hits it, lowered by choice
        let mut b = BlockState::default();
        for t in 0..5 {
            b.update(true, t, &c, true);
        }
        b.update(false, 5, &c, true);
        b.update(true, 6, &c, true);
        assert!(!b.held, "dropped an unused shield: locked out");
        b.update(true, 5 + 59, &c, true);
        assert!(!b.held, "still inside the 1 s lockout");
        b.update(true, 5 + 60, &c, true);
        assert!(b.held, "and back after it");
        // a shield that took a hit is free to lower
        let mut u = BlockState::default();
        u.update(true, 0, &c, true);
        u.mark_used();
        u.update(false, 3, &c, true);
        u.update(true, 4, &c, true);
        assert!(u.held, "a used shield re-raises at once");
        // being staggered out of a raise is not a choice and is not punished again
        let mut s = BlockState::default();
        s.update(true, 0, &c, true);
        s.stagger_until = 10;
        s.update(true, 1, &c, true);
        assert!(!s.held);
        s.update(true, 10, &c, true);
        assert!(s.held, "no lockout on top of a stagger");
    }

    #[test]
    fn the_demo_parry_window_is_a_tight_timing_check() {
        let g = crate::balance::Balance::demo().guard;
        let c = g.cfg();
        assert!(c.parry_ticks >= 7 && c.parry_ticks <= 9, "~0.13 s at 60 Hz: {}", c.parry_ticks);
        assert!((g.front_deg * 2.0 - 150.0).abs() < 1e-3 && g.hit_cost >= 0.2 && g.max_hold_s <= 2.5 && g.front_mult >= 0.15);
        assert!(c.lockout_ticks >= 50 && c.rearm_ticks >= 50, "spam and a spent guard both cost about a second");
        assert_eq!((c.stagger_ticks, c.counter_open_ticks), (48, 60), "the parry payoffs are unchanged: 0.8 s stagger, 1 s counter");
        let mut b = BlockState::default();
        b.update(true, 100, &c, true);
        let front = [0.0, 0.0, -3.0];
        assert_eq!(guard_vs(&b, 0.0, [0.0; 3], front, 100 + c.parry_ticks - 1, &c), Guard::Parried);
        assert_eq!(guard_vs(&b, 0.0, [0.0; 3], front, 100 + c.parry_ticks, &c), Guard::Blocked);
    }
}
