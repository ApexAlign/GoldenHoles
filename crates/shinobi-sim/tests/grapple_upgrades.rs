//! Grapple upgrades (docs/grapple.md): anchor assist, enemy hook (pull + stagger, shield), slingshot release, break
//! release, chained hooks.

use std::time::Duration;

use shinobi_proto::intent::{ClientPacket, PlayerIntent};
use shinobi_sim::balance::Balance;
use shinobi_sim::character::{Cfg, CharacterSystem, PhysicsWorld, PlayerInput};
use shinobi_sim::movement::step_player;
use shinobi_sim::rapier3d::prelude::*;
use shinobi_sim::tick::Server;

const DT: f32 = 1.0 / 60.0;
const FRAME: Duration = Duration::from_nanos(16_666_667);

fn world() -> PhysicsWorld {
    PhysicsWorld { bodies: RigidBodySet::new(), colliders: ColliderSet::new(), islands: IslandManager::new(), queries: QueryPipeline::new() }
}

fn boxc(w: &mut PhysicsWorld, c: [f32; 3], h: [f32; 3]) -> ColliderHandle {
    w.colliders.insert(ColliderBuilder::cuboid(h[0], h[1], h[2]).translation(vector![c[0], c[1], c[2]]).build())
}

fn player(w: &mut PhysicsWorld, cfg: Cfg, at: [f32; 3]) -> CharacterSystem {
    let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![at[0], at[1], at[2]]).build());
    w.colliders.insert_with_parent(ColliderBuilder::capsule_y(cfg.half_height, cfg.radius).build(), body, &mut w.bodies);
    CharacterSystem::new(body, cfg)
}

fn tick(ch: &mut CharacterSystem, w: &mut PhysicsWorld, i: &PlayerInput, now: &mut f32) {
    w.queries.update(&w.colliders);
    step_player(ch, i, w, *now, DT);
    *now += DT;
}

fn fire(dir: [f32; 3]) -> PlayerInput {
    PlayerInput { grapple_aim: Some(vector![dir[0], dir[1], dir[2]]), ..Default::default() }
}

fn idle() -> PlayerInput {
    PlayerInput::default()
}

// ---- 1) anchor assist -------------------------------------------------------------------------------------------

/// A tower 20 m away (front face z = -20, roof y = 8).
fn rim_scene(cfg: Cfg) -> (PhysicsWorld, CharacterSystem) {
    let mut w = world();
    boxc(&mut w, [0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
    boxc(&mut w, [0.0, 4.0, -23.0], [10.0, 4.0, 3.0]);
    let ch = player(&mut w, cfg, [0.0, 0.9, 0.0]);
    (w, ch)
}

/// Aim 3 degrees over the rim: a straight ray sails over the roof edge into the sky.
fn over_rim() -> [f32; 3] {
    let (dy, dz) = (8.0f32 - 1.7, 20.0f32);
    let a = dy.atan2(dz) + 3.0f32.to_radians();
    [0.0, a.sin(), -a.cos()]
}

#[test]
fn assist_snaps_a_near_miss_onto_the_rim_and_off_is_a_miss() {
    let mut cfg = Cfg::default();
    cfg.grapple_assist_deg = 0.0;
    let (mut w, mut ch) = rim_scene(cfg);
    let mut now = 0.0;
    tick(&mut ch, &mut w, &fire(over_rim()), &mut now);
    assert!(ch.grapple.is_none(), "no assist: a miss");

    let (mut w, mut ch) = rim_scene(Cfg::default());
    let mut now = 0.0;
    tick(&mut ch, &mut w, &fire(over_rim()), &mut now);
    let g = ch.grapple.expect("assist hooks the tower");
    eprintln!("assist anchor {:?}", g.anchor);
    assert!(g.anchor.y > 5.0, "near the rim (edge / top preferred), not the foot: {:?}", g.anchor);
    assert!(g.anchor.z > -26.1 && g.anchor.y < 8.1, "on the tower: {:?}", g.anchor);
}

#[test]
fn assist_does_not_reach_past_its_cone() {
    let (mut w, mut ch) = rim_scene(Cfg::default());
    let mut now = 0.0;
    tick(&mut ch, &mut w, &fire([0.0, 1.0, 0.0]), &mut now);
    assert!(ch.grapple.is_none());
}

#[test]
fn assist_is_deterministic_for_prediction_parity() {
    let dir = vector![0.0, 0.33, -0.94];
    let (mut w1, c1) = rim_scene(Cfg::default());
    let (mut w2, c2) = rim_scene(Cfg::default());
    w1.queries.update(&w1.colliders);
    w2.queries.update(&w2.colliders);
    let e = vector![0.0, 1.7, 0.0];
    assert_eq!(c1.grapple_target(&w1, e, dir), c2.grapple_target(&w2, e, dir));
}

// ---- 3) momentum + chaining -------------------------------------------------------------------------------------

#[test]
fn clean_release_cooldown_allows_chained_hooks() {
    let cfg = Cfg::default();
    assert!(cfg.grapple_cd <= 0.45 && cfg.grapple_cd > 0.0);
    let mut w = world();
    boxc(&mut w, [0.0, 30.0, -40.0], [30.0, 40.0, 1.0]);
    boxc(&mut w, [0.0, -50.0, 0.0], [200.0, 0.5, 200.0]);
    let mut ch = player(&mut w, cfg, [0.0, 5.0, 0.0]);
    let mut now = 0.0;
    tick(&mut ch, &mut w, &fire([0.0, 0.0, -1.0]), &mut now);
    assert!(ch.grapple.is_some());
    for _ in 0..8 {
        tick(&mut ch, &mut w, &idle(), &mut now);
    }
    tick(&mut ch, &mut w, &fire([0.0, 0.0, -1.0]), &mut now); // hook key again = release
    assert!(ch.grapple.is_none());
    let cd0 = ch.grapple_cooldown();
    assert!((cd0 - cfg.grapple_cd).abs() < 0.02, "clean release cd {cd0}");
    // too early: still cooling, no hook
    for _ in 0..10 {
        tick(&mut ch, &mut w, &idle(), &mut now);
    }
    tick(&mut ch, &mut w, &fire([0.0, 0.0, -1.0]), &mut now);
    assert!(ch.grapple.is_none(), "still cooling");
    // after the cooldown it chains
    for _ in 0..40 {
        tick(&mut ch, &mut w, &idle(), &mut now);
    }
    tick(&mut ch, &mut w, &fire([0.0, 0.0, -1.0]), &mut now);
    assert!(ch.grapple.is_some(), "second hook chains");
}

#[test]
fn a_jump_release_slingshots_past_the_plain_reel_speed() {
    let mut w = world();
    boxc(&mut w, [0.0, 30.0, -60.0], [60.0, 60.0, 1.0]);
    boxc(&mut w, [0.0, -50.0, 0.0], [200.0, 0.5, 200.0]);
    let mut ch = player(&mut w, Cfg::default(), [0.0, 5.0, 0.0]);
    let mut now = 0.0;
    tick(&mut ch, &mut w, &fire([0.0, 0.2, -1.0]), &mut now);
    for _ in 0..40 {
        tick(&mut ch, &mut w, &idle(), &mut now);
    }
    assert!(ch.grapple.is_some());
    let before = ch.vel.norm();
    let jump = PlayerInput { jump_pressed: true, jump_held: true, ..Default::default() };
    tick(&mut ch, &mut w, &jump, &mut now);
    assert!(ch.grapple.is_none());
    eprintln!("sling: {before:.1} -> {:.1} m/s", ch.vel.norm());
    assert!(ch.vel.norm() > before, "release adds momentum ({before} -> {})", ch.vel.norm());
}

// ---- 4) break release -------------------------------------------------------------------------------------------

#[test]
fn a_broken_anchor_releases_with_momentum_and_a_rez_does_not_catch_the_rope() {
    let mut w = world();
    let wall = boxc(&mut w, [0.0, 30.0, -60.0], [60.0, 60.0, 1.0]);
    boxc(&mut w, [0.0, -50.0, 0.0], [200.0, 0.5, 200.0]);
    let mut ch = player(&mut w, Cfg::default(), [0.0, 5.0, 0.0]);
    let mut now = 0.0;
    tick(&mut ch, &mut w, &fire([0.0, 0.0, -1.0]), &mut now);
    for _ in 0..25 {
        tick(&mut ch, &mut w, &idle(), &mut now);
    }
    assert!(ch.grapple.is_some());
    let speed = ch.vel.norm();
    assert!(speed > 15.0, "reeling: {speed}");
    let removed = w.colliders.remove(wall, &mut w.islands, &mut w.bodies, false).expect("box removed");
    tick(&mut ch, &mut w, &idle(), &mut now);
    assert!(ch.grapple.is_none(), "rope let go when the box broke");
    assert!(ch.vel.norm() >= speed * 0.95, "momentum kept: {speed} -> {}", ch.vel.norm());
    assert!((ch.grapple_cooldown() - Cfg::default().grapple_cd).abs() < 0.02);
    // the box re-forms: no rope comes back
    w.colliders.insert(removed);
    for _ in 0..10 {
        tick(&mut ch, &mut w, &idle(), &mut now);
    }
    assert!(ch.grapple.is_none());
}

// ---- 2) enemy hook ----------------------------------------------------------------------------------------------

fn balance() -> Balance {
    let mut b = Balance::demo();
    b.melee = shinobi_sim::balance::MeleeBalance::default();
    b
}

fn send(s: &mut Server, id: u16, it: PlayerIntent) {
    let t = s.tick;
    s.ingest(id, &ClientPacket { intents: vec![PlayerIntent { tick: t, ..it }], ..Default::default() }.encode().unwrap());
}

fn put(s: &mut Server, id: u16, at: [f32; 3]) {
    let i = s.players.iter().position(|p| p.id == id).unwrap();
    let b = s.players[i].ch.body;
    s.world.bodies[b].set_position(Isometry::translation(at[0], at[1], at[2]), true);
    shinobi_sim::character::sync_body_colliders(&mut s.world, b);
}

fn pos(s: &Server, id: u16) -> [f32; 3] {
    s.players.iter().find(|p| p.id == id).unwrap().pos(&s.world)
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn scene() -> Server {
    let mut s = Server::new(Cfg::default());
    s.set_balance(balance()).unwrap();
    s.add_static_box([0.0, -0.5, 0.0], [200.0, 0.5, 200.0]);
    s.add_static_box([0.0, 20.0, -81.0], [60.0, 20.0, 1.0]);
    s.add_player(1, [0.0, 0.81, 0.0]);
    s.add_player(2, [0.0, 0.81, -30.0]);
    for _ in 0..400 {
        s.frame(FRAME);
    }
    for p in s.players.iter_mut() {
        p.vitals.invuln_until_tick = 0;
    }
    s
}

fn step(s: &mut Server, intents: &[(u16, PlayerIntent)]) {
    for (id, it) in intents {
        send(s, *id, *it);
    }
    s.frame(FRAME);
}

fn zip() -> PlayerIntent {
    PlayerIntent { zip: true, ..Default::default() }
}

#[test]
fn hooking_an_enemy_drags_them_in_staggers_them_and_leaves_you_in_place() {
    let mut s = scene();
    put(&mut s, 1, [0.0, 0.81, 0.0]);
    put(&mut s, 2, [0.0, 0.81, -30.0]);
    step(&mut s, &[]);
    step(&mut s, &[(1, zip())]);
    assert!(s.players[0].ch.grapple.is_some(), "hooked");
    assert!(s.players[0].ch.pulling_enemy(), "the rope is on the enemy");
    let me0 = pos(&s, 1);
    let mut best = f32::MAX;
    let mut staggered = false;
    let mut released_at = None;
    let mut drift = 0.0f32;
    for k in 0..180u32 {
        step(&mut s, &[(1, PlayerIntent::default())]);
        best = best.min(dist(pos(&s, 1), pos(&s, 2)));
        staggered |= s.players[1].blk.staggered(s.tick);
        if released_at.is_none() {
            drift = drift.max(dist(me0, pos(&s, 1)));
        }
        if released_at.is_none() && s.players[0].ch.grapple.is_none() {
            released_at = Some(k);
        }
    }
    let k = released_at.expect("rope released");
    eprintln!("enemy hook: closest {best:.2} m, rope off after {:.2} s, hooker moved {:.2} m", k as f32 / 60.0, dist(me0, pos(&s, 1)));
    assert!(best < 3.0, "pulled in: {best}");
    assert!(k < 90, "inside 1.5 s");
    assert!(staggered, "victim staggered");
    assert!(drift < 0.5, "the hooker is not reeled: moved {drift}");
}

#[test]
fn a_raised_shield_stops_the_pull_and_drains_the_guard() {
    let mut s = scene();
    put(&mut s, 1, [0.0, 0.81, 0.0]);
    put(&mut s, 2, [0.0, 0.81, -30.0]);
    // 2 faces +z (yaw pi) and holds the shield from the start
    let guard = PlayerIntent { block: true, aim_yaw: 32768, ..Default::default() };
    step(&mut s, &[(2, guard)]);
    step(&mut s, &[(2, guard)]);
    let e0 = s.players[1].blk.energy;
    step(&mut s, &[(1, zip()), (2, guard)]);
    // the hook lands and the shield stops it on the same tick: the rope is already dropped
    for _ in 0..30 {
        step(&mut s, &[(1, PlayerIntent::default()), (2, guard)]);
    }
    let gap = dist(pos(&s, 1), pos(&s, 2));
    eprintln!("shield: gap {gap:.1} m, guard {e0:.2} -> {:.2}", s.players[1].blk.energy);
    assert!(gap > 25.0, "not pulled through a shield: {gap}");
    assert!(s.players[0].ch.grapple.is_none(), "rope dropped");
    assert!(s.players[1].blk.energy < e0 - 0.2, "guard drained");
}
