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

pub mod katana;
pub mod powerup;
pub mod shuriken;
pub mod viewmodel;
pub mod weapon;

pub const GROUP_WORLD: Group = Group::GROUP_1;
pub const GROUP_PLAYER: Group = Group::GROUP_7;
pub const GROUP_PROJECTILE: Group = Group::GROUP_4;
pub const GROUP_MELEE: Group = Group::GROUP_5;

/// Everything except enemies and (sensor) light-walls: what blocks shots and slashes.
pub fn solid_groups() -> Group {
    Group::ALL.difference(GROUP_ENEMY).difference(GROUP_TRAIL)
}

pub fn is_enemy(w: &PhysicsWorld, h: ColliderHandle) -> bool {
    w.colliders.get(h).map_or(false, |c| c.collision_groups().memberships.contains(GROUP_ENEMY))
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
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vector::new(-sy * cp, sp, -cy * cp)
    }

    pub fn flat_forward(&self) -> Vector<Real> {
        let (sy, cy) = self.yaw.sin_cos();
        Vector::new(-sy, 0.0, -cy)
    }
}

/// One-tick button edges relevant to combat.
#[derive(Clone, Copy, Default, Debug)]
pub struct CombatInput {
    pub fire: bool,
    pub slice: bool,
    /// No wire bit yet (Buttons uses 6 of 8 bits); the game maps a key here.
    pub ultimate: bool,
}

impl CombatInput {
    /// From a network intent: `CombatInput::new(b.has(Buttons::FIRE), b.has(Buttons::SLICE))`.
    pub fn new(fire: bool, slice: bool) -> Self {
        Self { fire, slice, ultimate: false }
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
    Slash { flip: bool, dir: Vector<Real>, lunge: bool, hits: u32 },
    ShurikenThrown { pos: Point<Real>, dir: Vector<Real> },
    ShurikenBounce { pos: Point<Real>, normal: Vector<Real> },
    ShurikenGone { pos: Point<Real> },
    /// Feed to `FpsCamera.recoil.kick`.
    Recoil { pitch_deg: f32, yaw_deg: f32 },
    PickedUp { id: u32, kind: powerup::PowerupKind },
    UltimateStart { origin: Point<Real> },
}

pub struct CombatSystem {
    pub loadout: weapon::Loadout,
    pub powerups: powerup::PowerupState,
}

impl CombatSystem {
    pub fn new(loadout: weapon::Loadout) -> Self {
        Self { loadout, powerups: powerup::PowerupState::default() }
    }

    /// LMB/fire = neon shurikens, slice = cyber-katana.
    pub fn ninja(ch: &crate::character::Cfg) -> Self {
        Self::new(
            weapon::Loadout::default()
                .with(shuriken::ShurikenLauncher::default())
                .with(katana::Katana::new(katana::KatanaCfg::for_character(ch))),
        )
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
        self.loadout.update(dt, now, input, aim, world, ch, player_input, events);
        self.powerups.update(dt, input, aim, world, ch.body, events);
    }
}

#[cfg(test)]
mod tests;
