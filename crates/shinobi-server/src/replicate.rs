//! Server -> client replication: per-peer delta baselines, reliable event streams (teams, ropes, blocks, obstacles),
//! and datagram assembly. Pure logic (no sockets) so it is unit-testable.

use std::collections::{HashMap, VecDeque};

use shinobi_proto::bits::BitWriter;
use shinobi_proto::datagram;
use shinobi_proto::gameplay::{self, Board, BoardRow, HitMsg, OwnExtras};
use shinobi_proto::quant::{q_pos3, q_vel};
use shinobi_proto::snapshot::{
    encode_delta, with_hp_frac, EntityState, MoveKind, Snapshot, FLAG_DEAD, FLAG_INVULN, FLAG_ROLL,
};
use shinobi_sim::character::{Cfg, MoveState};
use shinobi_sim::combat::DamageKind;
use shinobi_sim::events::{EventOutbox, NetEvent};
use shinobi_sim::match_state::{EndReason, MatchEvent, MatchState, PhaseKind};
use shinobi_sim::pickups::PickupEvent;
use shinobi_sim::pvp::{HazardSource, PvpEvent, NO_ATTACKER};
use shinobi_sim::tick::{Player, Server};
use shinobi_sim::obstacle::ObstacleStore;
use shinobi_sim::own_state::OwnState;

use crate::obstacles_spawner::ObstacleSpawner;

/// Kept snapshots = how stale an ack may be and still get a delta (32 ticks ~ 0.5 s); older -> full snapshot.
const HISTORY: usize = 32;
/// Reliable events get first call on the datagram; leaves room for 16 entities in a 1200 B MTU.
const EVENT_BUDGET: usize = 480;

#[derive(Default)]
struct PeerState {
    ack_snapshot: Option<u32>,
    outbox: EventOutbox,
    /// Hit markers / damage directions, each sent in `HIT_RESENDS` consecutive datagrams (unreliable: this is
    /// the loss tolerance). `(seq, hit, sends left)`, oldest first, seqs consecutive.
    hits: Vec<(u8, HitMsg, u8)>,
    /// Seq of the next hit pushed (wrapping).
    next_hit_seq: u8,
}

/// Scoreboard cadence: every second, plus immediately on a phase change.
const BOARD_EVERY_TICKS: u32 = 60;
/// Never let a stalled snapshot cadence grow a peer's hit backlog without bound.
const MAX_PENDING_HITS: usize = 32;
/// Datagrams each hit rides in (survives 2 consecutive losses).
const HIT_RESENDS: u8 = 3;

/// Optional world features. The game is a shooter: Tron is a visual style, so by default the server creates NO
/// obstacle waves (explicit opt-in). The light-cycle trail walls are gone from the server.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Features {
    /// Moving neon obstacle waves (`--obstacles`).
    pub obstacles: bool,
}

pub struct Replicator {
    history: VecDeque<Snapshot>,
    peers: HashMap<u16, PeerState>,
    obstacles: ObstacleStore,
    spawner: ObstacleSpawner,
    last_sent: Option<u32>,
    /// (tick, phase) of the last scoreboard that went out.
    last_board: Option<(u32, u8)>,
    seed: u64,
    features: Features,
    /// One line per gameplay event for the server console (kill feed), drained by `take_log`.
    log: Vec<String>,
    /// Last rope anchor (1/512 m) broadcast per roped player: changes become `RopeAttach` / `RopeRelease`.
    ropes: HashMap<u16, [i32; 3]>,
    /// Last team broadcast per player (`TeamAssign`), and the last `TeamScore` sent.
    teams: HashMap<u16, u8>,
    last_team_score: Option<([u16; 2], u8)>,
    /// Shurikens replicated per snapshot right now (shrinks under load, see `build_round`).
    disc_cap: usize,
    /// Stable snapshot entity id of every landed mine / smoke star, keyed by (owner, launcher uid).
    stuck_ids: HashMap<(u16, u16), u16>,
    /// Smallest intent arrival margin per player since the last snapshot round (taken from the intent queues every
    /// frame, reported in the owner block so clients can size their intent lead).
    margins: HashMap<u16, i32>,
    /// Playtest statistics (see `telemetry.rs`); reports come out through `take_telemetry`.
    telemetry: crate::telemetry::Telemetry,
}

impl Replicator {
    pub fn new(_cfg: &Cfg, obstacle_seed: u64) -> Self {
        Self {
            history: VecDeque::new(),
            peers: HashMap::new(),
            obstacles: ObstacleStore::default(),
            spawner: ObstacleSpawner::new(obstacle_seed),
            last_sent: None,
            last_board: None,
            seed: obstacle_seed,
            features: Features::default(),
            log: Vec::new(),
            ropes: HashMap::new(),
            teams: HashMap::new(),
            last_team_score: None,
            disc_cap: PROJECTILE_ID_MAX,
            stuck_ids: HashMap::new(),
            margins: HashMap::new(),
            telemetry: Default::default(),
        }
    }

    pub fn with_features(mut self, features: Features) -> Self {
        self.features = features;
        self
    }

    /// The world was rebuilt (supervisor recovery): put the hazard mirror of live obstacles back from our
    /// authoritative list.
    pub fn rebuild_world_state(&mut self, server: &mut Server, tick: u32) {
        for o in self.obstacles.not_ended_at(tick) {
            server.hazards.obstacles.apply_spawn(*o);
        }
    }

    /// New peer: queues the live world (pickups, obstacles, blocks, teams, ropes) so late joiners see it.
    pub fn add_peer(&mut self, server: &Server, id: u16, now_tick: u32) {
        let mut st = PeerState::default();
        for (pid, pos, _kind) in server.pickups.active() {
            st.outbox.push(NetEvent::PickupSpawned { tick: now_tick, id: pid, pos: q_pos3(pos), kind: 0 });
        }
        for o in self.obstacles.not_ended_at(now_tick) {
            st.outbox.push(NetEvent::ObstacleSpawn(*o));
        }
        // everyone's team (this peer's own included), and tell the others about the newcomer
        let mut changed = Vec::new();
        for p in server.players.iter().filter(|p| p.team < shinobi_sim::team::TEAM_COUNT) {
            st.outbox.push(NetEvent::TeamAssign { player: p.id, team: p.team });
            if self.teams.insert(p.id, p.team) != Some(p.team) {
                changed.push(NetEvent::TeamAssign { player: p.id, team: p.team });
            }
        }
        if let Some((score, result)) = self.team_score_now(server) {
            st.outbox.push(NetEvent::TeamScore { tick: now_tick, score, result });
        }
        for ev in changed {
            self.broadcast(ev);
        }
        st.outbox.push(NetEvent::BlockRules(server.blocks.cfg.rules()));
        let ms = &server.match_state;
        st.outbox.push(NetEvent::MatchRules {
            frag_limit: ms.cfg.frag_limit,
            team_frag_limit: ms.team_frag_limit,
            time_limit_secs: (ms.cfg.time_limit_ticks / shinobi_proto::TICK_HZ).min(u16::MAX as u32) as u16,
            respawn_ticks: server.balance.player.respawn_ticks.min(u16::MAX as u32) as u16,
        });
        // every live block (sorted by cell: deterministic); the byte-budgeted outbox drains it over a few datagrams
        for b in server.blocks.pieces() {
            st.outbox.push(NetEvent::BlockPlaced { owner: b.owner, cell: b.cell, kind: b.piece.kind.id(), axis: b.piece.axis, scale_q: b.scale_q });
            let hp_q = server.blocks.hp_q(b.cell);
            if hp_q < 255 {
                st.outbox.push(NetEvent::BlockDamaged { cell: b.cell, hp_q }); // hurt before this peer joined
            }
        }
        // ropes already hooked when this peer joined (sorted: deterministic)
        let mut roped: Vec<_> = self.ropes.iter().map(|(p, a)| (*p, *a)).collect();
        roped.sort();
        for (player, anchor) in roped {
            st.outbox.push(NetEvent::RopeAttach { tick: now_tick, player, anchor });
        }
        self.peers.insert(id, st);
    }

    pub fn remove_peer(&mut self, _server: &mut Server, id: u16, now_tick: u32) {
        if self.teams.remove(&id).is_some() {
            self.broadcast(NetEvent::TeamAssign { player: id, team: shinobi_sim::events::TEAM_NONE });
        }
        if self.ropes.remove(&id).is_some() {
            self.broadcast(NetEvent::RopeRelease { tick: now_tick, player: id });
        }
        self.peers.remove(&id);
    }

    /// Client-claimed acks are only ever used as lookup keys / upper-bounded pops, never trusted for state.
    /// `ack_snapshot_tick == 0` means "no snapshot received yet" (tick 0 is never a delta baseline), so a
    /// client that has not decoded anything keeps getting full snapshots.
    pub fn on_client_ack(&mut self, id: u16, ack_snapshot_tick: u32, ack_event_seq: u32) {
        let Some(p) = self.peers.get_mut(&id) else { return };
        if ack_snapshot_tick != 0
            && self.history.iter().any(|s| s.tick == ack_snapshot_tick) && Some(ack_snapshot_tick) > p.ack_snapshot {
            p.ack_snapshot = Some(ack_snapshot_tick);
        }
        p.outbox.ack(ack_event_seq);
    }

    fn broadcast(&mut self, ev: NetEvent) {
        for p in self.peers.values_mut() {
            p.outbox.push(ev);
        }
    }

    /// Everything that must happen once per server frame, after `Server::frame`: event streams (game, ropes,
    /// teams, blocks) and obstacle waves.
    pub fn post_frame(&mut self, server: &mut Server) {
        let Some(tick) = server.tick.checked_sub(1) else { return };
        // telemetry reads the event vectors first: the drain below takes them
        self.telemetry.observe(server);
        // The sim clears these event vecs at the start of its next `frame()`: drain now or lose them.
        self.drain_game_events(server, tick);
        self.sync_ropes(server, tick);
        self.sync_teams(server, tick);
        self.sync_blocks(server);
        for p in &mut server.players {
            if let Some(m) = p.queue.take_margin() {
                self.telemetry.observe_margin(p.id, m);
                let e = self.margins.entry(p.id).or_insert(m);
                *e = (*e).min(m);
            }
        }
        if self.features.obstacles {
            for d in self.spawner.poll(tick) {
                self.obstacles.apply_spawn(d);
                server.hazards.obstacles.apply_spawn(d);
                self.broadcast(NetEvent::ObstacleSpawn(d));
            }
            self.obstacles.prune(tick);
            server.hazards.obstacles.prune(tick);
        }
    }

    /// Star variant events of the last frame -> reliable `StarFx` / `StarBlind` (effects and sounds for everyone).
    fn sync_stars(&mut self, server: &Server, tick: u32) {
        use shinobi_sim::combat::stars::StarKind;
        use shinobi_sim::events::*;
        for (owner, e) in &server.events {
            let (what, pos) = match *e {
                CombatEvent::StarStuck { kind: StarKind::Mine, pos, .. } => (STAR_FX_MINE_STUCK, pos),
                CombatEvent::StarStuck { kind: StarKind::Smoke, pos, .. } => (STAR_FX_SMOKE_LANDED, pos),
                CombatEvent::StarBurst { pos } => (STAR_FX_MINE_BURST, pos),
                CombatEvent::StarFlash { pos } => (STAR_FX_FLASH_BURST, pos),
                CombatEvent::StarGone { kind: StarKind::Mine, pos } => (STAR_FX_MINE_GONE, pos),
                CombatEvent::StarGone { kind: StarKind::Smoke, pos } => (STAR_FX_SMOKE_GONE, pos),
                CombatEvent::StarBlind { victim, strength_q, ticks } => {
                    self.broadcast(NetEvent::StarBlind { tick, victim, strength_q, ticks });
                    continue;
                }
                _ => continue,
            };
            self.broadcast(NetEvent::StarFx { tick, what, owner: *owner, pos: q_pos3([pos.x, pos.y, pos.z]) });
        }
    }

    /// `Server::block_events` of the last frame -> reliable events (placed / removed to everyone, refusals to the
    /// builder only). The sim clears the vec at the start of its next frame, so this runs every `post_frame`.
    fn sync_blocks(&mut self, server: &mut Server) {
        use shinobi_sim::blocks::{BlockEvent, PlaceError};
        use shinobi_sim::events::{BLOCK_REFUSED_BLOCKED, BLOCK_REFUSED_COOLDOWN, BLOCK_REFUSED_OCCUPIED};
        // hits on one piece can come many times per frame (a volley of shurikens): only the latest health per cell is sent
        let mut hurt: Vec<([i32; 3], u8)> = Vec::new();
        for ev in std::mem::take(&mut server.block_events) {
            match ev {
                BlockEvent::Placed { owner, cell, piece, scale_q, .. } => {
                    hurt.retain(|h| h.0 != cell);
                    self.broadcast(NetEvent::BlockPlaced { owner, cell, kind: piece.kind.id(), axis: piece.axis, scale_q })
                }
                BlockEvent::Removed { cell, .. } => {
                    hurt.retain(|h| h.0 != cell);
                    self.broadcast(NetEvent::BlockRemoved { cell })
                }
                BlockEvent::DeRez { cell, .. } => {
                    hurt.retain(|h| h.0 != cell);
                    self.broadcast(NetEvent::BlockDerez { cell })
                }
                BlockEvent::Damaged { cell, hp_q, .. } => match hurt.iter_mut().find(|h| h.0 == cell) {
                    Some(h) => h.1 = h.1.min(hp_q),
                    None => hurt.push((cell, hp_q)),
                },
                BlockEvent::Refused { owner, why, .. } => {
                    let why = match why {
                        PlaceError::Occupied => BLOCK_REFUSED_OCCUPIED,
                        PlaceError::Blocked => BLOCK_REFUSED_BLOCKED,
                        PlaceError::Cooldown => BLOCK_REFUSED_COOLDOWN,
                    };
                    if let Some(p) = self.peers.get_mut(&owner) {
                        p.outbox.push(NetEvent::BlockRefused { why });
                    }
                }
            }
        }
        for (cell, hp_q) in hurt {
            self.broadcast(NetEvent::BlockDamaged { cell, hp_q });
        }
    }

    /// Team membership changes (join, auto-balance, `set_team`, leave) and team-score changes -> reliable events.
    fn sync_teams(&mut self, server: &Server, tick: u32) {
        // only real teams go on the wire (free-for-all players have none)
        let on_team = |p: &&Player| p.team < shinobi_sim::team::TEAM_COUNT;
        for p in server.players.iter().filter(on_team) {
            if self.teams.insert(p.id, p.team) != Some(p.team) {
                self.broadcast(NetEvent::TeamAssign { player: p.id, team: p.team });
            }
        }
        let gone: Vec<u16> = self.teams.keys().copied().filter(|id| !server.players.iter().filter(on_team).any(|p| p.id == *id)).collect();
        for id in gone {
            self.teams.remove(&id);
            self.broadcast(NetEvent::TeamAssign { player: id, team: shinobi_sim::events::TEAM_NONE });
        }
        let now = self.team_score_now(server);
        if now != self.last_team_score {
            self.last_team_score = now;
            if let Some((score, result)) = now {
                self.broadcast(NetEvent::TeamScore { tick, score, result });
            }
        }
    }

    /// `(scores, result)` while the server runs team deathmatch.
    fn team_score_now(&self, server: &Server) -> Option<([u16; 2], u8)> {
        use shinobi_sim::events::{TEAM_RESULT_CYAN, TEAM_RESULT_DRAW, TEAM_RESULT_ORANGE, TEAM_RESULT_RUNNING};
        let ms = &server.match_state;
        if !ms.team_mode {
            return None;
        }
        let result = if ms.in_sudden_death() {
            shinobi_sim::events::TEAM_RESULT_SUDDEN
        } else if ms.kind() == PhaseKind::Intermission {
            match ms.last_winner_team {
                Some(0) => TEAM_RESULT_CYAN,
                Some(_) => TEAM_RESULT_ORANGE,
                None => TEAM_RESULT_DRAW,
            }
        } else {
            TEAM_RESULT_RUNNING
        };
        Some((ms.team_scores(&server.players), result))
    }

    /// Rope attach / release edges -> reliable events. Other players' ropes are rare (a few per minute per player,
    /// ~7 B each), so reliable events are cheaper than a per-snapshot flag and survive loss. Dead players have no rope.
    fn sync_ropes(&mut self, server: &Server, tick: u32) {
        for p in &server.players {
            let now = p.ch.grapple.filter(|_| p.vitals.alive()).map(|g| q_pos3([g.anchor.x, g.anchor.y, g.anchor.z]));
            match (now, self.ropes.get(&p.id).copied()) {
                (Some(a), prev) if prev != Some(a) => {
                    self.ropes.insert(p.id, a);
                    self.broadcast(NetEvent::RopeAttach { tick, player: p.id, anchor: a });
                }
                (None, Some(_)) => {
                    self.ropes.remove(&p.id);
                    self.broadcast(NetEvent::RopeRelease { tick, player: p.id });
                }
                _ => {}
            }
        }
    }

    /// Playtest telemetry produced since the last call: (console lines, JSON lines for the telemetry file).
    pub fn take_telemetry(&mut self) -> (Vec<String>, Vec<String>) {
        self.telemetry.take()
    }

    /// Console lines produced since the last call.
    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    fn log_line(&mut self, tick: u32, text: String) {
        self.log.push(format!("[{:7.2}s] {text}", tick as f32 / shinobi_proto::TICK_HZ as f32));
        let extra = self.log.len().saturating_sub(256);
        self.log.drain(..extra); // never grow unbounded if the console is not drained
    }

    fn push_hit(&mut self, player: u16, hit: HitMsg) {
        if let Some(p) = self.peers.get_mut(&player) {
            p.hits.push((p.next_hit_seq, hit, HIT_RESENDS));
            p.next_hit_seq = p.next_hit_seq.wrapping_add(1);
            let extra = p.hits.len().saturating_sub(MAX_PENDING_HITS);
            p.hits.drain(..extra);
        }
    }

    /// pvp / match / pickup events -> reliable `NetEvent`s (low rate) and owner-only hit markers (high rate).
    /// Order matters: match events (incl. `MapChange`) go before pickup events so the client clears first.
    fn drain_game_events(&mut self, server: &mut Server, tick: u32) {
        let pvp = std::mem::take(&mut server.pvp_events);
        let matches = std::mem::take(&mut server.match_events);
        let pickups = std::mem::take(&mut server.pickup_events);
        let who = |id: u16| if id == NO_ATTACKER { 0 } else { id };
        self.sync_stars(server, tick);
        for ev in pvp {
            match ev {
                PvpEvent::Hit { attacker, victim, amount, kind, head, bonus, .. } => {
                    let (amount, kind) = (amount.round().clamp(0.0, 255.0) as u8, weapon_code(kind));
                    self.push_hit(attacker, HitMsg { other: victim, amount, kind, taken: false, kill: false, head, bonus });
                    self.push_hit(victim, HitMsg { other: who(attacker), amount, kind, taken: true, kill: false, head, bonus });
                }
                PvpEvent::Kill { tick, killer, victim, kind, head } => {
                    let kind = weapon_code(kind);
                    self.log_line(tick, format!("KILL    p{killer} {} p{victim}", kind_verb(kind)));
                    self.broadcast(NetEvent::Kill { tick, killer: who(killer), victim, kind });
                    self.push_hit(killer, HitMsg { other: victim, amount: 0, kind, taken: false, kill: true, head, bonus: false });
                    self.push_hit(victim, HitMsg { other: who(killer), amount: 0, kind, taken: true, kill: true, head, bonus: false });
                }
                PvpEvent::HazardHit { victim, amount, source, .. } => {
                    let (kind, owner) = hazard_code(source);
                    let amount = amount.round().clamp(0.0, 255.0) as u8;
                    self.push_hit(victim, HitMsg { other: owner, amount, kind, taken: true, kill: false, head: false, bonus: false });
                }
                PvpEvent::HazardKill { tick, victim, source, killer } => {
                    let (kind, _) = hazard_code(source);
                    let by = killer.map_or(String::new(), |k| match source {
                        // a ring-out or a wall-splat: someone put them there
                        HazardSource::Obstacle(id) if id == shinobi_sim::pvp::VOID_SOURCE_ID || id == shinobi_sim::melee::SPLAT_SOURCE_ID => format!(" (knocked by p{k})"),
                        _ => format!(" (p{k}'s trail)"),
                    });
                    // the sim's kill plane reports as obstacle id 65535 (`pvp::VOID_SOURCE_ID`)
                    let verb = if matches!(source, HazardSource::Obstacle(shinobi_sim::pvp::VOID_SOURCE_ID)) {
                        "fell out of the world"
                    } else if matches!(source, HazardSource::Obstacle(shinobi_sim::melee::SPLAT_SOURCE_ID)) {
                        "was splatted"
                    } else {
                        kind_verb(kind)
                    };
                    self.log_line(tick, format!("KILL    p{victim} {verb}{by}"));
                    self.broadcast(NetEvent::Kill { tick, killer: killer.unwrap_or(0), victim, kind });
                    self.push_hit(victim, HitMsg { other: killer.unwrap_or(0), amount: 0, kind, taken: true, kill: true, head: false, bonus: false });
                    if let Some(k) = killer {
                        self.push_hit(k, HitMsg { other: victim, amount: 0, kind, taken: false, kill: true, head: false, bonus: false });
                    }
                }
                PvpEvent::Respawn { tick, player } => {
                    self.log_line(tick, format!("RESPAWN p{player}"));
                    self.broadcast(NetEvent::Respawn { tick, player })
                }
                PvpEvent::EnemyHazardHit { .. } => {} // NPCs are not replicated yet
            }
        }
        for ev in matches {
            match ev {
                MatchEvent::Countdown { tick, phase, seconds } => {
                    self.log_line(tick, format!("MATCH   {} in {seconds}s", if phase == PhaseKind::Intermission { "next map" } else { "starting" }));
                    self.broadcast(NetEvent::MatchCountdown { tick, phase: phase_code(phase), seconds: seconds.min(255) as u8 })
                }
                MatchEvent::MatchStart { tick, map } => {
                    self.log_line(tick, format!("MATCH   START (map {map})"));
                    self.broadcast(NetEvent::MatchStart { tick, map })
                }
                MatchEvent::MatchEnd { tick, winner, reason } => {
                    let why = if matches!(reason, EndReason::TimeLimit) { "time limit" } else { "frag limit" };
                    self.log_line(tick, format!("MATCH   END ({why}) winner: {}", winner.map_or("draw".to_string(), |w| format!("p{w}"))));
                    self.broadcast(NetEvent::MatchEnd {
                        tick,
                        winner: winner.unwrap_or(0),
                        reason: matches!(reason, EndReason::TimeLimit) as u8,
                    })
                }
                MatchEvent::NextMap { tick, map } => self.map_change(server, tick, map),
            }
        }
        for ev in pickups {
            match ev {
                PickupEvent::Spawned { tick, id, pos, .. } => {
                    self.broadcast(NetEvent::PickupSpawned { tick, id, pos: q_pos3(pos), kind: 0 })
                }
                PickupEvent::Taken { tick, id, player, .. } => {
                    self.log_line(tick, format!("PICKUP  p{player} took cube #{id}"));
                    self.broadcast(NetEvent::PickupTaken { tick, id, player })
                }
            }
        }
        let _ = tick;
    }

    /// Host job on `MatchEvent::NextMap`: forget every obstacle (hazard mirror),
    /// restart the obstacle cadence, and tell clients to clear theirs. The level geometry itself comes from
    /// `Setup`; with a single level there is nothing to reload (a multi-map host would swap `server.world` statics here).
    fn map_change(&mut self, server: &mut Server, tick: u32, map: u32) {
        self.obstacles = ObstacleStore::default();
        server.hazards.obstacles = ObstacleStore::default();
        self.spawner.restart(tick, self.seed ^ (map as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        self.log_line(tick, format!("MATCH   map change -> {map} (obstacles cleared)"));
        self.broadcast(NetEvent::MapChange { tick, map });
    }

    /// Landed mines / smoke as snapshot entities with ids that stay with the object (see `STUCK_ID_BASE`), sorted by id.
    fn stuck_entities(&mut self, server: &Server) -> Vec<(u16, EntityState)> {
        use shinobi_proto::snapshot::{disc_flags, STUCK_ID_BASE, STUCK_NORMAL_SPEED};
        let sc = server.balance.combat.star;
        let mut seen: Vec<(u16, u16)> = Vec::new();
        let mut out = Vec::new();
        for (owner, d) in server.projectiles().into_iter().filter(|(_, d)| d.stuck) {
            let key = (owner, d.uid);
            seen.push(key);
            let id = match self.stuck_ids.get(&key) {
                Some(id) => *id,
                None => {
                    let used: std::collections::HashSet<u16> = self.stuck_ids.values().copied().collect();
                    let id = (STUCK_ID_BASE..STUCK_ID_BASE + 0x400).find(|i| !used.contains(i)).unwrap_or(STUCK_ID_BASE);
                    self.stuck_ids.insert(key, id);
                    id
                }
            };
            let n = d.normal * STUCK_NORMAL_SPEED;
            out.push((
                id,
                EntityState {
                    pos: q_pos3(d.pos.into()),
                    vel: [q_vel(n.x), q_vel(n.y), q_vel(n.z)],
                    yaw: owner,
                    kind: MoveKind::Airborne,
                    flags: disc_flags(0, d.kind as u8, true, d.armed(&sc)),
                    fx: (d.life_left(&sc) * 15.0).round() as u8,
                },
            ));
        }
        self.stuck_ids.retain(|k, _| seen.contains(k));
        out.sort_by_key(|e| e.0);
        out.dedup_by_key(|e| e.0);
        out
    }

    /// True when a snapshot round is due (every `divisor` ticks, robust to multi-tick frames).
    pub fn due(&self, server_tick: u32, divisor: u32) -> bool {
        match (server_tick.checked_sub(1), self.last_sent) {
            (Some(t), Some(last)) => t.wrapping_sub(last) >= divisor,
            (Some(_), None) => true,
            _ => false,
        }
    }

    /// Builds one datagram per peer. Returns `(player_id, bytes)`.
    pub fn build_round(&mut self, server: &Server) -> Vec<(u16, Vec<u8>)> {
        let Some(tick) = server.tick.checked_sub(1) else { return Vec::new() };
        // Fit the snapshot into its byte budget: when a firefight puts too many discs on screen, every peer gets
        // the same smaller disc set (one shared snapshot / baseline history), and the cap recovers when it calms.
        let mut cap = self.disc_cap;
        let stuck = self.stuck_entities(server);
        let (snap, mut encoded, worst) = loop {
            let snap = build_snapshot(tick, server, cap, &stuck);
            let encoded: HashMap<u16, Vec<u8>> = self
                .peers
                .iter()
                .map(|(id, peer)| {
                    let base = peer.ack_snapshot.and_then(|t| self.history.iter().find(|s| s.tick == t));
                    (*id, encode_delta(base, &snap))
                })
                .collect();
            let worst = encoded.values().map(Vec::len).max().unwrap_or(0);
            if worst <= SNAPSHOT_BUDGET || cap <= MIN_SNAPSHOT_DISCS {
                break (snap, encoded, worst);
            }
            cap = (cap * 3 / 4).max(MIN_SNAPSHOT_DISCS);
        };
        self.disc_cap = if worst < SNAPSHOT_BUDGET * 7 / 10 { (cap + 8).min(PROJECTILE_ID_MAX) } else { cap };
        self.last_sent = Some(tick);
        let phase = phase_code(server.match_state.kind());
        let board_due = self.last_board.map_or(true, |(t, ph)| tick.wrapping_sub(t) >= BOARD_EVERY_TICKS || ph != phase);
        let board = if board_due {
            self.last_board = Some((tick, phase));
            board_bytes(server, tick)
        } else {
            Vec::new()
        };

        let mut out = Vec::with_capacity(self.peers.len());
        for (id, peer) in self.peers.iter_mut() {
            let snap_bytes = encoded.remove(id).unwrap_or_default();
            let mut w = BitWriter::new();
            peer.outbox.write_pending(&mut w, EVENT_BUDGET);
            let send: Vec<HitMsg> = peer.hits.iter().map(|h| h.1).collect();
            let hit_seq = peer.hits.last().map_or(0, |h| h.0);
            for h in &mut peer.hits {
                h.2 -= 1;
            }
            peer.hits.retain(|h| h.2 > 0);
            let margin = self.margins.remove(id).map_or(shinobi_proto::gameplay::LEAD_MARGIN_MAX as i32, |m| m);
            let own = own_block(server, *id, &send, hit_seq, margin);
            out.push((*id, datagram::frame_snapshot_v3(&w.finish(), &own, &board, &snap_bytes)));
        }
        self.history.push_back(snap);
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
        out
    }
}

fn move_kind(p: &Player) -> (MoveKind, u8) {
    if let Some(is_roll) = p.ch.evading() {
        let flags = if is_roll { FLAG_ROLL } else { 0 } | if p.ch.invulnerable() { FLAG_INVULN } else { 0 };
        return (MoveKind::Evading, flags);
    }
    let k = match p.ch.state {
        MoveState::Grounded => MoveKind::Grounded,
        MoveState::Airborne => MoveKind::Airborne,
        MoveState::WallRun { .. } => MoveKind::WallRun,
    };
    (k, 0)
}

/// Owner-only block (`gameplay::OwnExtras`), built from the SAME post-step world as the snapshot:
/// movement state for reconciliation when the player is alive and has executed an intent, exact health, and
/// the hit markers accumulated since the last datagram.
fn own_block(server: &Server, id: u16, hits: &[HitMsg], hit_seq: u8, margin: i32) -> Vec<u8> {
    let Some(p) = server.players.iter().find(|p| p.id == id) else { return Vec::new() };
    let acked = p.queue.last_executed_tick().filter(|_| p.vitals.alive());
    let star = p.combat.shuriken_energy();
    let extras = OwnExtras {
        star_max: star.map_or(0, |s| s.1.round().clamp(1.0, 15.0) as u8),
        star_energy: star.map_or(0, |s| (s.0 * 16.0).round().clamp(0.0, 255.0) as u8),
        star_locked: star.map_or(false, |s| s.2),
        has_state: acked.is_some(),
        acked_tick: acked.unwrap_or(0),
        hp_x10: if p.vitals.alive() { (p.vitals.hp * 10.0).round().clamp(1.0, u16::MAX as f32) as u16 } else { 0 },
        hits: hits.to_vec(),
        hit_seq,
        echo_seq: p.last_pkt.map_or(0, |(s, _)| s),
        echo_age: p.last_pkt.map_or(0, |(_, t)| server.tick.saturating_sub(t).min(255) as u8),
        intent_complete: p.queue.complete_tick(),
        lead_margin: margin.clamp(shinobi_proto::gameplay::LEAD_MARGIN_MIN as i32, shinobi_proto::gameplay::LEAD_MARGIN_MAX as i32 - 1) as i8,
    };
    let mut w = BitWriter::new();
    extras.write(&mut w, |w| OwnState::from_snapshot(&p.ch.snapshot(&server.world)).write(w));
    w.finish()
}

fn weapon_code(k: DamageKind) -> u8 {
    match k {
        DamageKind::Shuriken => gameplay::KIND_SHURIKEN,
        DamageKind::Slash => gameplay::KIND_SLASH,
        DamageKind::GridBeam => gameplay::KIND_GRID_BEAM,
        DamageKind::Mine => gameplay::KIND_MINE,
    }
}

/// (kind code, owner id to show as `other`): obstacles have no owner, walls name theirs.
fn hazard_code(s: HazardSource) -> (u8, u16) {
    match s {
        HazardSource::Obstacle(_) => (gameplay::KIND_OBSTACLE, 0),
        HazardSource::Wall { owner, .. } => (gameplay::KIND_WALL, owner as u16),
    }
}

fn kind_verb(kind: u8) -> &'static str {
    match kind {
        gameplay::KIND_SHURIKEN => "shuriken'd",
        gameplay::KIND_SLASH => "sliced",
        gameplay::KIND_GRID_BEAM => "grid-beamed",
        gameplay::KIND_MINE => "stepped on a mine star",
        gameplay::KIND_OBSTACLE => "hit a neon obstacle",
        _ => "ran into a light wall",
    }
}

fn phase_code(k: PhaseKind) -> u8 {
    match k {
        PhaseKind::Warmup => gameplay::PHASE_WARMUP,
        PhaseKind::Live => gameplay::PHASE_LIVE,
        PhaseKind::Intermission => gameplay::PHASE_INTERMISSION,
    }
}

fn board_bytes(server: &Server, tick: u32) -> Vec<u8> {
    let ms = &server.match_state;
    let rows = MatchState::scoreboard(&server.players)
        .into_iter()
        .take(gameplay::MAX_BOARD_ROWS)
        .map(|e| BoardRow { id: e.id as u8, kills: e.kills, deaths: e.deaths, alive: e.alive })
        .collect();
    Board {
        phase: phase_code(ms.kind()),
        map: ms.map_index,
        ticks_left: ms.ticks_left(tick),
        winner: ms.last_winner.unwrap_or(0),
        rows,
    }
    .encode()
}

pub use shinobi_proto::snapshot::PROJECTILE_ID_BASE;
/// Discs replicated per snapshot (ids `PROJECTILE_ID_BASE..+cap`): ~6 B each as a delta, so 128 discs + 16 players
/// stay under one 1200 B datagram. Realistic 8v8 peaks around 20; beyond the cap the extra discs are simply not drawn
/// by clients (they still hit on the server).
const PROJECTILE_ID_MAX: usize = 128;
/// Per-peer snapshot bytes before the disc cap shrinks; leaves ~500 B of a 1200 B datagram for events, the owner
/// block and the scoreboard.
const SNAPSHOT_BUDGET: usize = 700;
const MIN_SNAPSHOT_DISCS: usize = 16;

fn build_snapshot(tick: u32, server: &Server, disc_cap: usize, stuck: &[(u16, EntityState)]) -> Snapshot {
    let mut entities: Vec<_> = server
        .players
        .iter()
        .map(|p| {
            let pos = p.pos(&server.world);
            let v = p.ch.vel;
            let (kind, mut flags) = move_kind(p);
            if !p.vitals.alive() {
                flags |= FLAG_DEAD;
            }
            if p.vitals.spawn_protected(tick) {
                flags |= FLAG_INVULN;
            }
            let flags = with_hp_frac(flags, p.vitals.hp_frac());
            (
                p.id,
                EntityState {
                    pos: q_pos3(pos),
                    vel: [q_vel(v.x), q_vel(v.y), q_vel(v.z)],
                    yaw: p.yaw,
                    kind,
                    flags,
                    fx: p.blk.fx_bits(tick) | if p.melee.marked(tick) { shinobi_proto::snapshot::FX_MARK } else { 0 },
                },
            )
        })
        .collect();
    entities.sort_by_key(|e| e.0);
    // shurikens: ids PROJECTILE_ID_BASE.., yaw = owner id, flags = bounce count, vel = dir * speed
    let speed = server.balance.shuriken_cfg().speed;
    for (i, (owner, d)) in server.projectiles().into_iter().filter(|(_, d)| !d.stuck).take(disc_cap).enumerate() {
        let v = d.dir * speed;
        entities.push((
            PROJECTILE_ID_BASE + i as u16,
            EntityState {
                pos: q_pos3(d.pos.into()),
                vel: [q_vel(v.x), q_vel(v.y), q_vel(v.z)],
                yaw: owner,
                kind: MoveKind::Airborne,
                flags: shinobi_proto::snapshot::disc_flags(d.bounces, d.kind as u8, false, false),
                fx: 0,
            },
        ));
    }
    entities.extend_from_slice(stuck);
    Snapshot { tick, entities }
}

/// Test helper: apply the event section of a datagram the way a client does.
#[cfg(test)]
fn read_events(
    events: &[u8],
    inbox: &mut shinobi_sim::events::EventInbox,
    apply: impl FnMut(NetEvent),
) -> shinobi_proto::bits::DecodeResult<()> {
    inbox.read(&mut shinobi_proto::bits::BitReader::new(events), apply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shinobi_proto::snapshot::decode_delta;
    use shinobi_sim::character::Cfg;
    use shinobi_sim::events::EventInbox;
    use std::time::Duration;

    fn world() -> Server {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [200.0, 0.5, 200.0]);
        s.add_player(1, [0.0, 0.81, 0.0]);
        s.add_player(2, [5.0, 0.81, 0.0]);
        s
    }

    #[test]
    fn snapshot_bandwidth_16_players_with_discs() {
        use shinobi_proto::snapshot::{encode_delta, Snapshot};
        let mk = |tick: u32, shift: i32| {
            let mut entities: Vec<(u16, EntityState)> = (1..=16u16)
                .map(|id| {
                    (id, EntityState { pos: [id as i32 * 4096 + shift * 8, 512, 0], vel: [640, 0, 0], yaw: id * 100, ..Default::default() })
                })
                .collect();
            for i in 0..96u16 {
                // 6 discs per player in flight at 40 m/s
                entities.push((
                    PROJECTILE_ID_BASE + i,
                    EntityState { pos: [i as i32 * 700 + shift * 341, 512, i as i32 * 90], vel: [2560, 0, 0], yaw: 1 + i / 6, flags: (i % 3) as u8, ..Default::default() },
                ));
            }
            Snapshot { tick, entities }
        };
        let base = mk(100, 0);
        let full = encode_delta(None, &base).len();
        let delta = encode_delta(Some(&base), &mk(103, 3)).len();
        eprintln!("16 players + 96 discs: full {full} B, delta(3 ticks) {delta} B");
        assert!(delta < 1200 && full < 1200 * 2, "delta snapshot {delta} B too big for MTU budget");
    }

    /// 8v8 with real bots fighting (discs in flight), ropes forced on, every player a peer on a perfect link.
    /// Steady-state datagrams must fit one MTU; prints size / cost numbers (`cargo test eight_v_eight -- --nocapture`).
    #[test]
    fn eight_v_eight_snapshot_bandwidth_and_step_cost() {
        use crate::config::BotSkill;
        use crate::setup::Setup;
        use crate::supervise::{make_bots, BotPlan};
        use shinobi_sim::character::GrappleState;
        use shinobi_sim::events::EventInbox;
        let cfg = Cfg::default();
        let mut s = Setup::standard().build(cfg);
        s.enable_teams(50);
        let plan = BotPlan { count: 0, skill: BotSkill::Hard, seed: 11, team_size: 8 };
        let mut bots = make_bots(&plan, &mut s);
        assert_eq!(s.players.len(), 16);
        let mut r = Replicator::new(&cfg, 1);
        let ids: Vec<u16> = s.players.iter().map(|p| p.id).collect();
        for id in &ids {
            r.add_peer(&s, *id, s.tick);
        }
        let mut inboxes: HashMap<u16, EventInbox> = ids.iter().map(|id| (*id, EventInbox::default())).collect();
        let (mut max_len, mut sum, mut n, mut max_discs, mut full_max) = (0usize, 0usize, 0usize, 0usize, 0usize);
        let mut step_ns = 0u128;
        const TICKS: u32 = 900;
        for t in 0..TICKS {
            // four players keep a rope up for 1 s out of every 3 s
            for (k, p) in s.players.iter_mut().take(4).enumerate() {
                let up = (t + k as u32 * 40) % 180 < 60;
                p.ch.grapple = up.then(|| GrappleState {
                    anchor: shinobi_sim::rapier3d::prelude::Vector::new(5.0 + k as f32, 8.0, -4.0),
                    len: 10.0,
                    time: 0.0,
                });
            }
            let t0 = std::time::Instant::now();
            bots.advance(&mut s, Duration::from_nanos(16_666_667));
            r.post_frame(&mut s);
            step_ns += t0.elapsed().as_nanos();
            max_discs = max_discs.max(s.projectiles().len());
            if r.due(s.tick, shinobi_proto::SNAPSHOT_DIVISOR) {
                let tick = s.tick - 1;
                for (id, bytes) in r.build_round(&s) {
                    let (events, ..) = datagram::split_snapshot_v3(&bytes).unwrap();
                    let inbox = inboxes.get_mut(&id).unwrap();
                    read_events(events, inbox, |_| {}).unwrap();
                    let ack = inbox.ack();
                    r.on_client_ack(id, tick, ack);
                    if t < 60 {
                        full_max = full_max.max(bytes.len());
                    } else {
                        max_len = max_len.max(bytes.len());
                        sum += bytes.len();
                        n += 1;
                    }
                }
            }
        }
        let ms = step_ns as f64 / TICKS as f64 / 1e6;
        eprintln!(
            "8v8: steady datagram max {max_len} B, mean {} B over {n}; first-second max {full_max} B; most discs in flight {max_discs}; sim+replicate {ms:.2} ms/tick",
            sum / n.max(1)
        );
        assert!(max_len <= 1200, "steady-state 8v8 datagram {max_len} B exceeds one MTU");
        assert!(ms < 16.0, "{ms:.2} ms per tick leaves no headroom at 60 Hz");
    }

    #[test]
    fn piece_damage_is_coalesced_per_cell_dropped_on_derez_and_replayed_to_joiners() {
        use shinobi_sim::blocks::Piece;
        let (mut s, mut r) = two_peers();
        let damaged_for = |r: &mut Replicator, s: &Server, id: u16| {
            let round = r.build_round(s);
            let mut inbox = shinobi_sim::events::EventInbox::default();
            let mut got = Vec::new();
            read_events(datagram_for(&round, id).0, &mut inbox, |e| got.push(e)).unwrap();
            got.into_iter().filter(|e| matches!(e, NetEvent::BlockDamaged { .. } | NetEvent::BlockDerez { .. })).collect::<Vec<_>>()
        };
        let tick = s.tick;
        for cell in [[3, 0, 0], [4, 0, 0]] {
            s.blocks.place(&mut s.world, 1, cell, Piece::FLOOR, 16, tick, &[]).unwrap();
        }
        s.blocks.cfg.hp = 100.0;
        // a volley of 4 hits on one piece in one frame -> one event with the lowest health
        let mut ev = Vec::new();
        for _ in 0..4 {
            s.blocks.damage(&mut s.world, [3, 0, 0], 12.0, tick, &mut ev);
        }
        // a piece that is hit and then de-rezzed in the same frame only reports the de-rez
        s.blocks.damage(&mut s.world, [4, 0, 0], 50.0, tick, &mut ev);
        assert!(s.blocks.damage(&mut s.world, [4, 0, 0], 60.0, tick, &mut ev));
        s.block_events.extend(ev);
        r.post_frame(&mut s);
        let got = damaged_for(&mut r, &s, 2);
        let hp = s.blocks.hp_q([3, 0, 0]);
        assert!(hp < 255 && hp > 100, "52 of 100 hp left -> {hp}");
        assert_eq!(got, vec![NetEvent::BlockDerez { cell: [4, 0, 0] }, NetEvent::BlockDamaged { cell: [3, 0, 0], hp_q: hp }], "{got:?}");
        // a joiner learns the piece was already hurt
        s.add_player(3, [9.0, 0.81, 0.0]);
        r.add_peer(&s, 3, s.tick);
        assert!(damaged_for(&mut r, &s, 3).contains(&NetEvent::BlockDamaged { cell: [3, 0, 0], hp_q: hp }));
    }

    #[test]
    fn blocks_are_broadcast_refusals_go_to_the_builder_and_joiners_get_the_world() {
        use shinobi_sim::blocks::{BlockEvent, PlaceError};
        use shinobi_sim::events::BLOCK_REFUSED_COOLDOWN;
        let (mut s, mut r) = two_peers();
        let blocks_for = |r: &mut Replicator, s: &Server, id: u16| {
            let round = r.build_round(s);
            let mut inbox = shinobi_sim::events::EventInbox::default();
            let mut got = Vec::new();
            read_events(datagram_for(&round, id).0, &mut inbox, |e| got.push(e)).unwrap();
            got.into_iter()
                .filter(|e| matches!(e, NetEvent::BlockPlaced { .. } | NetEvent::BlockRemoved { .. } | NetEvent::BlockRefused { .. }))
                .collect::<Vec<_>>()
        };
        let tick = s.tick;
        s.blocks.place(&mut s.world, 1, [2, 0, 0], shinobi_sim::blocks::Piece::WALL_Z, 16, 0, &[]).unwrap();
        s.block_events.push(BlockEvent::Placed { tick, owner: 1, cell: [2, 0, 0], piece: shinobi_sim::blocks::Piece { kind: shinobi_sim::blocks::PieceKind::Wall, axis: 1 }, scale_q: 16 });
        s.block_events.push(BlockEvent::Refused { tick, owner: 1, why: PlaceError::Cooldown });
        s.block_events.push(BlockEvent::Removed { tick, cell: [9, 9, 9] });
        r.post_frame(&mut s);
        let placed = NetEvent::BlockPlaced { owner: 1, cell: [2, 0, 0], kind: 1, axis: 1, scale_q: 16 };
        let builder = blocks_for(&mut r, &s, 1);
        assert!(builder.contains(&placed) && builder.contains(&NetEvent::BlockRemoved { cell: [9, 9, 9] }), "{builder:?}");
        assert!(builder.contains(&NetEvent::BlockRefused { why: BLOCK_REFUSED_COOLDOWN }), "{builder:?}");
        let other = blocks_for(&mut r, &s, 2);
        assert!(other.contains(&placed) && !other.iter().any(|e| matches!(e, NetEvent::BlockRefused { .. })), "{other:?}");
        // a joiner is told about every live block
        s.add_player(3, [9.0, 0.81, 0.0]);
        r.add_peer(&s, 3, s.tick);
        assert!(blocks_for(&mut r, &s, 3).contains(&placed));
    }

    struct SoakStats {
        mean_ms: f64,
        p50_ms: f64,
        p99_ms: f64,
        p999_ms: f64,
        max_ms: f64,
        slowest: Vec<(usize, f64)>,
        max_discs: usize,
        /// Largest datagram once the first 2 s (joins, first full snapshots) are over, and in the whole run.
        max_steady: usize,
        max_all: usize,
    }

    /// Free-for-all (the demo mode), 16 players all throwing shurikens, 4 ropes up, a field of player blocks that keeps growing, every datagram
    /// built and acked on a perfect link. `late_joiner`: a 17th peer joins at 1/3 of the run, mid-spam, and has the
    /// whole block field replayed to it.
    fn run_soak(ticks: u32, late_joiner: bool) -> SoakStats {
        use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState, PITCH_LEVEL};
        use shinobi_sim::blocks::BlockEvent;
        use shinobi_sim::character::GrappleState;
        use shinobi_sim::events::EventInbox;
        let cfg = Cfg::default();
        // free-for-all, the demo mode; SOAK_LEVEL=levels/ffa.lvl measures a real map instead of the built-in arena
        let setup = match std::env::var("SOAK_LEVEL") {
            Ok(path) => crate::setup::Setup::load(std::path::Path::new(&path)).expect("SOAK_LEVEL loads"),
            Err(_) => crate::setup::Setup::standard(),
        };
        let mut s = setup.build(cfg);
        for id in 1..=16u16 {
            let at = s.pick_spawn(None);
            s.add_player(id, at);
        }
        // 400 blocks laid out far from the spawn ring before anyone connects
        for k in 0..400i32 {
            let cell = [-40 + k % 20, 0, 30 + k / 20];
            s.blocks.place(&mut s.world, 1 + (k % 16) as u16, cell, shinobi_sim::blocks::Piece::FLOOR, 16, 0, &[]).unwrap();
        }
        let mut r = Replicator::new(&cfg, 1);
        for id in 1..=16u16 {
            r.add_peer(&s, id, s.tick);
        }
        let mut inboxes: HashMap<u16, EventInbox> = (1..=16u16).map(|id| (id, EventInbox::default())).collect();
        let (mut ticks_ns, mut max_discs, mut max_steady, mut max_all) = (Vec::<u64>::new(), 0usize, 0usize, 0usize);
        for t in 0..ticks {
            if late_joiner && t == ticks / 3 {
                s.add_player(17, [0.0, 0.81, 0.0]);
                r.add_peer(&s, 17, s.tick);
                inboxes.insert(17, EventInbox::default());
            }
            let n_players = s.players.len() as u16;
            for p in s.players.iter_mut().take(4) {
                p.ch.grapple = Some(GrappleState { anchor: shinobi_sim::rapier3d::prelude::Vector::new(0.0, 9.0, -6.0), len: 8.0, time: 0.0 });
            }
            if t % 6 == 0 {
                // the field keeps growing: place + announce (as `Server::frame` does for a build press)
                let cell = [-60 + (t / 6 % 40) as i32, 1, 30 + (t / 240) as i32];
                if s.blocks.place(&mut s.world, 2, cell, shinobi_sim::blocks::Piece::FLOOR, 16, 0, &[]).is_ok() {
                    let tick = s.tick;
                    s.block_events.push(BlockEvent::Placed { tick, owner: 2, cell, piece: shinobi_sim::blocks::Piece::FLOOR, scale_q: 16 });
                }
            }
            for id in 1..=n_players {
                let it = PlayerIntent {
                    tick: s.tick,
                    buttons: Buttons(Buttons::FIRE),
                    move_z: if (t / 90) % 2 == 0 { 100 } else { -100 },
                    aim_yaw: ((t * 37 + id as u32 * 4001) % 65536) as u16,
                    aim_pitch: PITCH_LEVEL,
                    ..Default::default()
                };
                let pkt = ClientPacket {
                    ack_snapshot_tick: s.tick.saturating_sub(2),
                    view: ViewState { from_tick: s.tick.saturating_sub(2), alpha: 0 },
                    intents: vec![it],
                    ..Default::default()
                };
                s.ingest(id, &pkt.encode().unwrap());
            }
            let t0 = std::time::Instant::now();
            s.frame(Duration::from_nanos(16_666_667));
            r.post_frame(&mut s);
            let mut round = Vec::new();
            if r.due(s.tick, shinobi_proto::SNAPSHOT_DIVISOR) {
                round = r.build_round(&s);
            }
            ticks_ns.push(t0.elapsed().as_nanos() as u64);
            max_discs = max_discs.max(s.projectiles().len());
            let tick = s.tick.saturating_sub(1);
            for (id, bytes) in round {
                max_all = max_all.max(bytes.len());
                if t >= 120 && !(late_joiner && (ticks / 3..ticks / 3 + 90).contains(&t)) {
                    max_steady = max_steady.max(bytes.len());
                }
                let (events, ..) = datagram::split_snapshot_v3(&bytes).unwrap();
                let inbox = inboxes.get_mut(&id).unwrap();
                read_events(events, inbox, |_| {}).unwrap();
                r.on_client_ack(id, tick, inbox.ack());
            }
        }
        let slowest = {
            let mut v: Vec<(usize, u64)> = ticks_ns.iter().copied().enumerate().collect();
            v.sort_by_key(|x| std::cmp::Reverse(x.1));
            v.into_iter().take(5).map(|(i, n)| (i, n as f64 / 1e6)).collect()
        };
        ticks_ns.sort_unstable();
        let pct = |q: f64| ticks_ns[((ticks_ns.len() as f64 * q) as usize).min(ticks_ns.len() - 1)] as f64 / 1e6;
        SoakStats {
            mean_ms: ticks_ns.iter().sum::<u64>() as f64 / ticks_ns.len() as f64 / 1e6,
            p50_ms: pct(0.5),
            p99_ms: pct(0.99),
            p999_ms: pct(0.999),
            max_ms: pct(1.0),
            slowest,
            max_discs,
            max_steady,
            max_all,
        }
    }

    /// Free-for-all (the default mode) end to end with 16 mixed players: 8 scripted humans + 8 bots. The match
    /// starts, somebody wins, the intermission freezes the world, the next map starts a fresh match with zeroed
    /// scores; nobody is a teammate of anybody.
    #[test]
    fn ffa_match_flows_start_to_end_to_next_match_with_16_mixed_players() {
        use crate::config::BotSkill;
        use crate::supervise::{make_bots, BotPlan};
        use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState, PITCH_LEVEL};
        use shinobi_sim::match_state::{EndReason, MatchEvent, PhaseKind};
        let cfg = Cfg::default();
        let mut s = crate::setup::Setup::standard().build(cfg);
        s.match_state.cfg.warmup_ticks = 60;
        s.match_state.cfg.intermission_ticks = 90;
        s.match_state.cfg.frag_limit = 4;
        s.match_state.cfg.time_limit_ticks = 5400;
        assert!(!s.match_state.team_mode, "FFA is the default");
        for id in 1..=8u16 {
            let at = s.pick_spawn(None);
            s.add_player(id, at);
        }
        let plan = BotPlan { count: 8, skill: BotSkill::Hard, seed: 5, team_size: 0 };
        let mut bots = make_bots(&plan, &mut s);
        assert_eq!(s.players.len(), 16);
        assert!(s.players.iter().all(|p| p.team == shinobi_sim::tick::NO_TEAM), "no teams in FFA: nobody is an ally");
        let mut r = Replicator::new(&cfg, 1);
        for id in s.players.iter().map(|p| p.id).collect::<Vec<_>>() {
            r.add_peer(&s, id, s.tick);
        }
        let mut telemetry: Vec<String> = Vec::new();
        let mut events: Vec<MatchEvent> = Vec::new();
        let (mut frozen_ticks, mut kills_at_second_start) = (0u32, None);
        for t in 0..9000u32 {
            for id in 1..=8u16 {
                let it = PlayerIntent {
                    tick: s.tick,
                    buttons: Buttons(Buttons::FIRE | if t % 40 == 0 { Buttons::SLICE } else { 0 }),
                    move_z: 100,
                    aim_yaw: ((t * 53 + id as u32 * 7919) % 65536) as u16,
                    aim_pitch: PITCH_LEVEL,
                    ..Default::default()
                };
                let pkt = ClientPacket {
                    ack_snapshot_tick: s.tick.saturating_sub(2),
                    view: ViewState { from_tick: s.tick.saturating_sub(2), alpha: 0 },
                    intents: vec![it],
                    ..Default::default()
                };
                s.ingest(id, &pkt.encode().unwrap());
            }
            bots.advance(&mut s, Duration::from_nanos(16_666_667));
            let frame_events: Vec<MatchEvent> = s.match_events.to_vec(); // post_frame drains them
            r.post_frame(&mut s);
            telemetry.extend(r.take_telemetry().0);
            if s.match_state.frozen() {
                frozen_ticks += 1;
            }
            for e in frame_events {
                if matches!(e, MatchEvent::MatchStart { .. }) && events.iter().any(|x| matches!(x, MatchEvent::MatchStart { .. })) {
                    kills_at_second_start = Some(s.players.iter().map(|p| p.vitals.kills as u32).sum::<u32>());
                }
                events.push(e);
            }
            if kills_at_second_start.is_some() {
                break;
            }
        }
        let starts = events.iter().filter(|e| matches!(e, MatchEvent::MatchStart { .. })).count();
        let end = events.iter().find_map(|e| match e {
            MatchEvent::MatchEnd { winner, reason, .. } => Some((*winner, *reason)),
            _ => None,
        });
        let next = events.iter().find_map(|e| match e {
            MatchEvent::NextMap { map, .. } => Some(*map),
            _ => None,
        });
        assert_eq!(starts, 2, "a match started, ended, and the next one started: {events:?}");
        let (winner, reason) = end.expect("the match ended");
        assert!(winner.is_some(), "a frag-limit / time-limit win in a 16-player brawl has a winner (got a draw)");
        assert!(matches!(reason, EndReason::FragLimit | EndReason::TimeLimit));
        assert_eq!(next, Some(1), "intermission moves to the next map");
        assert!(frozen_ticks >= 80, "the intermission froze the world for its ~90 ticks ({frozen_ticks})");
        assert_eq!(kills_at_second_start, Some(0), "scores are zeroed for the next match");
        assert_eq!(s.match_state.kind(), PhaseKind::Live);
        let log = r.take_log().join("\n");
        assert!(log.contains("MATCH"), "the server console narrates match flow:\n{log}");
        // playtest telemetry: a FINAL report for the finished match, humans listed individually with real numbers
        let t = telemetry.join("\n");
        assert!(t.contains("match 1 FINAL") && t.contains("8 human(s), 8 bot(s)"), "{t}");
        assert!(t.contains("p1 (human): kills") && t.contains("bots x8 (summed)"), "{t}");
        let thrown: u32 = t.lines().find(|l| l.contains("shuriken: thrown")).and_then(|l| l.split("thrown ").nth(1)).and_then(|x| x.split_whitespace().next()).and_then(|x| x.parse().ok()).unwrap();
        assert!(thrown > 20, "the scripted humans throw all match long: {thrown}\n{t}");
    }

    /// The client reads datagrams into a 2048 B buffer (`udp_game::RX_BUF`): a bigger one is truncated and dropped.
    /// Worst case: 16 players spamming + ropes + a 400-block field + a mid-spam joiner getting the whole field.
    #[test]
    fn worst_case_datagram_stays_under_the_client_rx_buffer() {
        const RX_BUF: usize = 2048; // crates/shinobi-client/src/udp_game.rs and udp_session.rs
        let st = run_soak(600, true);
        eprintln!("worst case: largest steady datagram {} B, whole run {} B, discs in flight {}", st.max_steady, st.max_all, st.max_discs);
        assert!(st.max_discs > 100, "the soak really is a firefight ({} discs)", st.max_discs);
        assert!(st.max_all < RX_BUF, "a {} B datagram would be truncated by the client", st.max_all);
        assert!(st.max_steady <= 1200, "steady-state datagram {} B exceeds one MTU", st.max_steady);
    }

    /// Release-mode soak: per-tick cost percentiles (sim + replication + every datagram).
    /// Run: `cargo test --release -p shinobi-server soak_8v8 -- --ignored --nocapture`.
    #[test]
    #[ignore = "perf report: run in --release"]
    fn soak_8v8_tick_cost_percentiles() {
        let st = run_soak(3600, true);
        eprintln!("slowest ticks (index, ms): {:.2?}", st.slowest);
        eprintln!(
            "soak 8v8 ({} build): 3600 ticks, mean {:.2} ms, p50 {:.2}, p99 {:.2}, p99.9 {:.2}, max {:.2} ms; most discs in flight {} (sim; up to {} replicated); largest steady datagram {} B, whole run {} B",
            if cfg!(debug_assertions) { "debug" } else { "release" },
            st.mean_ms, st.p50_ms, st.p99_ms, st.p999_ms, st.max_ms, st.max_discs, PROJECTILE_ID_MAX, st.max_steady, st.max_all
        );
        if !cfg!(debug_assertions) {
            assert!(st.p99_ms < 5.0, "p99 tick {:.2} ms: the demo budget is 5 ms at 16 players (docs/DEMO.md)", st.p99_ms);
        }
        assert!(st.max_all < 2048 && st.max_steady <= 1200);
    }

    #[test]
    fn teams_reach_every_peer_joiners_get_everyone_and_scores_follow_changes() {
        use shinobi_sim::events::{TEAM_NONE, TEAM_RESULT_RUNNING};
        let (mut s, mut r) = two_peers();
        s.enable_teams(50);
        let teams_for = |r: &mut Replicator, s: &Server, id: u16| {
            let round = r.build_round(s);
            let mut inbox = shinobi_sim::events::EventInbox::default();
            let mut got = Vec::new();
            read_events(datagram_for(&round, id).0, &mut inbox, |e| got.push(e)).unwrap();
            got.into_iter()
                .filter(|e| matches!(e, NetEvent::TeamAssign { .. } | NetEvent::TeamScore { .. }))
                .collect::<Vec<_>>()
        };
        // two_peers() added players before post_frame ever ran: peer 1 and 2 are told both memberships
        r.post_frame(&mut s);
        let got = teams_for(&mut r, &s, 1);
        assert!(got.contains(&NetEvent::TeamAssign { player: 1, team: 0 }), "{got:?}");
        assert!(got.contains(&NetEvent::TeamAssign { player: 2, team: 1 }), "{got:?}");
        assert!(got.iter().any(|e| matches!(e, NetEvent::TeamScore { score: [0, 0], result: TEAM_RESULT_RUNNING, .. })));
        // a tie that goes to sudden death is announced once
        s.match_state.start_sudden_death(s.tick, [0, 0]);
        r.post_frame(&mut s);
        let got = teams_for(&mut r, &s, 1);
        assert!(got.iter().any(|e| matches!(e, NetEvent::TeamScore { result: shinobi_sim::events::TEAM_RESULT_SUDDEN, .. })), "{got:?}");
        // a kill changes the score once
        s.players[0].vitals.kills = 3;
        r.post_frame(&mut s);
        let got = teams_for(&mut r, &s, 2);
        assert!(got.iter().any(|e| matches!(e, NetEvent::TeamScore { score: [3, 0], .. })), "{got:?}");
        // a late joiner is told every player's team, and the others are told about the newcomer
        s.add_player(3, [9.0, 0.81, 0.0]);
        r.add_peer(&s, 3, s.tick);
        let got = teams_for(&mut r, &s, 3);
        for (id, team) in [(1u16, 0u8), (2, 1), (3, 1)] {
            assert!(got.contains(&NetEvent::TeamAssign { player: id, team }), "joiner missing {id}: {got:?}");
        }
        assert!(teams_for(&mut r, &s, 1).contains(&NetEvent::TeamAssign { player: 3, team: 1 }));
        // leaving is announced
        s.remove_player(3);
        let t = s.tick;
        r.remove_peer(&mut s, 3, t);
        assert!(teams_for(&mut r, &s, 1).contains(&NetEvent::TeamAssign { player: 3, team: TEAM_NONE }));
    }

    #[test]
    fn joiners_get_the_match_rules() {
        let (s, mut r) = two_peers();
        let round = r.build_round(&s);
        let mut inbox = shinobi_sim::events::EventInbox::default();
        let mut got = Vec::new();
        read_events(datagram_for(&round, 2).0, &mut inbox, |e| got.push(e)).unwrap();
        let cfg = &s.match_state.cfg;
        let want = NetEvent::MatchRules {
            frag_limit: cfg.frag_limit,
            team_frag_limit: s.match_state.team_frag_limit,
            time_limit_secs: (cfg.time_limit_ticks / shinobi_proto::TICK_HZ) as u16,
            respawn_ticks: s.balance.player.respawn_ticks as u16,
        };
        assert!(got.contains(&want), "{got:?}");
    }

    #[test]
    fn ropes_are_broadcast_on_edges_only_and_replayed_to_late_joiners() {
        use shinobi_sim::character::GrappleState;
        let (mut s, mut r) = two_peers();
        let events_for = |r: &mut Replicator, s: &Server, id: u16| {
            let round = r.build_round(s);
            let mut inbox = shinobi_sim::events::EventInbox::default();
            let mut got = Vec::new();
            read_events(datagram_for(&round, id).0, &mut inbox, |e| got.push(e)).unwrap();
            got.into_iter().filter(|e| matches!(e, NetEvent::RopeAttach { .. } | NetEvent::RopeRelease { .. })).collect::<Vec<_>>()
        };
        // player 1 hooks: everyone is told once, with the quantized anchor
        s.players[0].ch.grapple = Some(GrappleState { anchor: shinobi_sim::rapier3d::prelude::Vector::new(3.0, 6.0, -8.0), len: 9.0, time: 0.0 });
        r.post_frame(&mut s);
        let want = NetEvent::RopeAttach { tick: s.tick - 1, player: 1, anchor: q_pos3([3.0, 6.0, -8.0]) };
        assert_eq!(events_for(&mut r, &s, 2), vec![want]);
        // unchanged rope costs nothing: no repeat event while hooked (and the peers have not acked, so only the
        // already-queued one is re-sent, never a second copy)
        r.post_frame(&mut s);
        assert_eq!(events_for(&mut r, &s, 2), vec![want], "still exactly one queued attach");
        // a peer that joins while the rope is up gets it replayed
        s.add_player(3, [9.0, 0.81, 0.0]);
        r.add_peer(&s, 3, s.tick);
        assert!(matches!(events_for(&mut r, &s, 3)[..], [NetEvent::RopeAttach { player: 1, .. }]));
        // release (or death) -> one release
        s.players[0].ch.grapple = None;
        r.post_frame(&mut s);
        let got = events_for(&mut r, &s, 2);
        assert_eq!(got.len(), 2, "attach then release: {got:?}");
        assert!(matches!(got[1], NetEvent::RopeRelease { player: 1, .. }));
        // leaving while roped releases it too
        s.players[0].ch.grapple = Some(GrappleState { anchor: shinobi_sim::rapier3d::prelude::Vector::new(1.0, 2.0, 3.0), len: 3.0, time: 0.0 });
        r.post_frame(&mut s);
        let t = s.tick;
        r.remove_peer(&mut s, 1, t);
        assert!(events_for(&mut r, &s, 2).iter().any(|e| matches!(e, NetEvent::RopeRelease { player: 1, .. })));
    }

    #[test]
    fn shurikens_replicate_as_projectile_entities() {
        use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState};
        let mut s = world();
        for tick in 1..=6u32 {
            let it = PlayerIntent { tick, buttons: Buttons(Buttons::FIRE), ..Default::default() };
            let pkt = ClientPacket {
                ack_snapshot_tick: 1,
                view: ViewState { from_tick: 1, alpha: 0 },
                intents: vec![it],
                ..Default::default()
            };
            s.ingest(1, &pkt.encode().unwrap());
            s.frame(Duration::from_millis(17));
        }
        assert!(!s.projectiles().is_empty(), "player 1 threw a disc");
        let snap = build_snapshot(s.tick, &s, PROJECTILE_ID_MAX, &[]);
        let discs: Vec<_> = snap.entities.iter().filter(|e| e.0 >= PROJECTILE_ID_BASE).collect();
        assert_eq!(discs.len(), s.projectiles().len());
        assert!(discs.iter().all(|(_, e)| e.yaw == 1));
        assert!(snap.entities.windows(2).all(|w| w[0].0 < w[1].0), "sorted, unique");
    }

    #[test]
    fn full_then_delta_then_ack_driven_baseline() {
        let mut s = world();
        let mut r = Replicator::new(&Cfg::default(), 1);
        r.add_peer(&s, 1, 0);
        r.add_peer(&s, 2, 0);
        s.frame(Duration::from_millis(40));
        r.post_frame(&mut s);
        assert!(r.due(s.tick, 2));
        let round = r.build_round(&s);
        assert_eq!(round.len(), 2);
        let (_, bytes) = round.iter().find(|(id, _)| *id == 1).unwrap();
        let (_events, _own, _board, snap) = datagram::split_snapshot_v3(bytes).unwrap();
        let got = decode_delta(snap, |_| None).unwrap();
        assert_eq!(got.entities.len(), 2);
        assert_eq!(got.tick, s.tick - 1);
        let first_tick = got.tick;

        // client acks; next round must delta against it and still decode
        r.on_client_ack(1, first_tick, 0);
        s.frame(Duration::from_millis(40));
        r.post_frame(&mut s);
        let round = r.build_round(&s);
        let (_, bytes) = round.iter().find(|(id, _)| *id == 1).unwrap();
        let (_e, _o, _b, snap) = datagram::split_snapshot_v3(bytes).unwrap();
        let first = got.clone();
        let next = decode_delta(snap, |t| (t == first.tick).then_some(&first)).unwrap();
        assert_eq!(next.tick, s.tick - 1);

        // bogus ack (tick never sent) is ignored, peer stays on its last good baseline
        r.on_client_ack(1, 9_999_999, 0);
        assert_eq!(r.peers[&1].ack_snapshot, Some(first_tick));
    }

    #[test]
    fn obstacles_reach_peers_and_late_joiners() {
        let mut s = world();
        let mut r = Replicator::new(&Cfg::default(), 1).with_features(Features { obstacles: true });
        r.add_peer(&s, 1, 0);
        for _ in 0..20 {
            s.frame(Duration::from_millis(17));
            r.post_frame(&mut s);
        }
        let round = r.build_round(&s);
        let (events, _, _, _) = datagram::split_snapshot_v3(&round[0].1).unwrap();
        let mut inbox = EventInbox::default();
        let (mut obstacle_events, mut other_events) = (0, 0);
        read_events(events, &mut inbox, |e| match e {
            NetEvent::ObstacleSpawn(_) => obstacle_events += 1,
            _ => other_events += 1, // match countdown, teams etc.
        })
        .unwrap();
        assert!(obstacle_events >= 3, "wave 0 was broadcast");
        assert_eq!(obstacle_events + other_events, inbox.ack() as usize);

        // late joiner gets the live obstacles too
        r.add_peer(&s, 3, s.tick);
        assert_eq!(
            r.peers[&3].outbox.pending(),
            r.obstacles.not_ended_at(s.tick).count() + 2,
            "live obstacles + the BlockRules + MatchRules events (free-for-all: no team events)"
        );
    }

    use shinobi_proto::bits::BitReader;
    use shinobi_proto::gameplay::{Board, OwnExtras, KIND_SLASH, KIND_WALL, PHASE_WARMUP};
    use shinobi_proto::snapshot::{hp_lost5, FLAG_DEAD};
    use shinobi_sim::pvp::HazardSource;

    fn datagram_for<'a>(round: &'a [(u16, Vec<u8>)], id: u16) -> (&'a [u8], &'a [u8], &'a [u8], &'a [u8]) {
        datagram::split_snapshot_v3(&round.iter().find(|(i, _)| *i == id).unwrap().1).unwrap()
    }

    fn decode_own(own: &[u8]) -> (OwnExtras, Option<OwnState>) {
        OwnExtras::read(&mut BitReader::new(own), OwnState::read).expect("own block decodes")
    }

    fn two_peers() -> (Server, Replicator) {
        let mut s = world();
        let mut r = Replicator::new(&Cfg::default(), 1);
        r.add_peer(&s, 1, 0);
        r.add_peer(&s, 2, 0);
        s.frame(Duration::from_millis(40));
        r.post_frame(&mut s);
        (s, r)
    }

    #[test]
    fn default_server_makes_no_obstacles() {
        let mut s = world();
        let mut r = Replicator::new(&Cfg::default(), 1);
        r.add_peer(&s, 1, 0);
        for i in 0..600 {
            let h = s.players[0].ch.body;
            s.world.bodies[h].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation((i % 40) as f32 * 0.5, 0.81, 0.0), true);
            s.frame(Duration::from_millis(17));
            r.post_frame(&mut s);
        }
        assert_eq!((r.obstacles.not_ended_at(s.tick).count(), s.hazards.obstacles.not_ended_at(s.tick).count()), (0, 0));
        let round = r.build_round(&s);
        let (events, _, _, _) = datagram::split_snapshot_v3(&round[0].1).unwrap();
        let mut inbox = shinobi_sim::events::EventInbox::default();
        read_events(events, &mut inbox, |e| {
            assert!(!matches!(e, NetEvent::ObstacleSpawn(_)), "{e:?}")
        })
        .unwrap();
        // opting in brings obstacle waves back
        let mut r = Replicator::new(&Cfg::default(), 1).with_features(Features { obstacles: true });
        r.add_peer(&s, 1, 0);
        r.post_frame(&mut s);
        assert!(r.obstacles.not_ended_at(s.tick).count() >= 3);
    }

    #[test]
    fn own_block_has_state_only_after_an_executed_intent_and_always_has_health() {
        use shinobi_proto::intent::{ClientPacket, PlayerIntent};
        let (mut s, mut r) = two_peers();
        let round = r.build_round(&s);
        let (own, state) = decode_own(datagram_for(&round, 1).1);
        assert_eq!((own.has_state, own.hp_x10, state.is_none()), (false, 1000, true), "no intent yet: health only");

        // player 1 sends an intent for the next tick; after the frame it is the last executed tick
        let t = s.tick;
        let pkt = ClientPacket { intents: vec![PlayerIntent { tick: t, ..Default::default() }], ..Default::default() };
        s.ingest(1, &pkt.encode().unwrap());
        s.frame(Duration::from_nanos(16_666_667));
        r.post_frame(&mut s);
        let round = r.build_round(&s);
        let (own, state) = decode_own(datagram_for(&round, 1).1);
        assert!(own.has_state && own.acked_tick == t && state.is_some(), "{own:?}");
        // owner-only: player 2 executed nothing, so no movement state for them
        assert!(!decode_own(datagram_for(&round, 2).1).0.has_state);
    }

    #[test]
    fn dead_player_gets_extras_only_and_dead_flag_with_zero_health_bits() {
        use shinobi_proto::intent::{ClientPacket, PlayerIntent};
        let (mut s, mut r) = two_peers();
        let t = s.tick;
        s.ingest(1, &ClientPacket { intents: vec![PlayerIntent { tick: t, ..Default::default() }], ..Default::default() }.encode().unwrap());
        s.frame(Duration::from_nanos(16_666_667));
        let tick = s.tick;
        s.players[0].vitals.hp = 0.0;
        s.players[0].vitals.dead_until_tick = Some(tick + 300);
        r.post_frame(&mut s);
        let round = r.build_round(&s);
        let (events, own_bytes, _, snap) = datagram_for(&round, 1);
        let _ = events;
        let (own, state) = decode_own(own_bytes);
        assert_eq!((own.has_state, own.hp_x10, state.is_none()), (false, 0, true));
        let snap = decode_delta(snap, |_| None).unwrap();
        let e = snap.get(1).unwrap();
        assert!(e.flags & FLAG_DEAD != 0 && hp_lost5(e.flags) == 31, "flags {:#010b}", e.flags);
        assert_eq!(hp_lost5(snap.get(2).unwrap().flags), 0, "the other player is healthy");
    }

    #[test]
    fn hits_go_to_the_two_parties_only_and_kills_go_to_everyone_reliably() {
        let (mut s, mut r) = two_peers();
        r.add_peer(&s, 3, 0); // a bystander (no Player needed for routing)
        let tick = s.tick;
        s.pvp_events.push(PvpEvent::Hit { tick, attacker: 1, victim: 2, amount: 34.4, kind: DamageKind::Slash, head: false, bonus: false });
        s.pvp_events.push(PvpEvent::Hit { tick, attacker: 1, victim: 2, amount: 68.8, kind: DamageKind::Shuriken, head: true, bonus: false });
        s.pvp_events.push(PvpEvent::HazardHit { tick, victim: 2, amount: 100.0, source: HazardSource::Wall { id: 9, owner: 1 } });
        s.pvp_events.push(PvpEvent::Kill { tick, killer: 1, victim: 2, kind: DamageKind::Slash, head: false });
        s.pvp_events.push(PvpEvent::Respawn { tick, player: 2 });
        r.post_frame(&mut s);
        assert!(s.pvp_events.is_empty(), "drained");
        let round = r.build_round(&s);

        // a peer without a Player (bystander) has no own block at all
        let hits_of = |id| match datagram_for(&round, id).1 {
            [] => Vec::new(),
            own => decode_own(own).0.hits,
        };
        let attacker = hits_of(1);
        assert_eq!(attacker.len(), 3); // the hit + the headshot + the kill marker
        assert!(attacker[1].head && !attacker[0].head && !attacker[2].head, "only the shuriken hit is a headshot: {attacker:?}");
        assert!(hits_of(2).iter().any(|h| h.taken && h.head), "the victim is told it was a headshot too");
        assert!(attacker.iter().all(|h| !h.taken && h.other == 2));
        assert_eq!((attacker[0].amount, attacker[0].kind, attacker[0].kill), (34, KIND_SLASH, false));
        assert!(attacker[2].kill);
        let victim = hits_of(2);
        assert!(victim.iter().all(|h| h.taken));
        assert!(victim.iter().any(|h| h.kind == KIND_WALL && h.other == 1 && h.amount == 100), "wall owner named: {victim:?}");
        assert!(victim.iter().any(|h| h.kill && h.other == 1));
        assert!(hits_of(3).is_empty(), "bystanders get no hit markers");
        s.pvp_events.push(PvpEvent::HazardKill { tick, victim: 2, source: HazardSource::Obstacle(shinobi_sim::pvp::VOID_SOURCE_ID), killer: None });
        r.post_frame(&mut s);
        let log = r.take_log().join("\n");
        assert!(log.contains("KILL    p2 fell out of the world"), "void kill has its own feed line:\n{log}");
        assert!(log.contains("KILL    p1 sliced p2") && log.contains("RESPAWN p2"), "console kill feed:\n{log}");
        assert!(r.take_log().is_empty(), "drained");
        // each hit rides in HIT_RESENDS datagrams (first was `round`), then stops
        let first = decode_own(datagram_for(&round, 1).1).0;
        for _ in 1..HIT_RESENDS {
            let again = decode_own(datagram_for(&r.build_round(&s), 1).1).0;
            assert_eq!((again.hits.clone(), again.hit_seq), (first.hits.clone(), first.hit_seq), "resend is identical");
        }
        assert!(hits_of_next(&mut r, &s, 1).is_empty(), "hit stops after its resends");

        // kill + respawn are RELIABLE and reach every peer, bystander included
        for id in [1u16, 2, 3] {
            let mut inbox = shinobi_sim::events::EventInbox::default();
            let mut got = Vec::new();
            read_events(datagram_for(&round, id).0, &mut inbox, |e| got.push(e)).unwrap();
            assert!(got.contains(&NetEvent::Kill { tick, killer: 1, victim: 2, kind: KIND_SLASH }), "peer {id}: {got:?}");
            assert!(got.contains(&NetEvent::Respawn { tick, player: 2 }), "peer {id}");
        }
    }

    fn hits_of_next(r: &mut Replicator, s: &Server, id: u16) -> Vec<shinobi_proto::gameplay::HitMsg> {
        let round = r.build_round(s);
        decode_own(datagram_for(&round, id).1).0.hits
    }

    #[test]
    fn hit_backlog_is_bounded_when_snapshots_stall() {
        let (mut s, mut r) = two_peers();
        let tick = s.tick;
        for _ in 0..200 {
            s.pvp_events.push(PvpEvent::Hit { tick, attacker: 1, victim: 2, amount: 1.0, kind: DamageKind::Shuriken, head: false, bonus: false });
            r.post_frame(&mut s);
        }
        assert!(r.peers[&1].hits.len() <= MAX_PENDING_HITS);
        let round = r.build_round(&s);
        assert_eq!(decode_own(datagram_for(&round, 1).1).0.hits.len(), gameplay::MAX_HITS, "newest 15 per datagram");
    }

    #[test]
    fn board_goes_out_on_cadence_and_on_phase_change() {
        let (mut s, mut r) = two_peers();
        let round = r.build_round(&s);
        let board = Board::decode(datagram_for(&round, 1).2).expect("first round carries a board");
        assert_eq!((board.phase, board.rows.len()), (PHASE_WARMUP, 2));
        assert_eq!(datagram_for(&round, 2).2, datagram_for(&round, 1).2, "same board for everyone");
        s.frame(Duration::from_millis(40));
        r.post_frame(&mut s);
        let round = r.build_round(&s);
        assert!(datagram_for(&round, 1).2.is_empty(), "no board between cadence ticks");
        for _ in 0..70 {
            s.frame(Duration::from_nanos(16_666_667));
            r.post_frame(&mut s);
        }
        let round = r.build_round(&s);
        assert!(!datagram_for(&round, 1).2.is_empty(), "board again after ~1 s");
    }

    #[test]
    fn late_joiner_is_seeded_with_pickups_and_map_change_clears_everything() {
        let mut s = world();
        s.add_pickup(1, [5.0, 1.0, 20.0], shinobi_sim::combat::powerup::PowerupKind::GridBeam, 10.0); // off the walking path
        let mut r = Replicator::new(&Cfg::default(), 1).with_features(Features { obstacles: true });
        r.add_peer(&s, 1, 0);
        for i in 0..40 {
            let h = s.players[0].ch.body;
            s.world.bodies[h].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(i as f32 * 0.5, 0.81, 0.0), true);
            s.frame(Duration::from_millis(17));
            r.post_frame(&mut s);
        }
        assert_eq!(s.pickups.active().count(), 1);

        r.add_peer(&s, 7, s.tick);
        let round = r.build_round(&s);
        let mut inbox = shinobi_sim::events::EventInbox::default();
        let mut got = Vec::new();
        read_events(datagram_for(&round, 7).0, &mut inbox, |e| got.push(e)).unwrap();
        assert!(got.iter().any(|e| matches!(e, NetEvent::PickupSpawned { id: 1, .. })), "pickup seeded: {got:?}");

        // host job: NextMap clears the obstacles and hazard mirror, and tells clients
        let tick = s.tick;
        s.match_events.push(MatchEvent::NextMap { tick, map: 1 });
        r.post_frame(&mut s);
        assert_eq!(r.obstacles.not_ended_at(s.tick).count(), 0);
        // peer 1 never acked anything, so MapChange sits behind a backlog of walls: a real client acks as it
        // reads and the stream drains over a few datagrams (in order, nothing lost)
        let mut inbox = shinobi_sim::events::EventInbox::default();
        let mut got = Vec::new();
        for _ in 0..10 {
            let round = r.build_round(&s);
            read_events(datagram_for(&round, 1).0, &mut inbox, |e| got.push(e)).unwrap();
            r.on_client_ack(1, 0, inbox.ack());
            if got.iter().any(|e| matches!(e, NetEvent::MapChange { .. })) {
                break;
            }
        }
        assert!(got.iter().any(|e| matches!(e, NetEvent::MapChange { map: 1, .. })), "MapChange delivered in order");
    }
}
