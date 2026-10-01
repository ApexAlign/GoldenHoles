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
}

pub struct TickCtx<'a> {
    pub world: &'a PhysicsWorld,
    pub body: RigidBodyHandle,
    pub events: &'a mut Vec<CombatEvent>,
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
    /// Anything in flight that will query the world on later ticks?
    fn active(&self) -> bool {
        false
    }
    /// Shuriken discs in flight (renderer / replication); empty for weapons without projectiles.
    fn discs(&self) -> &[super::shuriken::Disc] {
        &[]
    }
    /// Drop in-flight state (owner died).
    fn reset(&mut self) {}
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

    /// Every disc in flight across the loadout.
    pub fn discs(&self) -> impl Iterator<Item = &super::shuriken::Disc> {
        self.slots.iter().flat_map(|s| s.weapon.discs())
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

    /// Owner died: nothing they threw may keep flying (or resume on respawn).
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
    ) {
        for s in &mut self.slots {
            s.cooldown_left = (s.cooldown_left - dt).max(0.0);
            s.weapon.tick(dt, &mut TickCtx { world, body: ch.body, events });

            let pressed = match s.weapon.trigger() {
                Trigger::Primary => input.fire,
                Trigger::Secondary => input.slice,
            };
            if pressed && s.cooldown_left <= 0.0 {
                let mut ctx = FireCtx { world, ch, aim, now, input: player_input, events };
                if s.weapon.fire(&mut ctx) {
                    s.cooldown_left = s.weapon.cooldown();
                }
            }
        }
    }
}
