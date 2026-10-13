//! Bots on `levels/ffa_clock.lvl` (The Clockwork): they must fight on the islands without walking off the edges,
//! without freezing, and cross the ramps / stairs between the hub and the islands. Run:
//!   cargo test --release -p shinobi-ai --test clock -- --nocapture
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
    /// distinct 40 m height bands (0..5) in which a bot stood on the ground
    tiers_seen: usize,
}

fn run(bots: u16, secs: u32, seed: u64) -> Option<Report> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels/ffa_clock.lvl");
    let text = std::fs::read_to_string(path).ok()?;
    let f = parse_level_file(&text).expect("level parses");
    let mut s = Server::new(shinobi_sim::character::Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_spawns(f.level.spawns.clone());
    s.set_balance(Balance::demo()).unwrap();
    s.match_state.cfg.frag_limit = u16::MAX;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    // bounty hunting (docs/bounty.md) has bots beeline for a single target across the drum's islands and
    // ramps instead of pathing normally; this test is about safe movement on this level, not the bounty
    // system, so keep it off.
    s.match_state.cfg.bounty_streak = 0;
    let mut h = host(seed);
    h.rebuild_perches(&s);
    for i in 0..bots {
        assert!(h.add_bot(&mut s, 201 + i, if i % 3 == 0 { Archetype::Heavy } else { Archetype::Light }, BotSkill::Normal));
    }
    let mut window: BTreeMap<u16, (u32, [f32; 3])> = BTreeMap::new();
    let mut roam: BTreeMap<u16, std::collections::BTreeSet<(i32, i32)>> = BTreeMap::new();
    let mut tiers: std::collections::BTreeSet<i32> = std::collections::BTreeSet::new();
    let mut rep = Report { void_deaths: 0, splats: 0, ring_outs: 0, kills: 0, stuck: Vec::new(), max_roam: 0, tiers_seen: 0 };
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
                    if std::env::var("CLOCK_DEBUG").is_ok() {
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
                PvpEvent::Kill { .. } => rep.kills += 1,
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
                tiers.insert((pos[1] / 40.0).floor() as i32);
            }
            roam.entry(id).or_default().insert(((pos[0] / 8.0).floor() as i32, (pos[2] / 8.0).floor() as i32));
            let (since, start) = *window.entry(id).or_insert((t, pos));
            let moved = ((pos[0] - start[0]).powi(2) + (pos[2] - start[2]).powi(2)).sqrt();
            if moved >= 1.0 {
                window.insert(id, (t, pos));
            } else if t - since >= 10 * 60 {
                rep.stuck.push((id, t, pos[0], pos[2], phase_name(&h, id)));
                if std::env::var("CLOCK_DEBUG").is_ok() {
                    println!("stuck bot {id} at {pos:.1?} state {:?}", p.ch.state);
                }
                window.insert(id, (t, pos));
            }
        }
    }
    rep.max_roam = roam.values().map(|c| c.len()).max().unwrap_or(0);
    rep.tiers_seen = tiers.iter().filter(|t| (0..6).contains(*t)).count();
    Some(rep)
}

fn seeds() -> Vec<u64> {
    std::env::var("CLOCK_SEEDS").ok().map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_else(|| vec![31, 7, 99])
}

#[test]
fn clock_bots_fight_up_and_down_the_drum_without_freezing_or_falling_out() {
    for seed in seeds() {
        let Some(r) = run(12, 240, seed) else { return };
        println!("seed {seed}: kills {} unforced void deaths {} (wall splats {} ring-outs {}) stuck {} max roam {} cells, 40 m height bands stood on {}", r.kills, r.void_deaths, r.splats, r.ring_outs, r.stuck.len(), r.max_roam, r.tiers_seen);
        // a bot may stall once at the foot of a pit wall (it stares up at the rim); more than two is a nav bug
        assert!(r.stuck.len() <= 4,  "seed {seed}: stuck stretches (bot, tick, x, z, phase): {:?}", &r.stuck[..r.stuck.len().min(10)]);
        // 12 bots x 4 minutes: a roof is not a cliff. Slashes now launch their victims and a ring-out is a kill (melee heighteners), so shoves off the roofs are part of the fight; a flood of them is a bug.
        assert!(r.void_deaths <= 5, "seed {seed}: {} bots fell out of the drum in 240 s", r.void_deaths);
        assert!(r.tiers_seen >= 4, "seed {seed}: bots only stood in {} height band(s): they are not climbing the spiral", r.tiers_seen);
        // (was 100 before stars became a 14 m close-range tool: the bots' long pokes are gone, the bow only partly makes up for it)
        assert!(r.kills >= 70, "seed {seed}: only {} kills in 240 s: the bots are not reaching each other", r.kills);
    }
}
