//! Bots show the traversal-melee loop: in a bot-only free-for-all on the small map they hook targets from range and swing
//! while hooked or wall-running (hook-strikes / wall slashes), and none of it wedges the match.
//!   cargo test -p shinobi-ai --test hook_strike -- --nocapture
mod common;
use common::*;
use shinobi_ai::{Archetype, BotSkill};
use shinobi_sim::balance::Balance;
use shinobi_sim::combat::CombatEvent;
use shinobi_sim::level::parse_level_file;
use shinobi_sim::tick::Server;

fn server(level: &str) -> Option<(Server, Vec<[f32; 3]>)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels").join(level);
    let f = parse_level_file(&std::fs::read_to_string(path).ok()?).ok()?;
    let mut s = Server::new(shinobi_sim::character::Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_spawns(f.level.spawns.clone());
    s.set_balance(Balance::demo()).unwrap();
    Some((s, f.level.spawns))
}

#[test]
fn bots_hook_and_strike_from_the_rope_and_the_wall() {
    for level in ["ffa_small.lvl", "ffa.lvl"] {
        let Some((mut s, _)) = server(level) else { continue };
        s.match_state.cfg.frag_limit = 1000;
        s.match_state.cfg.time_limit_ticks = 60 * 60 * 10;
        let mut h = shinobi_ai::BotHost::new(shinobi_ai::BotHostCfg { seed: 5, ..Default::default() });
        h.rebuild_perches(&s);
        for i in 0..8u16 {
            assert!(h.add_bot(&mut s, 201 + i, if i % 3 == 0 { Archetype::Heavy } else { Archetype::Light }, BotSkill::Normal));
        }
        let (mut attaches, mut hook_slashes, mut wall_slashes, mut hits) = (0u32, 0u32, 0u32, 0u32);
        let mut max_speed = 0.0f32;
        for _ in 0..60 * 120 {
            // classify swings BEFORE the step consumes the state: a swing is a hook-strike if the swinger was hooked
            let hooked: Vec<u16> = s.players.iter().filter(|p| p.ch.grapple.is_some()).map(|p| p.id).collect();
            let walling: Vec<u16> = s.players.iter().filter(|p| p.ch.wall_normal().is_some()).map(|p| p.id).collect();
            step(&mut h, &mut s);
            for (id, e) in &s.events {
                match e {
                    CombatEvent::GrappleAttach { .. } => attaches += 1,
                    CombatEvent::Slash { hits: n, .. } => {
                        if hooked.contains(id) {
                            hook_slashes += 1;
                            hits += *n;
                        } else if walling.contains(id) {
                            wall_slashes += 1;
                            hits += *n;
                        }
                    }
                    _ => {}
                }
            }
            for p in &s.players {
                max_speed = max_speed.max(p.ch.vel.norm());
                assert!(p.ch.vel.iter().all(|v| v.is_finite()), "non-finite velocity");
            }
        }
        eprintln!("{level}: 8 bots x 120 s: {attaches} hooks, {hook_slashes} hook-strikes, {wall_slashes} wall slashes ({hits} hits), peak speed {max_speed:.1} m/s");
        assert!(attaches >= 3, "{level}: bots should hook now and then: {attaches}");
        assert!(hook_slashes + wall_slashes >= 1, "{level}: bots should land hook-strikes or wall slashes");
    }
}
