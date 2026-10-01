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
