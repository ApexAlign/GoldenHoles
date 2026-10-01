//! `levels/arena.lvl`: the demo arena, pinned against the real `Cfg` movement limits.
//! Geometry checks are analytic (boxes, ramps); the physics tests run the real character controller on the
//! real file, so a Cfg retune that breaks a corridor or a tier fails here instead of in front of players.

use std::collections::VecDeque;

use shinobi_sim::character::{Cfg, CharacterSystem, MoveState, PhysicsWorld, PlayerInput};
use shinobi_sim::level::{parse_level_file, LevelBox, LevelFile};
use shinobi_sim::movement::step_player;
use shinobi_sim::rapier3d::prelude::*;

const DT: f32 = 1.0 / 60.0;
const CELL: f32 = 0.5;
const HALF: f32 = 40.0; // playable half extent (inner faces of the perimeter)
const CEILING_Y: f32 = 16.0;

fn arena() -> LevelFile {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels/arena.lvl");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    parse_level_file(&text).unwrap_or_else(|e| panic!("arena.lvl: {e}"))
}

// ---- box geometry (boxes may be rolled about Z) ----

fn inside(b: &LevelBox, p: [f32; 3]) -> bool {
    let (s, c) = b.rot_z.sin_cos();
    let (dx, dy) = (p[0] - b.center[0], p[1] - b.center[1]);
    let (u, v) = (dx * c + dy * s, -dx * s + dy * c);
    u.abs() <= b.half[0] && v.abs() <= b.half[1] && (p[2] - b.center[2]).abs() <= b.half[2]
}

fn any_inside(l: &LevelFile, p: [f32; 3]) -> bool {
    l.level.boxes.iter().any(|b| inside(b, p))
}

/// Height of the box's top face above `(x, z)`, if it has one there.
fn top_at(b: &LevelBox, x: f32, z: f32) -> Option<f32> {
    if (z - b.center[2]).abs() > b.half[2] {
        return None;
    }
    let (s, c) = b.rot_z.sin_cos();
    let u = (x - b.center[0] + b.half[1] * s) / c;
    (u.abs() <= b.half[0]).then(|| b.center[1] + u * s + b.half[1] * c)
}

/// Axis-aligned bounds in x/y of a (possibly rolled) box.
fn xy_bounds(b: &LevelBox) -> ([f32; 2], [f32; 2]) {
    let (s, c) = b.rot_z.sin_cos();
    let ex = b.half[0] * c.abs() + b.half[1] * s.abs();
    let ey = b.half[0] * s.abs() + b.half[1] * c.abs();
    ([b.center[0] - ex, b.center[0] + ex], [b.center[1] - ey, b.center[1] + ey])
}

// ---- standable surface graph ----

#[derive(Clone, Copy, Debug)]
struct Node {
    h: f32,
}

struct Grid {
    n: usize,
    nodes: Vec<Vec<Node>>, // per cell, standable surface heights
}

impl Grid {
    fn idx(&self, ix: usize, iz: usize) -> usize {
        iz * self.n + ix
    }
    fn cell(&self, x: f32) -> usize {
        (((x + HALF) / CELL) as isize).clamp(0, self.n as isize - 1) as usize
    }
    fn centre(&self, i: usize) -> f32 {
        -HALF + (i as f32 + 0.5) * CELL
    }
}

fn standable(l: &LevelFile, cfg: &Cfg, x: f32, z: f32, own: usize, h: f32) -> bool {
    let r = cfg.radius;
    let body_top = h + 2.0 * (cfg.half_height + cfg.radius);
    for (i, b) in l.level.boxes.iter().enumerate() {
        if i == own {
            continue;
        }
        let (bx, by) = xy_bounds(b);
        let overlaps_footprint = x >= bx[0] - r && x <= bx[1] + r && (z - b.center[2]).abs() <= b.half[2] + r;
        // anything that starts above autostep height and below the head blocks standing here
        if overlaps_footprint && by[1] > h + cfg.max_step_for_tests() && by[0] < body_top {
            return false;
        }
    }
    true
}

trait StepHeight {
    fn max_step_for_tests(&self) -> f32;
}
impl StepHeight for Cfg {
    /// Matches `CharacterSystem::new`: autostep `max_height` is 0.35.
    fn max_step_for_tests(&self) -> f32 {
        0.35
    }
}

fn build_grid(l: &LevelFile, cfg: &Cfg) -> Grid {
    let n = (2.0 * HALF / CELL) as usize;
    let mut g = Grid { n, nodes: vec![Vec::new(); n * n] };
    let max_slope = 45f32.to_radians();
    for iz in 0..n {
        for ix in 0..n {
            let (x, z) = (g.centre(ix), g.centre(iz));
            let mut hs: Vec<f32> = Vec::new();
            for (i, b) in l.level.boxes.iter().enumerate() {
                if b.rot_z.abs() > max_slope {
                    continue;
                }
                if let Some(h) = top_at(b, x, z) {
                    if standable(l, cfg, x, z, i, h) {
                        hs.push(h);
                    }
                }
            }
            hs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            hs.dedup_by(|a, b| (*a - *b).abs() < 0.05);
            let k = g.idx(ix, iz);
            g.nodes[k] = hs.into_iter().map(|h| Node { h }).collect();
        }
    }
    g
}

/// Rise a player can climb in one move: autostep, single jump, jump + double jump at the apex (with margin).
fn max_rise(cfg: &Cfg) -> f32 {
    let apex = |v: f32| v * v / (2.0 * cfg.gravity);
    let single = apex(cfg.jump_speed);
    // the double jump fires when |vy| <= peak_window: height gained by then, plus the second impulse
    let w = cfg.peak_window;
    let at_window = (cfg.jump_speed * cfg.jump_speed - w * w) / (2.0 * cfg.gravity);
    let double = at_window + apex(cfg.double_jump_speed);
    assert!(double > single);
    double - 0.3 // margin for timing; the physics test below lands the 2.4 m tier for real
}

fn reachable(g: &Grid, cfg: &Cfg, start: (usize, usize, f32)) -> Vec<Vec<bool>> {
    let rise = max_rise(cfg);
    let mut seen: Vec<Vec<bool>> = g.nodes.iter().map(|v| vec![false; v.len()]).collect();
    let find = |k: usize, h: f32| g.nodes[k].iter().position(|n| (n.h - h).abs() < 0.06);
    let s = find(g.idx(start.0, start.1), start.2).expect("start surface exists");
    let mut q = VecDeque::new();
    seen[g.idx(start.0, start.1)][s] = true;
    q.push_back((start.0, start.1, s));
    while let Some((ix, iz, si)) = q.pop_front() {
        let h = g.nodes[g.idx(ix, iz)][si].h;
        // walking: 8-neighbours. Jumping up (rise > autostep): also reach 2 cells (1 m), because the 0.3 m capsule
        // clearance blanks the strip right next to a taller block and a player jumps up from just outside it.
        for dz in -2i32..=2 {
            for dx in -2i32..=2 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let far = dx.abs() == 2 || dz.abs() == 2;
                let (jx, jz) = (ix as i32 + dx, iz as i32 + dz);
                if jx < 0 || jz < 0 || jx >= g.n as i32 || jz >= g.n as i32 {
                    continue;
                }
                let k = g.idx(jx as usize, jz as usize);
                for (ni, n) in g.nodes[k].iter().enumerate() {
                    let up = n.h - h;
                    let ok = if far { up > 0.35 && up <= rise } else { up <= rise };
                    if ok && !seen[k][ni] {
                        seen[k][ni] = true;
                        q.push_back((jx as usize, jz as usize, ni));
                    }
                }
            }
        }
    }
    seen
}

fn is_reached(g: &Grid, seen: &[Vec<bool>], x: f32, z: f32, h: f32, tol: f32) -> bool {
    let k = g.idx(g.cell(x), g.cell(z));
    g.nodes[k].iter().enumerate().any(|(i, n)| (n.h - h).abs() <= tol && seen[k][i])
}

// ---- tests ----

#[test]
fn loads_with_the_expected_content() {
    let l = arena();
    assert_eq!(l.level.spawns.len(), 8, "8 spawns");
    assert!((6..=8).contains(&l.pickups.len()), "6-8 pickups, got {}", l.pickups.len());
    assert!(l.level.boxes.len() >= 25);
    assert!(l.pickups.iter().all(|p| p.respawn_secs >= 10.0));
}

#[test]
fn spawns_and_pickups_are_not_inside_geometry_and_stand_on_something() {
    let l = arena();
    let cfg = Cfg::default();
    let reach = cfg.half_height + cfg.radius;
    for s in &l.level.spawns {
        // sample the capsule's silhouette: centre line and four sides, feet to head
        for dy in [-reach + 0.05, -0.3, 0.0, 0.3, reach - 0.05] {
            for (ox, oz) in [(0.0, 0.0), (cfg.radius, 0.0), (-cfg.radius, 0.0), (0.0, cfg.radius), (0.0, -cfg.radius)] {
                let p = [s[0] + ox, s[1] + dy, s[2] + oz];
                assert!(!any_inside(&l, p), "spawn {s:?}: capsule point {p:?} is inside geometry");
            }
        }
        // standing: some top face is within a skin of the feet
        let feet = s[1] - reach;
        let ground = l.level.boxes.iter().filter_map(|b| top_at(b, s[0], s[2])).filter(|h| (h - feet).abs() < 0.1).count();
        assert!(ground > 0, "spawn {s:?} floats (feet at {feet})");
    }
    for p in &l.pickups {
        assert!(!any_inside(&l, p.pos), "pickup {:?} is inside geometry", p.pos);
        let support = l.level.boxes.iter().filter_map(|b| top_at(b, p.pos[0], p.pos[2])).any(|h| (p.pos[1] - h - 1.0).abs() < 0.2);
        assert!(support, "pickup {:?} is not ~1 m above a surface", p.pos);
    }
}

#[test]
fn spawns_are_spread_and_pickups_do_not_stack() {
    let l = arena();
    let d = |a: [f32; 3], b: [f32; 3]| ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
    for (i, a) in l.level.spawns.iter().enumerate() {
        for b in &l.level.spawns[i + 1..] {
            assert!(d(*a, *b) >= 20.0, "spawns {a:?} and {b:?} are only {:.1} m apart", d(*a, *b));
        }
    }
    for (i, a) in l.pickups.iter().enumerate() {
        for b in &l.pickups[i + 1..] {
            assert!(d(a.pos, b.pos) >= 8.0 || (a.pos[1] - b.pos[1]).abs() > 2.0, "pickups {:?} / {:?} too close", a.pos, b.pos);
        }
    }
}

#[test]
fn bounds_are_sealed_floor_walls_and_ceiling() {
    let l = arena();
    let step = 0.25;
    let n = (2.0 * (HALF + 0.5) / step) as i32;
    let at = |i: i32| -(HALF + 0.5) + i as f32 * step;
    for i in 0..=n {
        let t = at(i);
        // floor over the footprint, ceiling over footprint plus rim
        for j in 0..=n {
            let u = at(j);
            if t.abs() <= HALF && u.abs() <= HALF {
                assert!(any_inside(&l, [t, -0.25, u]), "floor gap at ({t},{u})");
            }
            assert!(any_inside(&l, [t, CEILING_Y + 0.25, u]), "ceiling gap at ({t},{u})");
        }
        // perimeter: every height from floor to ceiling on all four sides
        let mut y = 0.0;
        while y <= CEILING_Y {
            for p in [[-HALF - 0.25, y, t], [HALF + 0.25, y, t], [t, y, -HALF - 0.25], [t, y, HALF + 0.25]] {
                assert!(any_inside(&l, p), "perimeter gap at {p:?}");
            }
            y += 0.25;
        }
    }
}

#[test]
fn every_spawn_pickup_and_tier_is_reachable_with_cfg_limits() {
    let l = arena();
    let cfg = Cfg::default();
    let g = build_grid(&l, &cfg);
    let s0 = l.level.spawns[0];
    let seen = reachable(&g, &cfg, (g.cell(s0[0]), g.cell(s0[2]), 0.0));

    for s in &l.level.spawns {
        assert!(is_reached(&g, &seen, s[0], s[2], 0.0, 0.06), "spawn {s:?} cannot be reached from spawn 0");
    }
    for p in &l.pickups {
        assert!(is_reached(&g, &seen, p.pos[0], p.pos[2], p.pos[1] - 1.0, 0.15), "pickup {:?} is unreachable", p.pos);
    }
    // each tier exists and is reachable: ziggurat 1.2 / 2.4 / 3.6, ledges 2.4, ledge steps 3.6
    for (x, z, h, what) in [
        (6.0, 6.0, 1.2, "platform"),
        (2.0, 2.0, 2.4, "ziggurat tier 2"),
        (0.0, 0.0, 3.6, "ziggurat tier 3"),
        (-22.0, -33.0, 2.4, "ledge A"),
        (-22.0, -37.0, 3.6, "ledge A step"),
        (22.0, 33.0, 2.4, "ledge B"),
        (22.0, 37.0, 3.6, "ledge B step"),
    ] {
        assert!(is_reached(&g, &seen, x, z, h, 0.06), "{what} ({x}, {z}) at {h} m is unreachable");
    }
    // nothing walkable above the top tier is reachable (no skybox perches / ceiling walking)
    let mut max_h = 0.0f32;
    for (k, v) in g.nodes.iter().enumerate() {
        for (i, n) in v.iter().enumerate() {
            if seen[k][i] {
                max_h = max_h.max(n.h);
            }
        }
    }
    assert!((max_h - 3.6).abs() < 0.06, "highest reachable surface is {max_h}, expected the 3.6 m tier");
}

#[test]
fn corridor_maths_fit_the_wall_run_and_wall_jump_limits() {
    let l = arena();
    let cfg = Cfg::default();
    // corridor walls: thin (x-extent <= 0.6), tall (>= 2.5 half height), not the perimeter
    let walls: Vec<&LevelBox> =
        l.level.boxes.iter().filter(|b| b.half[0] <= 0.6 && b.half[1] >= 2.5 && b.center[0].abs() < HALF - 1.0 && b.rot_z == 0.0).collect();
    assert_eq!(walls.len(), 4, "two corridors, two walls each");
    let mut corridors = 0;
    for a in &walls {
        for b in &walls {
            if a.center[0] < b.center[0] && (a.center[2] - b.center[2]).abs() < 0.01 && (a.half[2] - b.half[2]).abs() < 0.01 {
                corridors += 1;
                let gap = (b.center[0] - b.half[0]) - (a.center[0] + a.half[0]);
                let length = 2.0 * a.half[2];
                let top = a.center[1] + a.half[1];

                // run length: the wall-run covers wall_speed * wall_max_time; leave a run-up and an exit
                let run = cfg.wall_speed * cfg.wall_max_time;
                assert!(length >= 25.0 && length >= run + 8.0, "corridor {length} m vs wall-run {run} m + run-up");
                // tall enough for a run started at the double-jump peak, with the probe rays and head room
                let peak = max_rise(&cfg) + 0.3;
                let needed = peak + 2.0 * (cfg.half_height + cfg.radius) + cfg.half_height;
                assert!(top >= needed, "wall top {top} m below the {needed} m a peak-height run needs");
                // wall-jump across: horizontal push over the hop (up and back to the same height), minus the capsule
                let air = 2.0 * cfg.jump_speed / cfg.gravity;
                let reach = cfg.wall_jump_push * air;
                let need = gap - 2.0 * cfg.radius;
                assert!(need <= reach / 1.15, "gap {gap} m needs {need} m of hop but the push only gives {reach:.2} m");
                assert!(gap >= 2.0 * (cfg.radius + cfg.wall_probe), "walls close enough to cling to both at once (gap {gap})");
                assert!((4.5..=7.0).contains(&gap), "gap {gap} not corridor-like");
            }
        }
    }
    assert_eq!(corridors, 2);
}

// ---- real controller on the real file ----

fn world_with_player(l: &LevelFile, at: [f32; 3]) -> (PhysicsWorld, CharacterSystem) {
    let cfg = Cfg::default();
    let mut w = PhysicsWorld {
        bodies: RigidBodySet::new(),
        colliders: ColliderSet::new(),
        islands: IslandManager::new(),
        queries: QueryPipeline::new(),
    };
    l.level.insert_into(&mut w);
    let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![at[0], at[1], at[2]]).build());
    w.colliders.insert_with_parent(ColliderBuilder::capsule_y(cfg.half_height, cfg.radius).build(), body, &mut w.bodies);
    (w, CharacterSystem::new(body, cfg))
}

fn pos(w: &PhysicsWorld, ch: &CharacterSystem) -> Vector<Real> {
    *w.bodies[ch.body].translation()
}

#[test]
fn the_ramp_walks_up_to_the_platform() {
    let l = arena();
    let (mut w, mut ch) = world_with_player(&l, [-12.6, 0.81, 0.0]);
    let run = PlayerInput { move_dir: vector![1.0, 0.0, 0.0], ..Default::default() };
    let mut now = 0.0;
    for _ in 0..150 {
        step_player(&mut ch, &run, &mut w, now, DT);
        now += DT;
    }
    let p = pos(&w, &ch);
    assert!(ch.state == MoveState::Grounded && p.y > 1.2 + 0.79 && p.x > -7.0, "stuck on the ramp at {p:?} ({:?})", ch.state);
}

#[test]
fn wall_run_lasts_long_enough_to_use_the_corridor_and_hop_across() {
    let l = arena();
    // enter corridor A on its east wall (inner face x = -19.2), airborne at running speed
    let (mut w, mut ch) = world_with_player(&l, [-19.8, 1.6, -4.0]);
    ch.vel = vector![0.0, 0.0, -9.0];
    let fwd = vector![0.0, 0.0, -1.0];
    let mut now = 0.0;
    let mut wall_ticks = 0;
    let start_z = pos(&w, &ch).z;
    for _ in 0..40 {
        step_player(&mut ch, &PlayerInput { move_dir: fwd, wall_cling: true, ..Default::default() }, &mut w, now, DT);
        now += DT;
        wall_ticks += matches!(ch.state, MoveState::WallRun { .. }) as u32;
    }
    assert_eq!(wall_ticks, 40, "wall-run must hold from the first tick");
    // wall-jump across the corridor: steer sideways through the hop, then ask for the far wall with forward+side
    // input (wall-run entry needs a heading along the wall). It must catch the west wall (normal +x).
    step_player(&mut ch, &PlayerInput { move_dir: vector![-1.0, 0.0, 0.0], wall_cling: true, jump_pressed: true, jump_held: true, ..Default::default() }, &mut w, now, DT);
    now += DT;
    let mut caught = false;
    for _ in 0..70 {
        let near_far_wall = pos(&w, &ch).x <= -24.1;
        let dir = if near_far_wall { vector![-0.5, 0.0, -0.866] } else { vector![-1.0, 0.0, 0.0] };
        step_player(&mut ch, &PlayerInput { move_dir: dir, wall_cling: true, ..Default::default() }, &mut w, now, DT);
        now += DT;
        if let MoveState::WallRun { normal, .. } = ch.state {
            caught |= normal.x > 0.5;
        }
    }
    assert!(caught, "the wall-jump did not reach the opposite wall (at {:?})", pos(&w, &ch));
    // and a full-length run on one wall covers a real distance
    let (mut w, mut ch) = world_with_player(&l, [-19.8, 1.6, -3.0]);
    ch.vel = vector![0.0, 0.0, -9.0];
    let z0 = pos(&w, &ch).z;
    for _ in 0..100 {
        step_player(&mut ch, &PlayerInput { move_dir: fwd, wall_cling: true, ..Default::default() }, &mut w, now, DT);
        now += DT;
    }
    let run_len = z0 - pos(&w, &ch).z;
    assert!(run_len > 12.0 && pos(&w, &ch).z > -29.5, "ran {run_len} m, ended at z {}", pos(&w, &ch).z);
    let _ = start_z;
}

#[test]
fn a_double_jump_from_the_corridor_exit_lands_on_the_2_4_ledge() {
    let l = arena();
    let (mut w, mut ch) = world_with_player(&l, [-22.0, 0.81, -18.0]);
    let fwd = vector![0.0, 0.0, -1.0];
    let mut now = 0.0;
    let (mut jumped, mut doubled) = (false, false);
    for _ in 0..240 {
        let p = pos(&w, &ch);
        let mut input = PlayerInput { move_dir: fwd, ..Default::default() };
        if !jumped && p.z <= -25.0 {
            input.jump_pressed = true;
            input.jump_held = true;
            jumped = true;
        } else if jumped && !doubled && ch.vel.y <= 2.0 && ch.vel.y > -1.0 && ch.state == MoveState::Airborne {
            input.jump_pressed = true;
            doubled = true;
        }
        step_player(&mut ch, &input, &mut w, now, DT);
        now += DT;
    }
    let p = pos(&w, &ch);
    assert!(jumped && doubled, "script never jumped / double-jumped");
    assert!(ch.state == MoveState::Grounded && p.y > 2.4 + 0.79 && p.z < -30.0, "did not land on the ledge: {p:?} ({:?})", ch.state);
}
