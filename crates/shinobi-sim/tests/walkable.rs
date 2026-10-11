//! Level walkability: nothing in a shipped map may be a knee-to-head-high block that can only be jumped. Every obstacle
//! is a lip the character steps over (<= `Cfg::step_height`), has a ramp or stair beside it, or is a full wall (>= 3 m).
//! `shinobi_sim::level::unwalkable_obstacles` is the validator; this test pins the maps and prints the others.

use shinobi_sim::character::Cfg;
use shinobi_sim::level::{parse_level_file, unwalkable_obstacles, LevelFile};

fn load(name: &str) -> Option<LevelFile> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels").join(name);
    parse_level_file(&std::fs::read_to_string(path).ok()?).ok()
}

fn offenders(name: &str) -> Option<Vec<String>> {
    let l = load(name)?;
    let step = Cfg::default().step_height;
    Some(
        unwalkable_obstacles(&l.level, step)
            .into_iter()
            .map(|i| {
                let b = &l.level.boxes[i];
                format!("box {i} at {:?} half {:?} (top {:.2})", b.center, b.half, b.center[1] + b.half[1])
            })
            .collect(),
    )
}

#[test]
fn ffa_sky_has_no_jump_only_obstacles() {
    let Some(o) = offenders("ffa_sky.lvl") else { return };
    assert!(o.is_empty(), "{} unwalkable obstacles in ffa_sky: {:?}", o.len(), &o[..o.len().min(10)]);
}

#[test]
fn ffa_isles_has_no_jump_only_obstacles() {
    let Some(o) = offenders("ffa_isles.lvl") else { return };
    assert!(o.is_empty(), "{} unwalkable obstacles in ffa_isles: {:?}", o.len(), &o[..o.len().min(10)]);
}

#[test]
fn ffa_split_has_no_jump_only_obstacles() {
    let Some(o) = offenders("ffa_split.lvl") else { return };
    assert!(o.is_empty(), "{} unwalkable obstacles in ffa_split: {:?}", o.len(), &o[..o.len().min(10)]);
}

#[test]
fn ffa_small_has_no_jump_only_obstacles() {
    let Some(o) = offenders("ffa_small.lvl") else { return };
    assert!(o.is_empty(), "{} unwalkable obstacles in ffa_small: {:?}", o.len(), &o[..o.len().min(10)]);
}

/// The big legacy maps are not fixed here; the counts are printed (run with --nocapture) and may not grow.
#[test]
fn legacy_maps_report_their_unwalkable_obstacles_and_do_not_get_worse() {
    for (name, cap) in [("ffa.lvl", 400usize), ("ffa_xl.lvl", 800), ("arena.lvl", 100), ("8v8.lvl", 400)] {
        let Some(o) = offenders(name) else { continue };
        eprintln!("{name}: {} unwalkable (jump-only) obstacles", o.len());
        assert!(o.len() <= cap, "{name}: {} unwalkable obstacles (cap {cap})", o.len());
    }
}

/// The validator itself: a 1.2 m block on a floor is flagged; with a ramp beside it, a 0.3 m curb, or at 3.5 m it is not.
#[test]
fn the_validator_flags_waist_high_blocks_only() {
    use shinobi_sim::level::{LevelBox, LevelDesc};
    let floor = LevelBox::new([0.0, -0.5, 0.0], [20.0, 0.5, 20.0]);
    let mk = |boxes: Vec<LevelBox>| LevelDesc { boxes, spawns: vec![[0.0, 0.81, 0.0]] };
    let step = 0.35;
    assert_eq!(unwalkable_obstacles(&mk(vec![floor, LevelBox::new([0.0, 0.6, 0.0], [1.5, 0.6, 1.5])]), step), vec![1], "1.2 m block");
    assert!(unwalkable_obstacles(&mk(vec![floor, LevelBox::new([0.0, 0.15, 0.0], [1.5, 0.15, 1.5])]), step).is_empty(), "0.3 m curb");
    assert!(unwalkable_obstacles(&mk(vec![floor, LevelBox::new([0.0, 1.75, 0.0], [1.5, 1.75, 1.5])]), step).is_empty(), "3.5 m wall");
    // a 20 degree ramp up to the 1.2 m block's west face (top meets the block top at x = -1.5)
    let (run, rise) = (3.3f32, 1.2f32);
    let th = (rise / run).atan();
    let len = (run * run + rise * rise).sqrt();
    let t = 0.4;
    let ramp = LevelBox::new([-1.5 - run / 2.0 + t * th.sin(), 0.6 - t * th.cos(), 0.0], [len / 2.0, t, 1.5]).rolled(th);
    assert!(unwalkable_obstacles(&mk(vec![floor, LevelBox::new([0.0, 0.6, 0.0], [1.5, 0.6, 1.5]), ramp]), step).is_empty(), "block with a ramp");
}
