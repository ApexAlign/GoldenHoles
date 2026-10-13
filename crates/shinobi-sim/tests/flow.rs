//! Flow (docs/traversal_melee.md "Flow"): a streak of chained traversal moves run through `CombatSystem::update`,
//! the same wiring the server and the client predictor use. The streak logic itself (tiers, resets) is unit-tested
//! in `combat/flow.rs`; this file checks the integration points: `CharacterSystem`'s age counters / grapple event
//! actually drive the streak, the streak actually writes `PlayerInput::speed_boost`, and it actually stacks a
//! damage bonus onto a katana hit.
//!
//! `CharacterSystem::update` is never stepped here (only `CombatSystem::update`, which reads it): the age fields
//! are poked directly, one "move" per tick, then put back out of range (as `CharacterSystem::update` itself would
//! a tick later by incrementing them past the `<= 1e-6` "just happened" window).

use shinobi_sim::balance::Balance;
use shinobi_sim::character::{Cfg, CharacterSystem, GrappleEvent, MoveState, PhysicsWorld, PlayerInput, GROUP_ENEMY};
use shinobi_sim::combat::{Aim, CombatEvent, CombatInput, CombatSystem, DamageKind};
use shinobi_sim::rapier3d::prelude::*;

const DT: f32 = 1.0 / 60.0;

/// Flow on (2 / 4 / 6 moves -> +3 / +6 / +10 % speed, +5 / +10 / +15 % katana damage), everything else neutral
/// (no combo bonus, no strike bonus) so the flow effect is isolated.
fn flow_balance() -> Balance {
    let mut b = Balance::default();
    b.combat.flow.tier1_moves = 2;
    b.combat.flow.tier2_moves = 4;
    b.combat.flow.tier3_moves = 6;
    b.combat.flow.tier1_speed = 1.03;
    b.combat.flow.tier2_speed = 1.06;
    b.combat.flow.tier3_speed = 1.10;
    b.combat.flow.tier1_dmg = 0.05;
    b.combat.flow.tier2_dmg = 0.10;
    b.combat.flow.tier3_dmg = 0.15;
    b.combat.flow.stop_speed = 1.0;
    b.combat.katana.damage = 20.0;
    b
}

struct Rig {
    w: PhysicsWorld,
    ch: CharacterSystem,
    combat: CombatSystem,
    now: f32,
}

impl Rig {
    /// Grounded at the origin, a target 1.6 m ahead (for the cut tests); the streak tests move it airborne.
    fn new(b: &Balance) -> Self {
        let cfg = Cfg::default();
        let mut w = PhysicsWorld { bodies: RigidBodySet::new(), colliders: ColliderSet::new(), islands: IslandManager::new(), queries: QueryPipeline::new() };
        let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![0.0, 0.81, 0.0]).build());
        w.colliders.insert(
            ColliderBuilder::cuboid(0.4, 0.9, 0.4)
                .translation(vector![0.0, 0.81, -1.6])
                .collision_groups(InteractionGroups::new(GROUP_ENEMY, Group::ALL))
                .user_data(7)
                .build(),
        );
        let mut ch = CharacterSystem::new(body, cfg);
        ch.state = MoveState::Grounded;
        let mut combat = CombatSystem::ninja(&cfg);
        combat.apply_balance(b, &cfg);
        Self { w, ch, combat, now: 0.0 }
    }

    /// One combat tick, no buttons.
    fn tick(&mut self) -> PlayerInput {
        self.w.queries.update(&self.w.colliders);
        let aim = Aim { origin: vector![0.0, 0.81, 0.0], yaw: 0.0, pitch: 0.0 };
        let mut input = PlayerInput::default();
        let mut ev = Vec::new();
        self.combat.update(DT, self.now, &CombatInput::new(false, false), &aim, &self.w, &self.ch, &mut input, &mut ev);
        self.now += DT;
        input
    }

    /// A cut (release, click), reporting the damage of the hit that connected.
    fn cut(&mut self) -> Option<f32> {
        self.w.queries.update(&self.w.colliders);
        let aim = Aim { origin: vector![0.0, 0.81, 0.0], yaw: 0.0, pitch: 0.0 };
        let mut input = PlayerInput::default();
        let mut ev = Vec::new();
        self.combat.update(DT, self.now, &CombatInput::new(false, true), &aim, &self.w, &self.ch, &mut input, &mut ev);
        self.now += DT;
        ev.iter().find_map(|e| match e {
            CombatEvent::Damage { amount, kind: DamageKind::Slash, .. } => Some(*amount),
            _ => None,
        })
    }

    /// A fresh wall-run entry: one tick with `wallrun_age` at 0, then aged back out of the "just happened" window.
    fn wallrun_move(&mut self) -> PlayerInput {
        self.ch.wallrun_age = 0.0;
        let i = self.tick();
        self.ch.wallrun_age = 99.0;
        i
    }

    /// A dash (not a ground roll).
    fn dash_move(&mut self) -> PlayerInput {
        self.ch.dash_age = 0.0;
        let i = self.tick();
        self.ch.dash_age = 99.0;
        i
    }

    /// A hook attach.
    fn hook_move(&mut self) -> PlayerInput {
        self.ch.grapple_event = GrappleEvent::Attached { origin: vector![0.0, 0.0, 0.0], anchor: vector![0.0, 0.0, -5.0] };
        let i = self.tick();
        self.ch.grapple_event = GrappleEvent::None;
        i
    }

    /// A wall-jump.
    fn walljump_move(&mut self) -> PlayerInput {
        self.ch.walljump_age = 0.0;
        let i = self.tick();
        self.ch.walljump_age = 99.0;
        i
    }
}

#[test]
fn chaining_wall_run_hook_dash_and_wall_jump_builds_the_streak_and_the_speed_tier() {
    let mut r = Rig::new(&flow_balance());
    r.ch.state = MoveState::Airborne; // a flow chain never touches the ground
    assert_eq!(r.combat.flow_streak(), 0);

    let i = r.wallrun_move();
    assert_eq!(r.combat.flow_streak(), 1);
    assert_eq!(i.speed_boost, 0.0, "tier 1 needs 2 moves");

    let i = r.hook_move();
    assert_eq!(r.combat.flow_streak(), 2);
    assert!((i.speed_boost - 0.03).abs() < 1e-5, "tier 1: +3 % speed, got {}", i.speed_boost);

    r.dash_move();
    let i = r.walljump_move();
    assert_eq!(r.combat.flow_streak(), 4);
    assert!((i.speed_boost - 0.06).abs() < 1e-5, "tier 2: +6 % speed, got {}", i.speed_boost);

    r.dash_move();
    let i = r.dash_move();
    assert_eq!(r.combat.flow_streak(), 6);
    assert!((i.speed_boost - 0.10).abs() < 1e-5, "tier 3: +10 % speed, got {}", i.speed_boost);
}

#[test]
fn a_full_ground_stop_resets_the_streak_but_a_fast_landing_roll_does_not() {
    let mut r = Rig::new(&flow_balance());
    r.ch.state = MoveState::Airborne;
    r.dash_move();
    r.dash_move();
    assert_eq!(r.combat.flow_streak(), 2);

    // grounded but still carrying speed (a landing roll into the next move): no reset
    r.ch.state = MoveState::Grounded;
    r.ch.vel = vector![8.0, 0.0, 0.0];
    r.tick();
    assert_eq!(r.combat.flow_streak(), 2, "a fast landing keeps the streak");

    // grounded and stopped dead: the streak dies
    r.ch.vel = vector![0.1, 0.0, 0.0];
    r.tick();
    assert_eq!(r.combat.flow_streak(), 0, "a full stop resets it");
}

#[test]
fn taking_a_hit_resets_the_streak() {
    let mut r = Rig::new(&flow_balance());
    r.ch.state = MoveState::Airborne;
    r.dash_move();
    r.dash_move();
    assert_eq!(r.combat.flow_streak(), 2);
    r.combat.on_hurt();
    assert_eq!(r.combat.flow_streak(), 0);
    let i = r.tick();
    assert_eq!(i.speed_boost, 0.0);
}

#[test]
fn the_flow_damage_bonus_stacks_onto_a_landed_katana_hit() {
    let mut off = Rig::new(&flow_balance());
    let base = off.cut().unwrap();
    assert_eq!(base, 20.0, "no flow yet: plain damage");

    let mut on = Rig::new(&flow_balance());
    on.ch.state = MoveState::Airborne;
    on.dash_move();
    on.dash_move();
    assert_eq!(on.combat.flow_streak(), 2, "tier 1 active");
    // land for a plain (non-air) cut, same as `off`, but with a landing-roll's speed so the ground-stop check
    // (this very tick) does not wipe the streak before the cut reads it
    on.ch.state = MoveState::Grounded;
    on.ch.vel = vector![5.0, 0.0, 0.0];
    let boosted = on.cut().unwrap();
    assert!((boosted - 21.0).abs() < 1e-3, "tier 1: +5 % katana damage, got {boosted}");
}

#[test]
fn flow_is_off_by_default() {
    let cfg = Cfg::default();
    let mut combat = CombatSystem::ninja(&cfg);
    combat.apply_balance(&Balance::default(), &cfg);
    assert_eq!(combat.flow_tier(), 0);
}
