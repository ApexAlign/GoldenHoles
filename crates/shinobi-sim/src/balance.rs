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
    pub assist_range: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KatanaBalance {
    pub damage: f32,
    pub blade_len: f32,
    pub half_width: f32,
    pub half_height: f32,
    /// How strongly the hit sweep bends toward the lock-on target (0 = off, 1 = onto the target), times the lock pull.
    pub magnet: f32,
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
        Self { hitstop_slash_ms: 60.0, hitstop_kill_ms: 80.0 }
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
                    assist_range: s.assist_range,
                },
                katana: KatanaBalance { damage: 120.0, blade_len: 1.6, half_width: 1.3, half_height: 1.0, magnet: 0.8 },
                knockback_slash: 9.0,
                knockback_shuriken: 3.0,
                knockback_ultimate: 12.0,
                head_min_height: 0.5,
                headshot_mult: 1.0,
                ultimate_speed: 60.0,
                ultimate_max_radius: 120.0,
                ultimate_damage: 9999.0,
                pickup_charge_secs: 20.0,
                lock_secs: 0.35,
                lock_pull_min: 0.25,
                lock_break_secs: 0.25,
            },
            blocks: BlockBalance::default(),
            feedback: FeedbackBalance::default(),
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
    /// * katana is a 2-hit kill (60) instead of a one-hit; its lunge+blade reach stays ~5 m (derived from the
    ///   character dash), so closing distance is still rewarded without being a guaranteed kill;
    /// * shuriken: 70 m/s (a flat, fast star), 12 damage at 5/s = 60 dps, so a clean body-shot stream kills in
    ///   ~1.7 s; level crosshair (head height) is a 1.5x headshot, ~1.1 s;
    /// * light-walls 34 per touch (3 to kill), obstacles 60 (2 to kill), 1 s re-arm so one pass is one hit, and
    ///   your own trail never hurts you (it still blocks you);
    /// * hp 100, respawn 3 s, 1.5 s spawn protection;
    /// * the match starts 3 s after the first player (min_players 1, so one human plus bots works), first to
    ///   10 frags or 5 minutes, 8 s intermission.
    pub fn demo() -> Self {
        let mut b = Balance::default();
        b.combat.katana.damage = 60.0;
        // shooter feel: fast flat-flying stars (no drop), 5 shots/s, head height pays 1.5x
        b.combat.shuriken.speed = 70.0;
        b.combat.shuriken.damage = 12.0;
        b.combat.shuriken.cooldown = 0.20;
        b.combat.headshot_mult = 1.5;
        // hazards must not feel like unfair deaths: a light-wall is a 3-touch kill (34 each), an obstacle needs
        // two hits (60), contact re-arms only after 1 s (one pass = one hit), your own trail never hurts you
        b.hazards.wall_damage = 34.0;
        b.hazards.obstacle_damage = 60.0;
        b.hazards.rearm_ticks = 60;
        b.hazards.owner_immune = true;
        b.matches.warmup_ticks = 3 * crate::match_state::TICK_HZ;
        b.matches.min_players = 1;
        b.matches.frag_limit = 10;
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
            assist_range: b.assist_range,
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
        s!("combat.shuriken.assist_range", F, self.combat.shuriken.assist_range);
        s!("combat.katana.damage", F, self.combat.katana.damage);
        s!("combat.katana.blade_len", F, self.combat.katana.blade_len);
        s!("combat.katana.half_width", F, self.combat.katana.half_width);
        s!("combat.katana.half_height", F, self.combat.katana.half_height);
        s!("combat.katana.magnet", F, self.combat.katana.magnet);
        s!("combat.lock_secs", F, self.combat.lock_secs);
        s!("combat.lock_pull_min", F, self.combat.lock_pull_min);
        s!("combat.lock_break_secs", F, self.combat.lock_break_secs);
        s!("feedback.hitstop_slash_ms", F, self.feedback.hitstop_slash_ms);
        s!("feedback.hitstop_kill_ms", F, self.feedback.hitstop_kill_ms);
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
        assert!(d.combat.katana.damage * 2.0 >= d.player.max_hp && d.combat.katana.damage < d.player.max_hp, "katana is a 2-hit kill");
        let dps = d.combat.shuriken.damage / d.combat.shuriken.cooldown;
        let ttk = d.player.max_hp / dps;
        assert!((1.4..2.5).contains(&ttk), "a clean body-shot stream takes 1.4-2.5 s to kill ({dps} dps, {ttk:.2} s)");
        assert!(ttk / d.combat.headshot_mult > 0.9, "headshots speed it up without making it instant");
        assert!(d.combat.shuriken.speed >= 60.0, "fast stars");
        assert_eq!((d.player.max_hp, d.player.respawn_ticks), (100.0, 180));
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
