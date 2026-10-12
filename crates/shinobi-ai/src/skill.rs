//! Skill presets and archetype profiles. `Cfg` (speed, jump, dash) is shared by every player, so an archetype is
//! only a *behaviour* profile; bots obey exactly the physics humans do.

use std::f32::consts::PI;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BotSkill {
    Easy,
    Normal,
    Hard,
}

impl BotSkill {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "easy" => Some(Self::Easy),
            "normal" => Some(Self::Normal),
            "hard" => Some(Self::Hard),
            _ => None,
        }
    }

    pub fn params(self) -> SkillParams {
        match self {
            Self::Easy => SkillParams {
                reaction_ticks: 24,
                aim_error: 0.16,
                turn_per_tick: 0.07,
                evade_chance: 0.35,
                strike_windup: 18,
                fire_gap_ticks: 24,
                use_walls: false,
                move_scale: 1.0,
                slice_miss: 0.4,
                parry_skill: 0.2,
            },
            Self::Normal => SkillParams {
                reaction_ticks: 16,
                aim_error: 0.10,
                turn_per_tick: 0.13,
                evade_chance: 0.7,
                strike_windup: 10,
                fire_gap_ticks: 18,
                use_walls: true,
                move_scale: 1.0,
                slice_miss: 0.25,
                parry_skill: 0.5,
            },
            Self::Hard => SkillParams {
                reaction_ticks: 5,
                aim_error: 0.025,
                turn_per_tick: 0.24,
                evade_chance: 0.95,
                strike_windup: 6,
                fire_gap_ticks: 11,
                use_walls: true,
                move_scale: 1.0,
                slice_miss: 0.12,
                parry_skill: 0.85,
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SkillParams {
    /// Ticks a threat (being in the target's view) must persist before the bot reacts.
    pub reaction_ticks: u32,
    /// Max aim noise, radians (uniform, re-drawn every 8 ticks so it reads as human tracking error).
    pub aim_error: f32,
    /// Aim slew cap per tick, radians. Must stay below `AntiCheatCfg::max_turn` (pi/2).
    pub turn_per_tick: f32,
    /// Probability the bot reacts to a threat at all once the reaction delay has elapsed.
    pub evade_chance: f32,
    /// Ticks of aim-lock before the katana dash-slice is pressed.
    pub strike_windup: u32,
    /// Minimum ticks between shuriken throws (the weapon cooldown is the floor).
    pub fire_gap_ticks: u32,
    /// May wall-run / perch.
    pub use_walls: bool,
    /// Fraction of full speed bots run at (host `aggression` below 1 slows them down: fewer chance meetings).
    pub move_scale: f32,
    /// Chance a katana dash is thrown on a bad read (aimed ~1.2 rad off, so the swing whiffs).
    pub slice_miss: f32,
    /// Skill at the timed block (0..1): the chance a bot raises its shield inside the parry window of an arrow it sees
    /// coming, and how readily it parries a katana cut it reads. A miss costs the 1 s block lockout, so bots do not spam.
    pub parry_skill: f32,
}

/// Behaviour profile. Light = cyber-ninja (flank, wall, perch, shurikens from odd angles).
/// Heavy = armour (front lane rush, occupies the player's view).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Archetype {
    Light,
    Heavy,
}

impl Archetype {
    /// Distance the bot tries to hold while zoning (no ticket / cooling down).
    pub fn zone_radius(self) -> f32 {
        match self {
            Self::Light => 11.0,
            Self::Heavy => 9.0,
        }
    }

    /// Slot angle relative to the target's facing (0 = dead ahead, +-pi = behind), by squad rank.
    /// Heavies fill the front arc, Lights take flanks and rear: the player's camera can cover one side at a time.
    pub fn slot_angle(self, rank: usize) -> f32 {
        let sign = if rank % 2 == 0 { 1.0 } else { -1.0 };
        let ring = (rank / 2) as f32;
        match self {
            Self::Heavy => sign * (0.12 + 0.30 * ring),
            Self::Light => sign * (PI * 0.42 + 0.30 * ring).min(PI),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parry_skill_climbs_with_the_tier_and_nobody_is_perfect() {
        let p = |s: BotSkill| s.params().parry_skill;
        assert!(0.0 < p(BotSkill::Easy) && p(BotSkill::Easy) < p(BotSkill::Normal) && p(BotSkill::Normal) < p(BotSkill::Hard) && p(BotSkill::Hard) < 1.0);
    }
}
