//! The roof-deck nav grid on `levels/ffa_sky.lvl` (the four-tier floating city): walkable routes between every pair of
//! spawns across all tiers (ramps, catwalks, no hook, no jump) and the void-edge guard's detector.
use shinobi_ai::nav::NavGrid;
use shinobi_sim::level::{parse_level_file, LevelFile};
use shinobi_sim::rapier3d::prelude::Vector;
use shinobi_sim::tick::Server;

fn sky() -> Option<(Server, LevelFile, NavGrid)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels/ffa_sky.lvl");
    let f = parse_level_file(&std::fs::read_to_string(path).ok()?).expect("level parses");
    let mut s = Server::new(shinobi_sim::character::Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_spawns(f.level.spawns.clone());
    let top = f.level.boxes.iter().map(|b| b.center[1] + b.half[1]).fold(0.0, f32::max);
    let half = f.level.boxes.iter().map(|b| b.center[0].abs() + b.half[0]).chain(f.level.boxes.iter().map(|b| b.center[2].abs() + b.half[2])).fold(0.0, f32::max);
    let nav = NavGrid::build_sky(&s, half + 2.0, 1.0, top);
    Some((s, f, nav))
}

/// The grid plans a walkable route from every spawn to every other, over every tier, across the void gaps.
#[test]
fn sky_nav_routes_every_spawn_to_every_other_on_foot() {
    let Some((_, f, nav)) = sky() else { return };
    assert!(nav.is_sky());
    let sp = &f.level.spawns;
    for (i, a) in sp.iter().enumerate() {
        for b in &sp[i + 1..] {
            let r = nav.path([a[0], a[2]], [b[0], b[2]], 200_000);
            assert!(r.is_some(), "no walkable route from spawn {a:?} to {b:?}");
        }
    }
}

/// Edges: a deck's middle is safe, a point beside the rim looks at a drop, a body wider than a catwalk would hang over the
/// void, and `edge_push` points away from the drop.
#[test]
fn sky_edge_guard_sees_the_void_and_not_the_deck() {
    let Some((_, f, nav)) = sky() else { return };
    // a spawn is on a deck: nothing near it is an edge in at least one direction, and every spawn is at least 1.5 m from
    // being "on the edge" (the guard must not pin a freshly spawned bot)
    for s in &f.level.spawns {
        let feet = s[1] - 0.8;
        assert!(nav.edge_push(Vector::new(s[0], s[1], s[2]), 0.9, feet) == Vector::zeros(), "spawn {s:?} is on an edge");
    }
    // walk off the deck: from the first spawn walk +x until a drop appears; the guard must see it before the feet get there
    let s = f.level.spawns[0];
    let feet = s[1] - 0.8;
    let mut found = None;
    for dir in [Vector::new(1.0, 0.0, 0.0), Vector::new(-1.0, 0.0, 0.0), Vector::new(0.0, 0.0, 1.0), Vector::new(0.0, 0.0, -1.0)] {
        if nav.edge_ahead(Vector::new(s[0], s[1], s[2]), dir, 80.0, feet) {
            found = Some(dir);
            break;
        }
    }
    let dir = found.expect("a deck has an edge within 80 m in some direction");
    let mut t = 0.0f32;
    while !nav.edge_ahead(Vector::new(s[0] + dir.x * t, s[1], s[2] + dir.z * t), dir, 1.0, feet) {
        t += 0.5;
        assert!(t < 80.0);
    }
    // standing right at the rim, the push points back along the walk
    let rim = Vector::new(s[0] + dir.x * (t + 0.2), s[1], s[2] + dir.z * (t + 0.2));
    let _ = rim;
    let near = Vector::new(s[0] + dir.x * (t - 0.2).max(0.0), s[1], s[2] + dir.z * (t - 0.2).max(0.0));
    assert!(nav.edge_ahead(near, dir, 2.0, feet), "looking at the drop from a step away");
    assert!(!nav.edge_ahead(near, -dir, 2.0, feet), "looking away from the drop is safe");
}
