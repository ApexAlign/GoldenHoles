//! Crash containment. One hostile client must never take the server down: sim / replication calls run under
//! `catch_unwind`; on a panic the physics world is rebuilt from a clean state (connected players respawn at
//! spawn points, tick counter and reliable-event streams continue) instead of aborting.
//!
//! Needs `panic = "unwind"` (the workspace release profile no longer sets `abort`).

use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

use shinobi_sim::character::Cfg;
use shinobi_sim::tick::Server;

use shinobi_ai::{Archetype, BotHost, BotHostCfg, BotSkill as AiSkill};

use crate::config::{BotSkill, BOT_ID_BASE};
use crate::replicate::Replicator;
use crate::setup::Setup;

/// This many panics inside `STORM_WINDOW` means the offender is still connected and re-triggers it every frame.
pub const STORM_PANICS: usize = 3;
pub const STORM_WINDOW: Duration = Duration::from_secs(5);

/// Runs `f`; a panic becomes `Err(message)`. State touched by `f` must be treated as poisoned by the caller.
pub fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|p| {
        p.downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".into())
    })
}

/// How many AI opponents, how hard, and their RNG seed. Applied at boot and again after a panic rebuild.
#[derive(Clone, Copy, Debug)]
pub struct BotPlan {
    pub count: usize,
    pub skill: BotSkill,
    pub seed: u64,
    /// Keep every team at this many players with bots (0 = off); `count` is then unused.
    pub team_size: usize,
}

impl BotPlan {
    /// The host must tick (`BotHost::advance`) whenever any bot can exist.
    pub fn active(&self) -> bool {
        self.count > 0 || self.team_size > 0
    }
}

/// Put a just-joined human on the team with fewer HUMANS (then fewer frags, then Cyan). `Server::add_player`
/// balances by total players, which with bot fill puts every human on the same side (both teams are already full of
/// bots and ties go to Cyan); `fill_team_bots` then trims a bot from the human's team.
pub fn balance_human_team(server: &mut Server, id: u16) {
    if !server.match_state.team_mode {
        return; // free-for-all: no teams, nobody is an ally
    }
    let humans = |team: u8| server.players.iter().filter(|p| p.id != id && p.id < BOT_ID_BASE && p.team == team).count();
    let team = (shinobi_sim::team::TEAM_CYAN..shinobi_sim::team::TEAM_COUNT)
        .min_by_key(|t| (humans(*t), server.team_kills(*t), *t))
        .unwrap_or(shinobi_sim::team::TEAM_CYAN);
    server.set_team(id, team);
}

/// Team-size fill: top every team up to `plan.team_size` with bots (at most one per team per call, so a
/// 16-player match builds up over a few frames instead of one spike) and drop surplus bots when humans took
/// their slots. Humans join the smaller team (`Server::balanced_team`), so a join normally frees one bot.
pub fn fill_team_bots(plan: &BotPlan, server: &mut Server, host: &mut BotHost) {
    if plan.team_size == 0 {
        return;
    }
    let skill = AiSkill::parse(plan.skill.as_str()).unwrap_or(AiSkill::Normal);
    for team in shinobi_sim::team::TEAM_CYAN..shinobi_sim::team::TEAM_COUNT {
        let n = server.team_size(team);
        if n < plan.team_size {
            let Some(id) = (BOT_ID_BASE..=255u16).find(|id| server.players.iter().all(|p| p.id != *id)) else { continue };
            let arch = if id % 2 == 0 { Archetype::Heavy } else { Archetype::Light };
            if host.add_bot(server, id, arch, skill) {
                server.set_team(id, team);
            }
        } else if n > plan.team_size {
            let surplus = server.players.iter().filter(|p| p.team == team && host.is_bot(p.id)).map(|p| p.id).max();
            if let Some(id) = surplus {
                host.remove_bot(server, id);
            }
        }
    }
}

/// A fresh `BotHost` with the plan's bots added to `server` (ids `BOT_ID_BASE..`, alternating archetypes).
/// Call after the level is built: bots read the level's perches and the hazard mirror.
pub fn make_bots(plan: &BotPlan, server: &mut Server) -> BotHost {
    let mut host = BotHost::new(BotHostCfg { seed: plan.seed, ..Default::default() });
    host.rebuild_perches(server);
    let skill = AiSkill::parse(plan.skill.as_str()).unwrap_or(AiSkill::Normal);
    for i in 0..plan.count {
        let arch = if i % 2 == 0 { Archetype::Heavy } else { Archetype::Light };
        host.add_bot(server, BOT_ID_BASE + i as u16, arch, skill);
    }
    for _ in 0..plan.team_size * 2 {
        fill_team_bots(plan, server, &mut host); // initial fill: both sides to team_size
    }
    host
}

/// Rebuild after a panic: fresh `Server` from the same `Setup` (geometry, spawns, pickups; keeps the tick so
/// client clocks stay valid), `ids` re-added at spawn points, solid walls + hazard mirror restored.
/// Per-player combat state, scores and the match phase reset (acceptable on recovery); connections and event
/// streams survive. Clients also lose their pickups' state, which re-announce themselves via `PickupSpawned`.
pub fn reset_world(
    server: &mut Server,
    rep: &mut Replicator,
    cfg: Cfg,
    setup: &Setup,
    ids: &[u16],
    plan: &BotPlan,
    bots: &mut BotHost,
) {
    let tick = server.tick;
    let mut fresh = setup.build(cfg);
    fresh.tick = tick;
    for id in ids {
        let at = fresh.pick_spawn(None);
        fresh.add_player(*id, at);
    }
    *bots = make_bots(plan, &mut fresh); // brains are poisoned along with the world: start them fresh
    *server = fresh;
    rep.rebuild_world_state(server, tick.saturating_sub(1));
}

#[derive(Default)]
pub struct PanicLog {
    recent: VecDeque<Instant>,
    pub total: u32,
}

impl PanicLog {
    /// Records a panic; true when it is the Nth within the storm window (caller should drop all peers).
    pub fn record(&mut self, now: Instant) -> bool {
        self.total += 1;
        self.recent.push_back(now);
        while self.recent.front().map_or(false, |t| now - *t > STORM_WINDOW) {
            self.recent.pop_front();
        }
        self.recent.len() >= STORM_PANICS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_is_caught_with_message() {
        assert_eq!(guarded(|| 7), Ok(7));
        assert_eq!(guarded(|| -> u8 { panic!("boom {}", 42) }), Err("boom 42".into()));
        assert_eq!(guarded(|| -> u8 { panic!("static") }), Err("static".into()));
    }

    #[test]
    fn reset_keeps_tick_and_players_and_walls() {
        let cfg = Cfg::default();
        let setup = Setup::standard();
        let mut server = setup.build(cfg);
        let mut rep = Replicator::new(&cfg, 1);
        let plan = BotPlan { count: 2, skill: BotSkill::Easy, seed: 7, team_size: 0 };
        let mut bots = make_bots(&plan, &mut server);
        assert_eq!(bots.bot_count(), 2);
        server.add_player(3, [0.0, 0.81, 0.0]);
        server.add_player(9, [2.0, 0.81, 0.0]);
        for _ in 0..40 {
            bots.advance(&mut server, Duration::from_millis(17));
            rep.post_frame(&mut server);
        }
        let tick = server.tick;
        // poisoned world: simulate the panic recovery path
        assert!(guarded(|| -> () { panic!("simulated sim panic") }).is_err());
        reset_world(&mut server, &mut rep, cfg, &setup, &[3, 9], &plan, &mut bots);
        assert_eq!(server.tick, tick, "clients' tick clocks must stay valid");
        let mut ids: Vec<_> = server.players.iter().map(|p| p.id).collect();
        ids.sort();
        assert_eq!(ids, vec![3, 9, 201, 202], "humans + bots come back");
        assert_eq!(bots.bot_count(), 2);
        // and the world still simulates afterwards
        for _ in 0..30 {
            bots.advance(&mut server, Duration::from_millis(17));
            rep.post_frame(&mut server);
        }
        assert!(server.tick > tick);
        assert_eq!(server.pickups.active().count(), setup.pickups.len(), "pickups rebuilt from Setup");
    }

    #[test]
    fn free_for_all_joiners_stay_teamless() {
        let mut server = Setup::standard().build(Cfg::default());
        server.add_player(1, [0.0, 0.81, 0.0]);
        balance_human_team(&mut server, 1);
        assert_eq!(server.players[0].team, shinobi_sim::tick::NO_TEAM);
    }

    #[test]
    fn humans_spread_over_both_teams_even_when_bots_fill_them() {
        let setup = Setup::standard();
        let mut server = setup.build(Cfg::default());
        let plan = BotPlan { count: 0, skill: BotSkill::Easy, seed: 7, team_size: 4 };
        server.enable_teams(50);
        let mut bots = make_bots(&plan, &mut server);
        for id in 1..=4u16 {
            server.add_player(id, [id as f32, 0.81, 0.0]);
            balance_human_team(&mut server, id);
            fill_team_bots(&plan, &mut server, &mut bots);
        }
        let humans = |t: u8| server.players.iter().filter(|p| p.id < BOT_ID_BASE && p.team == t).count();
        assert_eq!((humans(0), humans(1)), (2, 2), "2 humans per side, not 4 on Cyan");
        assert_eq!((server.team_size(0), server.team_size(1)), (4, 4), "bots made room on both sides");
    }

    #[test]
    fn team_size_fill_tops_both_sides_up_and_gives_slots_to_humans() {
        let setup = Setup::standard();
        let mut server = setup.build(Cfg::default());
        let plan = BotPlan { count: 0, skill: BotSkill::Easy, seed: 7, team_size: 3 };
        server.enable_teams(50);
        let mut bots = make_bots(&plan, &mut server);
        let sizes = |s: &Server| (s.team_size(0), s.team_size(1));
        assert_eq!(sizes(&server), (3, 3), "3v3 of bots from an empty server");
        assert_eq!(bots.bot_count(), 6);
        // a human joins the smaller / behind team and takes a bot slot there
        server.add_player(1, [0.0, 0.81, 0.0]);
        let team = server.players.iter().find(|p| p.id == 1).unwrap().team;
        assert_eq!(sizes(&server).0 + sizes(&server).1, 7);
        fill_team_bots(&plan, &mut server, &mut bots);
        assert_eq!(sizes(&server), (3, 3), "a bot made room for the human");
        assert_eq!(bots.bot_count(), 5);
        assert_eq!(server.players.iter().find(|p| p.id == 1).unwrap().team, team, "humans are never moved");
        // the human leaves: a bot is added again
        server.remove_player(1);
        fill_team_bots(&plan, &mut server, &mut bots);
        assert_eq!(sizes(&server), (3, 3));
        assert_eq!(bots.bot_count(), 6);
        // it simulates
        for _ in 0..30 {
            bots.advance(&mut server, Duration::from_millis(17));
        }
    }

    #[test]
    fn panic_storm_detected() {
        let mut log = PanicLog::default();
        let t = Instant::now();
        assert!(!log.record(t));
        assert!(!log.record(t + Duration::from_secs(1)));
        assert!(log.record(t + Duration::from_secs(2)));
        // old ones age out
        let mut log = PanicLog::default();
        log.record(t);
        log.record(t + Duration::from_secs(1));
        assert!(!log.record(t + Duration::from_secs(10)));
    }
}
