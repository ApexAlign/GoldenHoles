//! The roof-deck nav grid on `levels/ffa_sky.lvl`: routes over the bridges and the void-edge guard's detector.
use shinobi_ai::nav::NavGrid;
use shinobi_sim::level::{parse_level_file, LevelFile};
use shinobi_sim::rapier3d::prelude::Vector;
use shinobi_sim::tick::Server;

fn sky() -> Option<(Server, LevelFile)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels/ffa_sky.lvl");
    let f = parse_level_file(&std::fs::read_to_string(path).ok()?).expect("level parses");
    let mut s = Server::new(shinobi_sim::character::Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_spawns(f.level.spawns.clone());
    Some((s, f))
}

/// The roof grid plans a walkable route over the bridges from every spawn to every other, across the void gaps.
#[test]
fn sky_nav_routes_every_spawn_to_every_other_over_the_bridges() {
    let Some((s, f)) = sky() else { return };
    let nav = NavGrid::build_sky(&s, 62.0, 1.0, 92.0);
    assert!(nav.is_sky());
    let sp = &f.level.spawns;
    for (i, a) in sp.iter().enumerate() {
        for b in &sp[i + 1..] {
            assert!(nav.path([a[0], a[2]], [b[0], b[2]], 60_000).is_some(), "no walkable route from spawn {a:?} to {b:?}");
        }
    }
}

/// From the middle of a roof nothing is near; the open launch notch and the void beyond it are an edge; a bridge is safe
/// along its length and an edge across it.
#[test]
fn sky_edge_guard_sees_the_void_and_not_the_deck() {
    let Some((s, _)) = sky() else { return };
    let nav = NavGrid::build_sky(&s, 62.0, 1.0, 92.0);
    let feet = 56.0;
    let at = |x: f32, z: f32| Vector::new(x, 57.5, z);
    let (east, north) = (Vector::new(1.0, 0.0, 0.0), Vector::new(0.0, 0.0, 1.0));
    // NE corner roof (50, 50, 18 x 18 m): mid roof is safe, the notch on the east face (z 47..53) opens onto the void
    assert!(!nav.edge_ahead(at(44.0, 46.0), east, 3.0, feet), "mid-roof is not an edge");
    assert!(nav.edge_ahead(at(56.0, 50.0), east, 6.0, feet), "the east notch opens onto the void");
    // the bridge between the east slab and the NE corner runs along z at x = 44: safe along it, an edge across it
    assert!(!nav.edge_ahead(at(44.0, 20.0), north, 8.0, feet), "walking along the bridge is safe");
    assert!(nav.edge_ahead(at(44.0, 20.0), east, 4.0, feet), "stepping sideways off the bridge is not");
}
