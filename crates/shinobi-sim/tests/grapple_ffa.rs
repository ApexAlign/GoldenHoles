//! Wrist grapple over the real `levels/ffa.lvl` geometry: from every spawn, at a fan of yaws and pitches, fire the hook and
//! follow it hook -> pull -> release with the real character controller. Pins: any aim that has hookable geometry in range
//! hooks, the player never ends up inside geometry, the rope never snags (attached but not moving), speeds stay finite and
//! capped, and the release carries momentum.

use shinobi_sim::character::{Cfg, CharacterSystem, PhysicsWorld, PlayerInput};
use shinobi_sim::level::{parse_level_file, LevelBox, LevelFile};
use shinobi_sim::movement::step_player;
use shinobi_sim::rapier3d::prelude::*;

const DT: f32 = 1.0 / 60.0;

fn load() -> LevelFile {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels/ffa.lvl");
    parse_level_file(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn world(l: &LevelFile) -> PhysicsWorld {
    let mut w = PhysicsWorld { bodies: RigidBodySet::new(), colliders: ColliderSet::new(), islands: IslandManager::new(), queries: QueryPipeline::new() };
    l.level.insert_into(&mut w);
    w.queries.update(&w.colliders);
    w
}

fn inside(b: &LevelBox, p: [f32; 3]) -> bool {
    let (s, c) = b.rot_z.sin_cos();
    let (dx, dy) = (p[0] - b.center[0], p[1] - b.center[1]);
    let (u, v) = (dx * c + dy * s, -dx * s + dy * c);
    u.abs() <= b.half[0] && v.abs() <= b.half[1] && (p[2] - b.center[2]).abs() <= b.half[2]
}

fn player(w: &mut PhysicsWorld, at: [f32; 3]) -> CharacterSystem {
    let cfg = Cfg::default();
    let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![at[0], at[1], at[2]]).build());
    w.colliders.insert_with_parent(ColliderBuilder::capsule_y(cfg.half_height, cfg.radius).build(), body, &mut w.bodies);
    CharacterSystem::new(body, cfg)
}

fn aim(yaw: f32, pitch: f32) -> Vector<Real> {
    vector![-yaw.sin() * pitch.cos(), pitch.sin(), -yaw.cos() * pitch.cos()]
}

fn pos(w: &PhysicsWorld, ch: &CharacterSystem) -> Vector<Real> {
    *w.bodies[ch.body].translation()
}

#[derive(Default)]
struct Tally {
    shots: u32,
    hooked: u32,
    reachable: u32,
    missed_reachable: Vec<String>,
    snags: Vec<String>,
    inside: Vec<String>,
    bad_speed: Vec<String>,
    carried: u32,
    released: u32,
}

#[test]
fn hook_pull_release_over_the_ffa_map_never_snags_clips_or_goes_nonfinite() {
    let l = load();
    let cfg = Cfg::default();
    let mut t = Tally::default();
    for (si, sp) in l.level.spawns.iter().enumerate() {
        for yi in 0..12 {
            for pitch_deg in [-30.0f32, 0.0, 30.0, 60.0] {
                let yaw = yi as f32 * std::f32::consts::TAU / 12.0;
                let mut w = world(&l);
                let mut ch = player(&mut w, *sp);
                let mut now = 0.0;
                for _ in 0..20 {
                    step_player(&mut ch, &PlayerInput::default(), &mut w, now, DT);
                    now += DT;
                }
                let start = pos(&w, &ch);
                let eye = start + Vector::y() * ch.eye_above();
                let dir = aim(yaw, pitch_deg.to_radians());
                w.queries.update(&w.colliders);
                let reachable = ch.grapple_probe(&w, eye, dir).is_some();
                t.shots += 1;
                t.reachable += reachable as u32;
                step_player(&mut ch, &PlayerInput { grapple_aim: Some(dir), ..Default::default() }, &mut w, now, DT);
                now += DT;
                let tag = format!("spawn {si} yaw {yi} pitch {pitch_deg}");
                if ch.grapple.is_none() {
                    if reachable {
                        t.missed_reachable.push(tag);
                    }
                    continue;
                }
                t.hooked += 1;
                let anchor = ch.grapple.unwrap().anchor;
                let mut was_attached = true;
                let mut still = 0u32;
                let mut last = pos(&w, &ch);
                let mut release_speed = None;
                for k in 0..(cfg.grapple_max_time * 60.0) as u32 + 120 {
                    step_player(&mut ch, &PlayerInput::default(), &mut w, now, DT);
                    now += DT;
                    let p = pos(&w, &ch);
                    let v = ch.vel;
                    if !(p.iter().all(|x| x.is_finite()) && v.iter().all(|x| x.is_finite())) {
                        t.bad_speed.push(format!("{tag}: non-finite at tick {k}"));
                        break;
                    }
                    if v.norm() > (cfg.grapple_far_speed + 4.0).max(cfg.terminal) * 1.2 {
                        t.bad_speed.push(format!("{tag}: speed {:.1} at tick {k}", v.norm()));
                    }
                    if l.level.boxes.iter().any(|b| inside(b, [p.x, p.y, p.z])) {
                        let bx: Vec<String> = l.level.boxes.iter().filter(|b| inside(b, [p.x, p.y, p.z])).map(|b| format!("{:?} half {:?} rot_z {}", b.center, b.half, b.rot_z)).collect();
                        t.inside.push(format!("{tag}: centre inside geometry at tick {k} boxes {bx:?} anchor {:?} state {:?} vel {:?} pos {:?} grapple {:?}", anchor, ch.state, ch.vel, p, ch.grapple.is_some()));
                        break;
                    }
                    if ch.grapple.is_some() {
                        // attached: the player must keep moving (or the rope is dead weight against an edge)
                        still = if (p - last).norm() < 0.002 { still + 1 } else { 0 };
                        if still > 45 {
                            t.snags.push(format!("{tag}: stuck {still} ticks on a taut rope near anchor {:.1}", (anchor - p).norm()));
                            break;
                        }
                    } else if was_attached {
                        t.released += 1;
                        release_speed = Some(v.norm());
                        was_attached = false;
                    }
                    last = p;
                }
                if let Some(rs) = release_speed {
                    t.carried += (rs > 2.0) as u32;
                }
            }
        }
    }
    eprintln!(
        "grapple ffa: {} shots, {} reachable, {} hooked, {} released, {} carried >2 m/s | missed-reachable {} snags {} inside {} badspeed {}",
        t.shots, t.reachable, t.hooked, t.released, t.carried, t.missed_reachable.len(), t.snags.len(), t.inside.len(), t.bad_speed.len()
    );
    assert!(t.hooked > 100, "should hook plenty of the fan: {}", t.hooked);
    assert!(t.missed_reachable.is_empty(), "reachable geometry but no hook: {:?}", &t.missed_reachable[..t.missed_reachable.len().min(8)]);
    assert!(t.bad_speed.is_empty(), "{:?}", &t.bad_speed[..t.bad_speed.len().min(8)]);
    assert!(t.inside.is_empty(), "{:?}", &t.inside[..t.inside.len().min(8)]);
    assert!(t.snags.is_empty(), "{} snags: {:?}", t.snags.len(), &t.snags[..t.snags.len().min(8)]);
}
