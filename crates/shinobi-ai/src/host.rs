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
use crate::skill::{Archetype, BotSkill, SkillParams};
use crate::tickets::{AttackTickets, Request, TicketCfg};
use crate::view::{Frustum, PlayerView, WorldView};

#[derive(Clone, Copy, Debug)]
pub struct BotHostCfg {
    pub seed: u64,
    pub tickets: TicketCfg,
    pub frustum: Frustum,
    /// Re-pick a target only when another is this much closer (ratio).
    pub target_stickiness: f32,
    /// Demo feel: a human who has not been in a fight or seen an enemy for this many seconds gets company ...
    pub human_quiet_secs: f32,
    /// ... from this many of the nearest bots, which path to the human and then fight normally (0 = off).
    pub rubber_band_bots: usize,
    /// Pace of the fights, 1.0 = the skill's own numbers. Below 1 bots shoot less often and less accurately
    /// (fire gap / aggression, aim error / aggression): longer matches without touching the skill tiers.
    pub aggression: f32,
    /// Warm-up for a new human: aggression starts at `warmup_aggression` when the first human is seen and ramps
    /// linearly to `aggression` over `warmup_secs` (0 = off), so a fresh join is not met by full-pace fire.
    pub warmup_secs: f32,
    pub warmup_aggression: f32,
    /// During the warm-up at most this many bots go after any one human (the rest fight each other); the cap
    /// triples for the same time again, then lifts.
    pub warmup_engagers: usize,
}

impl Default for BotHostCfg {
    fn default() -> Self {
        Self {
            seed: 0x5348_494E_4F42_4921,
            tickets: TicketCfg::default(),
            frustum: Frustum::default(),
            target_stickiness: 0.75,
            human_quiet_secs: 15.0,
            rubber_band_bots: 2,
            warmup_secs: 25.0,
            warmup_aggression: 0.4,
            warmup_engagers: 1,
            aggression: 0.8, // demo pace: a touch below the skill tiers so a 10-kill FFA match runs minutes, not seconds
        }
    }
}

/// Skill numbers at pace `a`: fire gap and katana wind-up / a, aim error / min(a, 1), slower movement below 1.
fn scaled(mut p: SkillParams, a: f32) -> SkillParams {
    p.fire_gap_ticks = ((p.fire_gap_ticks as f32) / a).round() as u32;
    p.aim_error /= a.min(1.0);
    p.strike_windup = ((p.strike_windup as f32) / a).round() as u32;
    p.move_scale = (0.5 + 0.5 * a).min(1.0);
    p
}

/// A* plans the whole host may run per tick (bots over budget wait a tick: no frame is ever a burst of searches).
const PLAN_PER_TICK: u32 = 2;

/// During the warm-up bots leave a human below this health fraction alone.
const WARMUP_MERCY_HP: f32 = 0.35;

/// Enemy this close with a clear line counts as "the human has action".
const ACTION_RANGE: f32 = 35.0;
/// A rubber-band summons lasts at most this long (seconds) if no fight starts.
const SUMMON_MAX_SECS: u32 = 30;

pub struct BotHost {
    pub cfg: BotHostCfg,
    bots: Vec<BotMem>, // sorted by id
    pub tickets: AttackTickets,
    acc: Accumulator,
    step_ix: u64,
    perches: Vec<Perch>,
    perches_built: bool,
    /// Walkable-ground grid for path planning (built once per level, see `build_nav`).
    nav: Option<crate::nav::NavGrid>,
    sticky: Vec<(u16, u16)>, // (bot, target)
    /// Last tick each human was in a fight or saw an enemy: `(human, tick)`.
    human_action: Vec<(u16, u32)>,
    /// Bots currently sent to a quiet human: `(bot, human, since tick)`.
    summoned: Vec<(u16, u16, u32)>,
    /// Tick the first human was seen (starts the aggression warm-up).
    human_joined: Option<u32>,
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
            nav: None,
            human_action: Vec::new(),
            summoned: Vec::new(),
            human_joined: None,
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
        let base = skill.params();
        let mut mem = BotMem::new(id, archetype, scaled(base, self.aggression_at(server.tick)), self.cfg.seed);
        mem.base_skill = base;
        self.bots.insert(pos, mem);
        true
    }

    /// Effective aggression at `tick`: the configured pace, lowered during a new human's warm-up.
    fn aggression_at(&self, tick: u32) -> f32 {
        let full = self.cfg.aggression.clamp(0.2, 2.0);
        match self.human_joined {
            Some(t0) if self.cfg.warmup_secs > 0.0 => {
                let k = (tick.saturating_sub(t0) as f32 / (self.cfg.warmup_secs * TICK_HZ as f32)).min(1.0);
                let from = self.cfg.warmup_aggression.clamp(0.2, 2.0).min(full);
                from + (full - from) * k
            }
            _ => full,
        }
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
        self.perches = find_perches(server, 1.0, server.cfg().max_hop_rise(0.3)); // tallest box a held jump + double jump reaches
        self.perches_built = true;
        self.nav = Some(build_nav(server));
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
        for e in &server.pvp_events {
            if let shinobi_sim::pvp::PvpEvent::Hit { attacker, victim, .. } | shinobi_sim::pvp::PvpEvent::Kill { killer: attacker, victim, .. } = *e {
                for id in [attacker, victim] {
                    if !self.is_bot(id) {
                        self.note_action(id, tick);
                    }
                }
            }
        }
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

    fn note_action(&mut self, human: u16, tick: u32) {
        match self.human_action.iter_mut().find(|h| h.0 == human) {
            Some(h) => h.1 = tick,
            None => self.human_action.push((human, tick)),
        }
    }

    /// Soft rubber-band for the demo: keep every living human supplied with action. A human who has not been in a
    /// fight or seen an enemy for `human_quiet_secs` gets the nearest `rubber_band_bots` bots sent their way (they
    /// path to the human via the normal targeting, then fight like any bot). A summons ends when a fight starts,
    /// when the bot or human dies, or after `SUMMON_MAX_SECS`.
    fn rubber_band(&mut self, world: &WorldView, tick: u32) {
        let n = self.cfg.rubber_band_bots;
        let quiet_ticks = (self.cfg.human_quiet_secs * TICK_HZ as f32) as u32;
        let humans: Vec<PlayerView> = world.players.iter().filter(|p| p.alive && !p.is_bot).copied().collect();
        for h in &humans {
            if !self.human_action.iter().any(|a| a.0 == h.id) {
                self.human_action.push((h.id, tick)); // the clock starts when we first see them
            }
            // seen an enemy: anybody close with a clear line (checked a few times a second)
            if tick % 6 == 0 {
                let near = world.players.iter().any(|o| o.alive && o.id != h.id && (o.pos - h.pos).norm() <= ACTION_RANGE && world.los(h.eye(), o.eye()));
                if near {
                    self.note_action(h.id, tick);
                }
            }
        }
        let last = |id: u16, s: &Self| s.human_action.iter().find(|a| a.0 == id).map_or(tick, |a| a.1);
        // release finished summons
        let timeout = SUMMON_MAX_SECS * TICK_HZ;
        let keep: Vec<(u16, u16, u32)> = self
            .summoned
            .iter()
            .copied()
            .filter(|&(b, h, since)| {
                world.player(b).map_or(false, |p| p.alive) && world.player(h).map_or(false, |p| p.alive) && last(h, self) <= since && tick.wrapping_sub(since) < timeout
            })
            .collect();
        self.summoned = keep;
        if n == 0 {
            return;
        }
        for h in &humans {
            let quiet = tick.wrapping_sub(last(h.id, self)) >= quiet_ticks;
            let have = self.summoned.iter().filter(|s| s.1 == h.id).count();
            if !quiet || have >= n {
                continue;
            }
            // nearest living bots not already on a mission
            let mut cands: Vec<(f32, u16)> = self
                .bots
                .iter()
                .filter(|b| !self.summoned.iter().any(|s| s.0 == b.id))
                .filter_map(|b| world.player(b.id).filter(|p| p.alive).map(|p| ((p.pos - h.pos).norm(), p.id)))
                .collect();
            cands.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            for (_, b) in cands.into_iter().take(n - have) {
                self.summoned.push((b, h.id, tick));
            }
        }
    }

    /// One decision per bot for `server.tick`. Pure with respect to the server (read-only).
    pub fn think_all(&mut self, server: &Server) -> Vec<(u16, PlayerIntent)> {
        if self.nav.is_none() {
            self.nav = Some(build_nav(server)); // first think on a level nobody called rebuild_perches for
        }
        let ids: Vec<u16> = self.bots.iter().map(|b| b.id).collect();
        let world = WorldView::build(server, |id| ids.binary_search(&id).is_ok());
        let tick = world.tick;
        if self.human_joined.is_none() && world.players.iter().any(|p| ids.binary_search(&p.id).is_err()) {
            self.human_joined = Some(tick);
        }
        let a = self.aggression_at(tick);
        for b in &mut self.bots {
            b.skill = scaled(b.base_skill, a);
        }
        self.rubber_band(&world, tick);
        let budget = std::cell::Cell::new(PLAN_PER_TICK);

        // ---- director: targets (every bot is its own faction: nearest / weakest living enemy, sticky), then ranks
        // per target and archetype. Teammates (team mode only) are never targets; FFA players have no team.
        let mut targets: Vec<Option<u16>> = Vec::with_capacity(self.bots.len());
        // engager cap: `warmup_engagers` for the warm-up, three times that for as long again, then no cap
        let since_join = self.human_joined.map_or(f32::MAX, |t0| tick.saturating_sub(t0) as f32 / TICK_HZ as f32);
        let engager_cap = if self.cfg.warmup_secs <= 0.0 || since_join >= 2.0 * self.cfg.warmup_secs {
            usize::MAX
        } else if since_join < self.cfg.warmup_secs {
            self.cfg.warmup_engagers
        } else {
            self.cfg.warmup_engagers * 3
        };
        let warming = engager_cap != usize::MAX;
        let mut on_human: Vec<(u16, usize)> = Vec::new(); // (human, bots already assigned this tick)
        for b in &self.bots {
            let Some(me) = world.player(b.id).filter(|p| p.alive) else {
                targets.push(None);
                continue;
            };
            // sent to a quiet human: go to them first, fight normally once there
            if let Some(&(_, human, _)) = self.summoned.iter().find(|s| s.0 == b.id) {
                if world.player(human).map_or(false, |h| h.alive) {
                    targets.push(Some(human));
                    continue;
                }
            }
            let skip = |id: u16| b.unreachable.iter().any(|(t, until)| *t == id && *until > tick);
            let mut pool: Vec<&PlayerView> = world.players.iter().filter(|p| p.alive && p.id != b.id && !allied(me.team, p.team) && !skip(p.id)).collect();
            if warming {
                // a new human faces only a couple of bots at a time; the rest keep fighting each other
                let full = |id: u16| on_human.iter().find(|h| h.0 == id).map_or(0, |h| h.1) >= engager_cap;
                // ... and a badly hurt one is left alone (no healing in the arena yet: attrition would end the run)
                let others: Vec<&PlayerView> = pool
                    .iter()
                    .copied()
                    .filter(|p| ids.binary_search(&p.id).is_ok() || (!full(p.id) && p.hp_frac >= WARMUP_MERCY_HP))
                    .collect();
                if !others.is_empty() {
                    pool = others;
                }
            }
            if pool.is_empty() {
                // everybody is unreachable: fall back to the nearest anyway (the bot still faces and shoots them)
                pool = world.players.iter().filter(|p| p.alive && p.id != b.id && !allied(me.team, p.team)).collect();
            }
            // wounded targets look closer: cost = distance scaled from 0.6x (nearly dead) to 1.0x (full health)
            let cost = |p: &PlayerView| (p.pos - me.pos).norm() * (0.6 + 0.4 * p.hp_frac);
            let best = pool
                .iter()
                .map(|p| (cost(p), p.id))
                .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let prev = self.sticky.iter().find(|s| s.0 == b.id).map(|s| s.1);
            let prev_cost = prev.and_then(|id| pool.iter().find(|p| p.id == id).map(|p| (cost(p), id)));
            let pick = match (prev_cost, best) {
                (Some((dp, ip)), Some((db, _))) if db >= dp * self.cfg.target_stickiness => Some(ip),
                (_, Some((_, ib))) => Some(ib),
                _ => None,
            };
            if let Some(t) = pick.filter(|t| ids.binary_search(t).is_err()) {
                match on_human.iter_mut().find(|h| h.0 == t) {
                    Some(h) => h.1 += 1,
                    None => on_human.push((t, 1)),
                }
            }
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
            let ctx = ThinkCtx { world: &world, me, assign: assigns[i], has_ticket: false, perches: &self.perches, frustum, nav: self.nav.as_ref(), plan_budget: &budget };
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
                nav: self.nav.as_ref(),
                plan_budget: &budget,
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

/// Path grid for the level: extent from the static colliders (clamped), 1 m cells.
fn build_nav(server: &Server) -> crate::nav::NavGrid {
    use shinobi_sim::combat::GROUP_WORLD;
    let mut half = 20.0f32;
    let (mut top, mut has_floor) = (0.0f32, false);
    for (_, c) in server.world.colliders.iter() {
        if c.is_sensor() || !c.collision_groups().memberships.intersects(GROUP_WORLD) {
            continue;
        }
        let a = c.compute_aabb();
        half = half.max(a.maxs.x.abs()).max(a.mins.x.abs()).max(a.maxs.z.abs()).max(a.mins.z.abs());
        top = top.max(a.maxs.y);
        // a floor slab: top face at y = 0 and wide
        has_floor |= a.maxs.y.abs() < 0.1 && (a.maxs.x - a.mins.x).max(a.maxs.z - a.mins.z) >= 20.0;
    }
    if !has_floor && top > 8.0 {
        // floating buildings over the void: plan on the roof decks, bridges and ledges (and guard their edges)
        return crate::nav::NavGrid::build_sky(server, (half + 2.0).min(200.0), 1.0, top);
    }
    crate::nav::NavGrid::build(server, half.min(200.0), 1.0)
}
