//! 8v8 time-to-kill review (not a regression test): 16 bots, 8 per side, on `levels/8v8.lvl` with the shipped
//! balance, for a few minutes. Prints kills/min, average life, per-weapon kill share, damage per hit kind,
//! friendly damage (must be 0) and shots-to-kill.
//!   cargo test --release -p shinobi-ai --test ttk_8v8 -- --ignored --nocapture
//! `TTK_SECS` (default 300) sets the length.
mod common;
use std::collections::BTreeMap;

use common::*;
use shinobi_ai::{Archetype, BotSkill};
use shinobi_sim::balance::Balance;
use shinobi_sim::combat::DamageKind;
use shinobi_sim::level::parse_level_file;
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::tick::Server;

fn level_server(balance: Balance) -> Server {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../levels/8v8.lvl");
    let text = std::fs::read_to_string(path).expect("levels/8v8.lvl");
    let f = parse_level_file(&text).expect("parses");
    let mut s = Server::new(shinobi_sim::character::Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_team_spawns(f.level.spawns.clone(), shinobi_sim::level::parse_spawn_teams(&text));
    s.set_balance(balance).unwrap();
    // one long continuous match: no intermissions in the statistics
    s.match_state.cfg.frag_limit = u16::MAX;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    s
}

#[derive(Default)]
struct Life {
    born: u32,
}

#[test]
#[ignore]
fn eight_v_eight_bots() {
    let secs: u32 = std::env::var("TTK_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
    for (name, balance) in [("demo (shipped)", Balance::demo())] {
        let mut s = level_server(balance);
        let mut h = host(11);
        h.rebuild_perches(&s);
        let (added, _) = h.fill_teams(&mut s, 8, 201, Archetype::Light, BotSkill::Normal);
        assert_eq!(added.len(), 16);
        let ticks = secs * 60;
        let mut lives: BTreeMap<u16, Life> = BTreeMap::new();
        let mut life_ticks: Vec<u32> = Vec::new();
        let (mut kills, mut by_kind, mut dmg, mut hits): (u32, BTreeMap<String, u32>, BTreeMap<String, f32>, BTreeMap<String, u32>) = Default::default();
        let (mut friendly, mut deaths) = (0.0f32, 0u32);
        let team = |s: &Server, id: u16| s.players.iter().find(|p| p.id == id).map(|p| p.team);
        for _ in 0..ticks {
            step(&mut h, &mut s);
            for e in s.pvp_events.clone() {
                match e {
                    PvpEvent::Hit { attacker, victim, amount, kind, .. } => {
                        let k = format!("{kind:?}");
                        *dmg.entry(k.clone()).or_default() += amount;
                        *hits.entry(k).or_default() += 1;
                        if team(&s, attacker) == team(&s, victim) {
                            friendly += amount;
                        }
                    }
                    PvpEvent::Kill { tick, victim, kind, .. } => {
                        kills += 1;
                        deaths += 1;
                        *by_kind.entry(format!("{kind:?}")).or_default() += 1;
                        let l = lives.entry(victim).or_default();
                        life_ticks.push(tick.wrapping_sub(l.born));
                    }
                    PvpEvent::Respawn { tick, player } => lives.entry(player).or_default().born = tick,
                    _ => {}
                }
            }
        }
        let mins = secs as f32 / 60.0;
        let avg_life = life_ticks.iter().map(|t| *t as f32).sum::<f32>() / life_ticks.len().max(1) as f32 / 60.0;
        println!("== 8v8 bots, balance {name}, {secs} s ==");
        println!("kills/min {:.1}  (total {kills}, deaths {deaths}); avg life {avg_life:.1} s", kills as f32 / mins);
        for (k, n) in &by_kind {
            println!("  kill share {k}: {n} ({:.0}%)", 100.0 * *n as f32 / kills.max(1) as f32);
        }
        for (k, d) in &dmg {
            let n = hits[k];
            println!("  {k}: {n} hits, {d:.0} dmg, {:.1} dmg/hit", d / n as f32);
        }
        println!("friendly damage {friendly}");
        assert_eq!(friendly, 0.0);
        let _ = DamageKind::Slash;
    }
}
