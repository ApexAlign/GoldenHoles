//! Traversal + melee numbers (docs/traversal_melee.md): long grapple, hook-strike, wall-run up / down faces, wall slash.
//! Each test prints its numbers (`cargo test -p shinobi-sim --test traversal -- --nocapture`).

use shinobi_sim::character::{Cfg, CharacterSystem, MoveState, PhysicsWorld, PlayerInput, StrikeKind};
use shinobi_sim::movement::step_player;
use shinobi_sim::rapier3d::prelude::*;

const DT: f32 = 1.0 / 60.0;

fn world() -> PhysicsWorld {
    PhysicsWorld { bodies: RigidBodySet::new(), colliders: ColliderSet::new(), islands: IslandManager::new(), queries: QueryPipeline::new() }
}

fn boxc(w: &mut PhysicsWorld, c: [f32; 3], h: [f32; 3]) {
    w.colliders.insert(ColliderBuilder::cuboid(h[0], h[1], h[2]).translation(vector![c[0], c[1], c[2]]).build());
}

fn player(w: &mut PhysicsWorld, at: [f32; 3]) -> CharacterSystem {
    let cfg = Cfg::default();
    let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![at[0], at[1], at[2]]).build());
    w.colliders.insert_with_parent(ColliderBuilder::capsule_y(cfg.half_height, cfg.radius).build(), body, &mut w.bodies);
    CharacterSystem::new(body, cfg)
}

fn tick(ch: &mut CharacterSystem, w: &mut PhysicsWorld, i: &PlayerInput, now: &mut f32) {
    w.queries.update(&w.colliders);
    step_player(ch, i, w, *now, DT);
    *now += DT;
}

fn pos(w: &PhysicsWorld, ch: &CharacterSystem) -> Vector<Real> {
    *w.bodies[ch.body].translation()
}

fn fire(dir: [f32; 3]) -> PlayerInput {
    PlayerInput { grapple_aim: Some(vector![dir[0], dir[1], dir[2]]), ..Default::default() }
}

#[test]
fn grapple_reaches_ten_times_further_and_a_100_m_trip_is_fast() {
    let cfg = Cfg::default();
    assert!(cfg.grapple_range >= 10.0 * 40.0, "range {} m (was 40)", cfg.grapple_range);
    let mut w = world();
    boxc(&mut w, [0.0, 50.0, -330.0], [60.0, 80.0, 1.0]); // a face 329 m away
    boxc(&mut w, [0.0, -60.0, 0.0], [400.0, 0.5, 400.0]);
    let mut ch = player(&mut w, [0.0, 20.0, 0.0]);
    let mut now = 0.0;
    tick(&mut ch, &mut w, &fire([0.0, 0.0, -1.0]), &mut now);
    let g = ch.grapple.expect("a 329 m hook attaches");
    assert!((g.len - 329.0).abs() < 1.0, "rope {}", g.len);
    // time to cover the first 100 m of the reel
    let start = pos(&w, &ch);
    let mut t100 = None;
    for k in 0..900 {
        tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        if (pos(&w, &ch) - start).norm() >= 100.0 {
            t100 = Some(k as f32 * DT);
            break;
        }
    }
    let t100 = t100.expect("covers 100 m");
    eprintln!("grapple: range {} m (was 40); 100 m of reel in {t100:.2} s (old reel 18 m/s: {:.1} s)", cfg.grapple_range, 100.0 / 18.0);
    assert!(t100 < 3.2, "100 m took {t100} s");
    assert!(ch.vel.norm() <= cfg.grapple_far_speed + 2.5, "speed {}", ch.vel.norm());
}

#[test]
fn hook_then_katana_is_a_hook_strike_with_a_launch() {
    let mut w = world();
    boxc(&mut w, [0.0, 10.0, -60.0], [30.0, 40.0, 1.0]);
    boxc(&mut w, [0.0, -50.0, 0.0], [100.0, 0.5, 100.0]);
    let mut ch = player(&mut w, [0.0, 5.0, 0.0]);
    let mut now = 0.0;
    assert_eq!(ch.strike_kind(), None);
    tick(&mut ch, &mut w, &fire([0.0, 0.0, -1.0]), &mut now);
    for _ in 0..30 {
        tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
    }
    assert_eq!(ch.strike_kind(), Some(StrikeKind::Hook), "hooked = a strike is armed");
    let before = ch.vel;
    // the katana sets `strike` + the flat aim direction (Katana::fire / predict_slice)
    let strike = PlayerInput { strike: true, move_dir: vector![0.5, 0.0, -0.8660254], ..Default::default() };
    tick(&mut ch, &mut w, &strike, &mut now);
    assert!(ch.grapple.is_none(), "the strike cuts the rope");
    let h = vector![ch.vel.x, 0.0, ch.vel.z];
    eprintln!("hook-strike: speed {:.1} -> {:.1} m/s along the aim, vy {:.1}", before.norm(), h.norm(), ch.vel.y);
    assert!(h.norm() >= Cfg::default().hookstrike_speed - 0.5);
    assert!(h.normalize().dot(&vector![0.5, 0.0, -0.8660254]) > 0.99, "launched along the aim");
    // the window stays open for a moment after the rope is gone
    assert_eq!(ch.strike_kind(), Some(StrikeKind::Hook));
    for _ in 0..40 {
        tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
    }
    assert_eq!(ch.strike_kind(), None, "window closed");
}

fn tower() -> (PhysicsWorld, CharacterSystem) {
    let mut w = world();
    // a tower: face at x = 5 (wall box x in [5, 7]), 60 m tall, 40 m wide; ground at y = 0
    boxc(&mut w, [6.0, 30.0, 0.0], [1.0, 30.0, 20.0]);
    boxc(&mut w, [0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
    let ch = player(&mut w, [0.0, 0.81, 0.0]);
    (w, ch)
}

fn climb_input(k: u32) -> PlayerInput {
    let run = PlayerInput { move_dir: vector![1.0, 0.0, 0.0], jump_held: true, ..Default::default() };
    if k < 40 {
        PlayerInput { move_dir: vector![1.0, 0.0, 0.0], ..Default::default() }
    } else if k == 40 {
        PlayerInput { jump_pressed: true, ..run }
    } else {
        run
    }
}

#[test]
fn running_at_a_face_climbs_it_and_holding_away_runs_back_down() {
    let (mut w, mut ch) = tower();
    let mut now = 0.0;
    let mut climbing = false;
    let mut top_y = 0.0f32;
    for k in 0..320 {
        tick(&mut ch, &mut w, &climb_input(k), &mut now);
        if matches!(ch.state, MoveState::WallRun { .. }) {
            climbing = true;
        }
        top_y = top_y.max(pos(&w, &ch).y);
    }
    assert!(climbing, "caught the face head-on");
    eprintln!("climb: reached y = {top_y:.1} m up a 60 m face (wall-run time {} s, climb speed {} m/s)", ch.cfg.wall_max_time, ch.cfg.wall_climb_speed);
    assert!(top_y > 12.0, "climbed well above a jump: {top_y}");

    // now descend: hold away from the wall (-x) on the face
    let (mut w, mut ch) = tower();
    let mut now = 0.0;
    for k in 0..200 {
        tick(&mut ch, &mut w, &climb_input(k), &mut now);
    }
    let y_hi = pos(&w, &ch).y;
    assert!(matches!(ch.state, MoveState::WallRun { .. }), "still on the face at {y_hi:.1} m");
    let down = PlayerInput { move_dir: vector![-1.0, 0.0, 0.0], jump_held: true, ..Default::default() };
    let mut min_vy = 0.0f32;
    for _ in 0..40 {
        tick(&mut ch, &mut w, &down, &mut now);
        min_vy = min_vy.min(ch.vel.y);
    }
    let y_lo = pos(&w, &ch).y;
    eprintln!("descent: {y_hi:.1} m -> {y_lo:.1} m in 0.67 s, vy {min_vy:.1} m/s");
    assert!(y_lo < y_hi - 2.0 && min_vy < -3.0, "ran down the face");
}

#[test]
fn wall_run_is_stronger_and_a_wall_slash_launches_off_it() {
    let cfg = Cfg::default();
    eprintln!("wall-run: speed {} m/s (was 7.0), max {} s (was 2.2), jump push {}, climb {} m/s", cfg.wall_speed, cfg.wall_max_time, cfg.wall_jump_push, cfg.wall_climb_speed);
    assert!(cfg.wall_speed >= 9.0 && cfg.wall_max_time >= 3.5);
    let (mut w, mut ch) = tower();
    let mut now = 0.0;
    w.bodies[ch.body].set_position(Isometry::translation(4.4, 6.0, -10.0), true);
    let side = PlayerInput { move_dir: vector![0.2, 0.0, 1.0], jump_held: true, ..Default::default() };
    for _ in 0..8 {
        ch.vel = vector![0.0, 2.0, 7.0];
        tick(&mut ch, &mut w, &side, &mut now);
        if matches!(ch.state, MoveState::WallRun { .. }) {
            break;
        }
    }
    assert!(matches!(ch.state, MoveState::WallRun { .. }), "side wall-run");
    for _ in 0..20 {
        tick(&mut ch, &mut w, &side, &mut now);
    }
    assert_eq!(ch.strike_kind(), Some(StrikeKind::Wall));
    let sp = ch.vel.norm();
    let strike = PlayerInput { strike: true, move_dir: vector![-0.7071, 0.0, 0.7071], jump_held: true, ..Default::default() };
    tick(&mut ch, &mut w, &strike, &mut now);
    eprintln!("wall slash: run {sp:.1} m/s -> launch {:.1} m/s, away from the wall (vx {:.1}), vy {:.1}", ch.vel.norm(), ch.vel.x, ch.vel.y);
    assert!(!matches!(ch.state, MoveState::WallRun { .. }) && ch.vel.x < -3.0, "thrown off the face");
}

/// Chain: wall climb -> hook the next building from the wall -> a far, fast reel, strike armed.
#[test]
fn chain_wall_climb_to_hook_to_strike_stays_finite() {
    let (mut w, mut ch) = tower();
    boxc(&mut w, [-80.0, 40.0, 0.0], [1.0, 60.0, 40.0]); // building 80 m behind, face at x = -79
    let mut now = 0.0;
    for k in 0..160 {
        tick(&mut ch, &mut w, &climb_input(k), &mut now);
    }
    assert!(matches!(ch.state, MoveState::WallRun { .. }));
    // hook the far building from the wall, keep the speed, then strike
    tick(&mut ch, &mut w, &fire([-1.0, 0.0, 0.0]), &mut now);
    let g = ch.grapple.expect("hooked the next building from the wall");
    assert!(g.len > 70.0, "{}", g.len);
    let mut max_v = 0.0f32;
    for _ in 0..60 {
        tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        max_v = max_v.max(ch.vel.norm());
        assert!(ch.vel.iter().all(|v| v.is_finite()));
    }
    assert_eq!(ch.strike_kind(), Some(StrikeKind::Hook));
    eprintln!("chain: wall climb -> {:.0} m hook from the wall, peak speed {max_v:.1} m/s", g.len);
    assert!(max_v > 20.0, "the far hook is fast");
}
