//! Run walls: every shipped level carries long, tall wall-run panels with a hook anchor beside each (`scripts/runwalls.py`,
//! emitted by the `scripts/gen_*_level.py` generators into a `# --- run walls` section; `levels/arena.lvl` is patched by
//! `python3 scripts/runwalls.py levels/arena.lvl arena`). Each changed level is loaded and pinned: panel counts, size, a base
//! resting on a surface, nothing intersecting them or hanging within reach of their run zone, a hook anchor past the exit end,
//! spawns / pickups kept clear, the Split indestructible, the destructible towers (hive, clock) built of 2..6 m bricks.

use shinobi_sim::level::{parse_level_file, LevelBox, LevelFile};
use shinobi_sim::rapier3d::prelude::*;

/// (name, file, panels, long axis min length m, destructible)
const LEVELS: [(&str, &str, usize, f32, bool); 12] = [
    ("8v8", "8v8.lvl", 12, 20.0, false),
    ("arena", "arena.lvl", 10, 20.0, false),
    ("ffa", "ffa.lvl", 14, 20.0, false),
    ("ffa_xl", "ffa_xl.lvl", 14, 20.0, false),
    ("small", "ffa_small.lvl", 8, 20.0, false),
    ("sky", "ffa_sky.lvl", 12, 20.0, false),
    ("isles", "ffa_isles.lvl", 12, 20.0, false),
    ("hive", "ffa_hive.lvl", 8, 18.0, true),
    ("clock", "ffa_clock.lvl", 8, 12.0, true),
    ("split", "ffa_split.lvl", 9, 20.0, false),
    // the props variant is the Split plus props: its run walls are the Split's
    ("split_props", "ffa_split_props.lvl", 9, 20.0, false),
    ("runs", "ffa_runs.lvl", 0, 0.0, false),
];

struct Loaded {
    file: LevelFile,
    /// Index range in `file.level.boxes` of the `# --- run walls` section (empty for `runs`, which has its own test).
    rw: std::ops::Range<usize>,
}

fn load(file: &str) -> Loaded {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels").join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let parsed = parse_level_file(&text).unwrap_or_else(|e| panic!("{file}: {e}"));
    let (mut n, mut start, mut end, mut in_rw) = (0usize, None, None, false);
    for raw in text.lines() {
        if raw.starts_with("# ---") {
            if in_rw && end.is_none() {
                end = Some(n);
            }
            in_rw = raw.starts_with("# --- run walls");
            if in_rw {
                start = Some(n);
            }
        }
        let t = raw.split('#').next().unwrap().trim_start();
        if t.starts_with("box ") || t.starts_with("dbox ") {
            n += 1;
        }
    }
    if in_rw && end.is_none() {
        end = Some(n);
    }
    let rw = match (start, end) {
        (Some(a), Some(b)) => a..b,
        _ => 0..0,
    };
    Loaded { file: parsed, rw }
}

fn overlaps(a: &LevelBox, b: &LevelBox, margin: [f32; 3]) -> bool {
    (0..3).all(|i| (a.center[i] - b.center[i]).abs() < a.half[i] + b.half[i] + margin[i])
}

fn top_at(boxes: &[&LevelBox], x: f32, z: f32) -> Vec<f32> {
    boxes.iter().filter(|b| b.rot_z == 0.0 && (x - b.center[0]).abs() <= b.half[0] && (z - b.center[2]).abs() <= b.half[2]).map(|b| b.center[1] + b.half[1]).collect()
}

/// A panel is a tall thin run-wall slab; bricks of a segmented panel (same line, same height, touching) are merged.
#[derive(Clone, Debug)]
struct Panel {
    center: [f32; 3],
    half: [f32; 3],
    along_x: bool,
}

fn panels_of(boxes: &[LevelBox]) -> (Vec<Panel>, Vec<LevelBox>) {
    let (mut tall, mut other): (Vec<LevelBox>, Vec<LevelBox>) = (vec![], vec![]);
    for b in boxes {
        let thin = b.half[0].min(b.half[2]);
        if b.rot_z == 0.0 && b.half[1] >= 2.9 && thin <= 1.01 && b.half[0].max(b.half[2]) >= 2.9 {
            tall.push(*b)
        } else {
            other.push(*b)
        }
    }
    let mut out: Vec<Panel> = vec![];
    for b in tall {
        let along_x = b.half[0] > b.half[2];
        // merge into an existing panel when it continues it along the same line at the same height
        let hit = out.iter_mut().find(|p| {
            p.along_x == along_x
                && (p.center[1] - b.center[1]).abs() < 0.01
                && (p.half[1] - b.half[1]).abs() < 0.01
                && if along_x {
                    (p.center[2] - b.center[2]).abs() < 0.01 && (p.half[2] - b.half[2]).abs() < 0.01 && (p.center[0] - b.center[0]).abs() <= p.half[0] + b.half[0] + 0.01
                } else {
                    (p.center[0] - b.center[0]).abs() < 0.01 && (p.half[0] - b.half[0]).abs() < 0.01 && (p.center[2] - b.center[2]).abs() <= p.half[2] + b.half[2] + 0.01
                }
        });
        match hit {
            Some(p) => {
                let i = if along_x { 0 } else { 2 };
                let (lo, hi) = ((p.center[i] - p.half[i]).min(b.center[i] - b.half[i]), (p.center[i] + p.half[i]).max(b.center[i] + b.half[i]));
                p.center[i] = (lo + hi) / 2.0;
                p.half[i] = (hi - lo) / 2.0;
            }
            None => out.push(Panel { center: b.center, half: b.half, along_x }),
        }
    }
    (out, other)
}

fn as_box(p: &Panel) -> LevelBox {
    LevelBox::new(p.center, p.half)
}

#[test]
fn every_changed_level_loads_and_carries_its_pinned_number_of_run_walls() {
    for (name, file, count, min_len, destructible) in LEVELS {
        let ld = load(file);
        if name == "runs" {
            continue;
        }
        let rw: Vec<LevelBox> = ld.file.level.boxes[ld.rw.clone()].to_vec();
        assert!(!rw.is_empty(), "{name}: no `# --- run walls` section");
        let (panels, anchors) = panels_of(&rw);
        assert_eq!(panels.len(), count, "{name}: {} run-wall panels", panels.len());
        assert!((8..=16).contains(&panels.len()), "{name}: 8-16 panels per level");
        for p in &panels {
            let len = 2.0 * p.half[if p.along_x { 0 } else { 2 }];
            assert!(len >= min_len - 0.01, "{name}: panel {p:?} only {len} m long");
            assert!(2.0 * p.half[1] >= 5.99, "{name}: panel {p:?} only {} m tall", 2.0 * p.half[1]);
            assert!(len <= 24.01 && 2.0 * p.half[if p.along_x { 2 } else { 0 }] <= 2.01, "{name}: panel {p:?} too fat");
        }
        // permanence: only hive / clock (destructible towers) break their panels; everyone else, the Split above all, is bedrock
        for b in &rw {
            assert_eq!(b.hp > 0.0, destructible, "{name}: run-wall box {b:?} destructible = {}", b.hp > 0.0);
        }
        if name.starts_with("split") {
            assert!(ld.file.level.boxes.iter().all(|b| b.hp == 0.0 || name == "split_props"), "{name}: the Split stays indestructible");
        }
        if name == "split" {
            assert!(ld.file.level.boxes.iter().all(|b| b.hp == 0.0), "the Split has no destructible box at all");
        }
        // anchors: dedicated slabs for all but the clock (whose drum is all anchors and whose box budget is spent)
        if name == "clock" {
            assert!(anchors.is_empty());
        } else {
            assert_eq!(anchors.len(), panels.len(), "{name}: one hook anchor per panel");
        }
    }
}

#[test]
fn panels_rest_on_a_surface_and_nothing_else_touches_or_crowds_them() {
    for (name, file, _, _, _) in LEVELS {
        if name == "runs" {
            continue;
        }
        let ld = load(file);
        let boxes = &ld.file.level.boxes;
        let rw: Vec<LevelBox> = boxes[ld.rw.clone()].to_vec();
        // split_props = the Split + a props layer appended after it (crates, lamps: harmless beside a wall-run): judge only the Split's boxes
        let limit = if name == "split_props" { load("ffa_split.lvl").file.level.boxes.len() } else { boxes.len() };
        let rest: Vec<&LevelBox> = boxes.iter().enumerate().filter(|(i, _)| !ld.rw.contains(i) && *i < limit).map(|(_, b)| b).collect();
        let (panels, _) = panels_of(&rw);
        for p in &panels {
            let base = p.center[1] - p.half[1];
            let (ai, ci) = if p.along_x { (0, 2) } else { (2, 0) };
            // support: along the whole centre line, a non-run-wall box has its top at the panel base (the support rests exactly)
            let n = (2.0 * p.half[ai]) as i32;
            for k in 0..=n {
                let t = -p.half[ai] + 0.01 + (2.0 * p.half[ai] - 0.02) * k as f32 / n as f32;
                let (x, z) = if p.along_x { (p.center[0] + t, p.center[2]) } else { (p.center[0], p.center[2] + t) };
                let tops = top_at(&rest, x, z);
                assert!(tops.iter().any(|h| (h - base).abs() < 0.06), "{name}: panel {p:?} unsupported at ({x}, {z}), tops {tops:?}");
            }
            // clear: nothing but its own support intersects the panel; its run zone (3.5 m beside it, 1 m past the ends,
            // 0.5 m headroom) is empty of every other box
            let pb = as_box(p);
            let top = p.center[1] + p.half[1];
            let (zlo, zhi) = (base + 0.06, top + 0.97);
            let mut zh = p.half;
            zh[ai] += 1.0;
            zh[ci] += 3.5;
            zh[1] = (zhi - zlo) / 2.0;
            let zone = LevelBox::new([p.center[0], (zlo + zhi) / 2.0, p.center[2]], zh);
            for b in &rest {
                assert!(!overlaps(&pb, b, [-0.06; 3]), "{name}: panel {p:?} intersects {b:?}");
                assert!(!overlaps(&zone, b, [0.0; 3]), "{name}: {b:?} crowds the run zone of panel {p:?}");
            }
            for other in &panels {
                if std::ptr::eq(p, other) {
                    continue;
                }
                assert!(!overlaps(&pb, &as_box(other), [0.0; 3]), "{name}: two panels intersect: {p:?} {other:?}");
            }
        }
    }
}

#[test]
fn every_panel_has_a_hook_anchor_past_its_end_and_spawns_and_pickups_stay_clear() {
    for (name, file, _, _, _) in LEVELS {
        if name == "runs" {
            continue;
        }
        let ld = load(file);
        let boxes = &ld.file.level.boxes;
        let rw: Vec<LevelBox> = boxes[ld.rw.clone()].to_vec();
        let rest: Vec<&LevelBox> = boxes.iter().enumerate().filter(|(i, _)| !ld.rw.contains(i)).map(|(_, b)| b).collect();
        let (panels, anchors) = panels_of(&rw);
        for p in &panels {
            let ai = if p.along_x { 0 } else { 2 };
            let top = p.center[1] + p.half[1];
            // an anchor: a box above the panel top, hanging 8 m past one end (+-1.5 m), within 2 m of the panel line
            let near_end = |b: &LevelBox| {
                let d = (b.center[ai] - p.center[ai]).abs() - p.half[ai];
                let ci = 2 - ai;
                (6.5..=9.5).contains(&d) && (b.center[ci] - p.center[ci]).abs() < 1.0 && b.center[1] > top + 1.0 && b.center[1] < top + 3.5
            };
            let dedicated = anchors.iter().any(|b| near_end(b));
            // existing geometry anchors the clock's panels: any other box 4..30 m of an end that is high enough to hang from
            let existing = rest.iter().any(|b| {
                let d = ((b.center[0] - p.center[0]).powi(2) + (b.center[2] - p.center[2]).powi(2)).sqrt() - p.half[ai];
                (0.0..=30.0).contains(&d) && b.center[1] + b.half[1] > top - 1.0
            });
            assert!(if name == "clock" { existing } else { dedicated }, "{name}: panel {p:?} has no hook anchor past its end");
            let _ = existing;
        }
        for b in &anchors {
            assert!(b.center[1] - b.half[1] > 4.0, "{name}: anchor {b:?} hangs too low to be a swing point");
            assert!(!rest.iter().any(|o| overlaps(b, o, [0.0; 3])), "{name}: anchor {b:?} inside geometry");
        }
        // spawns and pickups: >= 2 m (horizontally) from every run-wall box, whatever its height (an anchor above a spawn raises
        // `top_at` and breaks the grounded-spawn rule of the level's own tests)
        let pts: Vec<[f32; 3]> = ld.file.level.spawns.iter().copied().chain(ld.file.pickups.iter().map(|p| p.pos)).collect();
        for pt in pts {
            for b in &rw {
                let (dx, dz) = ((pt[0] - b.center[0]).abs() - b.half[0], (pt[2] - b.center[2]).abs() - b.half[2]);
                // never directly under a box within 15 m (an anchor above a spawn raises `top_at` and breaks the level's grounded-spawn rule)
                assert!(dx > 0.0 || dz > 0.0 || pt[1] > b.center[1] + b.half[1] + 1.5 || b.center[1] - b.half[1] - pt[1] > 15.0, "{name}: point {pt:?} is directly under / inside run-wall box {b:?}");
                // and 2 m clear of any box that reaches its body height
                let reaches = b.center[1] + b.half[1] > pt[1] - 1.0 && b.center[1] - b.half[1] < pt[1] + 3.0;
                assert!(!reaches || dx > 2.0 || dz > 2.0, "{name}: point {pt:?} is within 2 m of run-wall box {b:?}");
            }
        }
        // the whole level still builds a collider world
        let mut w = shinobi_sim::character::PhysicsWorld { bodies: RigidBodySet::new(), colliders: ColliderSet::new(), islands: IslandManager::new(), queries: QueryPipeline::new() };
        ld.file.level.insert_into(&mut w);
        assert_eq!(w.colliders.len(), boxes.len(), "{name}");
    }
}

#[test]
fn the_symmetric_levels_keep_their_symmetry_with_the_run_walls() {
    // 8v8: mirror-symmetric across z and x (its own test also pins it); ffa / xl / small / arena: 180 degree rotational symmetry
    let key = |c: [f32; 3], h: [f32; 3]| [(c[0] * 100.0).round() as i64, (c[1] * 100.0).round() as i64, (c[2] * 100.0).round() as i64, (h[0] * 100.0).round() as i64, (h[1] * 100.0).round() as i64, (h[2] * 100.0).round() as i64];
    for (name, file, mx, mz) in [("8v8", "8v8.lvl", -1.0f32, 1.0f32), ("8v8", "8v8.lvl", 1.0, -1.0), ("arena", "arena.lvl", -1.0, -1.0), ("ffa", "ffa.lvl", -1.0, -1.0), ("ffa_xl", "ffa_xl.lvl", -1.0, -1.0), ("small", "ffa_small.lvl", -1.0, -1.0)] {
        let ld = load(file);
        let mut a: Vec<_> = ld.file.level.boxes[ld.rw.clone()].iter().map(|b| key(b.center, b.half)).collect();
        let mut b: Vec<_> = ld.file.level.boxes[ld.rw.clone()].iter().map(|b| key([b.center[0] * mx, b.center[1], b.center[2] * mz], b.half)).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "{name}: run walls are not symmetric (x {mx}, z {mz})");
    }
}
