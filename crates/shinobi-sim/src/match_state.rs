//! Match flow: Warmup -> Live -> Intermission -> (next map) Warmup. Deterministic: only the tick counter
//! and the scoreboard drive it.
//!
//! * Warmup: fighting works, nothing counts (everything is reset when Live starts). The countdown runs only
//!   while at least `min_players` are connected and restarts if they drop below.
//! * Live: first to `frag_limit` or `time_limit_ticks`, whichever comes first; both on the same tick is a
//!   FragLimit end. Winner = most kills, then fewest deaths, else a draw (`None`).
//! * Intermission: input and damage are frozen (hazards too); after `intermission_ticks` the map index
//!   advances (`MatchEvent::NextMap`; loading geometry / clearing trails is the host's job) and warmup starts.
//!
//! `begin_tick` runs before anyone's position is sampled for the tick (resets teleport safely), `end_tick`
//! after damage is applied (limits are checked once, so a same-tick trade is judged on the final scores).

use crate::character::PhysicsWorld;
use crate::pickups::Pickups;
use crate::pvp;
use crate::tick::Player;

pub const TICK_HZ: u32 = shinobi_proto::TICK_HZ;

/// What the server is running. Deathmatch is the default; the enemy modes need `Server::set_wave_spawns` /
/// `set_training_points`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GameMode {
    #[default]
    Deathmatch,
    /// Players vs endless goon waves; player-vs-player weapon damage is off.
    CoopWaves,
    /// Never-ending dummies that don't attack, respawn in place; no match end.
    Training,
}

#[derive(Clone, Copy, Debug)]
pub struct MatchCfg {
    pub warmup_ticks: u32,
    pub time_limit_ticks: u32,
    pub intermission_ticks: u32,
    pub frag_limit: u16,
    pub min_players: usize,
    pub mode: GameMode,
}

impl Default for MatchCfg {
    fn default() -> Self {
        Self {
            warmup_ticks: 10 * TICK_HZ,
            time_limit_ticks: 300 * TICK_HZ,
            intermission_ticks: 10 * TICK_HZ,
            frag_limit: 20,
            min_players: 2,
            mode: GameMode::Deathmatch,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// `countdown_from`: tick the countdown began; `None` while waiting for players.
    Warmup { countdown_from: Option<u32> },
    Live { since: u32 },
    Intermission { since: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    FragLimit,
    TimeLimit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhaseKind {
    Warmup,
    Live,
    Intermission,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchEvent {
    /// Whole seconds left in a warmup countdown or the intermission; fires when the number changes.
    Countdown { tick: u32, phase: PhaseKind, seconds: u32 },
    MatchStart { tick: u32, map: u32 },
    MatchEnd { tick: u32, winner: Option<u16>, reason: EndReason },
    /// Intermission over: host should load map `map`.
    NextMap { tick: u32, map: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScoreEntry {
    pub id: u16,
    pub kills: u16,
    pub deaths: u16,
    pub alive: bool,
}

pub struct MatchState {
    pub cfg: MatchCfg,
    pub phase: Phase,
    pub map_index: u32,
    /// Winner of the last finished match (`None` = draw or no match yet).
    pub last_winner: Option<u16>,
    /// Team deathmatch: the match is a team race (`team_frag_limit` team kills or the time limit); the individual
    /// frag race and `last_winner` are not used. Off by default (free-for-all).
    pub team_mode: bool,
    pub team_frag_limit: u16,
    /// Frags of players who left mid-match, per team (so leaving never lowers a team's score).
    pub team_banked: [u16; 2],
    /// Winning team of the last finished team match (`None` = draw / none yet).
    pub last_winner_team: Option<u8>,
    /// Team match tied at the end: sudden death lasts this long; the first team kill wins, else a draw.
    pub sudden_death_ticks: u32,
    /// `Some((deadline tick, scores when it began))` while sudden death runs.
    sudden: Option<(u32, [u16; 2])>,
    last_count: Option<(PhaseKind, u32)>,
}

impl Default for MatchState {
    fn default() -> Self {
        Self::new(MatchCfg::default())
    }
}

fn since(now: u32, then: u32) -> u32 {
    now.wrapping_sub(then)
}

impl MatchState {
    pub fn new(cfg: MatchCfg) -> Self {
        Self {
            cfg,
            phase: Phase::Warmup { countdown_from: None },
            map_index: 0,
            last_winner: None,
            team_mode: false,
            team_frag_limit: 50,
            team_banked: [0; 2],
            last_winner_team: None,
            sudden_death_ticks: 60 * TICK_HZ,
            sudden: None,
            last_count: None,
        }
    }

    /// Switch to team deathmatch: first team to `frag_limit` team kills (or the time limit) wins.
    pub fn with_teams(mut self, frag_limit: u16) -> Self {
        self.team_mode = true;
        self.team_frag_limit = frag_limit;
        self
    }

    pub fn kind(&self) -> PhaseKind {
        match self.phase {
            Phase::Warmup { .. } => PhaseKind::Warmup,
            Phase::Live { .. } => PhaseKind::Live,
            Phase::Intermission { .. } => PhaseKind::Intermission,
        }
    }

    /// Input and damage are frozen.
    pub fn frozen(&self) -> bool {
        matches!(self.phase, Phase::Intermission { .. })
    }

    /// Ticks left in the current timed phase (`None` while warmup waits for players). For HUD clocks.
    pub fn ticks_left(&self, now: u32) -> Option<u32> {
        let (from, len) = match self.phase {
            Phase::Warmup { countdown_from: Some(f) } => (f, self.cfg.warmup_ticks),
            Phase::Warmup { countdown_from: None } => return None,
            Phase::Live { since } => (since, self.cfg.time_limit_ticks),
            Phase::Intermission { since } => (since, self.cfg.intermission_ticks),
        };
        Some(len.saturating_sub(since(now, from)))
    }

    /// True while a tied team match is in sudden death (for HUDs / the console).
    pub fn in_sudden_death(&self) -> bool {
        self.sudden.is_some()
    }

    /// Team kills: current players' frags plus those of players who left.
    pub fn team_score(&self, players: &[Player], team: u8) -> u16 {
        let live: u32 = players.iter().filter(|p| p.team == team).map(|p| p.vitals.kills as u32).sum();
        (live + self.team_banked[crate::team::valid(team) as usize] as u32).min(u16::MAX as u32) as u16
    }

    pub fn team_scores(&self, players: &[Player]) -> [u16; 2] {
        [self.team_score(players, crate::team::TEAM_CYAN), self.team_score(players, crate::team::TEAM_ORANGE)]
    }

    /// Highest score first: kills desc, deaths asc, id asc.
    pub fn scoreboard(players: &[Player]) -> Vec<ScoreEntry> {
        let mut v: Vec<ScoreEntry> = players
            .iter()
            .map(|p| ScoreEntry { id: p.id, kills: p.vitals.kills, deaths: p.vitals.deaths, alive: p.vitals.alive() })
            .collect();
        v.sort_by_key(|e| (std::cmp::Reverse(e.kills), e.deaths, e.id));
        v
    }

    fn countdown_event(&mut self, tick: u32, ticks_left: u32, out: &mut Vec<MatchEvent>) {
        let (kind, secs) = (self.kind(), ticks_left.div_ceil(TICK_HZ));
        if self.last_count != Some((kind, secs)) {
            self.last_count = Some((kind, secs));
            out.push(MatchEvent::Countdown { tick, phase: kind, seconds: secs });
        }
    }

    /// Start of tick: phase transitions that reset the world. Teleports happen before positions are sampled.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_tick(
        &mut self,
        tick: u32,
        players: &mut [Player],
        world: &mut PhysicsWorld,
        spawns: crate::spawn::SpawnSet,
        spawn_counter: &mut u32,
        pickups: &mut Pickups,
        invuln_ticks: u32,
        out: &mut Vec<MatchEvent>,
    ) {
        match self.phase {
            Phase::Warmup { countdown_from } => {
                let enough = players.len() >= self.cfg.min_players;
                match (enough, countdown_from) {
                    (false, _) => {
                        self.phase = Phase::Warmup { countdown_from: None };
                        self.last_count = None;
                    }
                    (true, None) => self.phase = Phase::Warmup { countdown_from: Some(tick) },
                    (true, Some(from)) if since(tick, from) >= self.cfg.warmup_ticks => {
                        self.reset_all(tick, players, world, spawns, spawn_counter, pickups, invuln_ticks);
                        self.phase = Phase::Live { since: tick };
                        self.last_count = None;
                        out.push(MatchEvent::MatchStart { tick, map: self.map_index });
                        return;
                    }
                    _ => {}
                }
                if let Phase::Warmup { countdown_from: Some(f) } = self.phase {
                    let left = self.cfg.warmup_ticks.saturating_sub(since(tick, f));
                    self.countdown_event(tick, left, out);
                }
            }
            Phase::Live { .. } => {
                if players.is_empty() {
                    // nobody left: don't burn the clock on an empty room
                    self.phase = Phase::Warmup { countdown_from: None };
                    self.last_count = None;
                }
            }
            Phase::Intermission { since: s } => {
                if since(tick, s) >= self.cfg.intermission_ticks {
                    self.map_index = self.map_index.wrapping_add(1);
                    out.push(MatchEvent::NextMap { tick, map: self.map_index });
                    self.phase = Phase::Warmup { countdown_from: None };
                    self.last_count = None;
                } else {
                    let left = self.cfg.intermission_ticks - since(tick, s);
                    self.countdown_event(tick, left, out);
                }
            }
        }
    }

    /// After damage is applied: check the limits once.
    pub fn end_tick(&mut self, tick: u32, players: &[Player], out: &mut Vec<MatchEvent>) {
        let Phase::Live { since: s } = self.phase else { return };
        let board = Self::scoreboard(players);
        let teams = self.team_scores(players);
        let frag = if self.team_mode {
            teams.iter().any(|s| *s >= self.team_frag_limit)
        } else {
            board.first().map_or(false, |e| e.kills >= self.cfg.frag_limit)
        };
        let time = since(tick, s).saturating_add(1) >= self.cfg.time_limit_ticks;
        // training never ends; co-op has no frag race (time limit still applies)
        let mut frag = frag && self.cfg.mode == GameMode::Deathmatch;
        let mut end_now = frag || time;
        if self.team_mode && self.cfg.mode == GameMode::Deathmatch {
            match self.sudden {
                // sudden death: the first team kill decides, otherwise the clock runs out into a draw
                Some((deadline, base)) => {
                    end_now = teams != base || (deadline.wrapping_sub(tick.wrapping_add(1)) as i32) <= 0;
                    frag = false;
                }
                None if end_now && teams[0] == teams[1] => {
                    self.sudden = Some((tick.wrapping_add(1).wrapping_add(self.sudden_death_ticks), teams));
                    return;
                }
                None => {}
            }
        }
        if self.cfg.mode == GameMode::Training || !end_now {
            return;
        }
        self.sudden = None;
        let winner_team = match teams[0].cmp(&teams[1]) {
            _ if !self.team_mode => None,
            std::cmp::Ordering::Greater => Some(crate::team::TEAM_CYAN),
            std::cmp::Ordering::Less => Some(crate::team::TEAM_ORANGE),
            std::cmp::Ordering::Equal => None,
        };
        let winner = match (board.first(), board.get(1)) {
            _ if self.team_mode => None,
            (Some(a), Some(b)) if (a.kills, a.deaths) == (b.kills, b.deaths) => None,
            (Some(a), _) if a.kills > 0 || frag => Some(a.id),
            _ => None,
        };
        self.last_winner = winner;
        self.last_winner_team = winner_team;
        self.phase = Phase::Intermission { since: tick.wrapping_add(1) };
        self.last_count = None;
        out.push(MatchEvent::MatchEnd {
            tick,
            winner,
            reason: if frag { EndReason::FragLimit } else { EndReason::TimeLimit },
        });
    }

    /// Fresh slate for everyone: scores, hp, inventories, pickups, positions (spread by the selector).
    fn reset_all(
        &mut self,
        tick: u32,
        players: &mut [Player],
        world: &mut PhysicsWorld,
        spawns: crate::spawn::SpawnSet,
        spawn_counter: &mut u32,
        pickups: &mut Pickups,
        invuln_ticks: u32,
    ) {
        self.team_banked = [0; 2];
        self.last_winner_team = None;
        self.sudden = None;
        // everyone is "dead" for the selection pass so players are placed one after another, each avoiding
        // those already placed (order = player order: deterministic)
        for p in players.iter_mut() {
            p.vitals.kills = 0;
            p.vitals.deaths = 0;
            p.vitals.dead_until_tick = Some(tick);
            p.combat.powerups.clear();
        }
        for i in 0..players.len() {
            let s = pvp::pick_spawn_for(players, i, world, spawns, spawn_counter);
            pvp::revive(&mut players[i], world, s, tick, invuln_ticks);
        }
        pickups.reset();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, PITCH_LEVEL};

    use super::*;
    use crate::character::Cfg;
    use crate::combat::powerup::PowerupKind;
    use crate::tick::Server;

    const A: u16 = 1;
    const B: u16 = 2;

    fn cfg() -> MatchCfg {
        MatchCfg { warmup_ticks: 30, time_limit_ticks: 600, intermission_ticks: 20, frag_limit: 3, min_players: 2, mode: GameMode::Deathmatch }
    }

    fn server(n: u16) -> Server {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        s.set_spawns(vec![[-20.0, 0.81, 0.0], [20.0, 0.81, 0.0]]);
        s.match_state = MatchState::new(cfg());
        for id in 1..=n {
            s.add_player(id, [0.0, 0.81, -3.0 * id as f32]);
        }
        s
    }

    fn step(s: &mut Server, n: u32, ev: &mut Vec<MatchEvent>) {
        for _ in 0..n {
            s.frame(Duration::from_nanos(16_666_667));
            ev.extend(s.match_events.iter().copied());
        }
    }

    fn player(s: &mut Server, id: u16) -> &mut Player {
        s.players.iter_mut().find(|p| p.id == id).unwrap()
    }

    fn pos_of(s: &Server, id: u16) -> [f32; 3] {
        s.players.iter().find(|p| p.id == id).unwrap().pos(&s.world)
    }

    fn live(s: &mut Server) -> Vec<MatchEvent> {
        let mut ev = Vec::new();
        step(s, 40, &mut ev);
        assert!(matches!(s.match_state.phase, Phase::Live { .. }), "{:?}", s.match_state.phase);
        ev
    }

    #[test]
    fn warmup_waits_for_players_counts_down_then_resets_everything_and_goes_live() {
        let mut s = server(1);
        let mut ev = Vec::new();
        step(&mut s, 100, &mut ev);
        assert_eq!(s.match_state.phase, Phase::Warmup { countdown_from: None }, "one player: keep waiting");
        assert!(ev.is_empty() && s.match_state.ticks_left(s.tick).is_none());

        s.add_player(B, [0.0, 0.81, -6.0]);
        // warmup scores and loot must not survive into the match
        player(&mut s, A).vitals.kills = 7;
        player(&mut s, B).vitals.deaths = 4;
        player(&mut s, A).combat.powerups.grant(PowerupKind::GridBeam, 20.0);
        s.add_pickup(1, [10.0, 0.81, 10.0], PowerupKind::GridBeam, 5.0);
        step(&mut s, 5, &mut ev);
        assert!(s.pickups.active().count() == 1);
        step(&mut s, 40, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, MatchEvent::Countdown { phase: PhaseKind::Warmup, seconds: 1, .. })));
        assert!(ev.iter().any(|e| matches!(e, MatchEvent::MatchStart { map: 0, .. })));
        assert!(matches!(s.match_state.phase, Phase::Live { .. }));
        for id in [A, B] {
            let p = player(&mut s, id);
            assert_eq!((p.vitals.kills, p.vitals.deaths, p.vitals.hp), (0, 0, p.vitals.max_hp));
            assert!(!p.combat.powerups.has_charge());
            assert_eq!(p.guard.strikes, 0, "reset teleport is not a speed hack");
        }
        // everybody stands on a spawn point, and not the same one
        let (pa, pb) = (pos_of(&s, A), pos_of(&s, B));
        assert!((pa[0].abs() - 20.0).abs() < 0.5 && (pb[0].abs() - 20.0).abs() < 0.5 && pa[0] != pb[0], "{pa:?} {pb:?}");
    }

    #[test]
    fn team_match_is_a_team_race_and_leavers_keep_their_teams_score() {
        let mut s = server(4); // 1,3 Cyan; 2,4 Orange (auto-balance alternates)
        s.match_state = MatchState::new(cfg()).with_teams(5);
        assert_eq!(s.players.iter().map(|p| p.team).collect::<Vec<_>>(), vec![0, 1, 0, 1]);
        live(&mut s);
        // an individual with 3 kills (frag_limit) must NOT end a team match
        player(&mut s, 1).vitals.kills = 3;
        let mut ev = Vec::new();
        step(&mut s, 3, &mut ev);
        assert!(matches!(s.match_state.phase, Phase::Live { .. }), "individual frag limit is ignored in team mode");
        assert_eq!(s.match_state.team_scores(&s.players), [3, 0]);
        // the scorer leaves: their frags stay on Cyan's side
        s.remove_player(1);
        assert_eq!(s.match_state.team_scores(&s.players), [3, 0]);
        player(&mut s, 3).vitals.kills = 1;
        player(&mut s, 2).vitals.kills = 1;
        assert_eq!(s.match_state.team_scores(&s.players), [4, 1]);
        player(&mut s, 3).vitals.kills = 2; // Cyan 3 + 2 = 5 = limit
        step(&mut s, 3, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, MatchEvent::MatchEnd { winner: None, reason: EndReason::FragLimit, .. })));
        assert_eq!(s.match_state.last_winner_team, Some(crate::team::TEAM_CYAN));
    }

    #[test]
    fn tied_team_match_goes_to_sudden_death_first_kill_wins_else_draw() {
        let tied = |ev: &[MatchEvent]| ev.iter().any(|e| matches!(e, MatchEvent::MatchEnd { .. }));
        // 1) time runs out 2-2 -> sudden death, then a kill by Orange ends it
        let mut s = server(4); // 1,3 Cyan; 2,4 Orange
        s.match_state = MatchState::new(cfg()).with_teams(50);
        s.match_state.sudden_death_ticks = 120;
        live(&mut s);
        player(&mut s, 1).vitals.kills = 2;
        player(&mut s, 2).vitals.kills = 2;
        let mut ev = Vec::new();
        step(&mut s, 620, &mut ev); // past the 600-tick limit
        assert!(!tied(&ev), "a tie does not end the match");
        assert!(s.match_state.in_sudden_death() && matches!(s.match_state.phase, Phase::Live { .. }));
        player(&mut s, 4).vitals.kills = 1;
        step(&mut s, 3, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, MatchEvent::MatchEnd { reason: EndReason::TimeLimit, .. })));
        assert_eq!(s.match_state.last_winner_team, Some(crate::team::TEAM_ORANGE));
        assert!(!s.match_state.in_sudden_death());
        // 2) nobody scores: draw when sudden death runs out
        let mut s = server(4);
        s.match_state = MatchState::new(cfg()).with_teams(50);
        s.match_state.sudden_death_ticks = 60;
        live(&mut s);
        let mut ev = Vec::new();
        step(&mut s, 600 + 40, &mut ev);
        assert!(s.match_state.in_sudden_death() && !tied(&ev));
        step(&mut s, 80, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, MatchEvent::MatchEnd { winner: None, .. })));
        assert_eq!(s.match_state.last_winner_team, None, "draw");
    }

    #[test]
    fn frag_limit_ends_the_match_freezes_everything_then_moves_to_the_next_map() {
        let mut s = server(2);
        live(&mut s);
        player(&mut s, A).vitals.kills = 3;
        player(&mut s, B).vitals.deaths = 2;
        let mut ev = Vec::new();
        step(&mut s, 2, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, MatchEvent::MatchEnd { winner: Some(A), reason: EndReason::FragLimit, .. })));
        assert!(s.match_state.frozen());
        assert_eq!(s.match_state.last_winner, Some(A));

        // frozen: an attack changes nothing and walking goes nowhere
        let (hp, pos) = (player(&mut s, B).vitals.hp, pos_of(&s, A));
        let t = s.tick;
        let it = PlayerIntent { tick: t, buttons: Buttons(Buttons::SLICE), move_z: 127, aim_pitch: PITCH_LEVEL, ..Default::default() };
        s.ingest(A, &ClientPacket { ack_snapshot_tick: t, intents: vec![it], ..Default::default() }.encode().unwrap());
        step(&mut s, 5, &mut ev);
        assert_eq!(player(&mut s, B).vitals.hp, hp);
        assert_eq!(pos_of(&s, A), pos, "intermission freezes input");

        step(&mut s, 25, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, MatchEvent::NextMap { map: 1, .. })));
        assert_eq!(s.match_state.map_index, 1);
        assert!(matches!(s.match_state.phase, Phase::Warmup { .. }));
    }

    fn tie_run(a_deaths: u16) -> Vec<MatchEvent> {
        let mut s = server(2);
        live(&mut s);
        for id in [A, B] {
            player(&mut s, id).vitals.kills = 3; // both reach the limit on the same tick
        }
        player(&mut s, A).vitals.deaths = a_deaths;
        player(&mut s, B).vitals.deaths = 1;
        let mut ev = Vec::new();
        step(&mut s, 2, &mut ev);
        ev.into_iter().filter(|e| matches!(e, MatchEvent::MatchEnd { .. })).collect()
    }

    #[test]
    fn same_tick_frag_limit_tie_breaks_on_deaths_else_draws_the_same_way_every_run() {
        let draw = tie_run(1);
        assert!(matches!(draw[..], [MatchEvent::MatchEnd { winner: None, reason: EndReason::FragLimit, .. }]));
        assert_eq!(tie_run(1), draw, "deterministic");
        // control: fewer deaths wins
        let win = tie_run(0);
        assert!(matches!(win[..], [MatchEvent::MatchEnd { winner: Some(A), .. }]));
    }

    #[test]
    fn time_limit_ends_by_kills_and_an_empty_room_returns_to_warmup() {
        let mut s = server(2);
        live(&mut s);
        player(&mut s, B).vitals.kills = 2;
        let mut ev = Vec::new();
        step(&mut s, 700, &mut ev);
        assert!(ev.iter().any(|e| matches!(e, MatchEvent::MatchEnd { winner: Some(B), reason: EndReason::TimeLimit, .. })));

        // an empty server must not run a match
        let mut s = server(2);
        live(&mut s);
        s.remove_player(A);
        s.remove_player(B);
        let mut ev = Vec::new();
        step(&mut s, 5, &mut ev);
        assert_eq!(s.match_state.phase, Phase::Warmup { countdown_from: None });
    }

    #[test]
    fn scoreboard_sorts_kills_then_fewer_deaths_then_id() {
        let mut s = server(3);
        player(&mut s, 1).vitals.kills = 2;
        player(&mut s, 2).vitals.kills = 5;
        player(&mut s, 3).vitals.kills = 2;
        player(&mut s, 3).vitals.deaths = 1;
        let ids: Vec<u16> = MatchState::scoreboard(&s.players).iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![2, 1, 3]);
    }
}
