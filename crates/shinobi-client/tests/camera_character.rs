//! Camera driven by a real `CharacterSystem` through `FpsCamera::frame` and the input `Aim`.

use std::f32::consts::PI;

use shinobi_client::camera::{FpsCamera, FpsCameraCfg};
use shinobi_client::input::Aim;
use shinobi_sim::character::{Cfg, CharacterSystem, MoveState, PhysicsWorld, PlayerInput};
use shinobi_sim::rapier3d::prelude::*;

const DT: f32 = 1.0 / 60.0;

fn world() -> PhysicsWorld {
    PhysicsWorld {
        bodies: RigidBodySet::new(),
        colliders: ColliderSet::new(),
        islands: IslandManager::new(),
        queries: QueryPipeline::new(),
    }
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

fn tick(ch: &mut CharacterSystem, w: &mut PhysicsWorld, input: &PlayerInput, now: &mut f32) {
    w.queries.update(&w.colliders);
    ch.update(DT, input, w, *now);
    let next = *w.bodies[ch.body].next_position();
    w.bodies[ch.body].set_position(next, true);
    *now += DT;
}

/// Wall-run along a wall at x = +2 (`wall_x` face at 0.5 from the player), heading `dir` with view `yaw`.
fn wall_run_roll(wall_x: f32, dir: [f32; 3], yaw: f32) -> f32 {
    let mut w = world();
    boxc(&mut w, [wall_x, 5.0, 0.0], [1.0, 10.0, 60.0]);
    boxc(&mut w, [0.0, -50.0, 0.0], [100.0, 0.5, 100.0]);
    let mut ch = player(&mut w, [0.0, 5.0, 0.0]);
    ch.vel = vector![0.0, 0.0, 0.0] + vector![dir[0], dir[1], dir[2]] * 9.0;
    let input = PlayerInput { move_dir: vector![dir[0], dir[1], dir[2]], wall_cling: true, ..Default::default() };
    let aim = Aim { yaw, pitch: 0.0 };
    let (mut cam, mut now, mut roll) = (FpsCamera::new(FpsCameraCfg::default()), 0.0, 0.0);
    for _ in 0..45 {
        tick(&mut ch, &mut w, &input, &mut now);
        roll = cam.frame(DT, &ch, &aim).0.roll;
    }
    assert!(matches!(ch.state, MoveState::WallRun { .. }), "never wall-ran: {:?}", ch.state);
    roll
}

#[test]
fn wall_on_right_tilts_left_15_deg() {
    let roll = wall_run_roll(1.0 + 0.5, [0.0, 0.0, -1.0], 0.0); // facing -Z, wall at +X = right
    assert!((roll + 15f32.to_radians()).abs() < 0.01, "roll {roll}");
}

#[test]
fn wall_on_left_tilts_right_15_deg() {
    let roll = wall_run_roll(-1.0 - 0.5, [0.0, 0.0, -1.0], 0.0); // wall at -X = left
    assert!((roll - 15f32.to_radians()).abs() < 0.01, "roll {roll}");
}

#[test]
fn tilt_follows_the_view_not_the_world() {
    // same wall at +X, but running and looking toward +Z: the wall is now on the player's LEFT
    let roll = wall_run_roll(1.0 + 0.5, [0.0, 0.0, 1.0], PI);
    assert!((roll - 15f32.to_radians()).abs() < 0.01, "roll {roll}");
}

#[test]
fn double_jump_thrusts_camera_on_the_exact_tick() {
    let mut w = world();
    boxc(&mut w, [0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
    let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
    let mut cam = FpsCamera::new(FpsCameraCfg::default());
    let aim = Aim::default();
    let mut now = 0.0;
    let idle = PlayerInput::default();
    for _ in 0..10 {
        tick(&mut ch, &mut w, &idle, &mut now); // settle on the ground
    }
    assert_eq!(ch.state, MoveState::Grounded);

    let jump = PlayerInput { jump_pressed: true, jump_held: true, ..Default::default() };
    tick(&mut ch, &mut w, &jump, &mut now);
    assert!(!ch.double_jumped);
    let (p, _) = cam.frame(DT, &ch, &aim);
    assert!(p.y_offset.abs() < 1e-6, "first jump must not thrust the camera");

    // wait for the apex, then press jump again
    let mut fired_at = None;
    for i in 0..60 {
        let input = if ch.vel.y.abs() < 2.0 { PlayerInput { jump_pressed: true, jump_held: true, ..Default::default() } } else { idle };
        tick(&mut ch, &mut w, &input, &mut now);
        let (p, wp) = cam.frame(DT, &ch, &aim);
        if ch.double_jumped {
            fired_at = Some(i);
            assert!(p.fov > 90.0, "FOV punch on double jump, fov {}", p.fov);
            assert!(wp.y_dip.abs() < 0.1);
            break;
        }
    }
    assert!(fired_at.is_some(), "double jump never fired at the apex");
    let mut peak = 0.0f32;
    for _ in 0..20 {
        tick(&mut ch, &mut w, &idle, &mut now);
        peak = peak.max(cam.frame(DT, &ch, &aim).0.y_offset);
    }
    assert!(peak > 0.03, "camera thrust peak {peak}");
}
