//! Server -> client replication: per-peer delta baselines, reliable wall/obstacle event streams,
//! and datagram assembly. Pure logic (no sockets) so it is unit-testable.

use std::collections::{HashMap, VecDeque};

use shinobi_proto::bits::BitWriter;
use shinobi_proto::datagram;
use shinobi_proto::gameplay::{self, Board, BoardRow, HitMsg, OwnExtras};
use shinobi_proto::quant::{q_pos3, q_vel};
use shinobi_proto::secs_to_ticks;
use shinobi_proto::snapshot::{
    encode_delta, with_hp_frac, EntityState, MoveKind, Snapshot, FLAG_DEAD, FLAG_INVULN, FLAG_ROLL,
};
use shinobi_sim::character::{Cfg, MoveState, TRAIL_CAP};
use shinobi_sim::combat::DamageKind;
use shinobi_sim::events::{EventOutbox, NetEvent};
use shinobi_sim::match_state::{EndReason, MatchEvent, MatchState, PhaseKind};
use shinobi_sim::pickups::PickupEvent;
use shinobi_sim::pvp::{HazardSource, PvpEvent, NO_ATTACKER};
use shinobi_sim::tick::{Player, Server};
use shinobi_sim::obstacle::ObstacleStore;
use shinobi_sim::own_state::OwnState;
use shinobi_sim::walls::{OwnerTrail, WallId, WallStore};
use shinobi_sim::walls_world::{WallsWorld, WALL_GRACE_TICKS};

use crate::obstacles_spawner::ObstacleSpawner;

/// Kept snapshots = how stale an ack may be and still get a delta (32 ticks ~ 0.5 s); older -> full snapshot.
const HISTORY: usize = 32;
/// Reliable events get first call on the datagram; leaves room for 16 entities in a 1200 B MTU.
const EVENT_BUDGET: usize = 480;

#[derive(Default)]
struct PeerState {
    ack_snapshot: Option<u32>,
    outbox: EventOutbox,
    /// Hit markers / damage directions waiting for the next snapshot datagram (unreliable: loss is fine).
    hits: Vec<HitMsg>,
}

/// Scoreboard cadence: every second, plus immediately on a phase change.
const BOARD_EVERY_TICKS: u32 = 60;
/// Never let a stalled snapshot cadence grow a peer's hit backlog without bound.
const MAX_PENDING_HITS: usize = 32;

/// Optional world features. The game is a shooter: Tron is a visual style, so by default the server creates NO
/// trail walls and NO obstacle waves. Both stay available (wire codecs intact) behind explicit opt-ins until the
/// dead code is removed.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Features {
    /// Legacy light-cycle mechanic: trails become solid, damaging walls (hidden `--light-walls`).
    pub light_walls: bool,
    /// Moving neon obstacle waves (`--obstacles`).
    pub obstacles: bool,
}

pub struct Replicator {
    history: VecDeque<Snapshot>,
    peers: HashMap<u16, PeerState>,
    trails: HashMap<u16, OwnerTrail>,
    walls: WallStore,
    /// Solid colliders for the same walls (server-only; clients render from `WallStore`).
    solid: WallsWorld,
    obstacles: ObstacleStore,
    spawner: ObstacleSpawner,
    next_wall: WallId,
    life_ticks: u32,
    last_sent: Option<u32>,
    /// (tick, phase) of the last scoreboard that went out.
    last_board: Option<(u32, u8)>,
    seed: u64,
    features: Features,
    /// One line per gameplay event for the server console (kill feed), drained by `take_log`.
    log: Vec<String>,
}

impl Replicator {
    pub fn new(cfg: &Cfg, obstacle_seed: u64) -> Self {
        let life_ticks = secs_to_ticks(cfg.trail_life);
        Self {
            history: VecDeque::new(),
            peers: HashMap::new(),
            trails: HashMap::new(),
            walls: WallStore::default(),
            solid: WallsWorld::new(life_ticks, WALL_GRACE_TICKS, cfg.trail_height, cfg.trail_thick),
            obstacles: ObstacleStore::default(),
            spawner: ObstacleSpawner::new(obstacle_seed),
            next_wall: 1,
            life_ticks,
            last_sent: None,
            last_board: None,
            seed: obstacle_seed,
            features: Features::default(),
            log: Vec::new(),
        }
    }

    pub fn with_features(mut self, features: Features) -> Self {
        self.features = features;
        self
    }

    /// Contact-damage config must match our `WallsWorld` exactly (see `shinobi_sim::hazards`).
    fn configure_hazards(&self, server: &mut Server) {
        let c = &mut server.hazards.cfg;
        c.life_ticks = self.life_ticks;
        c.grace_ticks = WALL_GRACE_TICKS;
        c.wall_height = self.solid.height;
        c.wall_thick = self.solid.thick;
    }

    /// The world was rebuilt (supervisor recovery): put the solid wall colliders and the hazard mirror back
    /// from our authoritative lists.
    pub fn rebuild_world_state(&mut self, server: &mut Server, tick: u32) {
        self.solid = WallsWorld::new(self.life_ticks, WALL_GRACE_TICKS, self.solid.height, self.solid.thick);
        self.configure_hazards(server);
        let live: Vec<_> = self.walls.live_at(tick, self.life_ticks).map(|w| w.spawn).collect();
        for w in live {
            self.solid.add(w);
            server.hazards.walls.apply_spawn(w);
        }
        for o in self.obstacles.not_ended_at(tick) {
            server.hazards.obstacles.apply_spawn(*o);
        }
    }

    /// New peer: queues every live wall so late joiners see existing Tron walls.
    pub fn add_peer(&mut self, server: &Server, id: u16, now_tick: u32) {
        let mut st = PeerState::default();
        // Small + essential first (pickups, obstacles), bulk last (walls): the outbox is byte-budgeted per datagram,
        // so a joiner sees the rest of the world immediately and the long trail tail over the next datagrams.
        for (pid, pos, _kind) in server.pickups.active() {
            st.outbox.push(NetEvent::PickupSpawned { tick: now_tick, id: pid, pos: q_pos3(pos), kind: 0 });
        }
        for o in self.obstacles.not_ended_at(now_tick) {
            st.outbox.push(NetEvent::ObstacleSpawn(*o));
        }
        for w in self.walls.live_at(now_tick, self.life_ticks) {
            st.outbox.push(NetEvent::WallSpawn(w.spawn));
        }
        self.peers.insert(id, st);
    }

    pub fn remove_peer(&mut self, server: &mut Server, id: u16, now_tick: u32) {
        self.peers.remove(&id);
        if let Some(mut t) = self.trails.remove(&id) {
            for wid in t.clear() {
                self.kill_wall(server, wid, now_tick);
            }
        }
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

    fn kill_wall(&mut self, server: &mut Server, id: WallId, tick: u32) {
        self.walls.apply_kill(id, tick);
        self.solid.kill(id, tick);
        server.hazards.walls.apply_kill(id, tick);
        self.broadcast(NetEvent::WallKill { id, tick });
    }

    /// Everything that must happen once per server frame, after `Server::frame`: trail sampling, solid wall
    /// sync, obstacle waves. (Ticks inside a frame are 16 ms apart; wall spacing is 0.4 m, so nothing is missed.)
    pub fn post_frame(&mut self, server: &mut Server) {
        self.configure_hazards(server);
        if self.features.light_walls {
            self.sample_trails(server);
        }
        let Some(tick) = server.tick.checked_sub(1) else { return };
        // The sim clears these event vecs at the start of its next `frame()`: drain now or lose them.
        self.drain_game_events(server, tick);
        if self.features.light_walls {
            self.solid.sync(tick, &mut server.world);
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
            p.hits.push(hit);
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
        for ev in pvp {
            match ev {
                PvpEvent::Hit { attacker, victim, amount, kind, .. } => {
                    let (amount, kind) = (amount.round().clamp(0.0, 255.0) as u8, weapon_code(kind));
                    self.push_hit(attacker, HitMsg { other: victim, amount, kind, taken: false, kill: false });
                    self.push_hit(victim, HitMsg { other: who(attacker), amount, kind, taken: true, kill: false });
                }
                PvpEvent::Kill { tick, killer, victim, kind } => {
                    let kind = weapon_code(kind);
                    self.log_line(tick, format!("KILL    p{killer} {} p{victim}", kind_verb(kind)));
                    self.broadcast(NetEvent::Kill { tick, killer: who(killer), victim, kind });
                    self.push_hit(killer, HitMsg { other: victim, amount: 0, kind, taken: false, kill: true });
                    self.push_hit(victim, HitMsg { other: who(killer), amount: 0, kind, taken: true, kill: true });
                }
                PvpEvent::HazardHit { victim, amount, source, .. } => {
                    let (kind, owner) = hazard_code(source);
                    let amount = amount.round().clamp(0.0, 255.0) as u8;
                    self.push_hit(victim, HitMsg { other: owner, amount, kind, taken: true, kill: false });
                }
                PvpEvent::HazardKill { tick, victim, source, killer } => {
                    let (kind, _) = hazard_code(source);
                    let by = killer.map_or(String::new(), |k| format!(" (p{k}'s trail)"));
                    // the sim's kill plane reports as obstacle id 65535 (`pvp::VOID_SOURCE_ID`)
                    let verb = if matches!(source, HazardSource::Obstacle(shinobi_sim::pvp::VOID_SOURCE_ID)) {
                        "fell out of the world"
                    } else {
                        kind_verb(kind)
                    };
                    self.log_line(tick, format!("KILL    p{victim} {verb}{by}"));
                    self.broadcast(NetEvent::Kill { tick, killer: killer.unwrap_or(0), victim, kind });
                    self.push_hit(victim, HitMsg { other: killer.unwrap_or(0), amount: 0, kind, taken: true, kill: true });
                    if let Some(k) = killer {
                        self.push_hit(k, HitMsg { other: victim, amount: 0, kind, taken: false, kill: true });
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

    /// Host job on `MatchEvent::NextMap`: forget every wall / obstacle (server colliders, hazard mirror, trails),
    /// restart the obstacle cadence, and tell clients to clear theirs. The level geometry itself comes from
    /// `Setup`; with a single level there is nothing to reload (a multi-map host would swap `server.world` statics here).
    fn map_change(&mut self, server: &mut Server, tick: u32, map: u32) {
        self.solid.clear(&mut server.world);
        self.walls = WallStore::default();
        server.hazards.walls = WallStore::default();
        self.obstacles = ObstacleStore::default();
        server.hazards.obstacles = ObstacleStore::default();
        for t in self.trails.values_mut() {
            let _ = t.clear();
        }
        self.spawner.restart(tick, self.seed ^ (map as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        self.log_line(tick, format!("MATCH   map change -> {map} (walls and obstacles cleared)"));
        self.broadcast(NetEvent::MapChange { tick, map });
    }

    fn sample_trails(&mut self, server: &mut Server) {
        let Some(tick) = server.tick.checked_sub(1) else { return };
        let cfg = server.cfg();
        let (cfg_spacing, half) = (cfg.trail_spacing, cfg.half_height + cfg.radius);
        let poses: Vec<(u16, [f32; 3])> = server.players.iter().map(|p| (p.id, p.pos(&server.world))).collect();
        for (id, pos) in poses {
            let feet = [pos[0], pos[1] - half, pos[2]];
            let trail = self
                .trails
                .entry(id)
                .or_insert_with(|| OwnerTrail::new(id as u8, cfg_spacing, self.life_ticks, TRAIL_CAP));
            let step = trail.sample(tick, feet, &mut self.next_wall);
            if let Some(w) = step.spawned {
                self.walls.apply_spawn(w);
                self.solid.add(w);
                server.hazards.walls.apply_spawn(w);
                self.broadcast(NetEvent::WallSpawn(w));
            }
            for wid in step.killed {
                self.kill_wall(server, wid, tick);
            }
            // time expiry needs no message; the stores just forget it after a grace period
        }
        self.walls.prune(tick, self.life_ticks, 600);
        server.hazards.walls.prune(tick, self.life_ticks, 600);
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
        let snap = build_snapshot(tick, server);
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
            let base = peer.ack_snapshot.and_then(|t| self.history.iter().find(|s| s.tick == t));
            let snap_bytes = encode_delta(base, &snap);
            let mut w = BitWriter::new();
            peer.outbox.write_pending(&mut w, EVENT_BUDGET);
            let own = own_block(server, *id, &std::mem::take(&mut peer.hits));
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
fn own_block(server: &Server, id: u16, hits: &[HitMsg]) -> Vec<u8> {
    let Some(p) = server.players.iter().find(|p| p.id == id) else { return Vec::new() };
    let acked = p.queue.last_executed_tick().filter(|_| p.vitals.alive());
    let extras = OwnExtras {
        has_state: acked.is_some(),
        acked_tick: acked.unwrap_or(0),
        hp_x10: if p.vitals.alive() { (p.vitals.hp * 10.0).round().clamp(1.0, u16::MAX as f32) as u16 } else { 0 },
        hits: hits.to_vec(),
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

fn build_snapshot(tick: u32, server: &Server) -> Snapshot {
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
                },
            )
        })
        .collect();
    entities.sort_by_key(|e| e.0);
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
    fn walls_reach_peers_and_late_joiners() {
        let mut s = world();
        let mut r = Replicator::new(&Cfg::default(), 1).with_features(Features { light_walls: true, obstacles: true });
        r.add_peer(&s, 1, 0);
        // walk player 1 along +x by teleporting its body between frames (server-side test shortcut)
        for i in 0..20 {
            let h = s.players[0].ch.body;
            s.world.bodies[h].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(i as f32 * 0.5, 0.81, 0.0), true);
            s.frame(Duration::from_millis(17));
            r.post_frame(&mut s);
        }
        assert!(r.walls.len() > 5, "trail walls spawned: {}", r.walls.len());
        let round = r.build_round(&s);
        let (events, _, _, _) = datagram::split_snapshot_v3(&round[0].1).unwrap();
        let mut inbox = EventInbox::default();
        let mut client_walls = WallStore::default();
        let (mut obstacle_events, mut other_events) = (0, 0);
        read_events(events, &mut inbox, |e| match e {
            NetEvent::WallSpawn(w) => client_walls.apply_spawn(w),
            NetEvent::ObstacleSpawn(_) => obstacle_events += 1,
            _ => other_events += 1, // match countdown etc.
        })
        .unwrap();
        assert!(obstacle_events >= 3, "wave 0 was broadcast");
        assert_eq!(client_walls.len() + obstacle_events + other_events, inbox.ack() as usize);
        assert!(client_walls.len() > 5);

        // late joiner gets the live walls too
        r.add_peer(&s, 3, s.tick);
        assert_eq!(
            r.peers[&3].outbox.pending(),
            r.walls.live_at(s.tick, r.life_ticks).count() + r.obstacles.not_ended_at(s.tick).count()
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
    fn default_server_makes_no_trail_walls_and_no_obstacles() {
        let mut s = world();
        let mut r = Replicator::new(&Cfg::default(), 1);
        r.add_peer(&s, 1, 0);
        for i in 0..600 {
            let h = s.players[0].ch.body;
            s.world.bodies[h].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation((i % 40) as f32 * 0.5, 0.81, 0.0), true);
            s.frame(Duration::from_millis(17));
            r.post_frame(&mut s);
        }
        assert_eq!((r.walls.len(), r.solid.tracked(), r.solid.colliders(), r.obstacles.not_ended_at(s.tick).count()), (0, 0, 0, 0));
        assert_eq!((s.hazards.walls.len(), s.hazards.obstacles.not_ended_at(s.tick).count()), (0, 0));
        let round = r.build_round(&s);
        let (events, _, _, _) = datagram::split_snapshot_v3(&round[0].1).unwrap();
        let mut inbox = shinobi_sim::events::EventInbox::default();
        read_events(events, &mut inbox, |e| {
            assert!(!matches!(e, NetEvent::WallSpawn(_) | NetEvent::WallKill { .. } | NetEvent::ObstacleSpawn(_)), "{e:?}")
        })
        .unwrap();
        // opting in brings obstacle waves back
        let mut r = Replicator::new(&Cfg::default(), 1).with_features(Features { light_walls: false, obstacles: true });
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
        s.pvp_events.push(PvpEvent::Hit { tick, attacker: 1, victim: 2, amount: 34.4, kind: DamageKind::Slash });
        s.pvp_events.push(PvpEvent::HazardHit { tick, victim: 2, amount: 100.0, source: HazardSource::Wall { id: 9, owner: 1 } });
        s.pvp_events.push(PvpEvent::Kill { tick, killer: 1, victim: 2, kind: DamageKind::Slash });
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
        assert_eq!(attacker.len(), 2); // the hit + the kill marker
        assert!(attacker.iter().all(|h| !h.taken && h.other == 2));
        assert_eq!((attacker[0].amount, attacker[0].kind, attacker[0].kill), (34, KIND_SLASH, false));
        assert!(attacker[1].kill);
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
        // hits are one-shot: next round has none
        assert!(hits_of_next(&mut r, &s, 1).is_empty());

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
            s.pvp_events.push(PvpEvent::Hit { tick, attacker: 1, victim: 2, amount: 1.0, kind: DamageKind::Shuriken });
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
        let mut r = Replicator::new(&Cfg::default(), 1).with_features(Features { light_walls: true, obstacles: true });
        r.add_peer(&s, 1, 0);
        for i in 0..40 {
            let h = s.players[0].ch.body;
            s.world.bodies[h].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(i as f32 * 0.5, 0.81, 0.0), true);
            s.frame(Duration::from_millis(17));
            r.post_frame(&mut s);
        }
        assert_eq!(s.pickups.active().count(), 1);
        assert!(r.walls.len() > 5 && r.solid.tracked() > 5);

        r.add_peer(&s, 7, s.tick);
        let round = r.build_round(&s);
        let mut inbox = shinobi_sim::events::EventInbox::default();
        let mut got = Vec::new();
        read_events(datagram_for(&round, 7).0, &mut inbox, |e| got.push(e)).unwrap();
        assert!(got.iter().any(|e| matches!(e, NetEvent::PickupSpawned { id: 1, .. })), "pickup seeded: {got:?}");
        assert!(got.iter().any(|e| matches!(e, NetEvent::WallSpawn(_))), "walls seeded");

        // host job: NextMap clears server walls, hazard mirror, obstacles, and tells clients
        let tick = s.tick;
        s.match_events.push(MatchEvent::NextMap { tick, map: 1 });
        r.post_frame(&mut s);
        assert_eq!((r.walls.len(), r.solid.tracked(), r.solid.colliders()), (0, 0, 0));
        assert_eq!(s.hazards.walls.len(), 0);
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
