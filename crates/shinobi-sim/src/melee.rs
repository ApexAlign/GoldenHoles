//! Melee action heighteners, all server-side and all behind `balance::MeleeBalance` (zero = off):
//!
//! * **Momentum**: a slash is worth more the faster the swordsman moves (run, dash, grapple, fall), up to a cap.
//! * **Knockback + air juggle**: a slash launches its victim away from the attacker with a small pop; a victim already in
//!   the air is kept up (a floor on their vertical speed) so the next cut still connects.
//! * **Wall-splat**: a launched victim who is stopped dead by a wall inside the window takes bonus damage and is stunned.
//! * **Ring-out**: a victim who falls into the void within a few seconds of being hit is a kill for whoever hit them.
//! * **Kill streak**: each katana kill inside the window stacks bonus damage and heals a little (adrenaline).
//!
//! Pure bookkeeping on ticks; `pvp::apply_hits` and `tick::step` call into it.

/// Hazard source id of a wall-splat (like `pvp::VOID_SOURCE_ID`: an `Obstacle` id clients can special-case).
pub const SPLAT_SOURCE_ID: u16 = u16::MAX - 1;

/// Tunables in ticks / m / m/s (built from `balance::MeleeBalance`).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct MeleeCfg {
    /// Slash damage bonus per m/s of the attacker's speed, and its cap (0.6 = at most +60 %).
    pub momentum_per_mps: f32,
    pub momentum_cap: f32,
    /// Horizontal launch speed (m/s) a slash gives its victim, and the upward pop for a grounded one (0 = no knockback).
    pub knock_speed: f32,
    pub knock_pop: f32,
    /// A victim already airborne when cut keeps at least this much upward speed (the juggle).
    pub juggle_up: f32,
    /// Ticks after a knock in which hitting a wall counts as a splat, the damage and stagger ticks of one, and the
    /// minimum launch speed for it.
    pub splat_ticks: u32,
    pub splat_damage: f32,
    pub splat_stagger_ticks: u32,
    /// Ticks after being hit in which falling into the void credits the attacker (0 = no ring-out credit).
    pub ring_ticks: u32,
    /// Kill-streak window ticks, bonus per stacked kill, cap on stacks, heal per kill (hp).
    pub streak_ticks: u32,
    pub streak_bonus: f32,
    pub streak_cap: u32,
    pub streak_heal: f32,
    /// Execute threshold (fraction of max hp; 0 = off), air-cut slam speed and shock-wave radius / knock (0 = off), and the
    /// counter-cut window in ticks after a parry (0 = off). See `katana_moves.rs`.
    pub exec_frac: f32,
    pub slam_speed: f32,
    pub shock_radius: f32,
    pub shock_knock: f32,
    pub counter_ticks: u32,
}

/// Per-player melee state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeleeState {
    /// Who last hurt this player and when (ring-out credit).
    pub last_hit: Option<(u16, u32)>,
    /// A knock in progress: until which tick a wall hit is a splat, the launch speed and who launched.
    pub knock_until: u32,
    pub knock_speed: f32,
    pub knock_by: u16,
    /// Katana kills stacked in the streak window, and when it lapses.
    pub streak: u32,
    pub streak_until: u32,
}

impl MeleeState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

impl MeleeCfg {
    /// Damage multiplier for a slash by an attacker moving at `speed` m/s with this streak.
    pub fn damage_mult(&self, att: &MeleeState, speed: f32, tick: u32) -> f32 {
        let momentum = (speed * self.momentum_per_mps).clamp(0.0, self.momentum_cap);
        let streak = if tick < att.streak_until { att.streak.min(self.streak_cap) as f32 * self.streak_bonus } else { 0.0 };
        1.0 + momentum + streak
    }

    /// Is a blow on a victim with `hp` of `max_hp` an execution? Only a stunned victim (parried or splatted) at or under
    /// `exec_frac` of max hp (0 = off).
    pub fn executes(&self, hp: f32, max_hp: f32, stunned: bool) -> bool {
        self.exec_frac > 0.0 && stunned && hp > 0.0 && hp <= self.exec_frac * max_hp
    }

    /// The attacker's kill: stack the streak, return the hp to heal.
    pub fn on_kill(&self, att: &mut MeleeState, tick: u32) -> f32 {
        if self.streak_ticks == 0 {
            return 0.0;
        }
        att.streak = if tick < att.streak_until { att.streak + 1 } else { 1 };
        att.streak_until = tick + self.streak_ticks;
        self.streak_heal
    }

    /// A slash landed on `victim_vel` from `attacker_pos` (world metres): the new velocity (launch + juggle) and the
    /// launch speed to remember, or `None` when knockback is off. `airborne`: the victim is off the ground.
    pub fn knock(&self, victim_pos: [f32; 3], attacker_pos: [f32; 3], vel: [f32; 3], airborne: bool) -> Option<[f32; 3]> {
        if self.knock_speed <= 0.0 {
            return None;
        }
        let (dx, dz) = (victim_pos[0] - attacker_pos[0], victim_pos[2] - attacker_pos[2]);
        let len = (dx * dx + dz * dz).sqrt();
        if len < 1e-3 {
            return None;
        }
        let (nx, nz) = (dx / len, dz / len);
        let vy = if airborne { vel[1].max(self.juggle_up) } else { self.knock_pop.max(vel[1]) };
        Some([nx * self.knock_speed, vy, nz * self.knock_speed])
    }

    /// Per tick for a knocked player: did a wall just stop them dead? `speed` = their horizontal speed now, `airborne`
    /// = still off the ground (landing and sliding stop you too, but that is friction, not a wall). Returns the
    /// launcher's id once, clearing the knock.
    pub fn splat(&self, st: &mut MeleeState, speed: f32, airborne: bool, tick: u32) -> Option<u16> {
        if self.splat_ticks == 0 || st.knock_speed <= 0.0 {
            return None;
        }
        if tick >= st.knock_until || !airborne {
            st.knock_speed = 0.0;
            return None;
        }
        if speed < st.knock_speed * 0.35 {
            st.knock_speed = 0.0;
            return Some(st.knock_by);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> MeleeCfg {
        MeleeCfg { momentum_per_mps: 0.04, momentum_cap: 0.6, knock_speed: 8.0, knock_pop: 3.0, juggle_up: 5.0, splat_ticks: 30, splat_damage: 15.0, splat_stagger_ticks: 36, ring_ticks: 180, streak_ticks: 480, streak_bonus: 0.08, streak_cap: 4, streak_heal: 20.0, ..Default::default() }
    }

    #[test]
    fn momentum_and_streak_stack_and_cap() {
        let c = cfg();
        let mut a = MeleeState::default();
        assert_eq!(c.damage_mult(&a, 0.0, 0), 1.0);
        assert!((c.damage_mult(&a, 10.0, 0) - 1.4).abs() < 1e-5);
        assert!((c.damage_mult(&a, 100.0, 0) - 1.6).abs() < 1e-5, "momentum caps at +60 %");
        assert_eq!(c.on_kill(&mut a, 100), 20.0);
        assert_eq!(c.on_kill(&mut a, 200), 20.0);
        assert_eq!(a.streak, 2);
        assert!((c.damage_mult(&a, 0.0, 300) - 1.16).abs() < 1e-5, "two stacked kills: +16 %");
        assert_eq!(c.damage_mult(&a, 0.0, 700), 1.0, "the streak lapses");
        c.on_kill(&mut a, 800);
        assert_eq!(a.streak, 1, "and starts over");
    }

    #[test]
    fn only_a_stunned_victim_at_low_hp_is_executed() {
        let c = MeleeCfg { exec_frac: 0.25, ..cfg() };
        assert!(c.executes(25.0, 100.0, true), "on the line");
        assert!(!c.executes(26.0, 100.0, true), "too healthy");
        assert!(!c.executes(10.0, 100.0, false), "not stunned: a normal fight");
        assert!(!c.executes(0.0, 100.0, true), "already dead");
        assert!(!cfg().executes(10.0, 100.0, true), "off by default");
    }

    #[test]
    fn knock_launches_away_pops_grounded_victims_and_juggles_airborne_ones() {
        let c = cfg();
        let v = c.knock([0.0, 1.0, -3.0], [0.0, 1.0, 0.0], [0.0; 3], false).unwrap();
        assert!((v[2] + 8.0).abs() < 1e-4 && v[0].abs() < 1e-4 && v[1] == 3.0, "away from the attacker with a pop: {v:?}");
        let j = c.knock([0.0, 3.0, -3.0], [0.0, 1.0, 0.0], [0.0, -2.0, 0.0], true).unwrap();
        assert_eq!(j[1], 5.0, "an airborne victim is kept up for the next cut");
        assert!(MeleeCfg::default().knock([0.0; 3], [1.0, 0.0, 0.0], [0.0; 3], false).is_none(), "off by default");
    }

    #[test]
    fn a_wall_stop_inside_the_window_is_a_splat_but_landing_or_time_is_not() {
        let c = cfg();
        let knocked = MeleeState { knock_until: 130, knock_speed: 8.0, knock_by: 7, ..Default::default() };
        let mut s = knocked;
        assert_eq!(c.splat(&mut s, 7.5, true, 105), None, "still flying");
        assert_eq!(c.splat(&mut s, 1.0, true, 110), Some(7), "stopped dead in the air: a wall");
        assert_eq!(c.splat(&mut s, 0.0, true, 111), None, "only once");
        let mut s = knocked;
        assert_eq!(c.splat(&mut s, 1.0, false, 110), None, "landing is friction, not a wall");
        let mut s = knocked;
        assert_eq!(c.splat(&mut s, 1.0, true, 140), None, "too late");
    }
}
