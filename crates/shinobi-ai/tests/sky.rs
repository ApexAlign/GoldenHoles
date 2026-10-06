//! Bots on `levels/ffa_sky.lvl` (floating buildings, no ground, kill plane y = -20): they must fight on the roof
//! decks without walking off the edges (void deaths), without freezing, and across the bridges. Run:
//!   cargo test --release -p shinobi-ai --test sky -- --nocapture
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
    kills: u32,
    stuck: Vec<(u16, u32, f32, f32, &'static str)>,
    /// distinct 8 m x 8 m cells visited by each bot (a bot that never leaves its spawn deck scores low)
    max_roam: usize,
}

fn run(bots: u16, secs: u32, seed: u64) -> Option<Report> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels/ffa_sky.lvl");
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
    let mut rep = Report { void_deaths: 0, kills: 0, stuck: Vec::new(), max_roam: 0 };
    for t in 0..secs * 60 {
        step(&mut h, &mut s);
        for e in &s.pvp_events {
            match e {
                PvpEvent::HazardKill { .. } => rep.void_deaths += 1,
                PvpEvent::Kill { .. } => rep.kills += 1,
                _ => {}
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
    Some(rep)
}

fn seeds() -> Vec<u64> {
    std::env::var("SKY_SEEDS").ok().map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_else(|| vec![31, 7])
}

#[test]
fn sky_bots_fight_without_freezing_or_walking_off_the_roofs() {
    for seed in seeds() {
        let Some(r) = run(12, 240, seed) else { return };
        println!("seed {seed}: kills {} void deaths {} stuck {} max roam {} cells", r.kills, r.void_deaths, r.stuck.len(), r.max_roam);
        assert!(r.stuck.is_empty(), "seed {seed}: stuck stretches (bot, tick, x, z, phase): {:?}", &r.stuck[..r.stuck.len().min(10)]);
        // 12 bots x 4 minutes: a roof is not a cliff. A handful of falls is fight chaos (knockback), a stream of them is a bug.
        assert!(r.void_deaths <= 6, "seed {seed}: {} bots walked or got shoved off the roofs in 240 s", r.void_deaths);
        assert!(r.kills >= 20, "seed {seed}: only {} kills in 240 s: the bots are not reaching each other", r.kills);
    }
}
