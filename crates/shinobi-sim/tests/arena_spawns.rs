//! Spawn selection on the REAL arena (`levels/arena.lvl`): hidden beats visible, nobody spawns next to an
//! enemy. (Repeat spawns and spawn-kills in real fights: `shinobi-ai/tests/arena_spawns.rs`, with bots.)

use shinobi_sim::balance::Balance;
use shinobi_sim::character::Cfg;
use shinobi_sim::level::{parse_level_file, LevelFile};
use shinobi_sim::spawn::blocked;
use shinobi_sim::tick::Server;

fn level() -> LevelFile {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../levels/arena.lvl");
    parse_level_file(&std::fs::read_to_string(path).expect("levels/arena.lvl")).expect("parses")
}

fn arena(n: u16) -> Server {
    let f = level();
    let mut s = Server::new(Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_spawns(f.level.spawns.clone());
    s.set_balance(Balance::demo()).unwrap();
    s.match_state.cfg.frag_limit = u16::MAX;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    for id in 1..=n {
        let at = s.pick_spawn(None);
        s.add_player(id, at);
    }
    s
}

fn d2(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Every placement of up to 3 enemies on a coarse grid of floor positions.
fn enemy_sets() -> Vec<Vec<[f32; 3]>> {
    let pts: Vec<[f32; 3]> = [(-30.0, -30.0), (30.0, -30.0), (-30.0, 30.0), (30.0, 30.0), (0.0, 0.0), (-27.5, -20.0), (27.5, 20.0), (0.0, -25.0), (0.0, 25.0), (-20.0, 0.0), (20.0, 0.0)]
        .iter()
        .map(|(x, z)| [*x, 0.81, *z])
        .collect();
    let mut out = Vec::new();
    for a in 0..pts.len() {
        out.push(vec![pts[a]]);
        for b in a + 1..pts.len() {
            out.push(vec![pts[a], pts[b]]);
            for c in b + 1..pts.len() {
                out.push(vec![pts[a], pts[b], pts[c]]);
            }
        }
    }
    out
}

#[test]
fn chosen_spawns_are_far_from_every_enemy_and_hidden_whenever_a_hidden_one_exists() {
    let spawns = level().level.spawns;
    assert!(spawns.len() >= 8);
    let sets = enemy_sets();
    let (mut hidden_pick, mut visible_pick, mut min_seen) = (0, 0, f32::MAX);
    for enemies in &sets {
        let mut s = arena(enemies.len() as u16 + 1);
        // enemies = players 1..=n moved onto the grid; player n+1 is the one who needs a spawn
        for (i, e) in enemies.iter().enumerate() {
            let body = s.players[i].ch.body;
            s.world.bodies[body].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(e[0], e[1], e[2]), true);
            shinobi_sim::character::sync_body_colliders(&mut s.world, body);
        }
        let me = enemies.len() as u16 + 1;
        let pick = s.pick_spawn(Some(me));
        assert!(spawns.contains(&pick), "{pick:?} is a spawn point");
        let seen = enemies.iter().any(|e| !blocked(&s.world, *e, pick));
        let any_hidden = spawns.iter().any(|sp| enemies.iter().all(|e| blocked(&s.world, *e, *sp)));
        if any_hidden {
            assert!(!seen, "a hidden spawn existed but {pick:?} is visible to an enemy (enemies {enemies:?})");
            hidden_pick += 1;
        } else {
            visible_pick += 1;
        }
        let nearest = enemies.iter().map(|e| d2(*e, pick)).fold(f32::MAX, f32::min);
        min_seen = min_seen.min(nearest);
        assert!(nearest >= 6.0, "spawned only {nearest:.1} m from an enemy (enemies {enemies:?}, pick {pick:?})");
    }
    eprintln!("{} scenarios: {hidden_pick} had a hidden spawn (all picked hidden), {visible_pick} had none; closest spawn to any enemy {min_seen:.1} m", sets.len());
    assert!(hidden_pick > sets.len() / 3, "the arena actually offers hidden spawns in most situations");
}

/// A respawn sequence on the real arena: victim `4` respawns 96 times while three enemies hop around (they stay put
/// for 4 respawns at a time, like campers). Respawns must spread over the arena, rarely repeat the previous spawn
/// back to back (a lone hidden spawn forces a repeat while campers stay put), and never land next to an enemy.
#[test]
fn repeated_respawns_spread_over_the_arena_and_never_land_next_to_an_enemy() {
    let spawns = level().level.spawns;
    let mut s = arena(4);
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let mut rand = |m: f32| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        ((rng >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0) * m
    };
    let (mut picks, mut nearest_min) = (Vec::new(), f32::MAX);
    for round in 0..24 {
        let enemies: Vec<[f32; 3]> = (0..3).map(|_| [rand(34.0), 0.81, rand(34.0)]).collect();
        for _ in 0..4 {
            for (i, e) in enemies.iter().enumerate() {
                let body = s.players[i].ch.body;
                s.world.bodies[body].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(e[0], e[1], e[2]), true);
                shinobi_sim::character::sync_body_colliders(&mut s.world, body);
            }
            let pick = s.pick_spawn(Some(4));
            nearest_min = nearest_min.min(enemies.iter().map(|e| d2(*e, pick)).fold(f32::MAX, f32::min));
            picks.push(spawns.iter().position(|sp| *sp == pick).expect("a spawn point"));
        }
        let _ = round;
    }
    let mut used = std::collections::BTreeMap::<usize, u32>::new();
    for p in &picks {
        *used.entry(*p).or_default() += 1;
    }
    let same = picks.windows(2).filter(|w| w[0] == w[1]).count();
    let top = *used.values().max().unwrap() as f32 / picks.len() as f32;
    eprintln!("{} respawns over {} of {} spawns, same-as-previous {}, top share {:.0}%, closest enemy {nearest_min:.1} m: {used:?}", picks.len(), used.len(), spawns.len(), same, 100.0 * top);
    assert!(used.len() >= 5, "only {} distinct spawns: {used:?}", used.len());
    assert!(top <= 0.35, "one spawn took {:.0}%: {used:?}", 100.0 * top);
    assert!((same as f32) / (picks.len() as f32) <= 0.30, "{same} back-to-back repeats of {}", picks.len());
    assert!(nearest_min >= 8.0, "a respawn landed {nearest_min:.1} m from an enemy");
}
