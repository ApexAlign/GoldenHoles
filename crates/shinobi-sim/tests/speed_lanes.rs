use shinobi_sim::character::{sync_body_colliders, Cfg, CharacterSystem, MoveState, PhysicsWorld, PlayerInput};
use shinobi_sim::level::{parse_level_file, LevelFeatures};
use shinobi_sim::movement::step_player;
use shinobi_sim::rapier3d::prelude::*;

const DT: f32 = 1.0 / 60.0;
const SPAWN: [f32; 3] = [10.0, 5.0, 4.7];

fn scene(zone_lines: &str) -> (PhysicsWorld, CharacterSystem, LevelFeatures) {
    let text = format!(
        "box 0 -0.5 0 100 0.5 100\n\
         box 0 4 5.5 200 4 0.5\n\
         spawn 10 5 4.7\n{zone_lines}"
    );
    let f = parse_level_file(&text).unwrap();
    let mut w = PhysicsWorld {
        bodies: RigidBodySet::new(),
        colliders: ColliderSet::new(),
        islands: IslandManager::new(),
        queries: QueryPipeline::new(),
    };
    f.level.insert_into(&mut w);
    let cfg = Cfg::default();
    let body = w.bodies.insert(
        RigidBodyBuilder::kinematic_position_based()
            .translation(vector![SPAWN[0], SPAWN[1], SPAWN[2]])
            .build(),
    );
    w.colliders.insert_with_parent(
        ColliderBuilder::capsule_y(cfg.half_height, cfg.radius).build(),
        body,
        &mut w.bodies,
    );
    let mut ch = CharacterSystem::new(body, cfg);
    ch.vel = vector![cfg.wall_speed, 0.0, 0.0];
    ch.state = MoveState::WallRun {
        normal: vector![0.0, 0.0, -1.0],
        tangent: vector![1.0, 0.0, 0.0],
        time_left: 1000.0,
    };
    (w, ch, f.features)
}

fn pos(ch: &CharacterSystem, w: &PhysicsWorld) -> [f32; 3] {
    let p = w.bodies[ch.body].translation();
    [p.x, p.y, p.z]
}

fn step(ch: &mut CharacterSystem, w: &mut PhysicsWorld, features: &LevelFeatures, input: &PlayerInput, now: &mut f32) {
    let p = pos(ch, w);
    let normal = ch.wall_normal().map(|n| [n.x, n.y, n.z]);
    ch.speed_lane = features.speed_lane_for(p, normal);
    step_player(ch, input, w, *now, DT);
    *now += DT;
}

fn wall_input() -> PlayerInput {
    PlayerInput {
        move_dir: vector![1.0, 0.0, 0.0],
        jump_held: true,
        wall_cling: true,
        ..Default::default()
    }
}

#[test]
fn speed_lane_boosts_only_a_wall_run_and_respects_cap() {
    let (mut w, mut ch, features) = scene(
        "zone 50 4 4.5 50 4 0.5 speed_lane cap=1.6 decay=0.9 mult=1.15 wall_run_m=5\n",
    );
    let input = wall_input();
    let mut now = 0.0;
    for _ in 0..40 {
        step(&mut ch, &mut w, &features, &input, &mut now);
    }
    assert!(matches!(ch.state, MoveState::WallRun { .. }), "{:?}", ch.state);
    let base = ch.cfg.wall_speed;
    assert!(ch.vel.x > base * 1.10, "boosted wall run: {}", ch.vel.x);
    assert!(ch.vel.x <= base * 1.15 + 1e-3, "authored mult cap: {}", ch.vel.x);

    let (mut w, mut ch, features) = scene(
        "zone 50 4 4.5 50 4 0.5 speed_lane cap=1.6 decay=0.9 mult=3 wall_run_m=0\n",
    );
    let mut now = 0.0;
    for _ in 0..60 {
        step(&mut ch, &mut w, &features, &input, &mut now);
    }
    let base = ch.cfg.wall_speed;
    assert!(ch.vel.x <= base * 1.6 + 1e-3, "hard cap engaged: {}", ch.vel.x);
    assert!(ch.vel.x > base * 1.5, "cap is reachable: {}", ch.vel.x);
}

#[test]
fn speed_lane_state_does_not_boost_walking_or_airborne() {
    let (mut w1, mut c1, f1) = scene(
        "zone 50 4 4.5 50 4 0.5 speed_lane cap=1.6 decay=0.9 mult=1.15 wall_run_m=5\n",
    );
    let (mut w2, mut c2, _) = scene(
        "zone 50 4 4.5 50 4 0.5 speed_lane cap=1.6 decay=0.9 mult=1.15 wall_run_m=5\n",
    );
    let lane = f1.speed_lane_for([10.0, 4.0, 4.5], Some([0.0, 0.0, -1.0])).unwrap();
    let base = c1.cfg.wall_speed;

    for ch in [&mut c1, &mut c2] {
        ch.state = MoveState::Airborne;
        ch.vel = vector![base * 1.15, 0.0, 0.0];
    }
    c1.speed_lane = Some(lane);
    let mut now = 0.0;
    for i in 0..20 {
        step_player(&mut c1, &PlayerInput::default(), &mut w1, now, DT);
        step_player(&mut c2, &PlayerInput::default(), &mut w2, now, DT);
        now += DT;
        assert_eq!(pos(&c1, &w1), pos(&c2, &w2), "airborne tick {i}");
        assert_eq!(c1.vel, c2.vel, "airborne velocity tick {i}");
        assert!(c1.vel.x <= base * 1.15, "lane did not add speed in the air: {}", c1.vel.x);
    }

    for (w, ch) in [(&mut w1, &mut c1), (&mut w2, &mut c2)] {
        let body = ch.body;
        w.bodies[body].set_position(Isometry::translation(10.0, 0.81, 4.7), true);
        sync_body_colliders(w, body);
        ch.state = MoveState::Grounded;
        ch.vel = Vector::zeros();
    }
    c1.speed_lane = Some(lane);
    let input = PlayerInput { move_dir: vector![1.0, 0.0, 0.0], ..Default::default() };
    let mut now = 0.0;
    for i in 0..40 {
        step_player(&mut c1, &input, &mut w1, now, DT);
        step_player(&mut c2, &input, &mut w2, now, DT);
        now += DT;
        assert_eq!(pos(&c1, &w1), pos(&c2, &w2), "walking tick {i}");
        assert_eq!(c1.vel, c2.vel, "walking velocity tick {i}");
        assert!(matches!(c1.state, MoveState::Grounded), "{:?}", c1.state);
        assert!(c1.vel.x <= c1.cfg.run_speed + 1e-3, "walk speed unaffected: {}", c1.vel.x);
    }
}

#[test]
fn post_exit_band_decays_and_the_next_lane_resets_the_bonus() {
    let (mut w, mut ch, features) = scene(
        "zone 15 4 4.5 15 4 0.5 speed_lane cap=1.6 decay=0.9 mult=1.15 wall_run_m=10\n\
         zone 67.5 4 4.5 32.5 4 0.5 speed_lane cap=1.6 decay=0.9 mult=1.15 wall_run_m=10\n",
    );
    let base = ch.cfg.wall_speed;
    let input = wall_input();
    let mut now = 0.0;
    let mut prev_band_surplus: Option<f32> = None;
    let mut band_ticks = 0;
    let mut second_lane_seen = false;
    let mut second_entry_speed = 0.0f32;
    let mut second_full = false;
    for _ in 0..260 {
        let p = pos(&ch, &w);
        let lane = features.speed_lane_for(p, ch.wall_normal().map(|n| [n.x, n.y, n.z]));
        let in_second = lane.map_or(false, |l| l.inside) && p[0] > 35.0;
        step(&mut ch, &mut w, &features, &input, &mut now);
        assert!(matches!(ch.state, MoveState::WallRun { .. }), "wall run ended at x={}", p[0]);
        let surplus = ch.vel.x - base;
        if let Some(l) = lane {
            if !l.inside {
                band_ticks += 1;
                if let Some(prev) = prev_band_surplus {
                    assert!((surplus - prev * l.decay).abs() < 0.02, "surplus {prev} -> {surplus} at decay {}", l.decay);
                }
                prev_band_surplus = Some(surplus);
            } else {
                prev_band_surplus = None;
            }
        }
        if in_second && !second_lane_seen {
            second_lane_seen = true;
            second_entry_speed = ch.vel.x;
        }
        if second_lane_seen && ch.vel.x > base * 1.10 {
            second_full = true;
        }
    }
    assert!(band_ticks > 10, "the post-exit band must be exercised: {band_ticks} ticks");
    assert!(second_lane_seen, "the script reaches the second lane");
    assert!(second_full, "re-entry resets the bonus: {} -> {}", second_entry_speed, ch.vel.x);
    assert!(ch.vel.x > base * 1.10 && ch.vel.x <= base * 1.15 + 1e-3, "full boost after reset: {}", ch.vel.x);
}

fn lane_axes(z: &shinobi_sim::level::ZoneDef) -> (usize, usize) {
    if z.half[0] > z.half[2] { (0, 2) } else { (2, 0) }
}

#[test]
fn circuit_exposes_four_authored_speed_lanes_and_walking_does_not_match() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels/ffa_circuit.lvl");
    let f = parse_level_file(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let lanes: Vec<_> = f.features.zones.iter().filter(|z| z.tag == "speed_lane").collect();
    assert_eq!(lanes.len(), 4, "four authored Circuit data lanes");
    for z in &lanes {
        assert_eq!(z.tuning["wall_run_m"], 10.0);
        assert_eq!(z.tuning["mult"], 1.15);
        assert_eq!(z.tuning["cap"], 1.6);
        assert_eq!(z.tuning["decay"], 0.9);
        let (long, thin) = lane_axes(z);
        let dir = if z.center[thin] > 0.0 { -1.0 } else { 1.0 };
        let mut n = [0.0f32; 3];
        n[thin] = dir;
        let mut p = z.center;
        p[long] = z.center[long];
        p[thin] -= dir * 0.2;
        let q = f.features.speed_lane_for(p, Some(n)).expect("inside the authored lane");
        assert!(q.inside);
        assert!((q.multiplier() - 1.15).abs() < 1e-6, "full ramp at the lane middle");
        assert!(f.features.speed_lane_for([p[0], 0.81, p[2]], None).is_none(), "walking has no wall face");
    }

    let mut w = PhysicsWorld {
        bodies: RigidBodySet::new(),
        colliders: ColliderSet::new(),
        islands: IslandManager::new(),
        queries: QueryPipeline::new(),
    };
    f.level.insert_into(&mut w);
    let cfg = Cfg::default();
    let body = w.bodies.insert(
        RigidBodyBuilder::kinematic_position_based()
            .translation(vector![27.0, 0.81, 4.5])
            .build(),
    );
    w.colliders.insert_with_parent(
        ColliderBuilder::capsule_y(cfg.half_height, cfg.radius).build(),
        body,
        &mut w.bodies,
    );
    let mut ch = CharacterSystem::new(body, cfg);
    let input = PlayerInput { move_dir: vector![1.0, 0.0, 0.0], ..Default::default() };
    let mut now = 0.0;
    for _ in 0..60 {
        let p = pos(&ch, &w);
        assert!(f.features.speed_lane_for(p, ch.wall_normal().map(|n| [n.x, n.y, n.z])).is_none());
        step_player(&mut ch, &input, &mut w, now, DT);
        now += DT;
    }
    assert!(matches!(ch.state, MoveState::Grounded), "{:?}", ch.state);
    assert!(ch.vel.x <= ch.cfg.run_speed + 1e-3, "normal Circuit walking is unchanged: {}", ch.vel.x);
}

#[test]
fn naturally_entered_wall_run_picks_up_the_lane() {
    let (mut w, mut ch, features) = scene(
        "zone 150 4 4.5 150 4 0.5 speed_lane cap=1.6 decay=0.9 mult=1.15 wall_run_m=10\n",
    );
    let body = ch.body;
    w.bodies[body].set_position(Isometry::translation(30.0, 0.81, 4.7), true);
    sync_body_colliders(&mut w, body);
    ch.state = MoveState::Grounded;
    ch.vel = Vector::zeros();
    let base = ch.cfg.wall_speed;
    let mut now = 0.0;
    let run = PlayerInput { move_dir: vector![1.0, 0.0, 0.0], ..Default::default() };
    for _ in 0..30 {
        step(&mut ch, &mut w, &features, &run, &mut now);
    }
    assert!(matches!(ch.state, MoveState::Grounded), "run-up: {:?}", ch.state);

    step(
        &mut ch,
        &mut w,
        &features,
        &PlayerInput { jump_pressed: true, jump_held: true, wall_cling: true, move_dir: vector![1.0, 0.0, 0.0], ..Default::default() },
        &mut now,
    );
    let hold = PlayerInput { jump_held: true, wall_cling: true, move_dir: vector![1.0, 0.0, 0.0], ..Default::default() };
    let mut entered = false;
    let mut boosted = false;
    for _ in 0..120 {
        step(&mut ch, &mut w, &features, &hold, &mut now);
        if matches!(ch.state, MoveState::WallRun { .. }) {
            entered = true;
            boosted |= ch.vel.x > base * 1.10;
        }
    }
    assert!(entered, "jump + cling did not find the wall");
    assert!(boosted, "a naturally entered wall run did not pick up the lane: {}", ch.vel.x);
}
