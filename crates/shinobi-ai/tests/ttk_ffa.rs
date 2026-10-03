//! FFA bot harness (not a regression test): N bots, every one its own faction, on any level file. Sizes a map and
//! checks the bot director keeps fights dense. Prints kills/min, mean life, mean and median nearest-enemy distance
//! (sampled every second over living bots) and the share of time bots stood still.
//!   cargo test --release -p shinobi-ai --test ttk_ffa -- --ignored --nocapture
//! Env: `FFA_LEVEL` (path, default levels/ffa.lvl, falling back to arena), `FFA_BOTS` (default 16),
//! `FFA_SECS` (default 180), `FFA_SEED`.
mod common;
use std::collections::BTreeMap;

use common::*;
use shinobi_ai::{Archetype, BotSkill};
use shinobi_sim::balance::Balance;
use shinobi_sim::level::parse_level_file;
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::tick::Server;

fn level_path() -> std::path::PathBuf {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels");
    if let Ok(p) = std::env::var("FFA_LEVEL") {
        return std::path::PathBuf::from(p);
    }
    let ffa = root.join("ffa.lvl");
    if ffa.exists() { ffa } else { root.join("arena.lvl") }
}

fn server() -> (Server, String) {
    let path = level_path();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let f = parse_level_file(&text).expect("level parses");
    let mut s = Server::new(shinobi_sim::character::Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_spawns(f.level.spawns.clone());
    s.set_balance(Balance::demo()).unwrap();
    s.match_state.cfg.frag_limit = u16::MAX;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    (s, path.file_name().unwrap().to_string_lossy().into_owned())
}

#[test]
#[ignore]
fn ffa_bots_on_a_level() {
    let secs: u32 = std::env::var("FFA_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(180);
    let n: u16 = std::env::var("FFA_BOTS").ok().and_then(|v| v.parse().ok()).unwrap_or(16);
    let seed: u64 = std::env::var("FFA_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(31);
    let (mut s, name) = server();
    let mut h = host(seed);
    h.rebuild_perches(&s);
    for i in 0..n {
        assert!(h.add_bot(&mut s, 201 + i, if i % 3 == 0 { Archetype::Heavy } else { Archetype::Light }, BotSkill::Normal));
    }
    let (mut kills, mut hits) = (0u32, 0u32);
    let mut lives: BTreeMap<u16, u32> = BTreeMap::new();
    let mut life_ticks: Vec<u32> = Vec::new();
    let mut nearest: Vec<f32> = Vec::new();
    let (mut still, mut samples) = (0u64, 0u64);
    let mut by_phase: BTreeMap<&'static str, (u64, u64)> = BTreeMap::new(); // phase -> (still, samples)
    let mut last: BTreeMap<u16, [f32; 3]> = BTreeMap::new();
    let t0 = std::time::Instant::now();
    for t in 0..secs * 60 {
        step(&mut h, &mut s);
        for e in &s.pvp_events {
            match *e {
                PvpEvent::Hit { .. } => hits += 1,
                PvpEvent::Kill { tick, victim, .. } => {
                    kills += 1;
                    life_ticks.push(tick.wrapping_sub(lives.get(&victim).copied().unwrap_or(0)));
                }
                PvpEvent::Respawn { tick, player } => {
                    lives.insert(player, tick);
                }
                _ => {}
            }
        }
        if t % 60 == 30 {
            let alive: Vec<(u16, [f32; 3])> = s.players.iter().filter(|p| p.vitals.alive()).map(|p| (p.id, p.pos(&s.world))).collect();
            for (id, pos) in &alive {
                let d = alive.iter().filter(|(o, _)| o != id).map(|(_, q)| ((q[0] - pos[0]).powi(2) + (q[2] - pos[2]).powi(2)).sqrt()).fold(f32::MAX, f32::min);
                if d < f32::MAX {
                    nearest.push(d);
                }
                if let Some(prev) = last.get(id) {
                    samples += 1;
                    let is_still = ((pos[0] - prev[0]).powi(2) + (pos[2] - prev[2]).powi(2)).sqrt() < 1.5;
                    still += is_still as u64;
                    let e = by_phase.entry(h.bot(*id).map_or("?", |b| b.phase.name())).or_default();
                    e.0 += is_still as u64;
                    e.1 += 1;
                }
                last.insert(*id, *pos);
            }
        }
    }
    let cpu = t0.elapsed().as_secs_f32();
    let mins = secs as f32 / 60.0;
    nearest.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = nearest.iter().sum::<f32>() / nearest.len().max(1) as f32;
    let avg_life = life_ticks.iter().map(|t| *t as f32).sum::<f32>() / life_ticks.len().max(1) as f32 / 60.0;
    println!("== FFA {n} bots on {name}, {secs} s (sim cost {cpu:.1} s wall, {:.2} ms/tick) ==", cpu * 1000.0 / (secs * 60) as f32);
    println!("kills/min {:.1} ({kills} kills, {hits} hits), avg life {avg_life:.1} s", kills as f32 / mins);
    println!("nearest-enemy distance: mean {mean:.1} m, median {:.1} m, p90 {:.1} m", nearest[nearest.len() / 2], nearest[nearest.len() * 9 / 10]);
    println!("standing still (<1.5 m per sample): {:.0}%", 100.0 * still as f32 / samples.max(1) as f32);
    for (ph, (st, n)) in &by_phase {
        println!("  phase {ph:>8}: {:>5} samples ({:.0}% of all), {:.0}% of them still", n, 100.0 * *n as f32 / samples.max(1) as f32, 100.0 * *st as f32 / (*n).max(1) as f32);
    }
}
