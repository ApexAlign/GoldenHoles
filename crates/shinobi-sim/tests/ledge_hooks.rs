//! Grapple arrival (docs/traversal_melee.md): hook the top edge of a deck or a ramp from 20, 60 and 150 m and count the hooks
//! it takes to end up STANDING on it within 2 m of the edge. "Legacy" = the arrival settings before the soft-arrival /
//! mantle work (full momentum on release, no ledge assist); "new" = `Cfg::default()`.
//! `cargo test -p shinobi-sim --test ledge_hooks -- --nocapture`

use shinobi_sim::character::{Cfg, CharacterSystem, MoveState, PhysicsWorld, PlayerInput};
use shinobi_sim::movement::step_player;
use shinobi_sim::rapier3d::prelude::*;

const DT: f32 = 1.0 / 60.0;
const DECK_TOP: f32 = 24.0;
const DECK_HALF: f32 = 14.0; // deck footprint x, z in [-14, 14]
const RAMP_TILT: f32 = 0.419; // 24 degrees
const RAMP_HALF: f32 = 8.0;

fn legacy() -> Cfg {
    Cfg { grapple_arrive: 2.0, arrive_speed_keep: 1.0, cancel_keep: 1.0, mantle_height: 0.0, grapple_steer: 0.35, grapple_arrive_speed: 10.0, grapple_soft_dist: 6.0, ..Cfg::default() }
}

fn world(ramp: bool, dist: f32) -> PhysicsWorld {
    let mut w = PhysicsWorld { bodies: RigidBodySet::new(), colliders: ColliderSet::new(), islands: IslandManager::new(), queries: QueryPipeline::new() };
    let mut add = |c: [f32; 3], h: [f32; 3], rot: f32| {
        w.colliders.insert(ColliderBuilder::cuboid(h[0], h[1], h[2]).position(Isometry::new(vector![c[0], c[1], c[2]], vector![0.0, 0.0, rot])).build());
    };
    add([0.0, -0.5, 0.0], [400.0, 0.5, 400.0], 0.0); // the floor the player starts on
    add([0.0, DECK_TOP - 8.0, 0.0], [DECK_HALF, 8.0, DECK_HALF], 0.0); // the deck: top at DECK_TOP, footprint +-14 m
    if ramp {
        // a 24 degree ramp up to the deck's +x edge (its top end meets the deck top); rot_z < 0 lifts the -x end
        let (cx, cy) = (DECK_HALF + RAMP_HALF * RAMP_TILT.cos(), DECK_TOP - RAMP_HALF * RAMP_TILT.sin());
        add([cx, cy - 0.4, 0.0], [RAMP_HALF, 0.4, 6.0], -RAMP_TILT);
        // the player's start platform for the ramp scenarios: 6 m above the ramp's top end, far out along +x
        add([DECK_HALF + 2.0 * RAMP_HALF + dist, DECK_TOP + 6.0 - 3.0, 0.0], [6.0, 3.0, 6.0], 0.0);
    }
    w.queries.update(&w.colliders);
    w
}

fn player(w: &mut PhysicsWorld, cfg: Cfg, at: [f32; 3]) -> CharacterSystem {
    let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![at[0], at[1], at[2]]).build());
    w.colliders.insert_with_parent(ColliderBuilder::capsule_y(cfg.half_height, cfg.radius).build(), body, &mut w.bodies);
    CharacterSystem::new(body, cfg)
}

fn tick(ch: &mut CharacterSystem, w: &mut PhysicsWorld, i: &PlayerInput) {
    w.queries.update(&w.colliders);
    step_player(ch, i, w, 0.0, DT);
}

fn pos(w: &PhysicsWorld, ch: &CharacterSystem) -> Vector<Real> {
    *w.bodies[ch.body].translation()
}

/// Standing (grounded) on the deck within 2 m of its edge, or on the ramp within 2 m (along it) of its top end.
fn on_deck_near_edge(w: &PhysicsWorld, ch: &CharacterSystem, ramp: bool) -> bool {
    let p = pos(w, ch);
    if ch.state != MoveState::Grounded {
        return false;
    }
    let on_deck = (p.y - (DECK_TOP + 0.81)).abs() < 0.3 && p.x.abs() <= DECK_HALF && p.z.abs() <= DECK_HALF && (DECK_HALF - p.x.abs()).min(DECK_HALF - p.z.abs()) <= 2.0;
    if !ramp {
        return on_deck;
    }
    // ramp surface height at x: DECK_TOP - (x - DECK_HALF) * tan(tilt), for x in [DECK_HALF, DECK_HALF + 2 * cos(tilt) * HALF]
    let t = RAMP_TILT.tan();
    let on_ramp = p.x >= DECK_HALF && p.x <= DECK_HALF + 2.0 && p.z.abs() <= 6.0 && (p.y - (DECK_TOP - (p.x - DECK_HALF) * t + 0.81 / RAMP_TILT.cos())).abs() < 0.45;
    on_deck || on_ramp
}

/// One scenario: the player starts `dist` metres from the deck's near face (z = +14 side; the ramp scenario approaches
/// along +x toward the ramp's top end) on the floor and hooks the aim point `below` metres under the lip (0 = the lip
/// itself). Holds the stick toward the target while on the rope. Returns (hooks used, ended standing near the edge).
fn attempt(cfg: Cfg, dist: f32, below: f32, ramp: bool) -> (u32, bool) {
    let mut w = world(ramp, dist);
    let (start, toward, target) = if ramp {
        // along +x from a platform 6 m above the ramp's top end: hook the ramp surface `below` m (along it) under its top edge
        let t = RAMP_TILT.tan();
        let along = 0.5 + below; // metres of ramp below the top edge
        ([DECK_HALF + 2.0 * RAMP_HALF + dist - 5.0, DECK_TOP + 6.81, 0.0], vector![-1.0, 0.0, 0.0], vector![DECK_HALF + along * RAMP_TILT.cos(), DECK_TOP - along * RAMP_TILT.cos() * t, 0.0])
    } else {
        // from the floor, a face point `0.3 + below` m under the lip
        ([0.0, 0.81, DECK_HALF + dist], vector![0.0, 0.0, -1.0], vector![0.0, DECK_TOP - 0.3 - below, DECK_HALF])
    };
    let mut ch = player(&mut w, cfg, start);
    let mut hooks = 0;
    for _ in 0..6 {
        let p = pos(&w, &ch);
        let eye = p + Vector::y() * ch.eye_above();
        let dir = (target - eye).normalize();
        hooks += 1;
        tick(&mut ch, &mut w, &PlayerInput { grapple_aim: Some(dir), move_dir: toward, ..Default::default() });
        if ch.grapple.is_none() {
            continue; // missed: try again
        }
        // ride the rope with the stick held toward the target, then let the arrival play out
        let mut t = 0;
        while ch.grapple.is_some() && t < 900 {
            tick(&mut ch, &mut w, &PlayerInput { move_dir: toward, ..Default::default() });
            t += 1;
        }
        // settle: 2 s with no input
        for _ in 0..120 {
            tick(&mut ch, &mut w, &PlayerInput::default());
        }
        if on_deck_near_edge(&w, &ch, ramp) {
            return (hooks, true);
        }
        // fell off / stuck: re-hook from wherever we are
    }
    (hooks, false)
}

fn sweep(cfg: Cfg, label: &str) -> (f32, usize, usize, usize) {
    let (mut total, mut ok, mut first, mut n) = (0u32, 0usize, 0usize, 0usize);
    for ramp in [false, true] {
        for dist in [20.0f32, 60.0, 150.0] {
            for below in [0.0f32, 0.5, 1.7] {
                let (h, success) = attempt(cfg, dist, below, ramp);
                n += 1;
                total += h;
                ok += success as usize;
                first += (success && h == 1) as usize;
                eprintln!("  {label}: {} dist {dist:>5.0} m aim {:.1} m from the top edge -> {h} hook(s), standing on it: {success}", if ramp { "ramp" } else { "deck" }, 0.3 + below);
            }
        }
    }
    (total as f32 / n as f32, ok, first, n)
}

#[test]
fn ledge_hooks_land_on_the_deck_with_one_hook_now_and_took_several_before() {
    let (mean_old, ok_old, first_old, n) = sweep(legacy(), "legacy");
    let (mean_new, ok_new, first_new, _) = sweep(Cfg::default(), "new");
    eprintln!(
        "LEDGE HOOKS over {n} deck/ramp targets (20/60/150 m, lip and below): legacy {mean_old:.2} hooks on average, {first_old}/{n} on the first hook, {ok_old}/{n} ever; new {mean_new:.2} hooks, {first_new}/{n} on the first hook, {ok_new}/{n} ever"
    );
    assert_eq!(first_new, n, "new arrival: every ledge target lands standing on the first hook ({first_new}/{n})");
    assert!(ok_new == n, "every ledge target is reachable: {ok_new}/{n}");
    assert!(mean_new < mean_old || first_old == n, "the new arrival needs fewer hooks ({mean_new:.2} vs {mean_old:.2})");
}
