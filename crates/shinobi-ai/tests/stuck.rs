//! Stuck detector: 16 bots, every spawn used, 240 s on each big FFA level. A bot is stuck when it moved less than
//! 1 m in 10 s of continuous life while it had a living enemy to fight. No bot may be stuck, and the failure message
//! names where (so the level or the bot can be fixed). Skipped when the level files are absent.
mod common;
use std::collections::BTreeMap;

use common::*;
use shinobi_ai::{Archetype, BotSkill};
use shinobi_sim::balance::Balance;
use shinobi_sim::level::parse_level_file;
use shinobi_sim::tick::Server;

/// Returns the stuck events `(bot, tick, x, z, phase)`.
fn run(level: &str, bots: u16, secs: u32, seed: u64) -> Option<Vec<(u16, u32, f32, f32, &'static str)>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels").join(level);
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
    // per bot: position at the start of the current 10 s window, and the tick that window started (reset on death)
    let mut window: BTreeMap<u16, (u32, [f32; 3])> = BTreeMap::new();
    let mut out = Vec::new();
    for t in 0..secs * 60 {
        step(&mut h, &mut s);
        if t % 60 != 0 {
            continue;
        }
        let alive: Vec<(u16, [f32; 3])> = s.players.iter().filter(|p| p.vitals.alive()).map(|p| (p.id, p.pos(&s.world))).collect();
        for p in &s.players {
            let id = p.id;
            let pos = p.pos(&s.world);
            if !p.vitals.alive() || p.vitals.spawn_protected(s.tick) || alive.len() < 2 {
                window.remove(&id);
                continue;
            }
            let (since, start) = *window.entry(id).or_insert((t, pos));
            let moved = ((pos[0] - start[0]).powi(2) + (pos[2] - start[2]).powi(2)).sqrt();
            if moved >= 1.0 {
                window.insert(id, (t, pos)); // moving: restart the window here
            } else if t - since >= 10 * 60 {
                out.push((id, t, pos[0], pos[2], phase_name(&h, id)));
                window.insert(id, (t, pos)); // report once per stuck stretch
            }
        }
    }
    Some(out)
}

fn check(level: &str) {
    let seeds: Vec<u64> = std::env::var("STUCK_SEEDS").ok().map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_else(|| vec![31]);
    for seed in seeds {
        let Some(stuck) = run(level, 16, 240, seed) else { return };
        assert!(stuck.is_empty(), "{level} seed {seed}: {} stuck stretches > 10 s (bot, tick, x, z, phase): {:?}", stuck.len(), &stuck[..stuck.len().min(12)]);
    }
}

#[test]
fn no_bot_stays_stuck_on_ffa() {
    check("ffa.lvl");
}

#[test]
fn no_bot_stays_stuck_on_ffa_xl() {
    check("ffa_xl.lvl");
}

/// Planning cost is bounded PER TICK, not just on average: a burst of bots needing paths at once (match start, a
/// whole wave retargeting) must never become one long frame. Deterministic budget check instead of a wall-clock one.
#[test]
fn path_planning_never_exceeds_its_per_tick_budget() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels/ffa.lvl");
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let f = parse_level_file(&text).expect("level parses");
    let mut s = Server::new(shinobi_sim::character::Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_spawns(f.level.spawns.clone());
    s.set_balance(Balance::demo()).unwrap();
    let mut h = host(31);
    h.rebuild_perches(&s);
    for i in 0..16u16 {
        assert!(h.add_bot(&mut s, 201 + i, Archetype::Light, BotSkill::Normal));
    }
    let _ = shinobi_ai::nav::take_plan_stats();
    let (mut worst_calls, mut worst_nodes, mut total_calls) = (0u32, 0u64, 0u32);
    for _ in 0..60 * 60 {
        step(&mut h, &mut s);
        let (calls, nodes) = shinobi_ai::nav::take_plan_stats();
        worst_calls = worst_calls.max(calls);
        worst_nodes = worst_nodes.max(nodes);
        total_calls += calls;
    }
    // 2 plans of at most 12 000 nodes per tick (shinobi_ai::host::PLAN_PER_TICK, fsm PLAN_NODES)
    assert!(worst_calls <= 2, "{worst_calls} A* searches in one tick (budget 2)");
    assert!(worst_nodes <= 2 * 12_001, "{worst_nodes} nodes expanded in one tick (budget 24 000)");
    assert!(total_calls > 20, "bots did plan paths ({total_calls}): the test is exercising the planner");
}
