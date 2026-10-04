//! Server-side NPC goons ("Streets of Rage" mode).
//!
//! * Ids live in `ENEMY_ID_BASE..=ENEMY_ID_MAX`, disjoint from player ids (1..=255) and `NO_ATTACKER`, so an
//!   enemy that kills a player shows up as `PvpEvent::Kill { killer: <enemy id>, .. }`. HUD/Network name them.
//! * Two colliders each, like players: a solid movement capsule (`GROUP_ENEMY_BODY`, ignored by shots and
//!   slashes) and a parentless sensor hitbox (`GROUP_ENEMY`, `user_data` = id) that combat targets and the
//!   lag-compensation pass rewinds. Damage from players arrives as ordinary `CombatEvent::Damage` events
//!   (shuriken, slash, grid beam) and is routed by collider handle, never by `user_data` alone.
//! * Movement is a plain kinematic character controller with gravity (no `CharacterSystem`: no trails, no
//!   client prediction). Everything is deterministic: enemies sorted by id, randomness is
//!   `splitmix64(seed ^ tick ^ id)`, no wall clock, no hash-map iteration.
//! * Attacks are telegraphed: `Windup` is announced with the exact `hit_tick`, the attack direction is locked
//!   when it starts (stepping aside works) and resolves exactly at `hit_tick`. Damage or knockback during the
//!   windup cancels it (parry) unless the enemy has super armor.
//! * Enemy -> player damage is a normal `PvpHit` (attacker = enemy id), so protection, rolls, kills and
//!   respawns follow the same rules as player weapons.

use std::collections::VecDeque;

use rapier3d::control::{CharacterAutostep, CharacterLength, KinematicCharacterController};
use rapier3d::na::UnitQuaternion;
use rapier3d::parry::shape::Capsule;
use rapier3d::prelude::*;

use crate::balance::{Balance, EnemyStats};
use crate::character::{sync_body_colliders, Cfg, PhysicsWorld, GROUP_ENEMY};
use crate::combat::shuriken::{advance_disc, Disc, DiscStep, ShurikenCfg};
use crate::combat::{
    projectile_groups, CombatEvent, DamageKind, GROUP_ENEMY_BODY, GROUP_HITBOX, GROUP_MELEE, GROUP_PROJECTILE,
};
use crate::hazards::Hazards;
use crate::lagcomp::{PoseHistory, RewindTime};
use crate::match_state::GameMode;
use crate::pickups::{PickupPoint, Pickups};
use crate::pvp::{Cause, PvpHit};
use crate::tick::Player;

pub const ENEMY_ID_BASE: u16 = 1024;
pub const ENEMY_ID_MAX: u16 = 60_000;
/// Drop pickups use ids from here up (fixed points should stay below).
pub const DROP_ID_BASE: u32 = 1_000_000;
/// Line-of-sight is re-evaluated every N ticks per enemy (staggered by id): keeps big waves cheap.
const LOS_PERIOD: u32 = 4;
const GRAVITY: f32 = 28.0;
const KB_DECAY: f32 = 0.85;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnemyKind {
    /// Melee rusher.
    Grunt,
    /// Keeps its distance and throws shurikens.
    Thrower,
    /// Slow, high hp, super armor.
    Brute,
    /// Training target: never attacks.
    Dummy,
}

impl EnemyKind {
    fn is_melee(self) -> bool {
        matches!(self, EnemyKind::Grunt | EnemyKind::Brute)
    }
}

/// Animation-relevant AI state (replicated as a small tag + the event stream).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AiState {
    Idle { until: u32 },
    Patrol { dir: [f32; 2], until: u32 },
    Chase,
    /// Telegraph. The hit resolves at `hit_tick`, along `dir` (locked at `started`).
    Windup { started: u32, hit_tick: u32, dir: [f32; 2] },
    Recover { until: u32 },
    Stagger { until: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EnemyEvent {
    Spawned { tick: u32, id: u16, kind: EnemyKind, pos: [f32; 3] },
    /// Attack telegraph: show the windup; damage lands at `hit_tick` along `dir`.
    Windup { tick: u32, id: u16, kind: EnemyKind, hit_tick: u32, dir: [f32; 3] },
    /// Knocked out of what it was doing; `interrupted`: it was winding up (parried).
    Staggered { tick: u32, id: u16, interrupted: bool },
    Hurt { tick: u32, id: u16, hp: f32, amount: f32, by: Option<u16> },
    Killed { tick: u32, id: u16, kind: EnemyKind, by: Option<u16>, pos: [f32; 3] },
    Dropped { tick: u32, pickup_id: u32, pos: [f32; 3] },
    DiscThrown { tick: u32, id: u16, pos: [f32; 3], dir: [f32; 3] },
    WaveStart { tick: u32, wave: u32, count: u32 },
    WaveCleared { tick: u32, wave: u32 },
}

/// Damage waiting to be applied to an enemy (collected from combat events and hazards).
#[derive(Clone, Copy, Debug)]
pub struct EnemyDamage {
    pub enemy: u16,
    pub attacker: Option<u16>,
    pub attacker_pos: Option<[f32; 3]>,
    pub amount: f32,
    /// Horizontal speed given before the enemy's own `knockback_scale`.
    pub knockback: f32,
}

pub struct Enemy {
    pub id: u16,
    pub kind: EnemyKind,
    pub hp: f32,
    pub max_hp: f32,
    pub state: AiState,
    pub yaw: f32,
    pub target: Option<u16>,
    body: RigidBodyHandle,
    hitbox: ColliderHandle,
    home: [f32; 3],
    history: PoseHistory,
    vy: f32,
    kb: Vector<Real>,
    ready_at: u32,
    last_seen: u32,
    has_los: bool,
    /// Index into `training_points` when this is a training dummy (respawns there).
    point: Option<usize>,
}

impl Enemy {
    pub fn pos(&self, w: &PhysicsWorld) -> [f32; 3] {
        let t = w.bodies[self.body].translation();
        [t.x, t.y, t.z]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WavePhase {
    /// Not started / waiting until `next_at`.
    Waiting { next_at: u32 },
    Active,
}

pub struct Enemies {
    pub seed: u64,
    list: Vec<Enemy>,
    next_id: u16,
    discs: Vec<(u16, Disc)>,
    kills: Vec<(u16, u16)>,
    /// Wave spawn points (body-centre positions). Empty = use the server's player spawn list.
    pub spawn_points: Vec<[f32; 3]>,
    /// Training dummies stand here and respawn here.
    pub training_points: Vec<[f32; 3]>,
    wave: u32,
    wave_phase: Option<WavePhase>,
    pending: VecDeque<EnemyKind>,
    dummy_respawns: Vec<(usize, u32)>,
    training_started: bool,
    next_drop: u32,
    kcc: KinematicCharacterController,
}

impl Default for Enemies {
    fn default() -> Self {
        Self::new(0x5eed_5eed)
    }
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn flat(v: [f32; 3]) -> [f32; 2] {
    [v[0], v[2]]
}

fn dist2(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn unit2(from: [f32; 2], to: [f32; 2]) -> [f32; 2] {
    let d = [to[0] - from[0], to[1] - from[1]];
    let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
    if l < 1e-4 { [0.0, -1.0] } else { [d[0] / l, d[1] / l] }
}

/// Yaw convention shared with `combat::Aim`: 0 faces -Z.
fn yaw_of(d: [f32; 2]) -> f32 {
    (-d[0]).atan2(-d[1])
}

impl Enemies {
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            list: Vec::new(),
            next_id: ENEMY_ID_BASE,
            discs: Vec::new(),
            kills: Vec::new(),
            spawn_points: Vec::new(),
            training_points: Vec::new(),
            wave: 0,
            wave_phase: None,
            pending: VecDeque::new(),
            dummy_respawns: Vec::new(),
            training_started: false,
            next_drop: DROP_ID_BASE,
            kcc: KinematicCharacterController {
                up: Vector::y_axis(),
                offset: CharacterLength::Absolute(0.01),
                slide: true,
                autostep: Some(CharacterAutostep {
                    max_height: CharacterLength::Absolute(0.3),
                    min_width: CharacterLength::Absolute(0.2),
                    include_dynamic_bodies: false,
                }),
                snap_to_ground: Some(CharacterLength::Absolute(0.2)),
                ..KinematicCharacterController::default()
            },
        }
    }

    // ---- read API (snapshots / HUD) ------------------------------------------------------------------

    pub fn iter(&self) -> impl Iterator<Item = &Enemy> + '_ {
        self.list.iter()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn get(&self, id: u16) -> Option<&Enemy> {
        self.list.binary_search_by_key(&id, |e| e.id).ok().map(|i| &self.list[i])
    }

    /// Enemy kills credited to a player (separate from PvP frags).
    pub fn kills_of(&self, player: u16) -> u16 {
        self.kills.iter().find(|(p, _)| *p == player).map_or(0, |(_, n)| *n)
    }

    pub fn wave(&self) -> u32 {
        self.wave
    }

    /// Live enemy discs: (owner id, position, direction).
    pub fn discs(&self) -> impl Iterator<Item = (u16, [f32; 3], [f32; 3])> + '_ {
        self.discs.iter().map(|(o, d)| (*o, [d.pos.x, d.pos.y, d.pos.z], [d.dir.x, d.dir.y, d.dir.z]))
    }

    fn index_of(&self, id: u16) -> Option<usize> {
        self.list.binary_search_by_key(&id, |e| e.id).ok()
    }

    fn hash(&self, tick: u32, id: u16, salt: u64) -> u64 {
        splitmix(self.seed ^ ((tick as u64) << 24) ^ ((id as u64) << 8) ^ salt)
    }

    // ---- lifecycle -----------------------------------------------------------------------------------

    /// Spawn one enemy at body-centre `pos`. `None` at the cap (`enemy.max_alive`) or out of ids.
    pub fn spawn(&mut self, world: &mut PhysicsWorld, ch: &Cfg, bal: &Balance, kind: EnemyKind, pos: [f32; 3], tick: u32, events: &mut Vec<EnemyEvent>) -> Option<u16> {
        self.spawn_at(world, ch, bal, kind, pos, tick, None, events)
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_at(
        &mut self,
        world: &mut PhysicsWorld,
        ch: &Cfg,
        bal: &Balance,
        kind: EnemyKind,
        pos: [f32; 3],
        tick: u32,
        point: Option<usize>,
        events: &mut Vec<EnemyEvent>,
    ) -> Option<u16> {
        if self.list.len() >= bal.enemy.max_alive as usize {
            return None;
        }
        // next free id, wrapping inside the enemy range
        let span = (ENEMY_ID_MAX - ENEMY_ID_BASE) as u32 + 1;
        let id = (0..span)
            .map(|k| ENEMY_ID_BASE + ((self.next_id - ENEMY_ID_BASE) as u32 + k) as u16 % span as u16)
            .find(|id| self.index_of(*id).is_none())?;
        self.next_id = if id >= ENEMY_ID_MAX { ENEMY_ID_BASE } else { id + 1 };

        let body = world.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![pos[0], pos[1], pos[2]]).build());
        world.colliders.insert_with_parent(
            ColliderBuilder::capsule_y(ch.half_height, ch.radius)
                .collision_groups(InteractionGroups::new(GROUP_ENEMY_BODY, Group::ALL))
                .build(),
            body,
            &mut world.bodies,
        );
        let hitbox = world.colliders.insert(
            ColliderBuilder::capsule_y(ch.half_height, ch.radius)
                .translation(vector![pos[0], pos[1], pos[2]])
                .sensor(true)
                .collision_groups(InteractionGroups::new(
                    GROUP_ENEMY,
                    GROUP_PROJECTILE | GROUP_MELEE | crate::character::GROUP_TRAIL,
                ))
                .user_data(id as u128)
                .build(),
        );
        let st = bal.enemy.stats(kind);
        let e = Enemy {
            id,
            kind,
            hp: st.hp,
            max_hp: st.hp,
            state: AiState::Idle { until: tick.wrapping_add(30) },
            yaw: 0.0,
            target: None,
            body,
            hitbox,
            home: pos,
            history: PoseHistory::default(),
            vy: 0.0,
            kb: Vector::zeros(),
            ready_at: tick,
            last_seen: 0,
            has_los: false,
            point,
        };
        let at = self.list.partition_point(|x| x.id < id);
        self.list.insert(at, e);
        events.push(EnemyEvent::Spawned { tick, id, kind, pos });
        Some(id)
    }

    fn remove_at(&mut self, world: &mut PhysicsWorld, i: usize) -> Enemy {
        let e = self.list.remove(i);
        world.colliders.remove(e.hitbox, &mut world.islands, &mut world.bodies, false);
        world.bodies.remove(
            e.body,
            &mut world.islands,
            &mut world.colliders,
            &mut ImpulseJointSet::new(),
            &mut MultibodyJointSet::new(),
            true, // takes the solid capsule with it
        );
        self.discs.retain(|(o, _)| *o != e.id);
        e
    }

    /// Match reset / mode change: no enemies, discs, wave progress or kill credits left; bodies and colliders
    /// are freed.
    pub fn clear_all(&mut self, world: &mut PhysicsWorld) {
        while !self.list.is_empty() {
            let last = self.list.len() - 1;
            self.remove_at(world, last);
        }
        self.discs.clear();
        self.kills.clear();
        self.pending.clear();
        self.dummy_respawns.clear();
        self.wave = 0;
        self.wave_phase = None;
        self.training_started = false;
    }

    // ---- modes: waves / training ---------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn update_modes(
        &mut self,
        tick: u32,
        mode: GameMode,
        live: bool,
        world: &mut PhysicsWorld,
        players: &[Player],
        fallback_spawns: &[[f32; 3]],
        bal: &Balance,
        ch: &Cfg,
        events: &mut Vec<EnemyEvent>,
    ) {
        match mode {
            GameMode::Deathmatch => {}
            GameMode::Training => {
                if !self.training_started {
                    self.training_started = true;
                    for k in 0..self.training_points.len() {
                        let p = self.training_points[k];
                        self.spawn_at(world, ch, bal, EnemyKind::Dummy, p, tick, Some(k), events);
                    }
                }
                let due: Vec<usize> = self.dummy_respawns.iter().filter(|(_, at)| tick.wrapping_sub(*at) < u32::MAX / 2).map(|(k, _)| *k).collect();
                self.dummy_respawns.retain(|(_, at)| tick.wrapping_sub(*at) >= u32::MAX / 2);
                for k in due {
                    let p = self.training_points[k];
                    self.spawn_at(world, ch, bal, EnemyKind::Dummy, p, tick, Some(k), events);
                }
            }
            GameMode::CoopWaves => {
                if !live {
                    return;
                }
                let w = &bal.wave;
                match self.wave_phase {
                    None => {
                        self.wave_phase = Some(WavePhase::Waiting { next_at: tick.wrapping_add(w.first_wave_delay_ticks) });
                    }
                    Some(WavePhase::Waiting { next_at }) if tick.wrapping_sub(next_at) < u32::MAX / 2 => {
                        self.wave += 1;
                        let n = self.wave;
                        let grunts = w.first_wave_grunts + (n - 1) * w.grunts_per_wave;
                        let throwers = if n >= w.thrower_from_wave { 1 + (n - w.thrower_from_wave) / w.thrower_every } else { 0 };
                        let brutes = if n >= w.brute_from_wave { 1 + (n - w.brute_from_wave) / w.brute_every } else { 0 };
                        // brutes first so they walk in early; the rest interleave deterministically
                        for _ in 0..brutes {
                            self.pending.push_back(EnemyKind::Brute);
                        }
                        let (mut g, mut t) = (grunts, throwers);
                        while g + t > 0 {
                            if g > 0 {
                                self.pending.push_back(EnemyKind::Grunt);
                                g -= 1;
                            }
                            if t > 0 && (g == 0 || g % 2 == 0) {
                                self.pending.push_back(EnemyKind::Thrower);
                                t -= 1;
                            }
                        }
                        events.push(EnemyEvent::WaveStart { tick, wave: n, count: self.pending.len() as u32 });
                        self.wave_phase = Some(WavePhase::Active);
                    }
                    Some(WavePhase::Waiting { .. }) => {}
                    Some(WavePhase::Active) => {
                        // feed the queue in, one per tick, while there is room
                        if let Some(&kind) = self.pending.front() {
                            let spawns: &[[f32; 3]] = if self.spawn_points.is_empty() { fallback_spawns } else { &self.spawn_points };
                            let watchers: Vec<[f32; 3]> = players.iter().filter(|p| p.vitals.alive()).map(|p| p.pos(world)).collect();
                            world.queries.update(&world.colliders);
                            if let Some(k) = crate::spawn::choose(world, spawns, &watchers, self.hash(tick, 0, 0xA1) as u32) {
                                let at = spawns[k];
                                if self.spawn_at(world, ch, bal, kind, at, tick, None, events).is_some() {
                                    self.pending.pop_front();
                                }
                            }
                        } else if self.list.is_empty() {
                            events.push(EnemyEvent::WaveCleared { tick, wave: self.wave });
                            self.wave_phase = Some(WavePhase::Waiting { next_at: tick.wrapping_add(w.inter_wave_ticks) });
                        }
                    }
                }
            }
        }
    }

    // ---- per-tick AI + movement ----------------------------------------------------------------------

    /// Run all enemies for `tick` (call at the START of the tick, while hitboxes are at "now"). Enemy attacks on
    /// players are appended to `hits`; telegraphs and the like go to `events`.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        tick: u32,
        dt: f32,
        world: &mut PhysicsWorld,
        players: &[Player],
        bal: &Balance,
        ch: &Cfg,
        events: &mut Vec<EnemyEvent>,
        hits: &mut Vec<PvpHit>,
    ) {
        let watch: Vec<(usize, u16, [f32; 3])> = players
            .iter()
            .enumerate()
            .filter(|(_, p)| p.vitals.alive())
            .map(|(i, p)| (i, p.id, p.pos(world)))
            .collect();

        for idx in 0..self.list.len() {
            let (id, kind) = (self.list[idx].id, self.list[idx].kind);
            let st = bal.enemy.stats(kind).clone();
            let pos = self.list[idx].pos(world);

            // knockback decays whatever the AI is doing
            let mut kb = self.list[idx].kb;
            self.list[idx].kb *= KB_DECAY;
            if self.list[idx].kb.norm() < 0.1 {
                self.list[idx].kb = Vector::zeros();
            }

            if kind == EnemyKind::Dummy {
                self.move_enemy(world, ch, idx, Vector::zeros(), kb, dt);
                continue;
            }

            // ---- perception (staggered by id) ----
            if (tick.wrapping_add(id as u32)) % LOS_PERIOD == 0 {
                self.perceive(idx, tick, world, &watch, &st, bal);
            }
            let e = &self.list[idx];
            let target = e.target.and_then(|t| watch.iter().find(|(_, pid, _)| *pid == t).map(|(i, pid, p)| (*i, *pid, *p)));
            if e.target.is_some() && target.is_none() {
                self.list[idx].target = None; // target died or left
            }

            // ---- state machine ----
            let mut want = Vector::<Real>::zeros();
            let state = self.list[idx].state;
            match state {
                AiState::Stagger { until } => {
                    if tick.wrapping_sub(until) < u32::MAX / 2 {
                        self.list[idx].state = AiState::Chase;
                    }
                }
                AiState::Recover { until } => {
                    if tick.wrapping_sub(until) < u32::MAX / 2 {
                        self.list[idx].state = AiState::Chase;
                    }
                }
                AiState::Windup { hit_tick, dir, .. } => {
                    kb *= 1.0; // stands its ground; only knockback moves it
                    if tick.wrapping_sub(hit_tick) < u32::MAX / 2 {
                        self.strike(idx, tick, dir, world, players, &st, bal, events, hits);
                    }
                }
                AiState::Chase => {
                    let Some((_, _, tp)) = target else {
                        self.list[idx].state = AiState::Idle { until: tick.wrapping_add(bal.enemy.idle_min_ticks) };
                        continue_move(self, world, ch, idx, want, kb, dt);
                        continue;
                    };
                    let d = dist2(flat(pos), flat(tp));
                    let to = unit2(flat(pos), flat(tp));
                    self.list[idx].yaw = yaw_of(to);
                    let ready = tick.wrapping_sub(self.list[idx].ready_at) < u32::MAX / 2;
                    if kind.is_melee() {
                        if d <= st.attack_range && ready {
                            self.begin_windup(idx, tick, to, &st, events);
                        } else if d > st.attack_range * 0.8 {
                            want = vector![to[0], 0.0, to[1]] * st.speed;
                        }
                    } else {
                        // thrower: hold a preferred band, throw from inside range with a clear line
                        if d < st.pref_min {
                            want = vector![-to[0], 0.0, -to[1]] * st.speed;
                        } else if d > st.pref_max {
                            want = vector![to[0], 0.0, to[1]] * st.speed;
                        }
                        if d <= st.attack_range && ready && self.list[idx].has_los {
                            self.begin_windup(idx, tick, to, &st, events);
                        }
                    }
                }
                AiState::Idle { until } => {
                    if self.list[idx].target.is_some() {
                        self.list[idx].state = AiState::Chase;
                    } else if tick.wrapping_sub(until) < u32::MAX / 2 {
                        let h = self.hash(tick, id, 0xB2);
                        let ang = (h % 3600) as f32 / 3600.0 * std::f32::consts::TAU;
                        let dur = 60 + (h >> 16) as u32 % 90;
                        self.list[idx].state = if h & 1 == 0 {
                            AiState::Patrol { dir: [ang.cos(), ang.sin()], until: tick.wrapping_add(dur) }
                        } else {
                            AiState::Idle { until: tick.wrapping_add(bal.enemy.idle_min_ticks + (h >> 24) as u32 % (bal.enemy.idle_max_ticks - bal.enemy.idle_min_ticks + 1)) }
                        };
                    }
                }
                AiState::Patrol { dir, until } => {
                    if self.list[idx].target.is_some() {
                        self.list[idx].state = AiState::Chase;
                    } else {
                        let home = self.list[idx].home;
                        let back = dist2(flat(pos), flat(home)) > bal.enemy.patrol_radius;
                        let d = if back { unit2(flat(pos), flat(home)) } else { dir };
                        want = vector![d[0], 0.0, d[1]] * st.speed * 0.4;
                        self.list[idx].yaw = yaw_of(d);
                        if tick.wrapping_sub(until) < u32::MAX / 2 {
                            let h = self.hash(tick, id, 0xC3);
                            self.list[idx].state = AiState::Idle {
                                until: tick.wrapping_add(bal.enemy.idle_min_ticks + (h >> 8) as u32 % (bal.enemy.idle_max_ticks - bal.enemy.idle_min_ticks + 1)),
                            };
                        }
                    }
                }
            }
            continue_move(self, world, ch, idx, want, kb, dt);
        }

        // ---- enemy discs ----
        let dcfg = ShurikenCfg {
            speed: bal.enemy.disc_speed,
            max_bounces: 0,
            targets: GROUP_HITBOX,
            mask: projectile_groups().difference(GROUP_ENEMY),
            ..ShurikenCfg::default()
        };
        let mut scratch = Vec::new();
        let mut i = 0;
        while i < self.discs.len() {
            let (owner, mut d) = self.discs[i];
            let dmg = bal.enemy.thrower.damage;
            match advance_disc(world, &mut d, &dcfg, dt, None, &mut scratch) {
                DiscStep::Flying => {
                    self.discs[i].1 = d;
                    i += 1;
                }
                DiscStep::Hit { collider, .. } => {
                    if let Some(v) = players.iter().position(|p| p.vitals.hitbox == collider) {
                        hits.push(PvpHit { attacker: owner, victim: v, amount: dmg, cause: Cause::Weapon(DamageKind::Shuriken), head: false });
                    }
                    self.discs.remove(i);
                }
                DiscStep::Gone => {
                    self.discs.remove(i);
                }
            }
            scratch.clear();
        }
    }

    fn perceive(&mut self, idx: usize, tick: u32, world: &PhysicsWorld, watch: &[(usize, u16, [f32; 3])], st: &EnemyStats, bal: &Balance) {
        let pos = self.list[idx].pos(world);
        let cur = self.list[idx].target;
        // keep the current target while it is inside the leash and was seen recently
        if let Some(t) = cur {
            match watch.iter().find(|(_, pid, _)| *pid == t) {
                Some((_, _, p)) if dist2(flat(pos), flat(*p)) <= st.leash_radius => {
                    let los = !crate::spawn::blocked(world, pos, *p);
                    self.list[idx].has_los = los;
                    if los {
                        self.list[idx].last_seen = tick;
                    }
                    if tick.wrapping_sub(self.list[idx].last_seen) <= bal.enemy.lose_target_ticks {
                        return;
                    }
                }
                _ => {}
            }
            self.list[idx].target = None;
        }
        // nearest visible player inside the aggro radius (ties -> lowest id)
        let mut best: Option<(f32, u16)> = None;
        for (_, pid, p) in watch {
            let d = dist2(flat(pos), flat(*p));
            if d <= st.aggro_radius && !crate::spawn::blocked(world, pos, *p) && best.map_or(true, |b| (d, *pid) < b) {
                best = Some((d, *pid));
            }
        }
        if let Some((_, pid)) = best {
            self.list[idx].target = Some(pid);
            self.list[idx].has_los = true;
            self.list[idx].last_seen = tick;
            if matches!(self.list[idx].state, AiState::Idle { .. } | AiState::Patrol { .. }) {
                self.list[idx].state = AiState::Chase;
            }
        }
    }

    fn begin_windup(&mut self, idx: usize, tick: u32, dir: [f32; 2], st: &EnemyStats, events: &mut Vec<EnemyEvent>) {
        let e = &mut self.list[idx];
        let hit_tick = tick + st.windup_ticks;
        e.state = AiState::Windup { started: tick, hit_tick, dir };
        e.yaw = yaw_of(dir);
        events.push(EnemyEvent::Windup { tick, id: e.id, kind: e.kind, hit_tick, dir: [dir[0], 0.0, dir[1]] });
    }

    /// The windup elapsed: resolve against where everyone is NOW, along the direction locked at the start.
    #[allow(clippy::too_many_arguments)]
    fn strike(&mut self, idx: usize, tick: u32, dir: [f32; 2], world: &mut PhysicsWorld, players: &[Player], st: &EnemyStats, bal: &Balance, events: &mut Vec<EnemyEvent>, hits: &mut Vec<PvpHit>) {
        let (id, kind) = (self.list[idx].id, self.list[idx].kind);
        let pos = self.list[idx].pos(world);
        if kind.is_melee() {
            let d3 = vector![dir[0], 0.0, dir[1]];
            let reach = st.attack_range + 0.4;
            let shape = Cuboid::new(vector![0.9, 1.0, reach * 0.5]);
            let rot = UnitQuaternion::face_towards(&d3, &Vector::y());
            let at = Isometry::from_parts((vector![pos[0], pos[1], pos[2]] + d3 * (reach * 0.5)).into(), rot);
            let filter = QueryFilter::default().groups(InteractionGroups::new(GROUP_MELEE, GROUP_HITBOX));
            let mut hit = Vec::new();
            world.queries.intersections_with_shape(&world.bodies, &world.colliders, &at, &shape, filter, |h| {
                hit.push(h);
                true
            });
            for h in hit {
                if let Some(v) = players.iter().position(|p| p.vitals.hitbox == h) {
                    hits.push(PvpHit { attacker: id, victim: v, amount: st.damage, cause: Cause::Weapon(DamageKind::Slash), head: false });
                }
            }
        } else {
            let d = vector![dir[0], 0.0, dir[1]];
            let start = Point::new(pos[0], pos[1], pos[2]) + d * 0.8;
            self.discs.push((id, Disc { pos: start, dir: d, bounces: 0, age: 0.0, speed_scale: 1.0, homing: None }));
            events.push(EnemyEvent::DiscThrown { tick, id, pos: [start.x, start.y, start.z], dir: [d.x, d.y, d.z] });
        }
        let e = &mut self.list[idx];
        e.state = AiState::Recover { until: tick + st.recover_ticks };
        e.ready_at = tick + st.cooldown_ticks;
        let _ = bal;
    }

    fn move_enemy(&mut self, world: &mut PhysicsWorld, ch: &Cfg, idx: usize, want: Vector<Real>, kb: Vector<Real>, dt: f32) {
        let body = self.list[idx].body;
        let pos = *world.bodies[body].translation();
        let vy = {
            let e = &mut self.list[idx];
            e.vy = (e.vy - GRAVITY * dt).max(-40.0);
            e.vy
        };
        let desired = (want + kb) * dt + vector![0.0, vy * dt, 0.0];
        let shape = Capsule::new_y(ch.half_height, ch.radius);
        let filter = QueryFilter::default().exclude_rigid_body(body).exclude_sensors();
        let mv = self.kcc.move_shape(dt, &world.bodies, &world.colliders, &world.queries, &shape, &Isometry::from(pos), desired, filter, |_| {});
        if mv.grounded && self.list[idx].vy < 0.0 {
            self.list[idx].vy = -2.0;
        }
        let to = pos + mv.translation;
        if to.iter().all(|c| c.is_finite()) {
            world.bodies[body].set_position(Isometry::translation(to.x, to.y, to.z), true);
            sync_body_colliders(world, body);
        }
    }

    // ---- damage in -----------------------------------------------------------------------------------

    /// Map combat events of one attacker onto enemy hitboxes (by collider handle). Unknown `GROUP_ENEMY`
    /// colliders are ignored.
    pub fn collect_from_events(&self, attacker: u16, attacker_pos: [f32; 3], events: &[CombatEvent], bal: &Balance, out: &mut Vec<EnemyDamage>) {
        for e in events {
            if let CombatEvent::Damage { collider, amount, kind, .. } = *e {
                if let Some(en) = self.list.iter().find(|x| x.hitbox == collider) {
                    let knockback = match kind {
                        DamageKind::Slash => bal.combat.knockback_slash,
                        DamageKind::Shuriken => bal.combat.knockback_shuriken,
                        DamageKind::GridBeam => bal.combat.knockback_ultimate,
                    };
                    out.push(EnemyDamage { enemy: en.id, attacker: Some(attacker), attacker_pos: Some(attacker_pos), amount, knockback });
                }
            }
        }
    }

    /// Hazard (light-wall) hits on enemies: `Hazards::enemy_hits` carries the enemy id as `user_data`.
    pub fn collect_from_hazards(&self, hz: &Hazards, out: &mut Vec<EnemyDamage>) {
        for (ud, amount, src) in &hz.enemy_hits {
            let Ok(id) = u16::try_from(*ud) else { continue };
            if self.index_of(id).is_none() {
                continue;
            }
            let attacker = match src {
                crate::pvp::HazardSource::Wall { owner, .. } => Some(*owner as u16),
                _ => None,
            };
            out.push(EnemyDamage { enemy: id, attacker, attacker_pos: None, amount: *amount, knockback: 0.0 });
        }
    }

    /// Apply this tick's damage in one pass (call after every attacker has run). An enemy dies at most once.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_damage(
        &mut self,
        tick: u32,
        world: &mut PhysicsWorld,
        bal: &Balance,
        damage: &[EnemyDamage],
        pickups: &mut Pickups,
        events: &mut Vec<EnemyEvent>,
    ) {
        let mut dead: Vec<(u16, Option<u16>)> = Vec::new();
        for d in damage {
            let Some(i) = self.index_of(d.enemy) else { continue };
            if dead.iter().any(|(id, _)| *id == d.enemy) {
                continue;
            }
            let st = bal.enemy.stats(self.list[i].kind).clone();
            let pos = self.list[i].pos(world);
            let e = &mut self.list[i];
            e.hp -= d.amount;
            events.push(EnemyEvent::Hurt { tick, id: e.id, hp: e.hp.max(0.0), amount: d.amount, by: d.attacker });
            if e.hp <= 0.0 {
                dead.push((e.id, d.attacker));
                continue;
            }
            // knockback away from the attacker, scaled by the enemy's resistance
            if let Some(ap) = d.attacker_pos {
                let away = unit2(flat(ap), flat(pos));
                let sp = d.knockback * st.knockback_scale;
                if sp > 0.0 {
                    e.kb += vector![away[0], 0.0, away[1]] * sp;
                }
            }
            // stagger unless armoured; a windup caught mid-telegraph is cancelled (the parry)
            if !st.super_armor && st.stagger_ticks > 0 {
                let was_windup = matches!(e.state, AiState::Windup { .. });
                e.state = AiState::Stagger { until: tick + st.stagger_ticks };
                events.push(EnemyEvent::Staggered { tick, id: e.id, interrupted: was_windup });
            }
        }
        for (id, by) in dead {
            let Some(i) = self.index_of(id) else { continue };
            let pos = self.list[i].pos(world);
            let removed = self.remove_at(world, i);
            events.push(EnemyEvent::Killed { tick, id, kind: removed.kind, by, pos });
            if let Some(p) = by.filter(|p| *p < ENEMY_ID_BASE) {
                match self.kills.iter_mut().find(|(pid, _)| *pid == p) {
                    Some((_, n)) => *n = n.saturating_add(1),
                    None => {
                        self.kills.push((p, 1));
                        self.kills.sort_unstable();
                    }
                }
            }
            if let Some(k) = removed.point {
                self.dummy_respawns.push((k, tick.wrapping_add(bal.enemy.dummy_respawn_ticks)));
            }
            let st = bal.enemy.stats(removed.kind);
            if self.hash(tick, id, 0xD0) % 1000 < st.drop_per_mille as u64 {
                let pid = self.next_drop;
                self.next_drop = self.next_drop.wrapping_add(1).max(DROP_ID_BASE);
                pickups.drop_loot(
                    PickupPoint {
                        id: pid,
                        pos: [pos[0], pos[1], pos[2]],
                        kind: crate::combat::powerup::PowerupKind::GridBeam,
                        respawn_ticks: 0,
                        charge_secs: bal.combat.pickup_charge_secs,
                    },
                    tick.wrapping_add(bal.enemy.drop_ttl_ticks),
                );
                events.push(EnemyEvent::Dropped { tick, pickup_id: pid, pos });
            }
        }
    }

    // ---- hitbox bookkeeping (lag compensation) -------------------------------------------------------

    /// Put every enemy hitbox where `rt` says the attacker saw it (current pose when there is no history).
    pub fn rewind_hitboxes(&self, world: &mut PhysicsWorld, rt: Option<RewindTime>) {
        for e in &self.list {
            let pos = rt.and_then(|rt| e.history.sample(rt.tick, rt.alpha)).unwrap_or_else(|| e.pos(world));
            if let Some(c) = world.colliders.get_mut(e.hitbox) {
                c.set_position(Isometry::translation(pos[0], pos[1], pos[2]));
            }
        }
    }

    /// Hitboxes back to "now".
    pub fn restore_hitboxes(&self, world: &mut PhysicsWorld) {
        self.rewind_hitboxes(world, None);
    }

    /// End of tick: history records the post-move pose (the tick a snapshot would be labelled with).
    pub fn finish_tick(&mut self, world: &mut PhysicsWorld, tick: u32) {
        for e in &mut self.list {
            let p = {
                let t = world.bodies[e.body].translation();
                [t.x, t.y, t.z]
            };
            e.history.record(tick, p);
        }
        self.restore_hitboxes(world);
    }
}

fn continue_move(en: &mut Enemies, world: &mut PhysicsWorld, ch: &Cfg, idx: usize, want: Vector<Real>, kb: Vector<Real>, dt: f32) {
    en.move_enemy(world, ch, idx, want, kb, dt);
}
