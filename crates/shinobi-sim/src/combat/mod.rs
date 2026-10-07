//! Weapons, melee and pickups. Engine-agnostic (raw rapier3d), deterministic, no ECS.
//!
//! Sim side (server + client prediction): `CombatSystem::update` -> `CombatEvent`s.
//! Client-only cosmetics: `viewmodel::KatanaViewModel` consumes the events.
//!
//! Per tick, BEFORE `CharacterSystem::update`:
//! ```ignore
//! let aim = Aim::level(eye_pos, dq_yaw(intent.aim_yaw));
//! combat.update(dt, now, &CombatInput::new(intent.buttons.has(Buttons::FIRE), intent.buttons.has(Buttons::SLICE)), &aim, &world, &ch, &mut player_input, &mut events);
//! ch.update(dt, &player_input, &mut world, now);
//! ```
//! Collision groups reuse `character.rs` numbering (ENEMY = GROUP_2, TRAIL = GROUP_3).
//! Every non-enemy collider (level geometry, player) needs a membership that EXCLUDES `GROUP_ENEMY`
//! (`GROUP_WORLD` / `GROUP_PLAYER`): the default `Group::ALL` membership reads as "enemy".
//! Enemy colliders must: be in `GROUP_ENEMY`, accept `GROUP_PROJECTILE | GROUP_MELEE`, and carry
//! their id in `Collider::user_data`.
//! Call `world.queries.update(&world.colliders)` once per tick after enemies move.

use rapier3d::prelude::*;

use crate::character::{CharacterSystem, PhysicsWorld, PlayerInput, GROUP_ENEMY, GROUP_TRAIL};

pub mod aim_assist;
pub mod katana;
pub mod powerup;
pub mod shuriken;
pub mod weapon;

pub const GROUP_WORLD: Group = Group::GROUP_1;
pub const GROUP_PLAYER: Group = Group::GROUP_7;
/// Parentless sensor capsule per player (`user_data` = player id). Shots/slashes target it; see `pvp.rs`.
pub const GROUP_HITBOX: Group = Group::GROUP_8;
/// What shurikens, slashes and the ultimate can damage.
pub const TARGETS: Group = GROUP_ENEMY.union(GROUP_HITBOX);
/// Solid movement capsule of an NPC enemy (the shootable part is its sensor hitbox in `GROUP_ENEMY`).
pub const GROUP_ENEMY_BODY: Group = Group::GROUP_6;
pub const GROUP_PROJECTILE: Group = Group::GROUP_4;
pub const GROUP_MELEE: Group = Group::GROUP_5;

/// Everything except targets, player bodies and (sensor) light-walls: what blocks shots and slashes.
pub fn solid_groups() -> Group {
    Group::ALL.difference(TARGETS).difference(GROUP_TRAIL).difference(GROUP_PLAYER).difference(GROUP_ENEMY_BODY)
}

/// Groups a projectile may touch: solids + targets (not trails, not player movement capsules).
pub fn projectile_groups() -> Group {
    Group::ALL.difference(GROUP_TRAIL).difference(GROUP_PLAYER).difference(GROUP_ENEMY_BODY)
}

/// Sensors are skipped (light-walls etc.) except player and enemy hitboxes, which are sensors by design.
pub fn passes_projectile(_: ColliderHandle, c: &Collider) -> bool {
    !c.is_sensor() || c.collision_groups().memberships.intersects(GROUP_HITBOX.union(GROUP_ENEMY))
}

pub fn is_target(w: &PhysicsWorld, h: ColliderHandle) -> bool {
    w.colliders.get(h).map_or(false, |c| c.collision_groups().memberships.intersects(TARGETS))
}

/// Yaw 0 faces -Z (same as `CharacterSystem::facing`), positive yaw turns toward -X (CCW about +Y).
/// Network intents carry yaw only, so `pitch` is 0 for intent-driven aim: slashes are horizontal and
/// shurikens fly level. Clients with pitch can set it, but then the server must receive it too.
#[derive(Clone, Copy, Debug)]
pub struct Aim {
    pub origin: Vector<Real>,
    pub yaw: f32,
    pub pitch: f32,
}

impl Aim {
    /// From a network intent: `Aim::level(eye, net::quant::dq_yaw(intent.aim_yaw))`.
    pub fn level(origin: Vector<Real>, yaw: f32) -> Self {
        Self { origin, yaw, pitch: 0.0 }
    }

    pub fn forward(&self) -> Vector<Real> {
        let (sy, cy) = (libm::sinf(self.yaw), libm::cosf(self.yaw));
        let (sp, cp) = (libm::sinf(self.pitch), libm::cosf(self.pitch));
        Vector::new(-sy * cp, sp, -cy * cp)
    }

    pub fn flat_forward(&self) -> Vector<Real> {
        let (sy, cy) = (libm::sinf(self.yaw), libm::cosf(self.yaw));
        Vector::new(-sy, 0.0, -cy)
    }
}

/// One-tick button edges relevant to combat.
#[derive(Clone, Copy, Default, Debug)]
pub struct CombatInput {
    pub fire: bool,
    pub slice: bool,
    /// No wire bit yet (Buttons uses all 8 bits); the game maps a key here.
    pub ultimate: bool,
    /// With `fire`: a charged throw, the MARK star (see `ShurikenCfg::mark_cost`).
    pub mark: bool,
}

impl CombatInput {
    /// From a network intent: `CombatInput::new(b.has(Buttons::FIRE), b.has(Buttons::SLICE))`.
    pub fn new(fire: bool, slice: bool) -> Self {
        Self { fire, slice, ultimate: false, mark: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageKind {
    Shuriken,
    Slash,
    GridBeam,
}

#[derive(Clone, Copy, Debug)]
pub enum CombatEvent {
    /// `target` = enemy collider `user_data`.
    Damage { target: u128, collider: ColliderHandle, amount: f32, kind: DamageKind, point: Point<Real> },
    /// Katana swing. `lunge`: the dash actually fired (false = on cooldown, slash only).
    Slash {
        flip: bool,
        dir: Vector<Real>,
        lunge: bool,
        hits: u32,
        /// Slash volume (for lag-comp sweeps): starts at `origin`, `reach` long after wall truncation.
        origin: Point<Real>,
        reach: f32,
        half_width: f32,
        half_height: f32,
        damage: f32,
    },
    ShurikenThrown { pos: Point<Real>, dir: Vector<Real> },
    /// A MARK star struck `target` (`collider` = its hitbox): no damage, the server marks that player for the katana.
    MarkHit { target: u128, collider: ColliderHandle },
    ShurikenBounce { pos: Point<Real>, normal: Vector<Real> },
    ShurikenGone { pos: Point<Real> },
    /// Pushed just before the `Damage` of a disc that had already bounced when it hit `collider` (ricochet).
    RicochetHit { collider: ColliderHandle },
    /// Feed to `FpsCamera.recoil.kick`.
    Recoil { pitch_deg: f32, yaw_deg: f32 },
    PickedUp { id: u32, kind: powerup::PowerupKind },
    UltimateStart { origin: Point<Real> },
    /// Wrist grapple hooked: rope runs from the eye `origin` to the static `anchor` (docs/grapple.md).
    GrappleAttach { origin: Point<Real>, anchor: Point<Real> },
    /// Grapple fired at nothing hookable (sky, out of range, a player): play the 'fizzle' feedback.
    GrappleMiss { origin: Point<Real>, dir: Vector<Real> },
    /// Rope released (zip again, jump, arrived, timeout, line of sight lost).
    GrappleRelease { pos: Point<Real> },
    /// The katana connected with `target` (player id / NPC id) at `point`. `killed` is filled in by the server once
    /// the damage has been applied (always false in predicted, client-side events).
    SlashHit { target: u128, point: Point<Real>, killed: bool },
    /// The katana swing found nothing: play the whiff (dim arc, small recover).
    SlashMiss { origin: Point<Real>, dir: Vector<Real> },
    /// A weapon struck the player-built piece in `cell` for `amount`. The server turns it into `Blocks::damage`
    /// (which may de-rez the piece); the client predictor ignores it.
    PieceHit { cell: [i32; 3], amount: f32, kind: DamageKind },
}

pub struct CombatSystem {
    pub loadout: weapon::Loadout,
    /// Teammates' hitbox colliders, refreshed by the server every tick (empty = free-for-all).
    pub allies: Vec<ColliderHandle>,
    pub powerups: powerup::PowerupState,
    /// Predator lock-on (docs/lock-on-and-hud.md): same state machine on server and client predictor.
    pub lock: aim_assist::LockState,
    pub lock_cfg: aim_assist::LockCfg,
    /// Cone (degrees) and range a target must be inside to be acquired; from `combat.shuriken.assist_*`.
    pub assist_cone_deg: f32,
    pub assist_range: f32,
    /// Edge of the lock state machine during the most recent `update` (cleared at the start of the next one).
    pub lock_edge: Option<aim_assist::LockEdge>,
}

impl CombatSystem {
    /// Own in-flight projectiles (position, direction) for the renderer.
    pub fn projectiles(&self) -> Vec<([f32; 3], [f32; 3])> {
        let mut v = Vec::new();
        self.loadout.projectiles(&mut v);
        v
    }

    pub fn new(loadout: weapon::Loadout) -> Self {
        let s = shuriken::ShurikenCfg::default();
        Self {
            loadout,
            allies: Vec::new(),
            powerups: powerup::PowerupState::default(),
            lock: aim_assist::LockState::default(),
            lock_cfg: aim_assist::LockCfg::default(),
            assist_cone_deg: s.assist_cone_deg,
            assist_range: s.assist_range,
            lock_edge: None,
        }
    }

    /// LMB/fire = neon shurikens, slice = cyber-katana.
    pub fn ninja(ch: &crate::character::Cfg) -> Self {
        Self::new(
            weapon::Loadout::default()
                .with(shuriken::ShurikenLauncher::default())
                .with(katana::Katana::new(katana::KatanaCfg::for_character(ch))),
        )
    }

    /// Load tunables from a `Balance` (weapons + ultimate).
    pub fn apply_balance(&mut self, b: &crate::balance::Balance, ch: &crate::character::Cfg) {
        self.loadout.apply_balance(b, ch);
        self.lock_cfg = aim_assist::LockCfg { lock_secs: b.combat.lock_secs, pull_min: b.combat.lock_pull_min, break_secs: b.combat.lock_break_secs, blend_secs: b.combat.lock_blend_secs };
        self.assist_cone_deg = b.combat.shuriken.assist_cone_deg;
        self.assist_range = b.combat.shuriken.assist_range;
        self.powerups.ultimate = crate::combat::powerup::UltimateCfg {
            speed: b.combat.ultimate_speed,
            max_radius: b.combat.ultimate_max_radius,
            damage: b.combat.ultimate_damage,
        };
    }

    /// Star energy as (charges, pool size, locked out); `None` when the balance has no pool.
    pub fn shuriken_energy(&self) -> Option<(f32, f32, bool)> {
        self.loadout.energy()
    }

    /// Katana (secondary trigger) cooldown left, seconds. Mirrored into `CharacterSystem::slice_cooldown`
    /// after every combat tick so it travels in `OwnState` and the client can predict the lunge.
    pub fn slice_cooldown_left(&self) -> f32 {
        self.loadout.cooldown_left(weapon::Trigger::Secondary)
    }

    pub fn set_slice_cooldown_left(&mut self, v: f32) {
        self.loadout.set_cooldown_left(weapon::Trigger::Secondary, v);
    }

    /// True while this player's combat will query the world this tick regardless of input (discs / bursts
    /// in flight). Lets the server skip lag-comp work for idle players.
    pub fn busy(&self) -> bool {
        self.loadout.active() || !self.powerups.bursts().is_empty()
    }

    /// Owner died: drop everything in flight.
    pub fn reset_projectiles(&mut self) {
        self.loadout.reset();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        dt: f32,
        now: f32,
        input: &CombatInput,
        aim: &Aim,
        world: &PhysicsWorld,
        ch: &CharacterSystem,
        player_input: &mut PlayerInput,
        events: &mut Vec<CombatEvent>,
    ) {
        // lock first: one best_target per tick feeds the lock state and every weapon fired this tick
        let (eye, fwd) = (aim.origin, aim.forward());
        let held = self.lock.target.filter(|_| self.lock.holding());
        let found = aim_assist::best_target_held(world, eye, fwd, self.assist_cone_deg, self.assist_range, &self.allies, ch.body, held);
        self.lock_edge = self.lock.update(dt, &self.lock_cfg, found.as_ref());
        let hint = found.filter(|_| self.lock.holding()).map(|t| aim_assist::LockHint { dir: t.dir, pull: self.lock.pull(&self.lock_cfg), collider: t.collider, centre: t.centre, vel: self.lock.target_vel });
        self.loadout.update(dt, now, input, aim, world, ch, player_input, events, &self.allies, hint);
        self.powerups.update(dt, input, aim, world, ch.body, events);
    }
}

#[cfg(test)]
mod tests;
