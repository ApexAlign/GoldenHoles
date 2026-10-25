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
    /// Server-side facts for the katana moves (counter target, shield just lowered); empty on the client.
    pub moves: super::MoveCtx,
    /// The press carried the MARK flag (a charged throw): the shuriken launcher throws a mark star instead.
    pub mark: bool,
    /// Which throwable a plain press sends (`StarKind::Standard` unless the player cycled to a variant).
    pub star: super::stars::StarKind,
    /// Flow streak damage multiplier (1 = no bonus; `combat::flow`, docs/traversal_melee.md "Flow"). The katana
    /// stacks it on top of its own combo / strike bonuses.
    pub flow_dmg_mult: f32,
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
    /// Seconds of recovery for the use that was just made (a heavy cut recovers slower than a light one).
    fn cooldown_after(&self) -> f32 {
        self.cooldown()
    }
    /// Does the trigger fire this tick? `pressed` is the raw button level, `ready` = the cooldown is over. The default is
    /// "held and ready" (auto-repeat); the katana overrides it with press / hold-and-release logic.
    fn gate(&mut self, pressed: bool, _dt: f32, ready: bool) -> bool {
        pressed && ready
    }
    /// Seconds left on a queued (too early) press; 0 for weapons without a press buffer.
    fn buffered(&self) -> f32 {
        0.0
    }
    /// Overwrite the queued press (client replay restores it from the server's own-state).
    fn set_buffered(&mut self, _v: f32) {}
    /// 0..1 charge of a held trigger (HUD ring); 0 for weapons that do not charge.
    fn charge(&self) -> f32 {
        0.0
    }
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
    /// Mutable access to the in-flight discs (the server's star pass detonates / disarms landed mines).
    fn discs_mut(&mut self) -> Option<&mut Vec<super::shuriken::Disc>> {
        None
    }
    /// Skill combo counter (connected cuts in a row) for the HUD; 0 for weapons without one.
    fn combo_count(&self) -> u32 {
        0
    }
    /// The owner took damage: break any streak.
    fn on_hurt(&mut self) {}
    /// Star energy a cancel out of this weapon's recovery costs (negative = cancels off).
    fn cancel_cost(&self) -> f32 {
        -1.0
    }
    /// Spend `amount` of energy if this weapon has a pool and can afford it; true when it was paid (or there is no pool).
    fn spend_energy(&mut self, _amount: f32) -> bool {
        true
    }
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
    /// Handed to every weapon's `FireCtx` (set by the server each tick).
    pub moves: super::MoveCtx,
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

    /// Combo counter of the secondary (katana) weapon.
    pub fn combo_count(&self) -> u32 {
        self.slots.iter().find(|s| s.weapon.trigger() == Trigger::Secondary).map_or(0, |s| s.weapon.combo_count())
    }

    /// The owner was hit: every weapon drops its streak.
    pub fn on_hurt(&mut self) {
        for s in &mut self.slots {
            s.weapon.on_hurt();
        }
    }

    /// Cancel: free the secondary weapon's recovery now, for its `cancel_cost` in energy from the first weapon with a pool
    /// (no pool = free). False (nothing changes) when there is no recovery to cut, cancels are off or it cannot be paid.
    pub fn cancel_slice(&mut self) -> bool {
        let Some(ki) = self.slots.iter().position(|s| s.weapon.trigger() == Trigger::Secondary && s.cooldown_left > 0.0) else { return false };
        let cost = self.slots[ki].weapon.cancel_cost();
        if cost < 0.0 {
            return false;
        }
        if cost > 0.0 {
            if let Some(s) = self.slots.iter_mut().find(|s| s.weapon.energy().is_some()) {
                if !s.weapon.spend_energy(cost) {
                    return false;
                }
            }
        }
        self.slots[ki].cooldown_left = 0.0;
        true
    }

    /// Queued press (seconds left) of the first weapon on `t`.
    pub fn buffered(&self, t: Trigger) -> f32 {
        self.slots.iter().find(|s| s.weapon.trigger() == t).map_or(0.0, |s| s.weapon.buffered())
    }

    /// Overwrite the queued press of every weapon on `t`.
    pub fn set_buffered(&mut self, t: Trigger, v: f32) {
        for s in self.slots.iter_mut().filter(|s| s.weapon.trigger() == t) {
            s.weapon.set_buffered(v);
        }
    }

    /// Charge (0..1) of the first weapon on `t`.
    pub fn charge(&self, t: Trigger) -> f32 {
        self.slots.iter().find(|s| s.weapon.trigger() == t).map_or(0.0, |s| s.weapon.charge())
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

    /// The star launcher's discs, mutable (server star pass).
    pub fn discs_mut(&mut self) -> Option<&mut Vec<super::shuriken::Disc>> {
        self.slots.iter_mut().find_map(|s| s.weapon.discs_mut())
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
        flow_dmg_mult: f32,
    ) {
        for s in &mut self.slots {
            s.cooldown_left = (s.cooldown_left - dt).max(0.0);
            s.weapon.tick(dt, &mut TickCtx { world, body: ch.body, events, allies });

            let pressed = match s.weapon.trigger() {
                Trigger::Primary => input.fire,
                Trigger::Secondary => input.slice,
            };
            if s.weapon.gate(pressed, dt, s.cooldown_left <= 0.0) {
                let mut ctx = FireCtx { world, ch, aim, now, input: player_input, events, allies, lock, moves: self.moves, mark: input.mark && s.weapon.trigger() == Trigger::Primary, star: input.star, flow_dmg_mult };
                if s.weapon.fire(&mut ctx) {
                    s.cooldown_left = s.weapon.cooldown_after();
                }
            }
        }
    }
}
