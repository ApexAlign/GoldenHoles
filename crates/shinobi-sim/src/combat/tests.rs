
use super::*;
use crate::character::{Cfg, CharacterSystem, PhysicsWorld, PlayerInput, GROUP_ENEMY};

const DT: f32 = 1.0 / 60.0;

fn world() -> PhysicsWorld {
    PhysicsWorld {
        bodies: RigidBodySet::new(),
        colliders: ColliderSet::new(),
        islands: IslandManager::new(),
        queries: QueryPipeline::new(),
    }
}

fn add_wall(w: &mut PhysicsWorld, pos: Vector<Real>, half: Vector<Real>) {
    w.colliders.insert(ColliderBuilder::cuboid(half.x, half.y, half.z)
            .translation(pos)
            .collision_groups(InteractionGroups::new(GROUP_WORLD, Group::ALL))
            .build());
}

fn add_enemy(w: &mut PhysicsWorld, pos: Vector<Real>, id: u128) {
    w.colliders.insert(
        ColliderBuilder::cuboid(0.4, 0.9, 0.4)
            .translation(pos)
            .collision_groups(InteractionGroups::new(GROUP_ENEMY, Group::ALL))
            .user_data(id)
            .build(),
    );
}

fn add_player(w: &mut PhysicsWorld, pos: Vector<Real>) -> (CharacterSystem, Cfg) {
    let cfg = Cfg::default();
    let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(pos).build());
    w.colliders.insert_with_parent(ColliderBuilder::capsule_y(cfg.half_height, cfg.radius)
            .collision_groups(InteractionGroups::new(GROUP_PLAYER, Group::ALL))
            .build(), body, &mut w.bodies);
    (CharacterSystem::new(body, cfg), cfg)
}

fn damaged(events: &[CombatEvent], kind: DamageKind) -> Vec<u128> {
    events
        .iter()
        .filter_map(|e| match e {
            CombatEvent::Damage { target, kind: k, .. } if *k == kind => Some(*target),
            _ => None,
        })
        .collect()
}

fn slice_once(w: &PhysicsWorld, ch: &CharacterSystem, cfg: &Cfg, origin: Vector<Real>) -> (Vec<CombatEvent>, PlayerInput) {
    let mut sys = CombatSystem::ninja(cfg);
    let mut input = PlayerInput::default();
    let mut ev = Vec::new();
    sys.update(DT, 0.0, &CombatInput::new(false, true), &Aim::level(origin, 0.0), w, ch, &mut input, &mut ev);
    (ev, input)
}

#[test]
fn slice_hits_inside_box_misses_behind_wall_behind_player_and_off_to_the_side() {
    let mut w = world();
    let origin = vector![0.0, 1.0, 0.0];
    let (ch, cfg) = add_player(&mut w, origin);
    add_enemy(&mut w, vector![0.0, 1.0, -3.0], 1); // inside the box
    add_enemy(&mut w, vector![0.0, 1.0, 3.0], 2); // behind the player
    add_enemy(&mut w, vector![3.0, 1.0, -3.0], 3); // too far to the side
    add_enemy(&mut w, vector![0.0, 1.0, -9.0], 4); // beyond reach
    w.queries.update(&w.colliders);

    let (ev, input) = slice_once(&w, &ch, &cfg, origin);
    assert_eq!(damaged(&ev, DamageKind::Slash), vec![1]);
    assert!(input.dash_pressed, "slice must drive the character dash");
    assert!((input.move_dir - vector![0.0, 0.0, -1.0]).norm() < 1e-5);
    assert!(ev.iter().any(|e| matches!(e, CombatEvent::Slash { lunge: true, hits: 1, .. })));
}

#[test]
fn wall_truncates_the_slash() {
    let mut w = world();
    let origin = vector![0.0, 1.0, 0.0];
    let (ch, cfg) = add_player(&mut w, origin);
    add_wall(&mut w, vector![0.0, 1.0, -2.0], vector![5.0, 3.0, 0.1]);
    add_enemy(&mut w, vector![0.0, 1.0, -1.0], 1); // in front of the wall
    add_enemy(&mut w, vector![0.0, 1.0, -3.5], 2); // behind it, within raw reach
    w.queries.update(&w.colliders);

    let (ev, _) = slice_once(&w, &ch, &cfg, origin);
    assert_eq!(damaged(&ev, DamageKind::Slash), vec![1]);
}

#[test]
fn dash_on_cooldown_still_slashes_short_without_lunge() {
    let mut w = world();
    let origin = vector![0.0, 1.0, 0.0];
    let (mut ch, cfg) = add_player(&mut w, origin);
    w.queries.update(&w.colliders);
    // burn the dash
    ch.update(DT, &PlayerInput { dash_pressed: true, move_dir: vector![0.0, 0.0, -1.0], ..Default::default() }, &mut w, 0.0);
    assert!(!ch.dash_ready());
    add_enemy(&mut w, vector![0.0, 1.0, -3.0], 1); // out of blade-only reach (1.2 m)
    add_enemy(&mut w, vector![0.0, 1.0, -0.8], 2);
    w.queries.update(&w.colliders);

    let (ev, input) = slice_once(&w, &ch, &cfg, origin);
    assert!(!input.dash_pressed);
    assert_eq!(damaged(&ev, DamageKind::Slash), vec![2]);
    assert!(ev.iter().any(|e| matches!(e, CombatEvent::Slash { lunge: false, .. })));
}

#[test]
fn lunge_moves_the_player_about_the_configured_distance() {
    let mut w = world();
    add_wall(&mut w, vector![0.0, -0.5, 0.0], vector![100.0, 0.5, 100.0]); // floor, top at y = 0
    let start = vector![0.0, 0.81, 0.0];
    let (mut ch, cfg) = add_player(&mut w, start);
    w.queries.update(&w.colliders);

    let mut sys = CombatSystem::ninja(&cfg);
    let body = ch.body;
    let mut travelled = 0.0;
    let mut now = 0.0;
    for tick in 0..((cfg.dash_time / DT).ceil() as usize + 1) {
        let mut input = PlayerInput::default();
        let mut ev = Vec::new();
        let slice = tick == 0;
        let pos = *w.bodies[body].translation();
        sys.update(DT, now, &CombatInput::new(false, slice), &Aim::level(pos, 0.0), &w, &ch, &mut input, &mut ev);
        ch.update(DT, &input, &mut w, now);
        let next = *w.bodies[body].next_position();
        w.bodies[body].set_position(next, true);
        now += DT;
        travelled = (w.bodies[body].translation() - start).norm();
    }
    let expect = katana::dash_distance(&cfg);
    assert!((travelled - expect).abs() < expect * 0.2, "travelled {travelled}, expected ~{expect}");
    assert!(w.bodies[body].translation().z < -3.0, "lunge goes along aim (-Z)");
}

#[test]
fn shuriken_bounces_twice_then_dies_at_constant_speed() {
    let mut w = world();
    let origin = vector![0.0, 1.0, 0.0];
    let (ch, cfg) = add_player(&mut w, origin);
    // corridor: walls at x = -3 and x = +3
    add_wall(&mut w, vector![-3.0, 1.0, 0.0], vector![0.1, 3.0, 200.0]);
    add_wall(&mut w, vector![3.0, 1.0, 0.0], vector![0.1, 3.0, 200.0]);
    w.queries.update(&w.colliders);

    let mut sys = CombatSystem::ninja(&cfg);
    let mut input = PlayerInput::default();
    let mut ev = Vec::new();
    sys.update(DT, 0.0, &CombatInput::new(true, false), &Aim::level(origin, -0.4), &w, &ch, &mut input, &mut ev);
    for i in 0..200 {
        sys.update(DT, i as f32 * DT, &CombatInput::default(), &Aim::level(origin, 0.0), &w, &ch, &mut input, &mut ev);
    }
    let bounces = ev.iter().filter(|e| matches!(e, CombatEvent::ShurikenBounce { .. })).count();
    let gone = ev.iter().filter(|e| matches!(e, CombatEvent::ShurikenGone { .. })).count();
    assert_eq!((bounces, gone), (2, 1));
}

#[test]
fn shuriken_reflects_off_wall_and_hits_enemy_around_the_corner() {
    let mut w = world();
    let origin = vector![0.0, 1.0, 0.0];
    let (ch, cfg) = add_player(&mut w, origin);
    add_wall(&mut w, vector![0.0, 1.0, -10.3], vector![30.0, 3.0, 0.3]); // face at z = -10
    add_enemy(&mut w, vector![10.5, 1.0, -6.0], 77);
    w.queries.update(&w.colliders);

    let mut sys = CombatSystem::ninja(&cfg);
    let mut input = PlayerInput::default();
    let mut ev = Vec::new();
    // dir = (0.6, 0, -0.8): hits the wall at x = 7.5 and reflects toward the enemy
    let yaw = -(0.6f32).asin();
    sys.update(DT, 0.0, &CombatInput::new(true, false), &Aim::level(origin, yaw), &w, &ch, &mut input, &mut ev);
    for i in 0..200 {
        sys.update(DT, i as f32 * DT, &CombatInput::default(), &Aim::level(origin, 0.0), &w, &ch, &mut input, &mut ev);
    }
    assert_eq!(damaged(&ev, DamageKind::Shuriken), vec![77]);
    assert_eq!(ev.iter().filter(|e| matches!(e, CombatEvent::ShurikenBounce { .. })).count(), 1);
}

#[test]
fn grid_beam_hits_each_enemy_once_as_the_wave_passes() {
    let mut w = world();
    let origin = vector![0.0, 1.0, 0.0];
    let (ch, cfg) = add_player(&mut w, origin);
    add_enemy(&mut w, vector![10.0, 1.0, 0.0], 1);
    add_enemy(&mut w, vector![0.0, 5.0, -60.0], 2);
    w.queries.update(&w.colliders);

    let mut sys = CombatSystem::ninja(&cfg);
    sys.powerups.cubes.push(powerup::DataCube {
        id: 9,
        pos: Point::new(0.5, 1.0, 0.0),
        kind: powerup::PowerupKind::GridBeam,
        charge_secs: 20.0,
    });
    let mut input = PlayerInput::default();
    let mut ev = Vec::new();
    let aim = Aim::level(origin, 0.0);
    sys.update(DT, 0.0, &CombatInput::default(), &aim, &w, &ch, &mut input, &mut ev);
    assert!(ev.iter().any(|e| matches!(e, CombatEvent::PickedUp { id: 9, .. })));
    let go = CombatInput { ultimate: true, ..Default::default() };
    sys.update(DT, DT, &go, &aim, &w, &ch, &mut input, &mut ev);
    for i in 0..200 {
        sys.update(DT, i as f32 * DT, &CombatInput::default(), &aim, &w, &ch, &mut input, &mut ev);
    }
    let mut hit = damaged(&ev, DamageKind::GridBeam);
    hit.sort();
    assert_eq!(hit, vec![1, 2]);
}
