//! Client network session, sans-IO: the platform layer (UDP / WebTransport / test harness) moves bytes,
//! this decides what they mean. Feed datagrams into `on_datagram`, send whatever `next_packet` returns.
//!
//! Handshake (v3, see `shinobi_proto::datagram`): call `hello_due(local_tick)` every tick and send what it returns
//! (HELLO every `HELLO_RETRY_MS`, cookie 0 until a CHALLENGE arrives, newest cookie after). `state()` becomes
//! `Connected` on WELCOME; `connect_failed()` after `CONNECT_TIMEOUT_MS`. Then each sim tick: `next_packet`.

use std::collections::{HashMap, VecDeque};

use shinobi_proto::bits::{BitReader, DecodeError, DecodeResult};
use shinobi_proto::datagram::{
    decode_challenge, decode_kick, encode_hello, split_snapshot_v3, KickReason, Welcome, CONNECT_TIMEOUT_MS,
    HELLO_LEN, HELLO_RETRY_MS, TAG_CHALLENGE, TAG_KICK, TAG_SNAPSHOT, TAG_WELCOME,
};
use shinobi_proto::gameplay::{Board, HitMsg, OwnExtras};
use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, BASE_REDUNDANCY, MAX_REDUNDANCY};
use shinobi_proto::quant::dq_pos3;
use shinobi_proto::snapshot::decode_delta;
use shinobi_proto::TICK_HZ;
use shinobi_sim::blocks::{BlockInfo, BlockRules, Piece, PieceKind};
use shinobi_sim::events::{EventInbox, NetEvent};
use shinobi_sim::own_state::OwnState;

use crate::interp::InterpBuffer;

/// Owner-only authoritative movement state for the reconciler: everything is from the same server moment.
#[derive(Clone, Copy, Debug)]
pub struct OwnUpdate {
    /// Newest of our intents the server had executed (`PlayerIntent::tick`).
    pub acked_tick: u32,
    pub own: OwnState,
    /// Our entity position from the same snapshot, metres (None if the entity is missing from it).
    pub server_pos: Option<[f32; 3]>,
}

/// Intent-lead controller (see `NetClient::on_lead_margin`). Ticks unless noted. The server rejects an intent more
/// than `MAX_FUTURE_TICKS` (12) ahead of its own clock, so the lead must never leave more headroom than that.
const LEAD_MIN: u32 = 4;
const LEAD_MAX: u32 = 16;
/// Headroom the newest intent should have when it reaches the server: below LOW -> grow now.
const MARGIN_LOW: i32 = 2;
/// Headroom that is wasted: the minimum over `SHRINK_WINDOW` blocks at or above this -> shrink by one tick.
const MARGIN_HIGH: i32 = 6;
/// Headroom near the server's ceiling: the minimum over `CEILING_WINDOW` blocks at or above this -> shrink now.
const MARGIN_NEAR_CEILING: i32 = 10;
const MAX_GROW: u32 = 4;
/// Owner blocks (about 30 per second) of evidence needed before shrinking / the ceiling check.
const SHRINK_WINDOW: usize = 150;
const CEILING_WINDOW: usize = 30;
/// Blocks ignored after a change: samples already in flight still describe the old lead (about one RTT).
const SETTLE_BLOCKS: u32 = 12;

const MS_PER_TICK: f64 = 1000.0 / TICK_HZ as f64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnState {
    Connecting,
    Connected { player_id: u16 },
}

pub struct NetClient {
    state: ConnState,
    seq: u16,
    /// Newest first, contiguous ticks; resent for loss tolerance.
    recent: VecDeque<PlayerIntent>,
    pub interp: InterpBuffer,
    inbox: EventInbox,
    events: Vec<NetEvent>,
    /// Hooked ropes of other players (and us): player id -> anchor in metres. Kept from the reliable events.
    ropes: HashMap<u16, [f32; 3]>,
    /// Team of every player (from `TeamAssign`), team kills and the match result (from `TeamScore`).
    teams: HashMap<u16, u8>,
    team_score: [u16; 2],
    team_result: u8,
    /// Live player-built blocks: grid cell -> owner id (from `BlockPlaced` / `BlockRemoved`).
    blocks: HashMap<[i32; 3], (u16, Piece, u8)>,
    /// The server's build rules (`NetEvent::BlockRules`), once received.
    block_rules: Option<BlockRules>,
    /// (snapshot tick, local tick at arrival): anchor for the server-clock estimate.
    anchor: Option<(u32, f64)>,
    kicked: Option<KickReason>,
    own: Option<OwnUpdate>,
    /// Handshake: echoed in HELLO; 0 until the server's CHALLENGE.
    cookie: u64,
    connect_started: Option<f64>,
    last_hello: Option<f64>,
    /// Snapshot tick of the newest datagram whose owner extras (hits, hp) we applied: drops duplicates/reordered.
    last_extras_tick: Option<u32>,
    /// Wrapping seq of the newest hit already surfaced; hits are resent by the server, so dedup by seq.
    last_hit_seq: Option<u8>,
    hp_x10: Option<u16>,
    hits: Vec<HitMsg>,
    board: Option<Board>,
    /// Last tick handed out by `pace_tick`.
    cursor: Option<u32>,
    /// Server's `intent_complete`: every intent up to this tick has arrived; edges at or below it need no re-send.
    intent_complete: u32,
    /// Adaptive intent lead (ticks the stamp runs ahead of the server-clock estimate); `None` until `pace_tick`
    /// sets it from its argument. See `on_lead_margin`.
    lead: Option<u32>,
    /// Recent margins (newest last) and the settle counter, see `on_lead_margin`.
    lead_margins: VecDeque<i32>,
    lead_settle: u32,
    /// Stamp adjustment queued by a lead change: +n skips n tick numbers, -n repeats one stamp n times.
    lead_slew: i32,
    /// The last `pace_tick` repeated the previous stamp (the lead just shrank): the shell must not simulate it.
    stamp_held: bool,
    /// Datagrams that failed to decode (for a debug overlay / disconnect heuristics).
    pub bad_datagrams: u32,
}

impl Default for NetClient {
    fn default() -> Self {
        Self {
            state: ConnState::Connecting,
            seq: 0,
            recent: VecDeque::new(),
            interp: InterpBuffer::default(),
            inbox: EventInbox::default(),
            events: Vec::new(),
            ropes: HashMap::new(),
            teams: HashMap::new(),
            team_score: [0; 2],
            team_result: 0,
            blocks: HashMap::new(),
            block_rules: None,
            anchor: None,
            kicked: None,
            own: None,
            cookie: 0,
            connect_started: None,
            last_hello: None,
            last_extras_tick: None,
            last_hit_seq: None,
            hp_x10: None,
            hits: Vec::new(),
            board: None,
            cursor: None,
            intent_complete: 0,
            lead: None,
            lead_margins: VecDeque::new(),
            lead_settle: 0,
            lead_slew: 0,
            stamp_held: false,
            bad_datagrams: 0,
        }
    }
}

impl NetClient {
    pub fn state(&self) -> ConnState {
        self.state
    }

    pub fn player_id(&self) -> Option<u16> {
        match self.state {
            ConnState::Connected { player_id } => Some(player_id),
            ConnState::Connecting => None,
        }
    }

    /// `Some(reason)` once the server dropped us (anti-cheat, flood, timeout, full). Terminal: reconnect with a
    /// fresh `NetClient` (cheat/flood bans last ~10 s server-side).
    pub fn kicked(&self) -> Option<KickReason> {
        self.kicked
    }

    /// HELLO with the current cookie (0 until the server's CHALLENGE).
    pub fn hello(&self) -> [u8; HELLO_LEN] {
        encode_hello(self.cookie)
    }

    /// Retransmit policy: `Some(HELLO)` when one is due (first call, then every `HELLO_RETRY_MS`) while
    /// connecting; `None` once connected, kicked or failed. Call every tick.
    pub fn hello_due(&mut self, local_tick: f64) -> Option<[u8; HELLO_LEN]> {
        if self.state != ConnState::Connecting || self.kicked.is_some() || self.connect_failed(local_tick) {
            return None;
        }
        self.connect_started.get_or_insert(local_tick);
        let due = self.last_hello.map_or(true, |t| (local_tick - t) * MS_PER_TICK >= HELLO_RETRY_MS as f64);
        due.then(|| {
            self.last_hello = Some(local_tick);
            self.hello()
        })
    }

    /// No WELCOME within `CONNECT_TIMEOUT_MS` of the first HELLO (server down, blocked, or version mismatch
    /// without a KICK getting through).
    pub fn connect_failed(&self, local_tick: f64) -> bool {
        self.state == ConnState::Connecting
            && self.connect_started.map_or(false, |t| (local_tick - t) * MS_PER_TICK >= CONNECT_TIMEOUT_MS as f64)
    }

    /// `local_tick`: client clock in (fractional) sim ticks; any monotonic clock at 60 ticks/s works.
    pub fn on_datagram(&mut self, buf: &[u8], local_tick: f64) -> DecodeResult<()> {
        let r = match buf.first() {
            // before everything else: valid even while still connecting (server full) and repeated by the server
            Some(&TAG_KICK) => decode_kick(buf).map(|why| {
                self.kicked.get_or_insert(why);
            }),
            _ if self.kicked.is_some() => Ok(()), // dropped by the server: ignore stragglers
            Some(&TAG_CHALLENGE) => decode_challenge(buf).map(|c| {
                // newest wins while connecting; ignored once connected (stale retransmits)
                if self.state == ConnState::Connecting {
                    self.cookie = c;
                }
            }),
            Some(&TAG_WELCOME) => self.on_welcome(buf, local_tick),
            Some(&TAG_SNAPSHOT) if self.player_id().is_some() => self.on_snapshot(buf, local_tick),
            _ => Err(DecodeError::Malformed),
        };
        if r.is_err() {
            self.bad_datagrams += 1;
        }
        r
    }

    fn on_welcome(&mut self, buf: &[u8], local_tick: f64) -> DecodeResult<()> {
        let w = Welcome::decode(buf)?;
        // Welcome is re-sent if the first was lost; ignore repeats so the clock anchor isn't reset.
        if self.state == ConnState::Connecting {
            self.state = ConnState::Connected { player_id: w.player_id };
            self.anchor = Some((w.server_tick, local_tick));
        }
        Ok(())
    }

    fn on_snapshot(&mut self, buf: &[u8], local_tick: f64) -> DecodeResult<()> {
        let (ev_bytes, own_bytes, board_bytes, snap_bytes) = split_snapshot_v3(buf)?;
        // decode everything before applying any of it: a corrupt datagram must not half-apply
        let snap = decode_delta(snap_bytes, |t| self.interp.get(t))?;
        let mut pending = Vec::new();
        self.inbox.read(&mut BitReader::new(ev_bytes), |e| pending.push(e))?;
        let extras = if own_bytes.is_empty() {
            None
        } else {
            Some(OwnExtras::read(&mut BitReader::new(own_bytes), OwnState::read)?)
        };
        let board = if board_bytes.is_empty() { None } else { Some(Board::decode(board_bytes)?) };

        for ev in &pending {
            match *ev {
                NetEvent::RopeAttach { player, anchor, .. } => {
                    self.ropes.insert(player, dq_pos3(anchor));
                }
                NetEvent::RopeRelease { player, .. } | NetEvent::Kill { victim: player, .. } => {
                    self.ropes.remove(&player);
                }
                NetEvent::MapChange { .. } => self.ropes.clear(),
                NetEvent::TeamAssign { player, team } if team == shinobi_sim::events::TEAM_NONE => {
                    self.teams.remove(&player);
                }
                NetEvent::TeamAssign { player, team } => {
                    self.teams.insert(player, team);
                }
                NetEvent::BlockPlaced { owner, cell, kind, axis, scale_q } => {
                    self.blocks.insert(cell, (owner, Piece { kind: PieceKind::from_id(kind), axis }, scale_q));
                }
                NetEvent::BlockRules(r) => self.block_rules = Some(r),
                NetEvent::BlockRemoved { cell } | NetEvent::BlockDerez { cell } => {
                    self.blocks.remove(&cell);
                }
                NetEvent::TeamScore { score, result, .. } => {
                    self.team_score = score;
                    self.team_result = result;
                }
                _ => {}
            }
        }
        self.events.extend(pending);
        if let Some(b) = board {
            self.board = Some(b);
        }
        // hits are unreliable but resent by the server with a seq; a duplicated or reordered datagram must not replay them
        if let Some((x, state)) = extras {
            if self.last_extras_tick.map_or(true, |t| snap.tick > t) {
                self.last_extras_tick = Some(snap.tick);
                self.hp_x10 = Some(x.hp_x10);
                self.intent_complete = self.intent_complete.max(x.intent_complete);
                self.on_lead_margin(x.lead_margin as i32);
                let n = x.hits.len();
                for (i, h) in x.hits.into_iter().enumerate() {
                    let seq = x.hit_seq.wrapping_sub((n - 1 - i) as u8);
                    // newer than the last one surfaced (wrapping window of 127)
                    if self.last_hit_seq.map_or(true, |l| (1..128).contains(&seq.wrapping_sub(l))) {
                        self.last_hit_seq = Some(seq);
                        self.hits.push(h);
                    }
                }
                if let Some(own) = state {
                    let me = self.player_id();
                    let server_pos = me.and_then(|id| snap.get(id)).map(|e| dq_pos3(e.pos));
                    self.own = Some(OwnUpdate { acked_tick: x.acked_tick, own, server_pos });
                }
            }
        }
        if self.anchor.map_or(true, |(t, _)| snap.tick >= t) {
            self.anchor = Some((snap.tick, local_tick));
        }
        self.interp.push(snap, local_tick);
        Ok(())
    }

    /// Exact own health in `0.0..=100.0`-style units (server hp x10 / 10); `None` before the first extras block.
    pub fn hp(&self) -> Option<f32> {
        self.hp_x10.map(|h| h as f32 / 10.0)
    }

    /// Hits since last call: `taken == false` = you dealt it (hit marker), `true` = you took it.
    pub fn take_hits(&mut self) -> Vec<HitMsg> {
        std::mem::take(&mut self.hits)
    }

    /// Latest scoreboard + match phase.
    pub fn board(&self) -> Option<&Board> {
        self.board.as_ref()
    }

    /// Latest owner-state update since the last call (feed to `Reconciler::on_server_own_state`).
    pub fn take_own_state(&mut self) -> Option<OwnUpdate> {
        self.own.take()
    }

    /// Best guess of the server's current tick.
    pub fn server_tick_estimate(&self, local_tick: f64) -> Option<f64> {
        self.anchor.map(|(t, at)| t as f64 + (local_tick - at))
    }

    /// Tick to stamp on the intent produced now. The server only executes an intent on its exact tick and drops
    /// stale ones, so input must arrive slightly early: the clock estimate plus `lead_ticks` of headroom for
    /// one-way latency + jitter (~3-6 ticks on a good link; grow it with measured RTT).
    pub fn input_tick(&self, local_tick: f64, lead_ticks: u32) -> Option<u32> {
        self.server_tick_estimate(local_tick).map(|s| s.ceil() as u32 + lead_ticks)
    }

    /// Call once per sim tick to get the tick for that tick's intent. Increments by 1 so redundancy stays
    /// contiguous, and re-anchors to `input_tick` only when it drifted by more than `MAX_DRIFT_TICKS`
    /// (hitch, clock correction), so jitter in the clock estimate never makes tick numbers skip or repeat.
    ///
    /// `lead_ticks` is only the STARTING lead: the server reports how much headroom each intent had
    /// (`OwnExtras::lead_margin`) and `on_lead_margin` grows / shrinks the lead from that, so the intent stays ahead
    /// of its tick on slow or jittery links without a hand-tuned constant.
    pub fn pace_tick(&mut self, local_tick: f64, lead_ticks: u32) -> Option<u32> {
        const MAX_DRIFT_TICKS: i64 = 8;
        let lead = *self.lead.get_or_insert(lead_ticks.clamp(LEAD_MIN, LEAD_MAX));
        let want = self.input_tick(local_tick, lead)?;
        self.stamp_held = false;
        let next = match self.cursor {
            // a pending lead change moves the stamp now: skip ahead (grow) or hold one stamp (shrink)
            Some(c) if self.lead_slew > 0 => {
                let skip = self.lead_slew;
                self.lead_slew = 0;
                c.wrapping_add(1).wrapping_add(skip as u32)
            }
            Some(c) if self.lead_slew < 0 => {
                self.lead_slew += 1;
                self.stamp_held = true;
                c // repeat the stamp: the lead shrinks by one tick; the shell skips this local tick (`stamp_held`)
            }
            Some(c) if (c as i64 + 1 - want as i64).abs() <= MAX_DRIFT_TICKS => c + 1,
            _ => want,
        };
        self.cursor = Some(next);
        Some(next)
    }

    /// True when the stamp returned by the last `pace_tick` is a repeat (the lead shrank by one tick): the shell must
    /// not produce an intent or simulate a predicted step for it, so prediction stays in step with the server.
    pub fn stamp_held(&self) -> bool {
        self.stamp_held
    }

    /// Current intent lead in ticks (`None` before the first `pace_tick`).
    pub fn lead(&self) -> Option<u32> {
        self.lead
    }

    /// Server feedback: `margin` = smallest headroom (ticks) the newest intent had over the server clock since the
    /// last owner block (negative = it arrived after its tick and was executed late / as a repeat).
    ///
    /// The clock estimate and the link are noisy, so the controller is deliberately slow and one-sided:
    /// * grow at once when a block shows less than `MARGIN_LOW` headroom (by the shortfall, at most `MAX_GROW`);
    /// * shrink by one tick only when even the best of the last `SHRINK_WINDOW` blocks had `MARGIN_HIGH`+ spare,
    ///   or, near the server's future-tick ceiling, when all of the last `CEILING_WINDOW` blocks did;
    /// * after any change ignore the next `SETTLE_BLOCKS` blocks (they still describe the old lead) and start the
    ///   evidence window afresh, so growth never compounds on stale samples and it cannot oscillate.
    pub fn on_lead_margin(&mut self, margin: i32) {
        let Some(lead) = self.lead else { return };
        if margin >= shinobi_proto::gameplay::LEAD_MARGIN_MAX as i32 {
            return; // "no measurement" (the server had no packet to measure since the last block)
        }
        if self.lead_settle > 0 {
            self.lead_settle -= 1;
            return;
        }
        self.lead_margins.push_back(margin);
        while self.lead_margins.len() > SHRINK_WINDOW {
            self.lead_margins.pop_front();
        }
        let min_of = |n: usize, m: &VecDeque<i32>| m.iter().rev().take(n).copied().min().unwrap_or(i32::MIN);
        let change: i32 = if margin < MARGIN_LOW {
            ((MARGIN_LOW - margin) as u32).min(MAX_GROW).min(LEAD_MAX - lead) as i32
        } else if self.lead_margins.len() >= CEILING_WINDOW && min_of(CEILING_WINDOW, &self.lead_margins) >= MARGIN_NEAR_CEILING {
            let excess = (min_of(CEILING_WINDOW, &self.lead_margins) - MARGIN_HIGH + 1).max(1) as u32;
            -(excess.min(lead - LEAD_MIN.min(lead)) as i32)
        } else if self.lead_margins.len() >= SHRINK_WINDOW && min_of(SHRINK_WINDOW, &self.lead_margins) >= MARGIN_HIGH {
            -(1.min(lead - LEAD_MIN.min(lead)) as i32)
        } else {
            0
        };
        if change != 0 {
            self.lead = Some((lead as i32 + change) as u32);
            self.lead_slew += change;
            self.lead_margins.clear();
            self.lead_settle = SETTLE_BLOCKS;
        }
    }

    /// Tick the interpolated world should be drawn at.
    pub fn render_tick(&self, local_tick: f64) -> Option<f64> {
        self.server_tick_estimate(local_tick).map(|s| self.interp.render_tick(s))
    }

    /// Live blocks as `(owner, cell)`, sorted by cell. Centre = `(cell + 0.5) * block size` (size 1.0 by default).
    pub fn blocks(&self) -> Vec<(u16, [i32; 3])> {
        let mut v: Vec<_> = self.blocks.iter().map(|(c, (o, _, _))| (*o, *c)).collect();
        v.sort_by_key(|&(_, c)| c);
        v
    }

    /// Build rules from the server (None until the join catch-up arrives).
    pub fn block_rules(&self) -> Option<BlockRules> {
        self.block_rules
    }

    /// Live pieces with kind and scale, sorted by cell (what the shell draws and the predicted world collides with).
    pub fn block_pieces(&self) -> Vec<BlockInfo> {
        let mut v: Vec<_> = self.blocks.iter().map(|(c, (o, p, s))| BlockInfo { owner: *o, cell: *c, piece: *p, scale_q: *s }).collect();
        v.sort_by_key(|b| b.cell);
        v
    }

    /// `player`'s team (`shinobi_sim::team::TEAM_*`), once the server has told us.
    pub fn team_of(&self, player: u16) -> Option<u8> {
        self.teams.get(&player).copied()
    }

    /// How many players' teams we know (headless health checks).
    pub fn teams_known(&self) -> usize {
        self.teams.len()
    }

    /// Our own team.
    pub fn my_team(&self) -> Option<u8> {
        self.player_id().and_then(|id| self.team_of(id))
    }

    /// Team kills `[cyan, orange]` (all zero outside team deathmatch).
    pub fn team_scores(&self) -> [u16; 2] {
        self.team_score
    }

    /// The tied team match is in sudden death (first team kill wins).
    pub fn in_sudden_death(&self) -> bool {
        self.team_result == shinobi_sim::events::TEAM_RESULT_SUDDEN
    }

    /// `shinobi_sim::events::TEAM_RESULT_*`: running, sudden death, or who won the match that just ended.
    pub fn team_result(&self) -> u8 {
        self.team_result
    }

    /// Where `player`'s rope is hooked, if it is (from `RopeAttach` / `RopeRelease`).
    pub fn rope_anchor(&self, player: u16) -> Option<[f32; 3]> {
        self.ropes.get(&player).copied()
    }

    /// Reliable events (walls, obstacles) received since last call, in order.
    pub fn take_events(&mut self) -> Vec<NetEvent> {
        std::mem::take(&mut self.events)
    }

    /// Build the datagram for this tick's intent (with redundancy, acks and view time). `None` until connected.
    pub fn next_packet(&mut self, intent: PlayerIntent, local_tick: f64) -> Option<Vec<u8>> {
        self.player_id()?;
        if self.kicked.is_some() {
            return None;
        }
        // redundancy needs contiguous ticks; a gap (hitch, pause) restarts the window
        if self.recent.front().map_or(false, |p| p.tick.wrapping_add(1) != intent.tick) {
            self.recent.clear();
        }
        self.recent.push_front(intent);
        self.recent.truncate(MAX_REDUNDANCY);
        // Redundancy: the newest BASE_REDUNDANCY intents always; further back (up to MAX_REDUNDANCY) only to cover a
        // one-shot action (edge) the server has not confirmed yet (`intent_complete`), so a lost or late packet can
        // never swallow a dash / jump / zip / build: it is re-sent in every datagram until acked.
        let is_edge = |it: &PlayerIntent| it.buttons.has(Buttons::EDGES) || it.ultimate || it.build || it.zip;
        let mut n = BASE_REDUNDANCY.min(self.recent.len());
        for (i, it) in self.recent.iter().enumerate() {
            if it.tick > self.intent_complete && is_edge(it) {
                n = n.max(i + 1);
            }
        }

        let render = self.render_tick(local_tick).unwrap_or(0.0);
        let pkt = ClientPacket {
            seq: self.seq,
            ack_snapshot_tick: self.interp.newest_tick().unwrap_or(0),
            ack_event_seq: self.inbox.ack(),
            view: InterpBuffer::view_state(render),
            intents: self.recent.iter().take(n).copied().collect(),
        };
        self.seq = self.seq.wrapping_add(1);
        pkt.encode().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shinobi_proto::bits::BitWriter;
    use shinobi_proto::datagram::frame_snapshot_v3;
    use shinobi_proto::gameplay::{Board, HitMsg, OwnExtras};
    use shinobi_proto::intent::Buttons;
    use shinobi_proto::snapshot::{encode_delta, EntityState, Snapshot};
    use shinobi_sim::events::EventOutbox;

    fn snap(tick: u32, x: i32) -> Snapshot {
        Snapshot { tick, entities: vec![(1, EntityState { pos: [x, 512, 0], ..Default::default() })] }
    }

    /// What the server sends when nothing is pending: a zero event count.
    fn no_events() -> Vec<u8> {
        let mut w = BitWriter::new();
        EventOutbox::default().write_pending(&mut w, 256);
        w.finish()
    }

    fn welcome(c: &mut NetClient, id: u16, tick: u32) {
        c.on_datagram(&Welcome { player_id: id, server_tick: tick }.encode(), 0.0).unwrap();
    }

    fn intent(tick: u32) -> PlayerIntent {
        PlayerIntent { tick, buttons: Buttons(Buttons::WALL_CLING), move_z: 100, ..Default::default() }
    }

    #[test]
    fn handshake_then_snapshots_then_events_ack() {
        let mut c = NetClient::default();
        assert!(c.next_packet(intent(1), 0.0).is_none(), "no input before WELCOME");
        assert!(c.on_datagram(&frame_snapshot_v3(&no_events(), &[], &[], &[]), 0.0).is_err(), "snapshot before WELCOME rejected");
        welcome(&mut c, 7, 1000);
        assert_eq!(c.player_id(), Some(7));
        let first = ClientPacket::decode(&c.next_packet(intent(1), 0.0).unwrap()).unwrap();
        assert_eq!(first.ack_snapshot_tick, 0, "0 = no snapshot yet: server must send a full one");

        // full snapshot + one reliable wall event in the same datagram
        let mut ob = EventOutbox::default();
        ob.push(NetEvent::WallKill { id: 3, tick: 1002 });
        let mut w = BitWriter::new();
        ob.write_pending(&mut w, 256);
        let dgram = frame_snapshot_v3(&w.finish(), &[], &[], &encode_delta(None, &snap(1002, 4000)));
        c.on_datagram(&dgram, 2.0).unwrap();
        assert_eq!(c.take_events(), vec![NetEvent::WallKill { id: 3, tick: 1002 }]);
        assert!(c.take_events().is_empty());
        assert_eq!(c.interp.newest_tick(), Some(1002));

        // redelivery (server resends until acked) applies once
        c.on_datagram(&dgram, 2.1).unwrap_or(());
        assert!(c.take_events().is_empty());

        // next delta decodes against the stored baseline
        let d2 = encode_delta(Some(&snap(1002, 4000)), &snap(1004, 4200));
        c.on_datagram(&frame_snapshot_v3(&no_events(), &[], &[], &d2), 4.0).unwrap();
        assert_eq!(c.interp.newest_tick(), Some(1004));

        // outgoing packet acks what we hold
        let bytes = c.next_packet(intent(10), 4.0).unwrap();
        let pkt = ClientPacket::decode(&bytes).unwrap();
        assert_eq!(pkt.ack_snapshot_tick, 1004);
        assert_eq!(pkt.ack_event_seq, 1);
        assert_eq!(pkt.intents[0], intent(10));
    }

    #[test]
    fn redundancy_window_and_gap_reset() {
        let mut c = NetClient::default();
        welcome(&mut c, 1, 0);
        let mut last = None;
        for t in 1..=6 {
            last = c.next_packet(intent(t), t as f64);
        }
        let pkt = ClientPacket::decode(&last.unwrap()).unwrap();
        assert_eq!(pkt.intents.len(), BASE_REDUNDANCY, "nothing pending: only the base window");
        assert_eq!(pkt.intents.iter().map(|i| i.tick).collect::<Vec<_>>(), vec![6, 5, 4, 3]);
        let pkt = ClientPacket::decode(&c.next_packet(intent(50), 50.0).unwrap()).unwrap();
        assert_eq!(pkt.intents.len(), 1, "tick gap restarts the window");
        assert_eq!(pkt.seq, 6);
    }

    /// A one-shot action (zip / dash / jump / build) keeps riding in every datagram until the server's
    /// `intent_complete` covers it; then the window shrinks back to the base.
    #[test]
    fn unacked_edges_are_resent_until_the_server_confirms_them() {
        let mut c = NetClient::default();
        welcome(&mut c, 1, 0);
        let mut packet = |c: &mut NetClient, t: u32, zip: bool| {
            let it = PlayerIntent { zip, ..intent(t) };
            ClientPacket::decode(&c.next_packet(it, t as f64).unwrap()).unwrap()
        };
        for t in 1..=9 {
            packet(&mut c, t, t == 3); // zip pressed at tick 3
        }
        let p = packet(&mut c, 10, false);
        assert!(p.intents.iter().any(|i| i.tick == 3 && i.zip), "tick 3's zip still rides at tick 10: {:?}", p.intents.iter().map(|i| i.tick).collect::<Vec<_>>());
        assert_eq!(p.intents.len(), 8, "window stretched back to the edge (ticks 10..=3)");
        // the server's owner block reports every intent up to tick 5 as received
        let d = frame_snapshot_v3(&no_events(), &own_block_full(true, 4, 1000, vec![], 5, 6), &[], &encode_delta(None, &snap(20, 0)));
        c.on_datagram(&d, 11.0).unwrap();
        assert_eq!(packet(&mut c, 11, false).intents.len(), BASE_REDUNDANCY, "confirmed: back to the base window");
        // a never-confirmed edge is capped at MAX_REDUNDANCY
        let mut c = NetClient::default();
        welcome(&mut c, 1, 0);
        packet(&mut c, 1, true);
        let mut last = None;
        for t in 2..=40 {
            last = Some(packet(&mut c, t, false));
        }
        assert!(last.unwrap().intents.len() <= MAX_REDUNDANCY);
    }

    fn lead_client(start: u32) -> NetClient {
        let mut c = NetClient::default();
        welcome(&mut c, 1, 100);
        assert_eq!(c.pace_tick(0.0, start).map(|_| c.lead()), Some(Some(start)));
        c
    }

    #[test]
    fn lead_grows_at_once_on_a_late_block_then_ignores_stale_samples() {
        let mut c = lead_client(8);
        c.on_lead_margin(-3); // 5 ticks short of the target: grow by MAX_GROW (4)
        assert_eq!(c.lead(), Some(12));
        for _ in 0..SETTLE_BLOCKS {
            c.on_lead_margin(-5); // still describe the old lead: must not compound
        }
        assert_eq!(c.lead(), Some(12), "samples in flight during the settle window are ignored");
        c.on_lead_margin(0); // genuinely still short afterwards: grows again
        assert_eq!(c.lead(), Some(14));
        for _ in 0..100 {
            c.on_lead_margin(-16);
        }
        assert!(c.lead().unwrap() <= 16, "never beyond LEAD_MAX: {:?}", c.lead());
    }

    #[test]
    fn lead_shrinks_slowly_only_on_sustained_spare_headroom() {
        let mut c = lead_client(14);
        // a single dip below the shrink threshold within the window blocks the shrink
        for i in 0..SHRINK_WINDOW + 5 {
            c.on_lead_margin(if i == 100 { 4 } else { 8 });
        }
        assert_eq!(c.lead(), Some(14), "one low sample in the window keeps the lead");
        // a clean window of spare headroom shrinks by exactly one tick, then waits for fresh evidence
        let mut c = lead_client(14);
        for _ in 0..SHRINK_WINDOW + SETTLE_BLOCKS as usize + 5 {
            c.on_lead_margin(8);
        }
        assert_eq!(c.lead(), Some(13));
        // inside the hysteresis band (between LOW and HIGH) nothing ever changes
        let mut c = lead_client(10);
        for _ in 0..1000 {
            c.on_lead_margin(MARGIN_LOW + 1);
        }
        assert_eq!(c.lead(), Some(10));
        // never below LEAD_MIN
        let mut c = lead_client(LEAD_MIN);
        for _ in 0..2000 {
            c.on_lead_margin(15);
        }
        assert_eq!(c.lead(), Some(LEAD_MIN));
    }

    #[test]
    fn lead_comes_down_fast_near_the_servers_future_tick_ceiling() {
        // the server rejects intents more than 12 ticks ahead: sustained margins of 10+ must not wait 5 s
        let mut c = lead_client(16);
        for _ in 0..CEILING_WINDOW {
            c.on_lead_margin(11);
        }
        assert!(c.lead().unwrap() < 16, "{:?}", c.lead());
    }

    #[test]
    fn a_lead_change_moves_the_stamp_now() {
        let mut c = NetClient::default();
        welcome(&mut c, 1, 100);
        let a = c.pace_tick(0.0, 8).unwrap();
        let b = c.pace_tick(1.0, 8).unwrap();
        assert_eq!(b, a + 1);
        c.on_lead_margin(-2); // grow by 4: the next stamp skips ahead by 4
        assert_eq!(c.pace_tick(2.0, 8), Some(b + 1 + 4));
        assert_eq!(c.pace_tick(3.0, 8), Some(b + 6), "then it counts up normally again");
    }

    #[test]
    fn corrupt_or_baselineless_datagrams_are_counted_not_applied() {
        let mut c = NetClient::default();
        welcome(&mut c, 1, 0);
        let orphan = encode_delta(Some(&snap(90, 0)), &snap(92, 10)); // baseline we never received
        assert_eq!(c.on_datagram(&frame_snapshot_v3(&no_events(), &[], &[], &orphan), 1.0), Err(DecodeError::BaselineMissing(90)));
        assert!(c.on_datagram(&[TAG_SNAPSHOT, 9], 1.0).is_err());
        assert!(c.on_datagram(&[], 1.0).is_err());
        assert_eq!(c.bad_datagrams, 3);
        assert_eq!(c.interp.newest_tick(), None);
    }

    fn own_block_full(has_state: bool, acked: u32, hp_x10: u16, hits: Vec<HitMsg>, intent_complete: u32, lead_margin: i8) -> Vec<u8> {
        let x = OwnExtras { has_state, acked_tick: acked, hp_x10, hits, hit_seq: 1, intent_complete, lead_margin };
        let mut w = BitWriter::new();
        x.write(&mut w, |w| OwnState::default().write(w));
        w.finish()
    }

    fn own_block(has_state: bool, acked: u32, hp_x10: u16, hits: Vec<HitMsg>) -> Vec<u8> {
        let x = OwnExtras { has_state, acked_tick: acked, hp_x10, hits, hit_seq: 1, ..Default::default() };
        let mut w = BitWriter::new();
        x.write(&mut w, |w| OwnState::default().write(w));
        w.finish()
    }

    #[test]
    fn block_events_keep_the_live_block_set() {
        let mut c = NetClient::default();
        welcome(&mut c, 1, 0);
        let mut ob = EventOutbox::default();
        ob.push(NetEvent::BlockPlaced { owner: 2, cell: [1, 0, 0], kind: 0, axis: 0, scale_q: 16 });
        ob.push(NetEvent::BlockPlaced { owner: 1, cell: [0, 0, 5], kind: 1, axis: 1, scale_q: 16 });
        ob.push(NetEvent::BlockPlaced { owner: 1, cell: [1, 0, 0], kind: 0, axis: 0, scale_q: 16 }); // re-sent catch-up: same cell, idempotent
        ob.push(NetEvent::BlockRemoved { cell: [0, 0, 5] });
        let mut w = BitWriter::new();
        ob.write_pending(&mut w, 480);
        c.on_datagram(&frame_snapshot_v3(&w.finish(), &[], &[], &encode_delta(None, &snap(10, 0))), 1.0).unwrap();
        assert_eq!(c.blocks(), vec![(1, [1, 0, 0])]);
    }

    #[test]
    fn team_events_fill_team_of_and_scores() {
        use shinobi_sim::events::{TEAM_NONE, TEAM_RESULT_CYAN};
        let mut c = NetClient::default();
        welcome(&mut c, 1, 0);
        let mut ob = EventOutbox::default();
        ob.push(NetEvent::TeamAssign { player: 1, team: 1 });
        ob.push(NetEvent::TeamAssign { player: 2, team: 0 });
        ob.push(NetEvent::TeamScore { tick: 9, score: [50, 31], result: TEAM_RESULT_CYAN });
        let mut w = BitWriter::new();
        ob.write_pending(&mut w, 480);
        c.on_datagram(&frame_snapshot_v3(&w.finish(), &[], &[], &encode_delta(None, &snap(10, 0))), 1.0).unwrap();
        assert_eq!((c.my_team(), c.team_of(2), c.team_of(7)), (Some(1), Some(0), None));
        assert_eq!((c.team_scores(), c.team_result()), ([50, 31], TEAM_RESULT_CYAN));
        ob.push(NetEvent::TeamAssign { player: 2, team: TEAM_NONE });
        let mut w = BitWriter::new();
        ob.write_pending(&mut w, 480);
        c.on_datagram(&frame_snapshot_v3(&w.finish(), &[], &[], &encode_delta(None, &snap(11, 0))), 1.1).unwrap();
        assert_eq!(c.team_of(2), None, "left players are forgotten");
        assert!(!c.in_sudden_death());
        ob.push(NetEvent::TeamScore { tick: 12, score: [50, 50], result: shinobi_sim::events::TEAM_RESULT_SUDDEN });
        let mut w = BitWriter::new();
        ob.write_pending(&mut w, 480);
        c.on_datagram(&frame_snapshot_v3(&w.finish(), &[], &[], &encode_delta(None, &snap(12, 0))), 1.2).unwrap();
        assert!(c.in_sudden_death());
    }

    #[test]
    fn rope_anchor_follows_attach_release_and_death() {
        let mut c = NetClient::default();
        welcome(&mut c, 1, 0);
        let mut ob = EventOutbox::default();
        ob.push(NetEvent::RopeAttach { tick: 5, player: 2, anchor: [512, 1024, -512] });
        let mut w = BitWriter::new();
        ob.write_pending(&mut w, 480);
        let d = frame_snapshot_v3(&w.finish(), &[], &[], &encode_delta(None, &snap(10, 0)));
        c.on_datagram(&d, 1.0).unwrap();
        assert_eq!(c.rope_anchor(2), Some([1.0, 2.0, -1.0]));
        assert_eq!(c.rope_anchor(3), None);
        ob.push(NetEvent::Kill { tick: 6, killer: 3, victim: 2, kind: 0 });
        let mut w = BitWriter::new();
        ob.write_pending(&mut w, 480);
        let d = frame_snapshot_v3(&w.finish(), &[], &[], &encode_delta(None, &snap(11, 0)));
        c.on_datagram(&d, 1.1).unwrap();
        assert_eq!(c.rope_anchor(2), None, "a dead player's rope is gone");
    }

    #[test]
    fn resent_hits_survive_loss_and_surface_once() {
        let mk = |other: u16| HitMsg { other, amount: 10, kind: 0, taken: false, kill: false };
        let blk = |hits: Vec<HitMsg>, seq: u8| {
            let x = OwnExtras { has_state: false, acked_tick: 0, hp_x10: 1000, hits, hit_seq: seq, ..Default::default() };
            let mut w = BitWriter::new();
            x.write(&mut w, |w| OwnState::default().write(w));
            w.finish()
        };
        let mut c = NetClient::default();
        welcome(&mut c, 1, 0);
        // datagram carrying hit #254 (and the seq wraps to #0, #1) is lost; the next two resend it
        let d2 = frame_snapshot_v3(&no_events(), &blk(vec![mk(1), mk(2)], 255), &[], &encode_delta(None, &snap(10, 0)));
        let d3 = frame_snapshot_v3(&no_events(), &blk(vec![mk(1), mk(2), mk(3)], 0), &[], &encode_delta(None, &snap(11, 0)));
        let d4 = frame_snapshot_v3(&no_events(), &blk(vec![mk(2), mk(3)], 0), &[], &encode_delta(None, &snap(12, 0)));
        for (i, d) in [d2, d3, d4].iter().enumerate() {
            c.on_datagram(d, 1.0 + i as f64).unwrap();
        }
        let got: Vec<u16> = c.take_hits().iter().map(|h| h.other).collect();
        assert_eq!(got, vec![1, 2, 3], "each hit once, across the seq wrap");
    }

    #[test]
    fn own_extras_surface_with_position_hp_and_hits() {
        let mut c = NetClient::default();
        welcome(&mut c, 1, 0); // our entity id = 1 (see snap())
        let hit = HitMsg { other: 2, amount: 30, kind: 0, taken: false, kill: false };
        let d = frame_snapshot_v3(&no_events(), &own_block(true, 77, 755, vec![hit]), &[], &encode_delta(None, &snap(10, 6400)));
        c.on_datagram(&d, 1.0).unwrap();
        let u = c.take_own_state().expect("own update");
        assert_eq!(u.acked_tick, 77);
        assert!(u.server_pos.is_some());
        assert!(c.take_own_state().is_none(), "taken once");
        assert_eq!(c.hp(), Some(75.5));
        assert_eq!(c.take_hits(), vec![hit]);
        // the same datagram again (network duplication) must not replay the hit marker
        c.on_datagram(&d, 1.1).unwrap_or(());
        assert!(c.take_hits().is_empty());
        // dead: extras only, no movement state
        let d2 = frame_snapshot_v3(&no_events(), &own_block(false, 0, 0, vec![]), &[], &encode_delta(None, &snap(12, 6400)));
        c.on_datagram(&d2, 2.0).unwrap_or(());
        assert!(c.take_own_state().is_none());
        assert_eq!(c.hp(), Some(0.0));
        // truncated own block: whole datagram rejected, nothing applied
        let bad = frame_snapshot_v3(&no_events(), &[0x80], &[], &encode_delta(None, &snap(14, 0)));
        assert!(c.on_datagram(&bad, 3.0).is_err());
        assert_ne!(c.interp.newest_tick(), Some(14));
    }

    #[test]
    fn board_surfaces_and_bad_board_is_rejected() {
        use shinobi_proto::gameplay::{BoardRow, PHASE_LIVE};
        let mut c = NetClient::default();
        welcome(&mut c, 1, 0);
        assert!(c.board().is_none());
        let b = Board {
            phase: PHASE_LIVE,
            map: 2,
            ticks_left: Some(900),
            winner: 0,
            rows: vec![BoardRow { id: 1, kills: 3, deaths: 1, alive: true }],
        };
        let d = frame_snapshot_v3(&no_events(), &[], &b.encode(), &encode_delta(None, &snap(10, 0)));
        c.on_datagram(&d, 1.0).unwrap();
        assert_eq!(c.board(), Some(&b));
        let bad = frame_snapshot_v3(&no_events(), &[], &[0xFF], &encode_delta(None, &snap(12, 0)));
        let _ = c.on_datagram(&bad, 2.0);
        assert_eq!(c.board(), Some(&b), "a corrupt board never replaces the good one");
    }

    #[test]
    fn handshake_cookie_and_retransmit_policy() {
        use shinobi_proto::datagram::{decode_hello, encode_challenge, VERSION};
        let mut c = NetClient::default();
        let h = c.hello_due(0.0).expect("first HELLO immediately");
        assert_eq!(decode_hello(&h).unwrap().cookie, 0);
        assert!(c.hello_due(3.0).is_none(), "not before HELLO_RETRY_MS (100 ms = 6 ticks)");
        c.on_datagram(&encode_challenge(0xDEAD_BEEF), 4.0).unwrap();
        let h = c.hello_due(6.0).expect("retry due");
        let hello = decode_hello(&h).unwrap();
        assert_eq!((hello.cookie, hello.version), (0xDEAD_BEEF, VERSION));
        c.on_datagram(&encode_challenge(7), 7.0).unwrap(); // newest cookie wins
        assert_eq!(decode_hello(&c.hello()).unwrap().cookie, 7);
        assert!(!c.connect_failed(299.0));
        assert!(c.connect_failed(300.0), "5 s without WELCOME");
        assert!(c.hello_due(400.0).is_none(), "gives up");
        // WELCOME ends the handshake; later CHALLENGEs are ignored
        let mut c = NetClient::default();
        c.hello_due(0.0);
        welcome(&mut c, 3, 10);
        c.on_datagram(&encode_challenge(99), 1.0).unwrap();
        assert_eq!(decode_hello(&c.hello()).unwrap().cookie, 0);
        assert!(c.hello_due(50.0).is_none());
        assert!(!c.connect_failed(1000.0));
    }

    #[test]
    fn kick_is_terminal_and_not_a_bad_datagram() {
        use shinobi_proto::datagram::encode_kick;
        let mut c = NetClient::default();
        welcome(&mut c, 1, 0);
        assert!(c.next_packet(intent(1), 0.0).is_some());
        c.on_datagram(&encode_kick(KickReason::Cheat), 1.0).unwrap();
        c.on_datagram(&encode_kick(KickReason::Flood), 1.1).unwrap(); // repeats keep the first reason
        assert_eq!(c.kicked(), Some(KickReason::Cheat));
        assert_eq!(c.bad_datagrams, 0);
        assert!(c.next_packet(intent(2), 2.0).is_none(), "no input after a kick");
        assert!(c.on_datagram(&frame_snapshot_v3(&no_events(), &[], &[], &encode_delta(None, &snap(9, 0))), 3.0).is_ok());
        assert_eq!(c.interp.newest_tick(), None, "stragglers after a kick are ignored");
        // kick while still connecting (server full)
        let mut c = NetClient::default();
        c.on_datagram(&encode_kick(KickReason::ServerFull), 0.0).unwrap();
        assert_eq!(c.kicked(), Some(KickReason::ServerFull));
        assert!(c.on_datagram(&[TAG_KICK], 0.0).is_err(), "truncated kick is malformed");
    }

    #[test]
    fn pace_tick_is_contiguous_and_reanchors_on_big_drift() {
        let mut c = NetClient::default();
        assert_eq!(c.pace_tick(0.0, 4), None);
        welcome(&mut c, 1, 100);
        assert_eq!(c.pace_tick(0.0, 4), Some(104));
        // clock estimate jitters by +-2 ticks: tick numbers still just count up
        assert_eq!(c.pace_tick(1.0, 4), Some(105));
        assert_eq!(c.pace_tick(3.0, 4), Some(106));
        assert_eq!(c.pace_tick(2.0, 4), Some(107));
        // 3 s hitch: want ~ 284, far from 108 -> re-anchor
        assert_eq!(c.pace_tick(180.0, 4), Some(284));
        assert_eq!(c.pace_tick(181.0, 4), Some(285));
    }

    #[test]
    fn input_tick_leads_the_server_clock() {
        let mut c = NetClient::default();
        assert_eq!(c.input_tick(0.0, 4), None);
        welcome(&mut c, 1, 500);
        assert_eq!(c.input_tick(10.0, 4), Some(514));
        assert_eq!(c.input_tick(10.5, 4), Some(515), "rounds up, never stamps behind the estimate");
    }

    #[test]
    fn clock_estimate_follows_snapshots_and_repeat_welcome_is_ignored() {
        let mut c = NetClient::default();
        welcome(&mut c, 1, 500);
        assert_eq!(c.server_tick_estimate(10.0), Some(510.0));
        c.on_datagram(&Welcome { player_id: 9, server_tick: 9999 }.encode(), 11.0).unwrap();
        assert_eq!(c.player_id(), Some(1));
        c.on_datagram(&frame_snapshot_v3(&no_events(), &[], &[], &encode_delta(None, &snap(600, 0))), 20.0).unwrap();
        assert_eq!(c.server_tick_estimate(25.0), Some(605.0));
    }
}
