//! Every combat / enemy / match tunable in ONE struct, with a std-only `key = value` text format so designers
//! can retune without recompiling.
//!
//! ```text
//! # comments start with '#'; blank lines are fine; keys are `section.name`
//! player.max_hp = 120
//! combat.shuriken.damage = 30
//! enemy.grunt.windup_ticks = 24
//! wave.inter_wave_ticks = 240
//! ```
//! A partial file keeps the defaults for everything it doesn't mention. Typos are errors (unknown key, with the
//! line number), as are non-finite / negative / unparsable values, so a bad file never half-applies.
//! Movement speeds live in `character::Cfg`, not here (the katana lunge is derived from it).
//!
//! Applied by `Server::set_balance`: it updates existing players and the server-side systems; players added
//! afterwards pick it up at creation.

use crate::combat::katana::KatanaCfg;
use crate::combat::shuriken::ShurikenCfg;
use crate::enemies::EnemyKind;
use crate::lagcomp::MAX_REWIND_TICKS;
use crate::match_state::{GameMode, MatchCfg};

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerBalance {
    pub max_hp: f32,
    pub respawn_ticks: u32,
    /// Spawn protection. Must be >= `MAX_REWIND_TICKS` (a laggy rewound shot must not hit a fresh respawn).
    pub invuln_ticks: u32,
    /// Protection on first join (0 = none; the demo sets 3 s so a new player can find their feet). Otherwise same minimum.
    pub first_spawn_invuln_ticks: u32,
    /// Health regeneration: after `regen_delay_s` seconds without taking damage, heal `regen_per_s` hp per second
    /// up to max (0 = off; the demo turns it on).
    pub regen_delay_s: f32,
    pub regen_per_s: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShurikenBalance {
    pub speed: f32,
    pub damage: f32,
    pub max_bounces: u32,
    pub cooldown: f32,
    pub muzzle: f32,
    pub radius: f32,
    pub max_life: f32,
    pub max_discs: u32,
    pub recoil_pitch_deg: f32,
    pub assist_cone_deg: f32,
    pub assist_strength: f32,
    /// Fraction of the target's lead a locked throw aims ahead by (0 = off, 1 = full).
    pub assist_lead: f32,
    /// Locked throws curve toward the target at up to this many rad/s (0 = straight).
    pub homing_turn: f32,
    /// Speed fraction a disc keeps per bounce.
    pub bounce_retention: f32,
    pub assist_range: f32,
    /// Damage a bounce does to a player-built piece (see `blocks.hp`).
    pub piece_damage: f32,
    /// Star energy pool (charges; 0 = no pool), cost of one star, seconds per recovered charge, charges needed to throw
    /// again after the pool ran dry, and the cost of a charged MARK star (`combat/shuriken.rs`).
    pub energy_max: f32,
    pub energy_cost: f32,
    pub energy_regen_s: f32,
    pub energy_resume: f32,
    pub mark_cost: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KatanaBalance {
    pub damage: f32,
    pub blade_len: f32,
    pub half_width: f32,
    pub half_height: f32,
    /// How strongly the hit sweep bends toward the lock-on target (0 = off, 1 = onto the target), times the lock pull.
    pub magnet: f32,
    /// Damage a swing does to each player-built piece in the blade box.
    pub piece_damage: f32,
    /// Hook-strike (katana while hooked / just after the rope let go) and wall slash (katana in a wall-run): damage
    /// multiplier and blade reach added on top (plus 0.15 m per m/s of speed). See docs/traversal_melee.md.
    pub hook_damage_mult: f32,
    pub hook_reach: f32,
    pub wall_damage_mult: f32,
    pub wall_reach: f32,
    /// Rope slash (swing with the rope taut): damage multiplier for enemies near the rope line, line radius, reach from the eye.
    pub rope_damage_mult: f32,
    pub rope_radius: f32,
    pub rope_reach: f32,
    /// Seconds after a swing within which the next one continues the combo.
    pub combo_window: f32,
    /// Damage multiplier of the third swing of a combo (the first two must have connected); 1 = no finisher bonus.
    pub finisher_mult: f32,
    /// Extra blade length (m) of the finishing swing.
    pub finisher_reach: f32,
    /// Half-angle (degrees) of the target-centred hit cone (0 = box only) and its extra reach (m).
    pub cone_deg: f32,
    pub cone_extra: f32,
    /// Katana moves (docs/melee.md "Katana moves"): heavy cut (hold after a cut, release), air cut, whirl cut, counter cut.
    pub heavy_mult: f32,
    pub heavy_reach: f32,
    pub heavy_recover: f32,
    pub air_mult: f32,
    pub air_half_height: f32,
    pub whirl_radius: f32,
    pub whirl_damage: f32,
    pub whirl_recover: f32,
    pub counter_range: f32,
    pub counter_scale: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CombatBalance {
    pub shuriken: ShurikenBalance,
    pub katana: KatanaBalance,
    /// Horizontal speed (m/s) a hit gives an enemy, before its own `knockback_scale`.
    pub knockback_slash: f32,
    pub knockback_shuriken: f32,
    pub knockback_ultimate: f32,
    /// Shuriken hits at or above this height over the target's capsule centre are headshots (centre +- 0.8).
    pub head_min_height: f32,
    /// Damage multiplier for headshots (1 = none).
    pub headshot_mult: f32,
    pub ultimate_speed: f32,
    pub ultimate_max_radius: f32,
    pub ultimate_damage: f32,
    pub pickup_charge_secs: f32,
    /// Predator lock-on: seconds a target must stay in the cone to LOCK.
    pub lock_secs: f32,
    /// Fraction of the full assist pull while still acquiring (progress 0); 1.0 at LOCK.
    pub lock_pull_min: f32,
    /// Seconds the broken lock lingers (scatter / fade) before it returns to SEARCH.
    pub lock_break_secs: f32,
    /// Seconds the lock's assist takes to ramp in after acquiring (it ramps out over `lock_break_secs`).
    pub lock_blend_secs: f32,
}

/// Player-built pieces (`blocks.rs`). The hook values are inert until the owner decides them.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockBalance {
    pub thickness: f32,
    pub max_per_owner: u32,
    pub range: f32,
    pub cooldown_ticks: u32,
    /// Damage a piece absorbs before it de-rezzes.
    pub hp: f32,
    /// Older = more durable: damage / (1 + this * age seconds). 0 = off.
    pub age_bonus_per_sec: f32,
    /// Size growth per owner kill. 0 = off.
    pub scale_per_kill: f32,
    /// Largest scale factor; 0 = uncapped.
    pub scale_cap: f32,
}

impl Default for BlockBalance {
    fn default() -> Self {
        let c = crate::blocks::BlockCfg::default();
        Self {
            thickness: c.thickness,
            max_per_owner: c.max_per_owner as u32,
            range: c.range,
            cooldown_ticks: c.cooldown_ticks,
            hp: c.hp,
            age_bonus_per_sec: c.age_bonus_per_sec,
            scale_per_kill: c.scale_per_kill,
            scale_cap: c.scale_cap,
        }
    }
}

impl BlockBalance {
    pub fn cfg(&self) -> crate::blocks::BlockCfg {
        crate::blocks::BlockCfg {
            size: crate::blocks::BlockCfg::default().size,
            thickness: self.thickness,
            max_per_owner: self.max_per_owner as usize,
            range: self.range,
            cooldown_ticks: self.cooldown_ticks,
            hp: self.hp,
            age_bonus_per_sec: self.age_bonus_per_sec,
            scale_per_kill: self.scale_per_kill,
            scale_cap: self.scale_cap,
        }
    }
}

/// Client-side look assist (`aim_assist::AimAssist`): console-shooter friction and magnetism on the aim while a
/// target is in the lock cone. All strengths scale with the lock pull (weak while acquiring, full at lock).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AssistBalance {
    /// Look-speed reduction on target (0 = none, 0.5 = half speed at the cone centre).
    pub friction: f32,
    /// Fraction of the target's angular motion the crosshair follows by itself while it is on the target.
    pub magnet: f32,
    /// Pull toward the target centre, fraction of the remaining offset per second.
    pub centre: f32,
    /// Strength multiplier for touch input (thumb aiming is coarser than a mouse).
    pub touch_mult: f32,
}

impl Default for AssistBalance {
    fn default() -> Self {
        Self { friction: 0.2, magnet: 0.9, centre: 4.0, touch_mult: 1.6 }
    }
}

/// Melee action heighteners (`melee.rs`), in seconds / m / m/s. All zero in the default balance (off); the demo turns
/// them on.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct MeleeBalance {
    /// Slash damage bonus per m/s of attacker speed, capped at `momentum_cap` (0.6 = +60 %).
    pub momentum_per_mps: f32,
    pub momentum_cap: f32,
    /// Launch speed a slash gives its victim (0 = no knockback), the upward pop for a grounded victim, the vertical
    /// floor that keeps an already airborne victim up (the juggle).
    pub knock_speed: f32,
    pub knock_pop: f32,
    pub juggle_up: f32,
    /// Window in which a wall stop counts as a splat, the bonus damage of one and the stun it causes (0 = off).
    pub splat_window_s: f32,
    pub splat_damage: f32,
    pub splat_stun_s: f32,
    /// Seconds after being hit in which a fall into the void credits the attacker (0 = no ring-out credit).
    pub ring_window_s: f32,
    /// How long a MARK star's mark lasts and the bonus it gives the marker's next katana hit (0 = marks do nothing).
    pub mark_window_s: f32,
    pub mark_bonus: f32,
    /// Kill-streak window, damage bonus per stacked kill, the stack cap and the hp healed per kill (0 window = off).
    pub streak_window_s: f32,
    pub streak_bonus: f32,
    pub streak_cap: u32,
    pub streak_heal: f32,
    /// Execute: a katana blow on a stunned (parried / splatted) player at or under this fraction of max hp is a kill (0 = off).
    pub exec_frac: f32,
    /// Air cut slam: the downward speed a slashed victim is driven at, and on landing the shock-wave radius (m) and the
    /// outward speed (m/s) it throws bystanders at (0 = no slam).
    pub slam_speed: f32,
    pub shock_radius: f32,
    pub shock_knock: f32,
    /// A cut within this many seconds after landing a parry is a counter cut (0 = off).
    pub counter_window_s: f32,
}

impl MeleeBalance {
    pub fn cfg(&self) -> crate::melee::MeleeCfg {
        let t = |s: f32| (s * crate::match_state::TICK_HZ as f32).round().max(0.0) as u32;
        crate::melee::MeleeCfg {
            momentum_per_mps: self.momentum_per_mps,
            momentum_cap: self.momentum_cap,
            knock_speed: self.knock_speed,
            knock_pop: self.knock_pop,
            juggle_up: self.juggle_up,
            splat_ticks: t(self.splat_window_s),
            splat_damage: self.splat_damage,
            splat_stagger_ticks: t(self.splat_stun_s),
            ring_ticks: t(self.ring_window_s),
            mark_ticks: t(self.mark_window_s),
            mark_bonus: self.mark_bonus,
            streak_ticks: t(self.streak_window_s),
            streak_bonus: self.streak_bonus,
            streak_cap: self.streak_cap,
            streak_heal: self.streak_heal,
            exec_frac: self.exec_frac,
            slam_speed: self.slam_speed,
            shock_radius: self.shock_radius,
            shock_knock: self.shock_knock,
            counter_ticks: t(self.counter_window_s),
        }
    }
}

/// Block / parry and the strike-timing window (`block.rs`), in seconds. The default keeps the shield available
/// but adds no strike bonus (so unit tests keep their timing); `Balance::demo` turns the windows on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GuardBalance {
    /// Half-angle (degrees) of the protected frontal arc.
    pub front_deg: f32,
    /// Damage taken from the front while blocking (0 = immune, 1 = no block).
    pub front_mult: f32,
    /// A block raised within this many seconds before a hit lands is a parry.
    pub parry_s: f32,
    /// Seconds the guard lasts when held continuously.
    pub max_hold_s: f32,
    /// Seconds to refill a spent guard.
    pub regen_s: f32,
    /// Guard fraction a blocked hit costs.
    pub hit_cost: f32,
    /// After the guard is spent: seconds before it can be raised again.
    pub rearm_s: f32,
    /// Seconds a parried melee attacker cannot act.
    pub stagger_s: f32,
    /// Strike window (seconds) after one's own swing.
    pub swing_open_s: f32,
    /// Counter window (seconds) after being parried.
    pub counter_open_s: f32,
    /// Damage multiplier for a hit inside a strike window (1 = off).
    pub bonus_mult: f32,
    /// A guard broken by a hit staggers its owner this long; for `mercy_s` after any break damage taken is scaled by
    /// `mercy_mult` (a broken shield is not a free combo for the attacker).
    pub break_stagger_s: f32,
    pub mercy_s: f32,
    pub mercy_mult: f32,
}

impl Default for GuardBalance {
    fn default() -> Self {
        Self {
            front_deg: 70.0,
            front_mult: 0.15,
            parry_s: 0.25,
            max_hold_s: 2.5,
            regen_s: 3.0,
            hit_cost: 0.2,
            rearm_s: 1.0,
            stagger_s: 0.8,
            swing_open_s: 0.0,
            counter_open_s: 0.0,
            bonus_mult: 1.0,
            break_stagger_s: 0.0,
            mercy_s: 0.0,
            mercy_mult: 1.0,
        }
    }
}

impl GuardBalance {
    pub fn cfg(&self) -> crate::block::BlockCfg {
        let t = |s: f32| (s * crate::match_state::TICK_HZ as f32).round().max(0.0) as u32;
        crate::block::BlockCfg {
            front_rad: self.front_deg.to_radians(),
            front_mult: self.front_mult,
            parry_ticks: t(self.parry_s),
            max_hold_ticks: t(self.max_hold_s).max(1),
            regen_ticks: t(self.regen_s).max(1),
            hit_cost: self.hit_cost,
            rearm_ticks: t(self.rearm_s),
            stagger_ticks: t(self.stagger_s),
            swing_open_ticks: t(self.swing_open_s),
            counter_open_ticks: t(self.counter_open_s),
            bonus_mult: self.bonus_mult,
            break_stagger_ticks: t(self.break_stagger_s),
            mercy_ticks: t(self.mercy_s),
            mercy_mult: self.mercy_mult,
        }
    }
}

/// Client-side feedback timings. The sim never pauses: these only tell the shell how long to freeze the
/// viewmodel / shake the camera after a confirmed hit.
#[derive(Clone, Debug, PartialEq)]
pub struct FeedbackBalance {
    /// Hit-stop after a katana hit, milliseconds.
    pub hitstop_slash_ms: f32,
    /// Hit-stop after a katana kill.
    pub hitstop_kill_ms: f32,
}

impl Default for FeedbackBalance {
    fn default() -> Self {
        Self { hitstop_slash_ms: 25.0, hitstop_kill_ms: 40.0 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HazardBalance {
    pub obstacle_damage: f32,
    pub wall_damage: f32,
    /// Per (victim, source): one pass through a wall is one hit.
    pub rearm_ticks: u32,
    /// Nobody is hurt by their own light-wall (it still blocks them).
    pub owner_immune: bool,
    /// Environmental damage switches (both OFF: the game is a shooter, Tron is a visual style).
    pub walls_enabled: bool,
    pub obstacles_enabled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchBalance {
    pub warmup_ticks: u32,
    pub time_limit_ticks: u32,
    pub intermission_ticks: u32,
    pub frag_limit: u32,
    pub min_players: u32,
    /// 0 = deathmatch, 1 = co-op waves, 2 = training dummies.
    pub mode: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnemyStats {
    pub hp: f32,
    pub speed: f32,
    /// Notices a player with line of sight inside this radius.
    pub aggro_radius: f32,
    /// Gives up the chase beyond this (or after losing sight for `enemy.lose_target_ticks`).
    pub leash_radius: f32,
    /// Melee reach, or the throwing range for a thrower.
    pub attack_range: f32,
    /// Telegraph: the hit lands exactly this many ticks after the windup event. Must be >= 1.
    pub windup_ticks: u32,
    pub recover_ticks: u32,
    pub cooldown_ticks: u32,
    pub damage: f32,
    /// 1 = full knockback, 0 = immovable.
    pub knockback_scale: f32,
    pub stagger_ticks: u32,
    /// Windup and movement are not interrupted by hits.
    pub super_armor: bool,
    /// Chance (per mille) to drop a power-up on death.
    pub drop_per_mille: u32,
    /// Throwers back off inside `pref_min` and close in beyond `pref_max`.
    pub pref_min: f32,
    pub pref_max: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnemyBalance {
    pub grunt: EnemyStats,
    pub thrower: EnemyStats,
    pub brute: EnemyStats,
    pub dummy: EnemyStats,
    pub patrol_radius: f32,
    pub idle_min_ticks: u32,
    pub idle_max_ticks: u32,
    pub lose_target_ticks: u32,
    /// Hard cap on live enemies (the rest of a wave queues).
    pub max_alive: u32,
    pub drop_ttl_ticks: u32,
    pub dummy_respawn_ticks: u32,
    /// Speed (m/s) of thrower discs; slow enough to dodge.
    pub disc_speed: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WaveBalance {
    pub first_wave_delay_ticks: u32,
    pub inter_wave_ticks: u32,
    pub first_wave_grunts: u32,
    pub grunts_per_wave: u32,
    pub thrower_from_wave: u32,
    pub thrower_every: u32,
    pub brute_from_wave: u32,
    pub brute_every: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Balance {
    pub player: PlayerBalance,
    pub combat: CombatBalance,
    pub hazards: HazardBalance,
    pub blocks: BlockBalance,
    pub feedback: FeedbackBalance,
    pub assist: AssistBalance,
    pub guard: GuardBalance,
    pub melee: MeleeBalance,
    pub matches: MatchBalance,
    pub enemy: EnemyBalance,
    pub wave: WaveBalance,
}

impl EnemyBalance {
    pub fn stats(&self, kind: EnemyKind) -> &EnemyStats {
        match kind {
            EnemyKind::Grunt => &self.grunt,
            EnemyKind::Thrower => &self.thrower,
            EnemyKind::Brute => &self.brute,
            EnemyKind::Dummy => &self.dummy,
        }
    }
}

impl Default for Balance {
    fn default() -> Self {
        let m = MatchCfg::default();
        let s = ShurikenCfg::default();
        Self {
            player: PlayerBalance {
                max_hp: crate::pvp::MAX_HP,
                respawn_ticks: crate::pvp::RESPAWN_TICKS,
                invuln_ticks: crate::pvp::INVULN_TICKS,
                first_spawn_invuln_ticks: 0,
                regen_delay_s: 4.0,
                regen_per_s: 0.0,
            },
            combat: CombatBalance {
                shuriken: ShurikenBalance {
                    speed: s.speed,
                    damage: s.damage,
                    max_bounces: s.max_bounces as u32,
                    cooldown: s.cooldown,
                    muzzle: s.muzzle,
                    radius: s.radius,
                    max_life: s.max_life,
                    max_discs: s.max_discs as u32,
                    recoil_pitch_deg: s.recoil_pitch_deg,
                    assist_cone_deg: s.assist_cone_deg,
                    assist_strength: s.assist_strength,
                    assist_lead: s.assist_lead,
                    homing_turn: s.homing_turn,
                    bounce_retention: s.bounce_retention,
                    assist_range: s.assist_range,
                    piece_damage: s.piece_damage,
                    energy_max: s.energy_max,
                    energy_cost: s.energy_cost,
                    energy_regen_s: s.energy_regen_s,
                    energy_resume: s.energy_resume,
                    mark_cost: s.mark_cost,
                },
                katana: KatanaBalance { damage: 120.0, blade_len: 1.6, half_width: 1.0, half_height: 1.0, magnet: 0.8, piece_damage: 60.0, hook_damage_mult: 1.5, hook_reach: 4.0, wall_damage_mult: 1.35, wall_reach: 3.0, rope_damage_mult: 1.6, rope_radius: 1.5, rope_reach: 25.0, combo_window: 1.0, finisher_mult: 1.0, finisher_reach: 0.0, cone_deg: 0.0, cone_extra: 0.0, heavy_mult: 1.8, heavy_reach: 1.2, heavy_recover: 0.9, air_mult: 1.25, air_half_height: 2.2, whirl_radius: 3.0, whirl_damage: 12.0, whirl_recover: 0.6, counter_range: 7.0, counter_scale: 0.5 },
                knockback_slash: 9.0,
                knockback_shuriken: 3.0,
                knockback_ultimate: 12.0,
                head_min_height: 0.5,
                headshot_mult: 1.0,
                ultimate_speed: 60.0,
                ultimate_max_radius: 120.0,
                ultimate_damage: 9999.0,
                pickup_charge_secs: 20.0,
                lock_secs: 0.15,
                lock_pull_min: 0.6,
                lock_break_secs: 0.2,
                lock_blend_secs: 0.06,
            },
            blocks: BlockBalance::default(),
            feedback: FeedbackBalance::default(),
            assist: AssistBalance::default(),
            guard: GuardBalance::default(),
            melee: MeleeBalance::default(),
            hazards: HazardBalance { obstacle_damage: 100.0, wall_damage: 100.0, rearm_ticks: 30, owner_immune: false, walls_enabled: false, obstacles_enabled: false },
            matches: MatchBalance {
                warmup_ticks: m.warmup_ticks,
                time_limit_ticks: m.time_limit_ticks,
                intermission_ticks: m.intermission_ticks,
                frag_limit: m.frag_limit as u32,
                min_players: m.min_players as u32,
                mode: 0,
            },
            enemy: EnemyBalance {
                grunt: EnemyStats {
                    hp: 60.0,
                    speed: 4.5,
                    aggro_radius: 12.0,
                    leash_radius: 20.0,
                    attack_range: 1.6,
                    windup_ticks: 18,
                    recover_ticks: 20,
                    cooldown_ticks: 45,
                    damage: 15.0,
                    knockback_scale: 1.0,
                    stagger_ticks: 18,
                    super_armor: false,
                    drop_per_mille: 150,
                    pref_min: 0.0,
                    pref_max: 0.0,
                },
                thrower: EnemyStats {
                    hp: 40.0,
                    speed: 3.0,
                    aggro_radius: 16.0,
                    leash_radius: 24.0,
                    attack_range: 12.0,
                    windup_ticks: 24,
                    recover_ticks: 30,
                    cooldown_ticks: 90,
                    damage: 12.0,
                    knockback_scale: 1.0,
                    stagger_ticks: 18,
                    super_armor: false,
                    drop_per_mille: 250,
                    pref_min: 6.0,
                    pref_max: 10.0,
                },
                brute: EnemyStats {
                    hp: 220.0,
                    speed: 2.5,
                    aggro_radius: 10.0,
                    leash_radius: 16.0,
                    attack_range: 2.2,
                    windup_ticks: 36,
                    recover_ticks: 40,
                    cooldown_ticks: 90,
                    damage: 35.0,
                    knockback_scale: 0.3,
                    stagger_ticks: 0,
                    super_armor: true,
                    drop_per_mille: 600,
                    pref_min: 0.0,
                    pref_max: 0.0,
                },
                dummy: EnemyStats {
                    hp: 200.0,
                    speed: 0.0,
                    aggro_radius: 0.0,
                    leash_radius: 0.0,
                    attack_range: 0.0,
                    windup_ticks: 1,
                    recover_ticks: 0,
                    cooldown_ticks: 0,
                    damage: 0.0,
                    knockback_scale: 0.0,
                    stagger_ticks: 0,
                    super_armor: true,
                    drop_per_mille: 0,
                    pref_min: 0.0,
                    pref_max: 0.0,
                },
                patrol_radius: 6.0,
                idle_min_ticks: 60,
                idle_max_ticks: 180,
                lose_target_ticks: 90,
                max_alive: 24,
                drop_ttl_ticks: 900,
                dummy_respawn_ticks: 120,
                disc_speed: 16.0,
            },
            wave: WaveBalance {
                first_wave_delay_ticks: 180,
                inter_wave_ticks: 300,
                first_wave_grunts: 3,
                grunts_per_wave: 2,
                thrower_from_wave: 2,
                thrower_every: 2,
                brute_from_wave: 3,
                brute_every: 3,
            },
        }
    }
}

impl Balance {
    /// Demo / small-lobby (1v1 to 3v3, bots welcome) feel. Not the default so unit tests keep their timing;
    /// the server applies it with `server.set_balance(Balance::demo())`.
    ///
    /// * MELEE-FIRST. The katana is the weapon: 20 per cut, swings 0.42 s apart (`katana::SWING_CD_FRAC`), the third
    ///   connected cut of a combo (1 s window) is a 30 hp finisher with +0.6 m reach, so 100 hp falls to five cuts
    ///   (20, 20, 30, 20, 20) in ~1.7 s; the first cut carries the dash lunge (~3.3 m), so strike reach from a
    ///   standing start is ~5.7 m, and the lock triangle bends the whole sweep onto the target;
    /// * shuriken is the poke: 70 m/s (a flat, fast star), 7 damage at 5/s = 35 dps, ~2.8 s of clean fire for a kill
    ///   (headshots 1.5x, ~1.9 s); 0.6 m hit radius (generous graze);
    /// * katana blade 2.4 m long, 1.2 m half-width (a rough swing still connects);
    /// * light-walls 34 per touch (3 to kill), obstacles 60 (2 to kill), 1 s re-arm so one pass is one hit, and
    ///   your own trail never hurts you (it still blocks you);
    /// * hp 100, respawn 2.5 s, 1.5 s spawn protection (3 s on first join);
    /// * the match starts 3 s after the first player (min_players 1, so one human plus bots works), first to
    ///   10 frags or 6 minutes, 8 s intermission (docs/DEMO.md).
    pub fn demo() -> Self {
        let mut b = Balance::default();
        // MELEE-FIRST: stars are a 7 hp poke, the katana is the weapon. 100 hp = 15 star hits (~2.8 s of clean fire) but
        // only 5 slashes: 20, 20, finisher 30, then 20, 20 at one swing per 0.42 s = ~1.7 s of cutting. Closing the
        // distance (dash lunge, grapple, wall-run) is what wins fights, and a missed combo resets.
        b.combat.katana.damage = 20.0;
        b.combat.katana.finisher_mult = 1.5; // the third connected swing of a combo cuts for 30
        b.combat.katana.finisher_reach = 0.6; // and reaches 0.6 m further
        b.combat.katana.combo_window = 1.0;
        // forgiving blade: any enemy within 38 degrees of the swing and 1.0 m past the blade's reach is hit (target-centred)
        b.combat.katana.cone_deg = 38.0;
        b.combat.katana.cone_extra = 1.0;
        b.combat.katana.magnet = 1.0; // a locked target pulls the whole blade sweep onto it
        b.player.max_hp = 100.0;
        // block / parry: a held shield cuts frontal hits to 15 %, a block raised just before the hit parries it (negated,
        // a melee attacker staggers 0.8 s); the strike window (0.25 s after your own swing, 1 s after being parried)
        // makes hits on you worth 1.5x, which the lock triangle shows
        // melee heighteners: +4 % slash damage per m/s of your speed (cap +60 %), a slash launches its victim 8 m/s with a pop
        // (an airborne victim is kept up for the next cut), a launched victim stopped by a wall takes 15 and is stunned
        // 0.6 s, a fall into the void within 3 s of a hit is the attacker's kill, katana kills inside 8 s stack +8 % damage
        // (cap 4) and heal 20 hp
        b.melee = MeleeBalance {
            momentum_per_mps: 0.04,
            momentum_cap: 0.6,
            knock_speed: 8.0,
            knock_pop: 4.0,
            juggle_up: 5.0,
            splat_window_s: 0.5,
            splat_damage: 15.0,
            splat_stun_s: 0.6,
            ring_window_s: 3.0,
            mark_window_s: 4.0,
            mark_bonus: 0.4,
            streak_window_s: 8.0,
            streak_bonus: 0.08,
            streak_cap: 4,
            streak_heal: 20.0,
            exec_frac: 0.25,
            slam_speed: 14.0,
            shock_radius: 3.5,
            shock_knock: 7.0,
            counter_window_s: 0.25,
        };
        b.guard.swing_open_s = 0.25;
        b.guard.counter_open_s = 1.0;
        b.guard.bonus_mult = 1.5;
        // a held shield must feel SAFE (it used to break in 1.5 s and leave you dead 1.5 s later): an 8 s guard, a blocked
        // cut costs 6 % of it, a wide 200 degree arc, 3 % leak from the base cut (momentum / lunge bonuses never punch
        // through), a forgiving 0.35 s parry, quick re-arm, and a broken guard staggers 0.3 s then takes only 35 % damage
        // for 1.2 s so it never hands over a combo
        b.guard.front_deg = 100.0;
        b.guard.front_mult = 0.03;
        b.guard.parry_s = 0.35;
        b.guard.max_hold_s = 8.0;
        b.guard.regen_s = 2.0;
        b.guard.hit_cost = 0.06;
        b.guard.rearm_s = 0.5;
        b.guard.break_stagger_s = 0.3;
        b.guard.mercy_s = 1.2;
        b.guard.mercy_mult = 0.35;
        b.player.respawn_ticks = 150; // 2.5 s: the death animation fills ~1.6 s of it, then the countdown
        b.player.first_spawn_invuln_ticks = 3 * crate::match_state::TICK_HZ; // joining mid-fight must not be a death sentence
        b.player.regen_per_s = 20.0; // 4 s out of the fight and you are whole again: attrition never ends a run
        // shooter feel: fast flat-flying stars (no drop), 5 shots/s, head height pays 1.5x
        b.combat.shuriken.speed = 70.0;
        b.combat.shuriken.damage = 7.0; // a poke: ~2.8 s of clean fire for a kill (headshots 10.5), so it softens a target for the blade
        b.combat.shuriken.cooldown = 0.20;
        // STAR ENERGY: 6 charges, one per star, a charge back every 0.45 s; running dry locks throwing until 2 charges are back.
        // A volley is 6 x 7 = 42 hp at best, so a star-only kill takes more than two volleys and the stars are a poke / zoning
        // tool (the katana is the kill). Hold the throw ~0.4 s for a MARK star: 2 charges, no damage, +40 % on your next cut.
        b.combat.shuriken.energy_max = 6.0;
        b.combat.shuriken.energy_cost = 1.0;
        b.combat.shuriken.energy_regen_s = 0.45;
        b.combat.shuriken.energy_resume = 2.0;
        b.combat.shuriken.mark_cost = 2.0;
        b.combat.shuriken.radius = 0.6; // a star that grazes the body counts: hit reach = hitbox 0.3 + 0.6
        b.combat.headshot_mult = 1.5;
        // katana: a longer, wider blade (the lunge reach itself is derived from the dash) so a rough swing still connects
        b.combat.katana.blade_len = 2.4; // + the dash lunge (~3.3 m) = ~5.7 m of strike reach from a standing start
        b.combat.katana.half_width = 1.2;
        // hazards must not feel like unfair deaths: a light-wall is a 3-touch kill (34 each), an obstacle needs
        // two hits (60), contact re-arms only after 1 s (one pass = one hit), your own trail never hurts you
        b.hazards.wall_damage = 34.0;
        b.hazards.obstacle_damage = 60.0;
        b.hazards.rearm_ticks = 60;
        b.hazards.owner_immune = true;
        b.matches.warmup_ticks = 3 * crate::match_state::TICK_HZ;
        b.matches.min_players = 1;
        b.matches.frag_limit = 10;
        b.matches.time_limit_ticks = 6 * 60 * crate::match_state::TICK_HZ;
        b.matches.intermission_ticks = 8 * crate::match_state::TICK_HZ;
        b
    }
}

// ---- derived configs ----------------------------------------------------------------------------------

impl Balance {
    pub fn shuriken_cfg(&self) -> ShurikenCfg {
        let b = &self.combat.shuriken;
        ShurikenCfg {
            speed: b.speed,
            damage: b.damage,
            max_bounces: b.max_bounces.min(u8::MAX as u32) as u8,
            cooldown: b.cooldown,
            muzzle: b.muzzle,
            radius: b.radius,
            max_life: b.max_life,
            max_discs: b.max_discs as usize,
            recoil_pitch_deg: b.recoil_pitch_deg,
            assist_cone_deg: b.assist_cone_deg,
            assist_strength: b.assist_strength,
            assist_lead: b.assist_lead,
            homing_turn: b.homing_turn,
            bounce_retention: b.bounce_retention,
            assist_range: b.assist_range,
            piece_damage: b.piece_damage,
            energy_max: b.energy_max,
            energy_cost: b.energy_cost,
            energy_regen_s: b.energy_regen_s,
            energy_resume: b.energy_resume,
            mark_cost: b.mark_cost,
            ..ShurikenCfg::default()
        }
    }

    /// Overlay the tunables on a character-derived katana config (cooldown and lunge distance stay derived).
    pub fn apply_katana(&self, k: &mut KatanaCfg) {
        let b = &self.combat.katana;
        k.damage = b.damage;
        k.blade_len = b.blade_len;
        k.half_width = b.half_width;
        k.half_height = b.half_height;
        k.magnet = b.magnet;
        k.piece_damage = b.piece_damage;
        k.hook_damage_mult = b.hook_damage_mult;
        k.hook_reach = b.hook_reach;
        k.wall_damage_mult = b.wall_damage_mult;
        k.wall_reach = b.wall_reach;
        k.rope_damage_mult = b.rope_damage_mult;
        k.rope_radius = b.rope_radius;
        k.rope_reach = b.rope_reach;
        k.combo_window = b.combo_window;
        k.finisher_mult = b.finisher_mult;
        k.finisher_reach = b.finisher_reach;
        k.cone_deg = b.cone_deg;
        k.cone_extra = b.cone_extra;
        k.heavy_mult = b.heavy_mult;
        k.heavy_reach = b.heavy_reach;
        k.heavy_recover = b.heavy_recover;
        k.air_mult = b.air_mult;
        k.air_half_height = b.air_half_height;
        k.whirl_radius = b.whirl_radius;
        k.whirl_damage = b.whirl_damage;
        k.whirl_recover = b.whirl_recover;
        k.counter_range = b.counter_range;
        k.counter_scale = b.counter_scale;
    }

    pub fn match_cfg(&self) -> MatchCfg {
        let m = &self.matches;
        MatchCfg {
            warmup_ticks: m.warmup_ticks,
            time_limit_ticks: m.time_limit_ticks,
            intermission_ticks: m.intermission_ticks,
            frag_limit: m.frag_limit.min(u16::MAX as u32) as u16,
            min_players: m.min_players as usize,
            mode: match m.mode {
                1 => GameMode::CoopWaves,
                2 => GameMode::Training,
                _ => GameMode::Deathmatch,
            },
        }
    }
}

// ---- text format --------------------------------------------------------------------------------------

enum Slot<'a> {
    F(&'a mut f32),
    U(&'a mut u32),
    B(&'a mut bool),
}

fn enemy_slots<'a>(name: &str, e: &'a mut EnemyStats) -> Vec<(String, Slot<'a>)> {
    let k = |f: &str| format!("enemy.{name}.{f}");
    vec![
        (k("hp"), Slot::F(&mut e.hp)),
        (k("speed"), Slot::F(&mut e.speed)),
        (k("aggro_radius"), Slot::F(&mut e.aggro_radius)),
        (k("leash_radius"), Slot::F(&mut e.leash_radius)),
        (k("attack_range"), Slot::F(&mut e.attack_range)),
        (k("windup_ticks"), Slot::U(&mut e.windup_ticks)),
        (k("recover_ticks"), Slot::U(&mut e.recover_ticks)),
        (k("cooldown_ticks"), Slot::U(&mut e.cooldown_ticks)),
        (k("damage"), Slot::F(&mut e.damage)),
        (k("knockback_scale"), Slot::F(&mut e.knockback_scale)),
        (k("stagger_ticks"), Slot::U(&mut e.stagger_ticks)),
        (k("super_armor"), Slot::B(&mut e.super_armor)),
        (k("drop_per_mille"), Slot::U(&mut e.drop_per_mille)),
        (k("pref_min"), Slot::F(&mut e.pref_min)),
        (k("pref_max"), Slot::F(&mut e.pref_max)),
    ]
}

impl Balance {
    /// The single table behind loading, dumping and (by construction) the documentation of every key.
    fn slots(&mut self) -> Vec<(String, Slot<'_>)> {
        let mut v: Vec<(String, Slot<'_>)> = Vec::new();
        macro_rules! s {
            ($name:literal, $kind:ident, $place:expr) => {
                v.push(($name.to_string(), Slot::$kind(&mut $place)))
            };
        }
        s!("player.max_hp", F, self.player.max_hp);
        s!("player.respawn_ticks", U, self.player.respawn_ticks);
        s!("player.invuln_ticks", U, self.player.invuln_ticks);
        s!("player.first_spawn_invuln_ticks", U, self.player.first_spawn_invuln_ticks);
        s!("player.regen_delay_s", F, self.player.regen_delay_s);
        s!("player.regen_per_s", F, self.player.regen_per_s);

        s!("combat.shuriken.speed", F, self.combat.shuriken.speed);
        s!("combat.shuriken.damage", F, self.combat.shuriken.damage);
        s!("combat.shuriken.max_bounces", U, self.combat.shuriken.max_bounces);
        s!("combat.shuriken.cooldown", F, self.combat.shuriken.cooldown);
        s!("combat.shuriken.muzzle", F, self.combat.shuriken.muzzle);
        s!("combat.shuriken.radius", F, self.combat.shuriken.radius);
        s!("combat.shuriken.max_life", F, self.combat.shuriken.max_life);
        s!("combat.shuriken.max_discs", U, self.combat.shuriken.max_discs);
        s!("combat.shuriken.recoil_pitch_deg", F, self.combat.shuriken.recoil_pitch_deg);
        s!("combat.shuriken.assist_cone_deg", F, self.combat.shuriken.assist_cone_deg);
        s!("combat.shuriken.assist_strength", F, self.combat.shuriken.assist_strength);
        s!("combat.shuriken.assist_lead", F, self.combat.shuriken.assist_lead);
        s!("combat.shuriken.homing_turn", F, self.combat.shuriken.homing_turn);
        s!("combat.shuriken.bounce_retention", F, self.combat.shuriken.bounce_retention);
        s!("combat.shuriken.assist_range", F, self.combat.shuriken.assist_range);
        s!("combat.katana.damage", F, self.combat.katana.damage);
        s!("combat.katana.blade_len", F, self.combat.katana.blade_len);
        s!("combat.katana.half_width", F, self.combat.katana.half_width);
        s!("combat.katana.half_height", F, self.combat.katana.half_height);
        s!("combat.katana.magnet", F, self.combat.katana.magnet);
        s!("combat.katana.piece_damage", F, self.combat.katana.piece_damage);
        s!("combat.katana.hook_damage_mult", F, self.combat.katana.hook_damage_mult);
        s!("combat.katana.hook_reach", F, self.combat.katana.hook_reach);
        s!("combat.katana.rope_damage_mult", F, self.combat.katana.rope_damage_mult);
        s!("combat.katana.rope_radius", F, self.combat.katana.rope_radius);
        s!("combat.katana.rope_reach", F, self.combat.katana.rope_reach);
        s!("combat.katana.wall_damage_mult", F, self.combat.katana.wall_damage_mult);
        s!("combat.katana.wall_reach", F, self.combat.katana.wall_reach);
        s!("combat.katana.combo_window", F, self.combat.katana.combo_window);
        s!("combat.katana.finisher_mult", F, self.combat.katana.finisher_mult);
        s!("combat.katana.finisher_reach", F, self.combat.katana.finisher_reach);
        s!("combat.katana.cone_deg", F, self.combat.katana.cone_deg);
        s!("combat.katana.cone_extra", F, self.combat.katana.cone_extra);
        s!("combat.katana.heavy_mult", F, self.combat.katana.heavy_mult);
        s!("combat.katana.heavy_reach", F, self.combat.katana.heavy_reach);
        s!("combat.katana.heavy_recover", F, self.combat.katana.heavy_recover);
        s!("combat.katana.air_mult", F, self.combat.katana.air_mult);
        s!("combat.katana.air_half_height", F, self.combat.katana.air_half_height);
        s!("combat.katana.whirl_radius", F, self.combat.katana.whirl_radius);
        s!("combat.katana.whirl_damage", F, self.combat.katana.whirl_damage);
        s!("combat.katana.whirl_recover", F, self.combat.katana.whirl_recover);
        s!("combat.katana.counter_range", F, self.combat.katana.counter_range);
        s!("combat.katana.counter_scale", F, self.combat.katana.counter_scale);
        s!("combat.shuriken.piece_damage", F, self.combat.shuriken.piece_damage);
        s!("combat.shuriken.energy_max", F, self.combat.shuriken.energy_max);
        s!("combat.shuriken.energy_cost", F, self.combat.shuriken.energy_cost);
        s!("combat.shuriken.energy_regen_s", F, self.combat.shuriken.energy_regen_s);
        s!("combat.shuriken.energy_resume", F, self.combat.shuriken.energy_resume);
        s!("combat.shuriken.mark_cost", F, self.combat.shuriken.mark_cost);
        s!("combat.lock_secs", F, self.combat.lock_secs);
        s!("combat.lock_pull_min", F, self.combat.lock_pull_min);
        s!("combat.lock_break_secs", F, self.combat.lock_break_secs);
        s!("combat.lock_blend_secs", F, self.combat.lock_blend_secs);
        s!("feedback.hitstop_slash_ms", F, self.feedback.hitstop_slash_ms);
        s!("feedback.hitstop_kill_ms", F, self.feedback.hitstop_kill_ms);
        s!("assist.friction", F, self.assist.friction);
        s!("assist.magnet", F, self.assist.magnet);
        s!("assist.centre", F, self.assist.centre);
        s!("assist.touch_mult", F, self.assist.touch_mult);
        s!("melee.momentum_per_mps", F, self.melee.momentum_per_mps);
        s!("melee.momentum_cap", F, self.melee.momentum_cap);
        s!("melee.knock_speed", F, self.melee.knock_speed);
        s!("melee.knock_pop", F, self.melee.knock_pop);
        s!("melee.juggle_up", F, self.melee.juggle_up);
        s!("melee.splat_window_s", F, self.melee.splat_window_s);
        s!("melee.splat_damage", F, self.melee.splat_damage);
        s!("melee.splat_stun_s", F, self.melee.splat_stun_s);
        s!("melee.ring_window_s", F, self.melee.ring_window_s);
        s!("melee.mark_window_s", F, self.melee.mark_window_s);
        s!("melee.mark_bonus", F, self.melee.mark_bonus);
        s!("melee.streak_window_s", F, self.melee.streak_window_s);
        s!("melee.streak_bonus", F, self.melee.streak_bonus);
        s!("melee.streak_cap", U, self.melee.streak_cap);
        s!("melee.streak_heal", F, self.melee.streak_heal);
        s!("melee.exec_frac", F, self.melee.exec_frac);
        s!("melee.slam_speed", F, self.melee.slam_speed);
        s!("melee.shock_radius", F, self.melee.shock_radius);
        s!("melee.shock_knock", F, self.melee.shock_knock);
        s!("melee.counter_window_s", F, self.melee.counter_window_s);
        s!("guard.front_deg", F, self.guard.front_deg);
        s!("guard.front_mult", F, self.guard.front_mult);
        s!("guard.parry_s", F, self.guard.parry_s);
        s!("guard.max_hold_s", F, self.guard.max_hold_s);
        s!("guard.regen_s", F, self.guard.regen_s);
        s!("guard.hit_cost", F, self.guard.hit_cost);
        s!("guard.rearm_s", F, self.guard.rearm_s);
        s!("guard.stagger_s", F, self.guard.stagger_s);
        s!("guard.swing_open_s", F, self.guard.swing_open_s);
        s!("guard.counter_open_s", F, self.guard.counter_open_s);
        s!("guard.bonus_mult", F, self.guard.bonus_mult);
        s!("guard.break_stagger_s", F, self.guard.break_stagger_s);
        s!("guard.mercy_s", F, self.guard.mercy_s);
        s!("guard.mercy_mult", F, self.guard.mercy_mult);
        s!("combat.knockback_slash", F, self.combat.knockback_slash);
        s!("combat.knockback_shuriken", F, self.combat.knockback_shuriken);
        s!("combat.knockback_ultimate", F, self.combat.knockback_ultimate);
        s!("combat.head_min_height", F, self.combat.head_min_height);
        s!("combat.headshot_mult", F, self.combat.headshot_mult);
        s!("combat.ultimate_speed", F, self.combat.ultimate_speed);
        s!("combat.ultimate_max_radius", F, self.combat.ultimate_max_radius);
        s!("combat.ultimate_damage", F, self.combat.ultimate_damage);
        s!("combat.pickup_charge_secs", F, self.combat.pickup_charge_secs);

        s!("blocks.thickness", F, self.blocks.thickness);
        s!("blocks.max_per_owner", U, self.blocks.max_per_owner);
        s!("blocks.range", F, self.blocks.range);
        s!("blocks.cooldown_ticks", U, self.blocks.cooldown_ticks);
        s!("blocks.hp", F, self.blocks.hp);
        s!("blocks.age_bonus_per_sec", F, self.blocks.age_bonus_per_sec);
        s!("blocks.scale_per_kill", F, self.blocks.scale_per_kill);
        s!("blocks.scale_cap", F, self.blocks.scale_cap);

        s!("hazards.obstacle_damage", F, self.hazards.obstacle_damage);
        s!("hazards.wall_damage", F, self.hazards.wall_damage);
        s!("hazards.rearm_ticks", U, self.hazards.rearm_ticks);
        s!("hazards.owner_immune", B, self.hazards.owner_immune);
        s!("hazards.walls_enabled", B, self.hazards.walls_enabled);
        s!("hazards.obstacles_enabled", B, self.hazards.obstacles_enabled);

        s!("match.warmup_ticks", U, self.matches.warmup_ticks);
        s!("match.time_limit_ticks", U, self.matches.time_limit_ticks);
        s!("match.intermission_ticks", U, self.matches.intermission_ticks);
        s!("match.frag_limit", U, self.matches.frag_limit);
        s!("match.min_players", U, self.matches.min_players);
        s!("match.mode", U, self.matches.mode);

        s!("enemy.patrol_radius", F, self.enemy.patrol_radius);
        s!("enemy.idle_min_ticks", U, self.enemy.idle_min_ticks);
        s!("enemy.idle_max_ticks", U, self.enemy.idle_max_ticks);
        s!("enemy.lose_target_ticks", U, self.enemy.lose_target_ticks);
        s!("enemy.max_alive", U, self.enemy.max_alive);
        s!("enemy.drop_ttl_ticks", U, self.enemy.drop_ttl_ticks);
        s!("enemy.dummy_respawn_ticks", U, self.enemy.dummy_respawn_ticks);
        s!("enemy.disc_speed", F, self.enemy.disc_speed);

        s!("wave.first_wave_delay_ticks", U, self.wave.first_wave_delay_ticks);
        s!("wave.inter_wave_ticks", U, self.wave.inter_wave_ticks);
        s!("wave.first_wave_grunts", U, self.wave.first_wave_grunts);
        s!("wave.grunts_per_wave", U, self.wave.grunts_per_wave);
        s!("wave.thrower_from_wave", U, self.wave.thrower_from_wave);
        s!("wave.thrower_every", U, self.wave.thrower_every);
        s!("wave.brute_from_wave", U, self.wave.brute_from_wave);
        s!("wave.brute_every", U, self.wave.brute_every);

        v.extend(enemy_slots("grunt", &mut self.enemy.grunt));
        v.extend(enemy_slots("thrower", &mut self.enemy.thrower));
        v.extend(enemy_slots("brute", &mut self.enemy.brute));
        v.extend(enemy_slots("dummy", &mut self.enemy.dummy));
        v
    }

    /// All keys, for tooling / docs.
    pub fn keys() -> Vec<String> {
        Balance::default().slots().into_iter().map(|(k, _)| k).collect()
    }

    /// Overlay `text` on the defaults. Any error rejects the whole file.
    pub fn from_text(text: &str) -> Result<Balance, Vec<String>> {
        let mut b = Balance::default();
        b.apply_text(text)?;
        Ok(b)
    }

    /// Overlay `text` on `self`; `self` is untouched if anything is wrong. Errors carry 1-based line numbers.
    pub fn apply_text(&mut self, text: &str) -> Result<(), Vec<String>> {
        let mut next = self.clone();
        let mut errors = Vec::new();
        {
            let mut slots = next.slots();
            for (n, raw) in text.lines().enumerate() {
                let line = raw.split('#').next().unwrap_or("").trim();
                if line.is_empty() {
                    continue;
                }
                let Some((key, val)) = line.split_once('=') else {
                    errors.push(format!("line {}: expected `key = value`, got `{line}`", n + 1));
                    continue;
                };
                let (key, val) = (key.trim(), val.trim());
                let Some((_, slot)) = slots.iter_mut().find(|(k, _)| k == key) else {
                    errors.push(format!("line {}: unknown key `{key}`", n + 1));
                    continue;
                };
                let bad = |what: &str| format!("line {}: `{key}` {what} (got `{val}`)", n + 1);
                match slot {
                    Slot::F(f) => match val.parse::<f32>() {
                        Ok(x) if x.is_finite() && x >= 0.0 => **f = x,
                        Ok(_) => errors.push(bad("must be finite and >= 0")),
                        Err(_) => errors.push(bad("must be a number")),
                    },
                    Slot::U(u) => match val.parse::<u32>() {
                        Ok(x) => **u = x,
                        Err(_) => errors.push(bad("must be a whole number >= 0")),
                    },
                    Slot::B(b) => match val {
                        "true" => **b = true,
                        "false" => **b = false,
                        _ => errors.push(bad("must be true or false")),
                    },
                }
            }
        }
        errors.extend(next.validate());
        if errors.is_empty() {
            *self = next;
            Ok(())
        } else {
            Err(errors)
        }
    }

    pub fn load_file(path: &std::path::Path) -> Result<Balance, Vec<String>> {
        let text = std::fs::read_to_string(path).map_err(|e| vec![format!("{}: {e}", path.display())])?;
        Balance::from_text(&text)
    }

    /// Every key with its current value (round-trips through `from_text`).
    pub fn to_text(&self) -> String {
        let mut c = self.clone();
        let mut out = String::new();
        for (k, slot) in c.slots() {
            let v = match slot {
                Slot::F(f) => format!("{f}"),
                Slot::U(u) => format!("{u}"),
                Slot::B(b) => format!("{b}"),
            };
            out.push_str(&format!("{k} = {v}\n"));
        }
        out
    }

    /// Cross-field rules a single key can't express.
    pub fn validate(&self) -> Vec<String> {
        let mut e = Vec::new();
        if self.player.max_hp <= 0.0 {
            e.push("player.max_hp must be > 0".into());
        }
        if self.player.invuln_ticks < MAX_REWIND_TICKS {
            e.push(format!(
                "player.invuln_ticks must be >= {MAX_REWIND_TICKS} (lag-compensation window), got {}",
                self.player.invuln_ticks
            ));
        }
        if self.player.first_spawn_invuln_ticks != 0 && self.player.first_spawn_invuln_ticks < MAX_REWIND_TICKS {
            e.push(format!(
                "player.first_spawn_invuln_ticks must be 0 or >= {MAX_REWIND_TICKS} (lag-compensation window), got {}",
                self.player.first_spawn_invuln_ticks
            ));
        }
        let sh = &self.combat.shuriken;
        if sh.energy_max < 0.0 || sh.energy_cost <= 0.0 || sh.energy_regen_s <= 0.0 || sh.energy_resume < 0.0 || sh.mark_cost < 0.0 {
            e.push("combat.shuriken.energy_*: energy_max >= 0, energy_cost and energy_regen_s > 0, energy_resume and mark_cost >= 0".into());
        }
        if sh.energy_max > 0.0 && sh.energy_max + 1e-4 < sh.energy_cost.max(sh.mark_cost) {
            e.push("combat.shuriken.energy_max must cover at least one star and one mark star".into());
        }
        let m = &self.melee;
        if [m.momentum_per_mps, m.momentum_cap, m.knock_speed, m.knock_pop, m.juggle_up, m.splat_window_s, m.splat_damage, m.splat_stun_s, m.ring_window_s, m.mark_window_s, m.mark_bonus, m.streak_window_s, m.streak_bonus, m.streak_heal, m.exec_frac, m.slam_speed, m.shock_radius, m.shock_knock, m.counter_window_s].iter().any(|v| *v < 0.0) {
            e.push("melee.* values must be >= 0".into());
        }
        let g = &self.guard;
        if !(1.0..=180.0).contains(&g.front_deg) || !(0.0..=1.0).contains(&g.front_mult) || !(0.0..=1.0).contains(&g.hit_cost) {
            e.push("guard.front_deg must be 1..180, guard.front_mult and guard.hit_cost 0..1".into());
        }
        if !(0.0..=1.0).contains(&g.mercy_mult) || g.break_stagger_s < 0.0 || g.mercy_s < 0.0 {
            e.push("guard.mercy_mult must be 0..1, guard.break_stagger_s and guard.mercy_s >= 0".into());
        }
        if g.max_hold_s <= 0.0 || g.regen_s <= 0.0 || [g.parry_s, g.rearm_s, g.stagger_s, g.swing_open_s, g.counter_open_s].iter().any(|v| *v < 0.0) || g.bonus_mult < 1.0 {
            e.push("guard times must be >= 0 (max_hold_s and regen_s > 0) and guard.bonus_mult >= 1".into());
        }
        if self.player.regen_delay_s < 0.0 || self.player.regen_per_s < 0.0 {
            e.push("player.regen_delay_s and player.regen_per_s must be >= 0".into());
        }
        if self.matches.min_players == 0 {
            e.push("match.min_players must be >= 1".into());
        }
        if self.matches.mode > 2 {
            e.push("match.mode must be 0 (deathmatch), 1 (waves) or 2 (training)".into());
        }
        if self.enemy.max_alive == 0 {
            e.push("enemy.max_alive must be >= 1".into());
        }
        if self.enemy.idle_min_ticks > self.enemy.idle_max_ticks {
            e.push("enemy.idle_min_ticks must be <= enemy.idle_max_ticks".into());
        }
        for (name, s) in [("grunt", &self.enemy.grunt), ("thrower", &self.enemy.thrower), ("brute", &self.enemy.brute), ("dummy", &self.enemy.dummy)] {
            if s.hp <= 0.0 {
                e.push(format!("enemy.{name}.hp must be > 0"));
            }
            if s.windup_ticks == 0 {
                e.push(format!("enemy.{name}.windup_ticks must be >= 1 (the telegraph is what makes attacks dodgeable)"));
            }
            if s.pref_min > s.pref_max {
                e.push(format!("enemy.{name}.pref_min must be <= pref_max"));
            }
        }
        if self.wave.thrower_every == 0 || self.wave.brute_every == 0 {
            e.push("wave.thrower_every and wave.brute_every must be >= 1".into());
        }
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_join_is_protected_in_the_demo_balance_only() {
        use crate::character::Cfg;
        let mut d = crate::tick::Server::new(Cfg::default());
        d.set_balance(Balance::demo()).unwrap();
        d.add_player(1, [0.0, 1.0, 0.0]);
        assert!(d.players[0].vitals.spawn_protected(d.tick + 179));
        assert!(!d.players[0].vitals.spawn_protected(d.tick + 180));
        let mut p = crate::tick::Server::new(Cfg::default());
        p.add_player(1, [0.0, 1.0, 0.0]);
        assert!(!p.players[0].vitals.spawn_protected(p.tick));
        assert!(Balance::from_text("player.first_spawn_invuln_ticks = 3\n").is_err());
    }

    #[test]
    fn defaults_are_valid_and_round_trip_through_text() {
        let b = Balance::default();
        assert!(b.validate().is_empty(), "{:?}", b.validate());
        let text = b.to_text();
        assert_eq!(Balance::from_text(&text).unwrap(), b);
        // every key is listed exactly once
        let keys = Balance::keys();
        let mut sorted = keys.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len());
        assert_eq!(text.lines().count(), keys.len());
    }

    #[test]
    fn a_partial_file_overrides_only_what_it_names_and_ignores_comments() {
        let b = Balance::from_text(
            "# designer tweaks\nplayer.max_hp = 150   # tankier\n\n  enemy.grunt.windup_ticks=30\ncombat.knockback_slash = 4.5\nenemy.brute.super_armor = false\n",
        )
        .unwrap();
        let d = Balance::default();
        assert_eq!(b.player.max_hp, 150.0);
        assert_eq!(b.enemy.grunt.windup_ticks, 30);
        assert_eq!(b.combat.knockback_slash, 4.5);
        assert!(!b.enemy.brute.super_armor);
        // untouched
        assert_eq!(b.player.respawn_ticks, d.player.respawn_ticks);
        assert_eq!(b.enemy.thrower, d.enemy.thrower);
        assert_eq!(b.combat.shuriken, d.combat.shuriken);
    }

    #[test]
    fn typos_and_bad_values_are_errors_with_line_numbers_and_nothing_half_applies() {
        let text = "player.max_hp = 90\nplayer.max_hpp = 5\ncombat.shuriken.speed = fast\nenemy.grunt.hp = -3\nenemy.grunt.hp = NaN\nwave.brute_every = 1.5\nthis is not a pair\nenemy.grunt.super_armor = maybe\n";
        let errs = Balance::from_text(text).unwrap_err();
        let joined = errs.join("\n");
        for needle in ["line 2: unknown key `player.max_hpp`", "line 3:", "line 4:", "line 5:", "line 6:", "line 7:", "line 8:"] {
            assert!(joined.contains(needle), "missing `{needle}` in:\n{joined}");
        }
        // apply_text leaves the target alone on failure
        let mut b = Balance::default();
        assert!(b.apply_text(text).is_err());
        assert_eq!(b, Balance::default(), "player.max_hp = 90 must not have been applied");
    }

    #[test]
    fn cross_field_rules_reject_dangerous_combinations() {
        let e = Balance::from_text("player.invuln_ticks = 3\n").unwrap_err().join(" ");
        assert!(e.contains("invuln_ticks") && e.contains("lag-compensation"));
        assert!(Balance::from_text("enemy.grunt.windup_ticks = 0\n").is_err(), "no telegraph, no dodge");
        assert!(Balance::from_text("enemy.thrower.pref_min = 20\n").is_err());
        assert!(Balance::from_text("enemy.idle_min_ticks = 999\n").is_err());
        assert!(Balance::from_text("match.mode = 7\n").is_err());
        // control: the boundary value is fine
        assert!(Balance::from_text(&format!("player.invuln_ticks = {}\n", MAX_REWIND_TICKS)).is_ok());
    }

    fn b0_owner_immune() -> bool {
        Balance::default().hazards.owner_immune
    }

    #[test]
    fn demo_preset_is_valid_playable_and_round_trips() {
        let d = Balance::demo();
        assert!(d.validate().is_empty(), "{:?}", d.validate());
        assert_eq!(Balance::from_text(&d.to_text()).unwrap(), d);
        // sensible for 1v1..3v3: nobody dies to one slash or a handful of stars in a blink
        let b = Balance::default();
        // melee-first: the katana is a combo, 5 cuts kill (20, 20, finisher 30, 20, 20) and 3 never do; stars are a poke
        let k = &d.combat.katana;
        let cuts_to_kill = |hp: f32| {
            let (mut dealt, mut n) = (0.0, 0);
            while dealt < hp {
                n += 1;
                dealt += if n % 3 == 0 { k.damage * k.finisher_mult } else { k.damage };
            }
            n
        };
        assert_eq!(cuts_to_kill(d.player.max_hp), 5, "five connected cuts kill a full-health player");
        assert!(k.damage * 3.0 + k.damage * (k.finisher_mult - 1.0) < d.player.max_hp, "a 3-cut combo alone never kills");
        assert!(k.finisher_mult > 1.0 && k.finisher_reach > 0.0 && k.magnet >= 0.8);
        let dps = d.combat.shuriken.damage / d.combat.shuriken.cooldown;
        let ttk = d.player.max_hp / dps;
        assert!(d.combat.shuriken.damage <= 7.0 + 1e-3 && k.damage <= 20.0 + 1e-3, "the user's numbers: 7 hp stars, 20 hp cuts");
        // stars are a poke: a clean stream takes ~2.8 s, so it is slower than the ~1.7 s of cutting (melee wins fights)
        assert!((2.4..3.4).contains(&ttk), "a clean body-shot stream takes 2.4-3.4 s to kill ({dps} dps, {ttk:.2} s)");
        assert!(ttk / d.combat.headshot_mult > 1.2, "headshots speed it up without making it instant");
        // star energy: the pool caps a volley well under the player's hp and the regen trickle makes a star-only kill a
        // long job, so stars are a poke / zoning tool and the katana is the kill
        let sh = &d.combat.shuriken;
        assert!(sh.energy_max >= 4.0 && sh.energy_max <= 8.0, "a pool of a handful of charges");
        let volley = sh.energy_max.floor() * sh.damage;
        assert!(volley < d.player.max_hp * 0.5, "a full volley ({volley} hp) is under half a life: stars are not a kill button");
        let star_only_kill_s = sh.energy_max.floor() * sh.cooldown + (d.player.max_hp - volley).max(0.0) / (sh.damage / sh.energy_regen_s);
        assert!(star_only_kill_s >= 4.0, "a star-only kill (volley, then the regen trickle) takes {star_only_kill_s:.1} s of clean hits");
        assert!(sh.mark_cost >= 2.0 * sh.energy_cost && d.melee.mark_bonus > 0.0 && d.melee.mark_window_s >= 3.0, "the mark star is a 2-charge, multi-second setup for the katana");
        assert!(d.combat.shuriken.speed >= 60.0, "fast stars");
        assert_eq!((d.player.max_hp, d.player.respawn_ticks), (100.0, 150), "2.5 s: the death animation fills it");
        // hazards: not instant kills, one pass = one hit, own trail harmless
        let h = &d.hazards;
        assert!(h.wall_damage * 3.0 >= d.player.max_hp && h.wall_damage * 2.0 < d.player.max_hp, "walls: 3 touches kill");
        assert!(h.obstacle_damage * 2.0 >= d.player.max_hp && h.obstacle_damage < d.player.max_hp, "obstacles: 2 hits kill");
        assert!(h.rearm_ticks >= 60 && h.owner_immune);
        assert!(!h.walls_enabled && !h.obstacles_enabled, "shooter demo: no environmental damage");
        assert!(!b0_owner_immune(), "default balance unchanged");
        // match flow for a lone human + bots
        let m = d.match_cfg();
        assert_eq!((m.min_players, m.warmup_ticks, m.frag_limit), (1, 180, 10));
        assert_eq!(m.time_limit_ticks, 6 * 60 * 60, "6 minutes");
        assert!(d != b && b.matches.min_players == 2, "default preset untouched");
    }

    #[test]
    fn derived_configs_follow_the_balance() {
        let b = Balance::from_text("combat.shuriken.damage = 40\ncombat.shuriken.max_bounces = 1\ncombat.katana.damage = 77\nmatch.mode = 1\nmatch.frag_limit = 5\n").unwrap();
        let s = b.shuriken_cfg();
        assert_eq!((s.damage, s.max_bounces), (40.0, 1));
        let mut k = KatanaCfg::for_character(&crate::character::Cfg::default());
        let (cooldown, lunge) = (k.cooldown, k.lunge_distance);
        b.apply_katana(&mut k);
        assert_eq!(k.damage, 77.0);
        assert_eq!((k.cooldown, k.lunge_distance), (cooldown, lunge), "cooldown and lunge stay derived from the character");
        let m = b.match_cfg();
        assert_eq!((m.mode, m.frag_limit), (GameMode::CoopWaves, 5));
    }
}
