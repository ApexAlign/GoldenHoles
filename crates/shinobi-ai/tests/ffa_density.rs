//! FFA density review (not a regression test): N bots free-for-all on each FFA map, same balance as the demo.
//! Prints kills/min (whole server and per player), void deaths, mean distance to the nearest living enemy (sampled every
//! second) and the share of time a bot spends within 40 m (grapple range) of an enemy.
//!   cargo test --release -p shinobi-ai --test ffa_density -- --ignored --nocapture
//! `FFA_SECS` (default 240) sets the length, `FFA_BOTS` (default 12) the bot count, `FFA_MAPS` (default "ffa.lvl,ffa_xl.lvl").
mod common;
use common::*;
use shinobi_ai::{Archetype, BotSkill};
use shinobi_sim::balance::Balance;
use shinobi_sim::level::parse_level_file;
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::tick::Server;

fn server_for(map: &str, balance: Balance) -> Server {
    let path = format!("{}/../../levels/{map}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let f = parse_level_file(&text).expect("parses");
    let mut s = Server::new(shinobi_sim::character::Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_spawns(f.level.spawns.clone());
    s.set_balance(balance).unwrap();
    s.match_state.cfg.frag_limit = u16::MAX;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    s
}

#[test]
#[ignore]
fn ffa_bots_on_each_map() {
    let secs: u32 = std::env::var("FFA_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(240);
    let bots: usize = std::env::var("FFA_BOTS").ok().and_then(|v| v.parse().ok()).unwrap_or(12);
    let maps = std::env::var("FFA_MAPS").unwrap_or_else(|_| "ffa.lvl,ffa_xl.lvl".into());
    for map in maps.split(',') {
        let mut s = server_for(map, Balance::demo());
        let mut h = host(21);
        h.rebuild_perches(&s);
        for i in 0..bots {
            assert!(h.add_bot(&mut s, 100 + i as u16, if i % 3 == 0 { Archetype::Heavy } else { Archetype::Light }, BotSkill::Normal));
        }
        let (mut kills, mut void, mut nearest_sum, mut nearest_n, mut within40, mut samples) = (0u32, 0u32, 0.0f64, 0u32, 0u32, 0u32);
        for t in 0..secs * 60 {
            step(&mut h, &mut s);
            for e in s.pvp_events.clone() {
                if let PvpEvent::Kill { kind, .. } = e {
                    kills += 1;
                    void += format!("{kind:?}").contains("Hazard") as u32;
                }
            }
            if t % 60 == 0 {
                let ps: Vec<[f32; 3]> = s.players.iter().filter(|p| p.vitals.alive()).map(|p| p.pos(&s.world)).collect();
                for (i, a) in ps.iter().enumerate() {
                    let d = ps.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, b)| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()).fold(f32::MAX, f32::min);
                    if d < f32::MAX {
                        nearest_sum += d as f64;
                        nearest_n += 1;
                        within40 += (d < 40.0) as u32;
                    }
                    samples += 1;
                }
            }
        }
        let mins = secs as f32 / 60.0;
        println!(
            "== {map}: {bots} bots, {secs} s == kills/min {:.1} ({:.2}/min/player), void deaths {void}, mean nearest enemy {:.1} m, within 40 m {:.0}% of samples",
            kills as f32 / mins,
            kills as f32 / mins / bots as f32,
            nearest_sum / nearest_n.max(1) as f64,
            100.0 * within40 as f32 / samples.max(1) as f32
        );
    }
}

#[test]
#[ignore]
fn ffa_bots_leave_their_alcoves() {
    let map = std::env::var("FFA_MAP").unwrap_or_else(|_| "ffa.lvl".into());
    let mut s = server_for(&map, Balance::demo());
    let mut h = host(21);
    h.rebuild_perches(&s);
    for i in 0..12 {
        assert!(h.add_bot(&mut s, 100 + i, Archetype::Light, BotSkill::Normal));
    }
    let start: Vec<(u16, [f32; 3])> = s.players.iter().map(|p| (p.id, p.pos(&s.world))).collect();
    for _ in 0..60 * 30 {
        step(&mut h, &mut s);
    }
    let mut stuck = 0;
    for (id, a) in &start {
        let b = pos(&s, *id);
        let d = ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
        println!("bot {id}: start ({:.0},{:.0}) now ({:.0},{:.0}) moved {d:.1} m phase {}", a[0], a[2], b[0], b[2], phase_name(&h, *id));
        stuck += (d < 8.0) as u32;
    }
    println!("== {map}: {stuck}/12 bots still within 8 m of their spawn after 30 s");
}
