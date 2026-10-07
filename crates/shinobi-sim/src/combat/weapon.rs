use rapier3d::prelude::*;

use super::{Aim, CombatEvent, CombatInput};
use crate::character::{CharacterSystem, PhysicsWorld, PlayerInput};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    Primary,
    Secondary,
}

/// Read-only world + output channels handed to a weapon when it fires.
/// `input` is the player's pending `PlayerInput`: weapons that move the player (katana lunge)
/// drive the existing `CharacterSystem` dash through it instead of pushing the body.
pub struct FireCtx<'a> {
    pub world: &'a PhysicsWorld,
    pub ch: &'a CharacterSystem,
    pub aim: &'a Aim,
    pub now: f32,
    pub input: &'a mut PlayerInput,
    pub events: &'a mut Vec<CombatEvent>,
    /// Hitboxes of the owner's teammates: neither blades nor discs touch them (friendly fire off).
    pub allies: &'a [ColliderHandle],
    /// Current lock-on target and pull strength (None while searching / breaking).
    pub lock: Option<super::aim_assist::LockHint>,
    /// The press carried the MARK flag (a charged throw): the shuriken launcher throws a mark star instead.
    pub mark: bool,
}

pub struct TickCtx<'a> {
    pub world: &'a PhysicsWorld,
    pub body: RigidBodyHandle,
    pub events: &'a mut Vec<CombatEvent>,
    pub allies: &'a [ColliderHandle],
}

/// Modular weapon: owns its own state (projectiles, combo flip, ...).
pub trait Weapon: Send + Sync {
    fn name(&self) -> &'static str;
    fn trigger(&self) -> Trigger;
    /// Seconds between uses.
    fn cooldown(&self) -> f32;
    /// Return false to refuse (cooldown not consumed).
    fn fire(&mut self, ctx: &mut FireCtx) -> bool;
    /// Re-read tunables (called when the server loads a `Balance`).
    fn apply_balance(&mut self, _b: &crate::balance::Balance, _ch: &crate::character::Cfg) {}
    /// Star energy as (charges, pool size, locked out), for weapons that have a pool.
    fn energy(&self) -> Option<(f32, f32, bool)> {
        None
    }
    /// Anything in flight that will query the world on later ticks?
    fn active(&self) -> bool {
        false
    }
    /// In-flight projectiles as (position, direction) for rendering.
    fn projectiles(&self, _out: &mut Vec<([f32; 3], [f32; 3])>) {}
    /// Drop in-flight state (owner died).
    fn reset(&mut self) {}
    /// Append in-flight discs (for snapshots / rendering).
    fn collect_discs(&self, _out: &mut Vec<super::shuriken::Disc>) {}
    /// Advance in-flight state (projectiles). Runs every tick.
    fn tick(&mut self, _dt: f32, _ctx: &mut TickCtx) {}
}

struct Slot {
    weapon: Box<dyn Weapon>,
    cooldown_left: f32,
}

#[derive(Default)]
pub struct Loadout {
    slots: Vec<Slot>,
}

impl Loadout {
    pub fn with(mut self, w: impl Weapon + 'static) -> Self {
        self.slots.push(Slot { weapon: Box::new(w), cooldown_left: 0.0 });
        self
    }

    pub fn active(&self) -> bool {
        self.slots.iter().any(|s| s.weapon.active())
    }

    /// The first weapon's energy pool, if any.
    pub fn energy(&self) -> Option<(f32, f32, bool)> {
        self.slots.iter().find_map(|s| s.weapon.energy())
    }

    /// Remaining cooldown (seconds) of the first weapon on `t`; 0 = ready.
    pub fn cooldown_left(&self, t: Trigger) -> f32 {
        self.slots.iter().find(|s| s.weapon.trigger() == t).map_or(0.0, |s| s.cooldown_left)
    }

    /// Overwrite that cooldown (client replay restores it from the server's own-state).
    pub fn set_cooldown_left(&mut self, t: Trigger, v: f32) {
        for s in self.slots.iter_mut().filter(|s| s.weapon.trigger() == t) {
            s.cooldown_left = v;
        }
    }

    pub fn apply_balance(&mut self, b: &crate::balance::Balance, ch: &crate::character::Cfg) {
        for s in &mut self.slots {
            s.weapon.apply_balance(b, ch);
        }
    }

    /// Every in-flight disc of this loadout.
    pub fn discs(&self) -> Vec<super::shuriken::Disc> {
        let mut v = Vec::new();
        for s in &self.slots {
            s.weapon.collect_discs(&mut v);
        }
        v
    }

    /// Owner died: nothing they threw may keep flying (or resume on respawn).
    pub fn projectiles(&self, out: &mut Vec<([f32; 3], [f32; 3])>) {
        for s in &self.slots {
            s.weapon.projectiles(out);
        }
    }

    pub fn reset(&mut self) {
        for s in &mut self.slots {
            s.weapon.reset();
        }
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
        allies: &[ColliderHandle],
        lock: Option<super::aim_assist::LockHint>,
    ) {
        for s in &mut self.slots {
            s.cooldown_left = (s.cooldown_left - dt).max(0.0);
            s.weapon.tick(dt, &mut TickCtx { world, body: ch.body, events, allies });

            let pressed = match s.weapon.trigger() {
                Trigger::Primary => input.fire,
                Trigger::Secondary => input.slice,
            };
            if pressed && s.cooldown_left <= 0.0 {
                let mut ctx = FireCtx { world, ch, aim, now, input: player_input, events, allies, lock, mark: input.mark && s.weapon.trigger() == Trigger::Primary };
                if s.weapon.fire(&mut ctx) {
                    s.cooldown_left = s.weapon.cooldown();
                }
            }
        }
    }
}
