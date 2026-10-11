//! Bots on `levels/ffa_hive.lvl` (THE HIVE, nine ring floors round an open well): they must fight on the floors without
//! walking into the well, without freezing, and be seen on many floors. Run:
//!   cargo test --release -p shinobi-ai --test hive -- --nocapture
mod common;
use std::collections::BTreeMap;

use common::*;
use shinobi_ai::{Archetype, BotSkill};
use shinobi_sim::balance::Balance;
use shinobi_sim::level::parse_level_file;
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::tick::Server;

struct Report {
    void_deaths: u32,
    splats: u32,
    ring_outs: u32,
    kills: u32,
    stuck: Vec<(u16, u32, f32, f32, &'static str)>,
    /// distinct 8 m x 8 m cells visited by each bot (a bot that never leaves its spawn deck scores low)
    max_roam: usize,
    /// distinct floors (0..9) on which a grounded bot stood (spawns count)
    tiers_seen: usize,
    /// floor changes of a living, unprotected bot between two one-second samples (ramps, falls onto a lower deck, hooks)
    climbs: u32,
}

fn run(bots: u16, secs: u32, seed: u64) -> Option<Report> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels/ffa_hive.lvl");
    let text = std::fs::read_to_string(path).ok()?;
    let f = parse_level_file(&text).expect("level parses");
    let mut s = Server::new(shinobi_sim::character::Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_spawns(f.level.spawns.clone());
    s.set_balance(Balance::demo()).unwrap();
    s.match_state.cfg.frag_limit = u16::MAX;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    let mut h = host(seed);
    h.rebuild_perches(&s);
    for i in 0..bots {
        assert!(h.add_bot(&mut s, 201 + i, if i % 3 == 0 { Archetype::Heavy } else { Archetype::Light }, BotSkill::Normal));
    }
    let mut window: BTreeMap<u16, (u32, [f32; 3])> = BTreeMap::new();
    let mut roam: BTreeMap<u16, std::collections::BTreeSet<(i32, i32)>> = BTreeMap::new();
    let mut tiers: std::collections::BTreeSet<i32> = std::collections::BTreeSet::new();
    let mut rep = Report { void_deaths: 0, splats: 0, ring_outs: 0, kills: 0, stuck: Vec::new(), max_roam: 0, tiers_seen: 0, climbs: 0 };
    let mut band_of: BTreeMap<u16, i32> = BTreeMap::new();
    let mut last: BTreeMap<u16, std::collections::VecDeque<([f32; 3], &'static str, [f32; 3], String)>> = BTreeMap::new();
    for t in 0..secs * 60 {
        step(&mut h, &mut s);
        for e in &s.pvp_events {
            match e {
                PvpEvent::HazardKill { source, killer, .. } if *source != shinobi_sim::pvp::VOID => {
                    let _ = killer;
                    rep.splats += 1; // melee wall splat: a legitimate kill
                }
                PvpEvent::HazardKill { victim, killer: Some(_), .. } => {
                    let _ = victim;
                    rep.ring_outs += 1; // shoved into the void by a player: a ring-out kill, credited
                }
                PvpEvent::HazardKill { victim, .. } => {
                    rep.void_deaths += 1; // nobody pushed: the bot walked, dashed or lunged off by itself
                    if std::env::var("HIVE_DEBUG").is_ok() {
                        if let Some(tr) = last.get(victim) {
                            println!("void death t={:.1}s bot {victim}; trail (every 6 ticks, newest last):", t as f32 / 60.0);
                            // from the last tick the bot was not simply falling: where did it leave the deck?
                            let leave = tr.iter().rposition(|e| !e.3.contains("Airborne")).unwrap_or(0);
                            let from = leave.saturating_sub(40);
                            for (p, ph, v, st) in tr.iter().skip(from).take(110).step_by(5) {
                                println!("    pos {:.1?} vel {:.1?} {ph} {st}", p, v);
                            }
                        }
                    }
                }
                PvpEvent::Kill { killer, victim, kind, .. } => {
                    rep.kills += 1;
                    if std::env::var("HIVE_KILLS").is_ok() {
                        let at = |id: &u16| s.players.iter().find(|p| p.id == *id).map(|p| p.pos(&s.world));
                        if let (Some(a), Some(b)) = (at(killer), at(victim)) {
                            println!("kill t={:.0}s {:?} dy {:.0} dxz {:.0} killer y {:.0}", t as f32 / 60.0, kind, b[1] - a[1], ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt(), a[1]);
                        }
                    }
                }
                _ => {}
            }
        }
        for p in &s.players {
            if p.vitals.alive() && p.pos(&s.world)[1] > 2.0 {
                let v = s.world.bodies[p.ch.body].linvel();
                let _ = v;
                let tr = last.entry(p.id).or_default();
                tr.push_back((p.pos(&s.world), phase_name(&h, p.id), [p.ch.vel.x, p.ch.vel.y, p.ch.vel.z], format!("{:?}", p.ch.state)));
                if tr.len() > 900 {
                    tr.pop_front();
                }
            }
        }
        if t % 60 != 0 {
            continue;
        }
        let alive = s.players.iter().filter(|p| p.vitals.alive()).count();
        for p in &s.players {
            let id = p.id;
            let pos = p.pos(&s.world);
            if !p.vitals.alive() || p.vitals.spawn_protected(s.tick) || alive < 2 {
                window.remove(&id);
                continue;
            }
            if p.ch.state == shinobi_sim::character::MoveState::Grounded {
                let band = (((pos[1] - 0.81 - 10.0) / 20.0).round() as i32).clamp(0, 8);
                tiers.insert(band);
                if let Some(prev) = band_of.insert(id, band) {
                    if prev != band {
                        rep.climbs += 1;
                    }
                }
            }
            roam.entry(id).or_default().insert(((pos[0] / 8.0).floor() as i32, (pos[2] / 8.0).floor() as i32));
            let (since, start) = *window.entry(id).or_insert((t, pos));
            let moved = ((pos[0] - start[0]).powi(2) + (pos[2] - start[2]).powi(2)).sqrt();
            if moved >= 1.0 {
                window.insert(id, (t, pos));
            } else if t - since >= 10 * 60 {
                rep.stuck.push((id, t, pos[0], pos[2], phase_name(&h, id)));
                window.insert(id, (t, pos));
            }
        }
    }
    rep.max_roam = roam.values().map(|c| c.len()).max().unwrap_or(0);
    rep.tiers_seen = tiers.len();
    Some(rep)
}

fn seeds() -> Vec<u64> {
    std::env::var("HIVE_SEEDS").ok().map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_else(|| vec![31, 7, 99])
}

#[test]
fn hive_bots_fight_without_freezing_or_walking_into_the_well() {
    for seed in seeds() {
        let Some(r) = run(12, 240, seed) else { return };
        println!("seed {seed}: kills {} unforced void deaths {} (wall splats {} ring-outs {}) stuck {} max roam {} cells, floors seen {} floor changes {}", r.kills, r.void_deaths, r.splats, r.ring_outs, r.stuck.len(), r.max_roam, r.tiers_seen, r.climbs);
        // a bot may stall once in an alcove corner (it stares at a wall); more than two is a nav bug
        assert!(r.stuck.len() <= 2, "seed {seed}: stuck stretches (bot, tick, x, z, phase): {:?}", &r.stuck[..r.stuck.len().min(10)]);
        // the well is a 190 m drop: slashes launch their victims and a ring-out is a kill (credited, not counted here); walking, dashing or lunging in by themselves is a bug
        assert!(r.void_deaths <= 5, "seed {seed}: {} bots walked or fell into the well in 240 s", r.void_deaths);
        assert!(r.tiers_seen >= 4, "seed {seed}: bots only stood on {} floor(s)", r.tiers_seen);
        assert!(r.kills >= 100, "seed {seed}: only {} kills in 240 s: the bots are not reaching each other", r.kills);
    }
}
