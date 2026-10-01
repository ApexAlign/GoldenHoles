//! Bot host: owns every brain, the squad director and the ticket queue, and drives the authoritative `Server`
//! one 60 Hz tick at a time.
//!
//! `Server::step` is private and `Server::frame` may run 0..=5 steps, but a brain must decide once per tick after
//! seeing tick T-1's result. So the host owns the outer fixed-step accumulator (`tick::Accumulator`, integer math,
//! spiral-of-death clamp) and calls `server.frame` with exactly one tick of time per step. Per-tick durations
//! cycle 16_666_667, 16_666_667, 16_666_666 ns: three calls sum to exactly 50 ms = 3 ticks, so the server's own
//! integer accumulator yields exactly one step per call with zero drift. Events of every sub-step are merged back
//! into `server.events / pvp_events / pickup_events / match_events`, so callers read the same fields as before.
//!
//! Per tick, in this order (nothing depends on bot iteration order beyond ascending id):
//! 1. build a read-only `WorldView` (players + live walls sorted by id)
//! 2. director: assign each bot a target (nearest human, sticky) and a flank slot
//! 3. perceive + collect off-screen strike requests
//! 4. settle attack tickets once, in a fixed order
//! 5. every brain thinks -> `PlayerIntent`
//! 6. intents go into the players' `IntentQueue` (same path as network packets), then the server steps

use std::time::Duration;

use shinobi_proto::intent::{ClientPacket, PlayerIntent};
use shinobi_proto::TICK_HZ;
use shinobi_sim::tick::{allied, Accumulator, FrameReport, Server};

use crate::fsm::{Assignment, BotMem, Perception, ThinkCtx};
use crate::nav::{find_perches, Perch};
use crate::skill::{Archetype, BotSkill};
use crate::tickets::{AttackTickets, Request, TicketCfg};
use crate::view::{Frustum, PlayerView, WorldView};

#[derive(Clone, Copy, Debug)]
pub struct BotHostCfg {
    pub seed: u64,
    pub tickets: TicketCfg,
    pub frustum: Frustum,
    /// Re-pick a target only when another is this much closer (ratio).
    pub target_stickiness: f32,
}

impl Default for BotHostCfg {
    fn default() -> Self {
        Self { seed: 0x5348_494E_4F42_4921, tickets: TicketCfg::default(), frustum: Frustum::default(), target_stickiness: 0.75 }
    }
}

pub struct BotHost {
    pub cfg: BotHostCfg,
    bots: Vec<BotMem>, // sorted by id
    pub tickets: AttackTickets,
    acc: Accumulator,
    step_ix: u64,
    perches: Vec<Perch>,
    perches_built: bool,
    sticky: Vec<(u16, u16)>, // (bot, target)
}

/// Three consecutive tick durations summing to exactly 3/60 s.
const STEP_NS: [u64; 3] = [16_666_667, 16_666_667, 16_666_666];

impl BotHost {
    pub fn new(cfg: BotHostCfg) -> Self {
        Self {
            cfg,
            bots: Vec::new(),
            tickets: AttackTickets::new(cfg.tickets),
            acc: Accumulator::new(TICK_HZ, 5),
            step_ix: 0,
            perches: Vec::new(),
            perches_built: false,
            sticky: Vec::new(),
        }
    }

    /// Adds a bot at a spawn point chosen like a joining player. `false` if the id is already in use.
    /// Wall ownership on the wire is `id as u8`, so bot ids must be unique modulo 256 among all players.
    pub fn add_bot(&mut self, server: &mut Server, id: u16, archetype: Archetype, skill: BotSkill) -> bool {
        if server.players.iter().any(|p| p.id == id || p.id as u8 == id as u8) || self.bots.iter().any(|b| b.id == id) {
            return false;
        }
        let spawn = server.pick_spawn(None);
        server.add_player(id, spawn);
        let pos = self.bots.binary_search_by_key(&id, |b| b.id).unwrap_or_else(|i| i);
        self.bots.insert(pos, BotMem::new(id, archetype, skill.params(), self.cfg.seed));
        true
    }

    /// `add_bot` onto a given team.
    pub fn add_bot_on(&mut self, server: &mut Server, id: u16, archetype: Archetype, skill: BotSkill, team: u8) -> bool {
        if !self.add_bot(server, id, archetype, skill) {
            return false;
        }
        server.set_team(id, team);
        // spawn where the team's points are (add_bot picked a spot before the team was known)
        let at = server.pick_spawn(Some(id));
        if let Some(p) = server.players.iter().find(|p| p.id == id) {
            let body = p.ch.body;
            server.world.bodies[body].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(at[0], at[1], at[2]), true);
            shinobi_sim::character::sync_body_colliders(&mut server.world, body);
        }
        true
    }

    /// Keep every team at `per_side` players: bots join short teams (ids from `first_id` up, skipping used ones),
    /// and surplus bots leave teams that humans have filled (highest id first). Returns `(added, removed)`.
    pub fn fill_teams(&mut self, server: &mut Server, per_side: usize, first_id: u16, archetype: Archetype, skill: BotSkill) -> (Vec<u16>, Vec<u16>) {
        let (mut added, mut removed) = (Vec::new(), Vec::new());
        let mut next = first_id;
        for team in shinobi_sim::team::TEAM_CYAN..shinobi_sim::team::TEAM_COUNT {
            while server.team_size(team) > per_side {
                let Some(id) = self.bots.iter().rev().map(|b| b.id).find(|id| server.players.iter().any(|p| p.id == *id && p.team == team)) else { break };
                self.remove_bot(server, id);
                removed.push(id);
            }
            while server.team_size(team) < per_side {
                while !self.add_bot_on(server, next, archetype, skill, team) {
                    next = next.checked_add(1).expect("bot ids exhausted");
                }
                added.push(next);
            }
        }
        (added, removed)
    }

    pub fn remove_bot(&mut self, server: &mut Server, id: u16) -> bool {
        let Some(i) = self.bots.iter().position(|b| b.id == id) else { return false };
        self.bots.remove(i);
        self.tickets.release(id, server.tick);
        self.sticky.retain(|s| s.0 != id);
        server.remove_player(id);
        true
    }

    /// Frees one bot (highest id first), e.g. when a human needs the slot.
    pub fn remove_one(&mut self, server: &mut Server) -> Option<u16> {
        let id = self.bots.last()?.id;
        self.remove_bot(server, id).then_some(id)
    }

    pub fn bot_count(&self) -> usize {
        self.bots.len()
    }

    pub fn bot_ids(&self) -> Vec<u16> {
        self.bots.iter().map(|b| b.id).collect()
    }

    pub fn bot(&self, id: u16) -> Option<&BotMem> {
        self.bots.iter().find(|b| b.id == id)
    }

    pub fn bots(&self) -> &[BotMem] {
        &self.bots
    }

    pub fn is_bot(&self, id: u16) -> bool {
        self.bots.binary_search_by_key(&id, |b| b.id).is_ok()
    }

    /// Call after loading a level: finds boxes the Light bots can hop onto.
    pub fn rebuild_perches(&mut self, server: &Server) {
        self.perches = find_perches(server, 1.0, 2.5);
        self.perches_built = true;
    }

    pub fn perches(&self) -> &[Perch] {
        &self.perches
    }

    /// Drop-in replacement for `Server::frame(elapsed)`: runs each due 60 Hz tick with bots deciding first.
    pub fn advance(&mut self, server: &mut Server, elapsed: Duration) -> FrameReport {
        let adv = self.acc.advance(elapsed);
        let mut report = FrameReport { ticks: 0, discarded: adv.discarded, kicked: Vec::new() };
        server.events.clear();
        server.pvp_events.clear();
        server.pickup_events.clear();
        server.match_events.clear();
        let (mut ev, mut pvp, mut pk, mut me) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for _ in 0..adv.steps {
            self.step_once(server, &mut report, (&mut ev, &mut pvp, &mut pk, &mut me));
        }
        server.events = ev;
        server.pvp_events = pvp;
        server.pickup_events = pk;
        server.match_events = me;
        report.kicked.sort_unstable();
        report.kicked.dedup();
        report
    }

    /// Exactly one tick: bots think, then the server steps. For tests and fixed-step drivers.
    pub fn tick(&mut self, server: &mut Server) -> FrameReport {
        let mut report = FrameReport { ticks: 0, discarded: 0, kicked: Vec::new() };
        let (mut ev, mut pvp, mut pk, mut me) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        self.step_once(server, &mut report, (&mut ev, &mut pvp, &mut pk, &mut me));
        server.events = ev;
        server.pvp_events = pvp;
        server.pickup_events = pk;
        server.match_events = me;
        report
    }

    #[allow(clippy::type_complexity)]
    fn step_once(
        &mut self,
        server: &mut Server,
        report: &mut FrameReport,
        sinks: (
            &mut Vec<(u16, shinobi_sim::combat::CombatEvent)>,
            &mut Vec<shinobi_sim::pvp::PvpEvent>,
            &mut Vec<shinobi_sim::pickups::PickupEvent>,
            &mut Vec<shinobi_sim::match_state::MatchEvent>,
        ),
    ) {
        if !self.perches_built {
            self.rebuild_perches(server);
        }
        let intents = self.think_all(server);
        let tick = server.tick;
        for (id, it) in intents {
            if let Some(p) = server.players.iter_mut().find(|p| p.id == id) {
                // no ViewState: bots see ground truth, so there is nothing to rewind
                p.queue.push_packet(&ClientPacket { intents: vec![it], ..Default::default() }, tick);
            }
        }
        let ns = STEP_NS[(self.step_ix % 3) as usize];
        self.step_ix += 1;
        let r = server.frame(Duration::from_nanos(ns));
        debug_assert_eq!(r.ticks, 1, "host must own the clock: one server step per bot tick");
        report.ticks += r.ticks;
        report.kicked.extend(r.kicked);
        sinks.0.append(&mut server.events);
        sinks.1.append(&mut server.pvp_events);
        sinks.2.append(&mut server.pickup_events);
        sinks.3.append(&mut server.match_events);
        // anti-cheat may have kicked a bot: forget it
        let alive_ids: Vec<u16> = server.players.iter().map(|p| p.id).collect();
        let gone: Vec<u16> = self.bots.iter().map(|b| b.id).filter(|id| !alive_ids.contains(id)).collect();
        for id in gone {
            self.bots.retain(|b| b.id != id);
            self.tickets.release(id, tick);
            self.sticky.retain(|s| s.0 != id);
        }
    }

    /// One decision per bot for `server.tick`. Pure with respect to the server (read-only).
    pub fn think_all(&mut self, server: &Server) -> Vec<(u16, PlayerIntent)> {
        let ids: Vec<u16> = self.bots.iter().map(|b| b.id).collect();
        let world = WorldView::build(server, |id| ids.binary_search(&id).is_ok());
        let tick = world.tick;

        // ---- director: targets (nearest living human, sticky), then ranks per target and archetype
        let humans: Vec<_> = world.players.iter().filter(|p| p.alive && !p.is_bot).collect();
        let mut targets: Vec<Option<u16>> = Vec::with_capacity(self.bots.len());
        for b in &self.bots {
            let Some(me) = world.player(b.id).filter(|p| p.alive) else {
                targets.push(None);
                continue;
            };
            // never an ally: humans on the other side first, else any living enemy (bot-vs-bot)
            let foe = |p: &PlayerView| p.id != b.id && !allied(me.team, p.team);
            let human_foes: Vec<&PlayerView> = humans.iter().copied().filter(|p| foe(p)).collect();
            let pool: Vec<&PlayerView> = if human_foes.is_empty() {
                world.players.iter().filter(|p| p.alive && foe(p)).collect()
            } else {
                human_foes
            };
            let dist = |id: u16| pool.iter().find(|p| p.id == id).map(|p| (p.pos - me.pos).norm());
            let best = pool
                .iter()
                .map(|p| ((p.pos - me.pos).norm(), p.id))
                .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let prev = self.sticky.iter().find(|s| s.0 == b.id).map(|s| s.1);
            let pick = match (prev.and_then(|id| dist(id).map(|d| (d, id))), best) {
                (Some((dp, ip)), Some((db, _))) if db >= dp * self.cfg.target_stickiness => Some(ip),
                (_, Some((_, ib))) => Some(ib),
                _ => None,
            };
            targets.push(pick);
        }
        self.sticky = self.bots.iter().zip(&targets).filter_map(|(b, t)| t.map(|t| (b.id, t))).collect();
        let rank_of = |i: usize| -> usize {
            let (b, t) = (&self.bots[i], targets[i]);
            self.bots
                .iter()
                .zip(&targets)
                .take(i)
                .filter(|(o, ot)| **ot == t && o.archetype == b.archetype)
                .count()
        };
        let assigns: Vec<Assignment> = (0..self.bots.len())
            .map(|i| {
                let rank = rank_of(i);
                Assignment { target: targets[i], slot_angle: self.bots[i].archetype.slot_angle(rank), rank }
            })
            .collect();

        // ---- pass 1: perceive and collect ticket requests
        let frustum = self.cfg.frustum;
        let mut percs: Vec<Perception> = Vec::with_capacity(self.bots.len());
        let mut requests: Vec<Request> = Vec::new();
        for (i, b) in self.bots.iter_mut().enumerate() {
            let Some(me) = world.player(b.id) else {
                percs.push(Perception::default());
                continue;
            };
            let ctx = ThinkCtx { world: &world, me, assign: assigns[i], has_ticket: false, perches: &self.perches, frustum };
            let p = b.perceive(&ctx);
            if let Some(r) = b.strike_request(&ctx, &p) {
                requests.push(r);
            }
            percs.push(p);
        }

        // ---- settle tickets once, order-independent
        self.tickets.settle(tick, &requests, |id| world.player(id).map_or(false, |p| p.alive));

        // ---- pass 2: think
        let mut out = Vec::with_capacity(self.bots.len());
        let mut releases = Vec::new();
        for (i, b) in self.bots.iter_mut().enumerate() {
            let Some(me) = world.player(b.id) else { continue };
            let ctx = ThinkCtx {
                world: &world,
                me,
                assign: assigns[i],
                has_ticket: self.tickets.holds(b.id),
                perches: &self.perches,
                frustum,
            };
            let o = b.think(&ctx, &percs[i]);
            if o.release_ticket {
                releases.push(b.id);
            }
            out.push((b.id, o.intent));
        }
        for id in releases {
            self.tickets.release(id, tick);
        }
        out
    }
}
