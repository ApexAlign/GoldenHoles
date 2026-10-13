//! Flow: a streak of chained traversal moves (wall-run, hook, dash, wall-jump) run back-to-back without a ground
//! reset. Same shape as the katana combo counter (`combat::katana`: a streak that climbs on every connected hit,
//! applied here to movement instead of combat), and it feeds straight back into the katana: a small move-speed
//! multiplier while it is active, plus a damage bonus on hits landed while flowing (docs/traversal_melee.md, "Flow").
//!
//! Server-authoritative, bit-identical client prediction (same inputs in -> same streak out), like the combo
//! counter. Not on the wire: the streak is derived every tick from `CharacterSystem`'s own age counters
//! (`dash_age`, `wallrun_age`, `hook_age` via `GrappleEvent::Attached`, `walljump_age`), the same way the katana's
//! hook / wall-jump air-cut bonuses already read them.

/// Three escalating tiers: moves needed to reach each one, the move-speed multiplier it grants, and the damage
/// bonus a landed hit gets while that tier is active. 0 moves = that tier (and every tier after it) is off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlowCfg {
    /// Chained traversal moves needed for tiers 1 / 2 / 3.
    pub tier_moves: [u32; 3],
    /// Move-speed multiplier at each tier (1 = none).
    pub tier_speed: [f32; 3],
    /// Damage bonus on a hit landed while that tier is active (0.1 = +10 %).
    pub tier_dmg: [f32; 3],
    /// Flat horizontal speed (m/s) at or below which landing on the ground counts as a full stop and resets the
    /// streak; above it (a landing roll, a slide, straight into the next move) the streak survives.
    pub stop_speed: f32,
}

impl Default for FlowCfg {
    fn default() -> Self {
        Self { tier_moves: [0, 0, 0], tier_speed: [1.0, 1.0, 1.0], tier_dmg: [0.0, 0.0, 0.0], stop_speed: 1.0 }
    }
}

/// The streak counter and the tier it derives, mirroring `katana::Katana`'s `streak` field.
pub struct Flow {
    pub cfg: FlowCfg,
    streak: u32,
}

impl Flow {
    pub fn new(cfg: FlowCfg) -> Self {
        Self { cfg, streak: 0 }
    }

    pub fn apply_balance(&mut self, cfg: FlowCfg) {
        self.cfg = cfg;
    }

    /// Chained traversal moves in the current streak.
    pub fn streak(&self) -> u32 {
        self.streak
    }

    /// 0 (no bonus) .. 3. `tier_moves` is ascending; the streak must clear a tier's move count to hold it.
    pub fn tier(&self) -> u32 {
        self.cfg.tier_moves.iter().filter(|&&m| m > 0 && self.streak >= m).count() as u32
    }

    /// Move-speed multiplier of the current tier (1 = none).
    pub fn speed_mult(&self) -> f32 {
        match self.tier() {
            0 => 1.0,
            t => self.cfg.tier_speed[(t - 1) as usize],
        }
    }

    /// Damage multiplier a landed hit gets from the current tier (1 = none).
    pub fn dmg_mult(&self) -> f32 {
        match self.tier() {
            0 => 1.0,
            t => 1.0 + self.cfg.tier_dmg[(t - 1) as usize],
        }
    }

    /// One traversal move (wall-run entry, hook attach, dash, wall-jump) just happened: extend the streak.
    pub fn add_move(&mut self) {
        self.streak += 1;
    }

    /// Call every tick before `add_move`: landing fully and standing still resets the streak. A quick landing roll
    /// or sliding straight into the next move (`flat_speed` above `stop_speed`) does not.
    pub fn ground_check(&mut self, grounded: bool, flat_speed: f32) {
        if grounded && flat_speed <= self.cfg.stop_speed {
            self.streak = 0;
        }
    }

    /// The owner took a hit: break the streak (mirrors the katana combo's `on_hurt`).
    pub fn on_hurt(&mut self) {
        self.streak = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> FlowCfg {
        FlowCfg { tier_moves: [2, 4, 6], tier_speed: [1.03, 1.06, 1.10], tier_dmg: [0.05, 0.10, 0.15], stop_speed: 1.0 }
    }

    #[test]
    fn tiers_unlock_at_their_move_counts_and_stack_the_speed_and_damage_bonus() {
        let mut f = Flow::new(cfg());
        assert_eq!((f.tier(), f.speed_mult(), f.dmg_mult()), (0, 1.0, 1.0));
        f.add_move();
        assert_eq!(f.tier(), 0, "one move: not yet tier 1");
        f.add_move();
        assert_eq!((f.streak(), f.tier()), (2, 1));
        assert!((f.speed_mult() - 1.03).abs() < 1e-6);
        assert!((f.dmg_mult() - 1.05).abs() < 1e-6);
        f.add_move();
        f.add_move();
        assert_eq!((f.streak(), f.tier()), (4, 2));
        assert!((f.speed_mult() - 1.06).abs() < 1e-6);
        f.add_move();
        f.add_move();
        assert_eq!((f.streak(), f.tier()), (6, 3));
        assert!((f.speed_mult() - 1.10).abs() < 1e-6);
        assert!((f.dmg_mult() - 1.15).abs() < 1e-6);
        // past the top tier the streak keeps climbing but the bonus caps
        f.add_move();
        assert_eq!(f.tier(), 3);
    }

    #[test]
    fn a_full_ground_stop_resets_it_but_a_fast_landing_roll_does_not() {
        let mut f = Flow::new(cfg());
        f.add_move();
        f.add_move();
        assert_eq!(f.streak(), 2);
        // grounded but still moving fast (a landing roll into the next move): no reset
        f.ground_check(true, 8.0);
        assert_eq!(f.streak(), 2);
        f.add_move();
        assert_eq!(f.streak(), 3);
        // grounded and stopped: the streak dies
        f.ground_check(true, 0.2);
        assert_eq!(f.streak(), 0);
        // airborne, whatever the speed: never a ground reset
        f.add_move();
        f.add_move();
        f.ground_check(false, 0.0);
        assert_eq!(f.streak(), 2);
    }

    #[test]
    fn a_hit_taken_resets_the_streak() {
        let mut f = Flow::new(cfg());
        f.add_move();
        f.add_move();
        f.add_move();
        assert_eq!(f.tier(), 1);
        f.on_hurt();
        assert_eq!((f.streak(), f.tier(), f.speed_mult(), f.dmg_mult()), (0, 0, 1.0, 1.0));
    }

    #[test]
    fn tiers_off_by_default() {
        let f = Flow::new(FlowCfg::default());
        assert_eq!(f.tier(), 0);
        let mut f = f;
        for _ in 0..20 {
            f.add_move();
        }
        assert_eq!((f.tier(), f.speed_mult(), f.dmg_mult()), (0, 1.0, 1.0), "zero move-counts: flow stays off");
    }
}
