//! Kinetic Battery and Aegis aura (`docs/weapons/shield.md`). Server-authoritative, passive, no movement impact.

pub const BATTERY_MAX: u16 = 1000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShieldCfg {
    pub blocked_star_charge: f32,
    pub parry_charge: f32,
    pub aura_ticks: u32,
    pub aura_damage_mult: f32,
}

impl Default for ShieldCfg {
    fn default() -> Self {
        Self { blocked_star_charge: 0.08, parry_charge: 0.25, aura_ticks: 300, aura_damage_mult: 1.2 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShieldState {
    /// Fixed 0..=1000. `BATTERY_MAX` is the trigger and is consumed by the aura.
    pub battery: u16,
    /// Tick after which the active aura is over (exclusive). 0 with `aura_total == 0` means inactive.
    pub aura_until: u32,
    /// Authored duration of the active aura, for the HUD; 0 when inactive.
    pub aura_total: u16,
    /// Ticks of continuous `core_vent` occupation already banked toward `fill_ticks`.
    pub vent_fill_ticks: u32,
    /// Tick before which this player's `core_vent` is cooling down.
    pub vent_ready_tick: u32,
    /// Tick before which a completed `core_vent` fill keeps this player revealed to everyone (`reveal_ticks`).
    pub vent_reveal_until: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShieldTick {
    None,
    AuraStart,
    AuraEnd,
}

fn charge_q(fraction: f32) -> u16 {
    libm::roundf(fraction.clamp(0.0, 1.0) * BATTERY_MAX as f32) as u16
}

impl ShieldState {
    pub fn battery_frac(&self) -> f32 {
        self.battery as f32 / BATTERY_MAX as f32
    }

    pub fn aura_active(&self, tick: u32) -> bool {
        self.aura_total != 0 && tick < self.aura_until
    }

    pub fn aura_ticks_left(&self, tick: u32) -> u16 {
        if self.aura_active(tick) {
            self.aura_until.saturating_sub(tick).min(u16::MAX as u32) as u16
        } else {
            0
        }
    }

    /// Add a charge fraction; the aura trigger is resolved by `update` at the end of the tick.
    pub fn add_charge(&mut self, fraction: f32) {
        self.battery = self.battery.saturating_add(charge_q(fraction)).min(BATTERY_MAX);
    }

    pub fn blocked_star(&mut self, cfg: &ShieldCfg) {
        self.add_charge(cfg.blocked_star_charge);
    }

    pub fn parry(&mut self, cfg: &ShieldCfg) {
        self.add_charge(cfg.parry_charge);
    }

    /// One tick of `core_vent` occupation. `true` when this tick completes a fill.
    pub fn vent_tick(&mut self, tick: u32, fill_ticks: u32, cooldown_ticks: u32) -> bool {
        if tick < self.vent_ready_tick {
            return false;
        }
        let fill = fill_ticks.max(1);
        self.vent_fill_ticks = self.vent_fill_ticks.saturating_add(1).min(fill);
        let banked = (self.vent_fill_ticks as u64 * BATTERY_MAX as u64 / fill as u64) as u16;
        self.battery = self.battery.max(banked);
        if self.vent_fill_ticks >= fill {
            self.battery = BATTERY_MAX;
            self.vent_fill_ticks = 0;
            self.vent_ready_tick = tick.saturating_add(cooldown_ticks);
            return true;
        }
        false
    }

    /// End-of-tick transition: expire an old aura, then start one if the Battery is full. Cannot retrigger while
    /// active; a Battery that filled during the aura starts the next one on the expiry tick.
    pub fn update(&mut self, tick: u32, cfg: &ShieldCfg) -> ShieldTick {
        if self.aura_total != 0 && tick >= self.aura_until {
            self.aura_total = 0;
            self.aura_until = 0;
            if self.battery < BATTERY_MAX {
                return ShieldTick::AuraEnd;
            }
        }
        if self.aura_total == 0 && self.battery >= BATTERY_MAX {
            self.battery = 0;
            self.aura_total = cfg.aura_ticks.max(1).min(u16::MAX as u32) as u16;
            self.aura_until = tick.saturating_add(self.aura_total as u32);
            return ShieldTick::AuraStart;
        }
        ShieldTick::None
    }

    /// Death cancels the aura and the vent reveal. The Battery and the vent cooldown persist to the next spawn.
    pub fn on_death(&mut self) {
        self.aura_until = 0;
        self.aura_total = 0;
        self.vent_reveal_until = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_and_parry_charges_are_exact_and_saturate() {
        let cfg = ShieldCfg::default();
        let mut s = ShieldState::default();
        s.blocked_star(&cfg);
        assert_eq!(s.battery, 80);
        s.parry(&cfg);
        assert_eq!(s.battery, 330);
        for _ in 0..20 {
            s.parry(&cfg);
        }
        assert_eq!(s.battery, BATTERY_MAX);
    }

    #[test]
    fn vent_fills_progressively_and_keeps_progress_when_you_leave() {
        let mut s = ShieldState::default();
        assert!(!s.vent_tick(10, 4, 100));
        assert!(!s.vent_tick(11, 4, 100));
        assert_eq!(s.battery, 500);
        // outside: no call, progress and Battery stay; re-entry continues
        assert!(!s.vent_tick(20, 4, 100));
        assert_eq!(s.battery, 750);
        assert!(s.vent_tick(21, 4, 100));
        assert_eq!(s.battery, BATTERY_MAX);
        assert_eq!(s.vent_ready_tick, 121);
        assert_eq!(s.vent_fill_ticks, 0);
        assert!(!s.vent_tick(120, 4, 100), "cooling down");
        assert_eq!(s.vent_fill_ticks, 0);
        assert!(s.vent_tick(121, 1, 100), "cooldown over: a one-tick fill completes");
        assert_eq!(s.battery, BATTERY_MAX);
    }

    #[test]
    fn aura_lasts_its_full_duration_and_only_one_runs_at_a_time() {
        let cfg = ShieldCfg::default();
        let mut s = ShieldState::default();
        s.battery = BATTERY_MAX;
        assert_eq!(s.update(100, &cfg), ShieldTick::AuraStart);
        assert_eq!(s.battery, 0);
        assert_eq!(s.aura_ticks_left(100), 300);
        assert!(s.aura_active(399));
        assert!(!s.aura_active(400));
        // refilled during the aura: the next one starts on the expiry tick
        s.battery = BATTERY_MAX;
        assert_eq!(s.update(399, &cfg), ShieldTick::None);
        assert_eq!(s.update(400, &cfg), ShieldTick::AuraStart);
        assert_eq!(s.aura_ticks_left(400), 300);
    }
}
