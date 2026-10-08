//! Client -> server input packets.
//!
//! Each datagram carries the newest intent plus older ones (contiguous ticks): `BASE_REDUNDANCY` normally, and back
//! to the oldest button edge the server has not confirmed (`intent_complete` in the owner block) so a lost or late
//! packet never drops an input or a button edge. The server de-duplicates by tick and applies an edge that arrives
//! after its tick was already simulated exactly once, on the next tick (`IntentQueue`).

use std::collections::VecDeque;

use super::bits::{BitReader, BitWriter, DecodeError, DecodeResult};
use super::quant::dq_axis;

/// Most intents one datagram can carry (4-bit count). Normally the newest 4 go out; while a button edge is still
/// unacknowledged the window stretches back to it (see `ClientPacket::needs_history`), up to this many.
pub const MAX_REDUNDANCY: usize = 16;
/// Intents every datagram carries even when nothing is pending.
pub const BASE_REDUNDANCY: usize = 4;
/// Wire width of `aim_pitch` (-90..90 deg over 4096 steps = 0.044 deg/step).
pub const PITCH_BITS: u32 = 12;
pub const PITCH_MAX: u16 = (1 << PITCH_BITS) - 1;
/// Code for pitch 0 (level).
pub const PITCH_LEVEL: u16 = 1 << (PITCH_BITS - 1);
/// Newest intent may lead the server clock by at most this many ticks (24 = 400 ms: the largest adaptive lead plus a
/// catch-up burst after a client hitch). Anything further is dropped.
pub const MAX_FUTURE_TICKS: u32 = 24;
/// Intents further ahead than this are a clock cheat or a flood and cost a strike; between `MAX_FUTURE_TICKS` and this
/// they are only dropped (a client that hitches and then catches up in one burst runs ahead of the server clock
/// for a moment, and its redundancy re-sends them once the server has caught up).
pub const STRIKE_FUTURE_TICKS: u32 = 120;

/// Client -> server datagram. Name used by the design docs; one type, no parallel codec.
pub type PlayerIntentPacket = ClientPacket;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Buttons(pub u8);

impl Buttons {
    pub const JUMP_PRESSED: u8 = 1 << 0;
    pub const JUMP_HELD: u8 = 1 << 1;
    pub const DASH: u8 = 1 << 2;
    pub const ROLL: u8 = 1 << 3;
    pub const SLICE: u8 = 1 << 4;
    pub const FIRE: u8 = 1 << 5;
    /// Held state (not an edge): client is pressing into a wall. Server still validates the wall contact.
    pub const WALL_CLING: u8 = 1 << 6;
    /// Held: slide / crouch-slide. Wire bit 7 is the last free bit of the button byte.
    pub const SLIDE: u8 = 1 << 7;
    pub const MASK: u8 = 0xff;
    /// Design-doc names: `FIRE` throws a shuriken; `SLICE` is the katana dash-slice (lunge + cut in one action,
    /// separate from the evasive `DASH`).
    pub const THROW_SHURIKEN: u8 = Self::FIRE;
    pub const KATANA_DASH_SLICE: u8 = Self::SLICE;
    /// One-tick edges; must not be re-fired when the server repeats an intent.
    pub const EDGES: u8 = Self::JUMP_PRESSED | Self::DASH | Self::ROLL | Self::SLICE | Self::FIRE;

    pub fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

/// One simulation tick of player input. Everything is quantised to what the wire carries,
/// so the client predicts from exactly the values the server will simulate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PlayerIntent {
    /// Server tick this input is meant to execute on.
    pub tick: u32,
    pub buttons: Buttons,
    /// Stick in -127..127 in the *view* frame: `move_x` strafe (+right), `move_z` forward (+forward).
    /// The server rotates by `aim_yaw`, so the client never claims a world-space direction.
    pub move_x: i8,
    pub move_z: i8,
    /// Aim yaw, 0..65535 = 0..2pi (0 faces -Z, positive turns toward -X).
    pub aim_yaw: u16,
    /// Aim pitch, 0..=PITCH_MAX = -90..+90 deg.
    pub aim_pitch: u16,
    /// One-tick edge (cleared when the server repeats an intent after packet loss): fire the ultimate. Own wire
    /// bit instead of a `Buttons` flag because all 8 button bits are taken.
    pub ultimate: bool,
    /// One-tick edge: place a block at the aimed spot (server validates range, charges, overlap; see the block
    /// building rules). Own wire bit, cleared on lost-packet repeat like `ultimate`.
    pub build: bool,
    /// With `build`: place a wall (`M`) instead of a floor (`N`). Meaningless without `build`.
    pub build_wall: bool,
    /// With `build` (and not `build_wall`): place a ramp instead of a floor. Meaningless without `build`.
    pub build_ramp: bool,
    /// One-tick edge: fire the zipline gun at the aimed hookable anchor (server validates range, line of sight,
    /// cooldown, line cap). Own wire bit, cleared on lost-packet repeat.
    pub zip: bool,
    /// Held (a level, not an edge, so a repeated intent keeps it): the block / parry shield. While it is held the
    /// aim is locked where it was when the hold began (the server ignores any other aim), movement is unaffected.
    /// Own wire bit: all 8 button bits are taken.
    pub block: bool,
    /// With `FIRE`: a charged throw, the MARK star (a one-tick edge like `FIRE`, cleared on lost-packet repeat).
    pub mark: bool,
    /// With `FIRE` (and no `mark`): the throwable, 0 standard / 1 mine / 2 flash / 3 smoke (`StarKind`). Two wire bits,
    /// present only while `FIRE` is set; carried with a lost packet's FIRE edge.
    pub star: u8,
    /// Held (a level, repeated with the last intent): the bow is being drawn (right button while a yumi is in hand,
    /// `shinobi_sim::power`). The walk slows while it is set; releasing it after a full draw looses the arrow.
    pub draw: bool,
}

/// Neutral intent: no buttons, no stick, looking level (pitch code 2048 = 0 deg, not straight down).
impl Default for PlayerIntent {
    fn default() -> Self {
        Self { tick: 0, buttons: Buttons(0), move_x: 0, move_z: 0, aim_yaw: 0, aim_pitch: PITCH_LEVEL, ultimate: false, build: false, build_wall: false, build_ramp: false, zip: false, block: false, mark: false, star: 0, draw: false }
    }
}

impl PlayerIntent {
    /// Move vector with length clamped to 1 (a hostile client can send the (-127,-127) corner).
    pub fn move_vec(&self) -> (f32, f32) {
        let (x, z) = (dq_axis(self.move_x), dq_axis(self.move_z));
        let len = (x * x + z * z).sqrt();
        if len > 1.0 { (x / len, z / len) } else { (x, z) }
    }

    /// Move vector rotated into world space by the aim yaw (server-side; length already clamped to 1).
    pub fn world_move(&self) -> (f32, f32) {
        let (strafe, fwd) = self.move_vec();
        let yaw = super::quant::dq_yaw(self.aim_yaw);
        // libm, not std: std trig bits differ per platform libc, and client prediction must equal the server
        let (s, c) = (libm::sinf(yaw), libm::cosf(yaw));
        // forward = (-sin, -cos), right = (cos, -sin)
        (fwd * -s + strafe * c, fwd * -c + strafe * -s)
    }

    fn same_body(&self, o: &Self) -> bool {
        self.buttons == o.buttons
            && self.move_x == o.move_x
            && self.move_z == o.move_z
            && self.aim_yaw == o.aim_yaw
            && self.aim_pitch == o.aim_pitch
            && self.ultimate == o.ultimate
            && self.build == o.build
            && self.build_wall == o.build_wall
            && self.build_ramp == o.build_ramp
            && self.zip == o.zip
            && self.block == o.block
            && self.mark == o.mark
            && self.star == o.star
            && self.draw == o.draw
    }

    fn write_body(&self, w: &mut BitWriter) {
        w.write_bits(self.buttons.0 as u32 & Buttons::MASK as u32, 8);
        w.write_bits(self.move_x as u8 as u32, 8);
        w.write_bits(self.move_z as u8 as u32, 8);
        w.write_bits(self.aim_yaw as u32, 16);
        w.write_bits(self.aim_pitch.min(PITCH_MAX) as u32, PITCH_BITS);
        w.write_bool(self.ultimate);
        // the piece kind rides only on a build press (it is an edge): idle intents stay one bit lighter
        w.write_bool(self.build);
        if self.build {
            w.write_bool(self.build_wall);
            w.write_bool(self.build_ramp);
        }
        w.write_bool(self.zip);
        w.write_bool(self.block);
        w.write_bool(self.mark);
        if self.buttons.has(Buttons::FIRE) {
            w.write_bits(self.star as u32 & 3, 2);
        }
        w.write_bool(self.draw);
    }

    fn read_body(r: &mut BitReader, tick: u32) -> DecodeResult<Self> {
        let (buttons, move_x, move_z, aim_yaw, aim_pitch, ultimate) = (
            Buttons(r.read_bits(8)? as u8),
            r.read_bits(8)? as u8 as i8,
            r.read_bits(8)? as u8 as i8,
            r.read_bits(16)? as u16,
            r.read_bits(PITCH_BITS)? as u16,
            r.read_bool()?,
        );
        let build = r.read_bool()?;
        let (build_wall, build_ramp) = if build { (r.read_bool()?, r.read_bool()?) } else { (false, false) };
        let (zip, block, mark) = (r.read_bool()?, r.read_bool()?, r.read_bool()?);
        let star = if buttons.has(Buttons::FIRE) { r.read_bits(2)? as u8 } else { 0 };
        let draw = r.read_bool()?;
        Ok(Self { tick, buttons, move_x, move_z, aim_yaw, aim_pitch, ultimate, build, build_wall, build_ramp, zip, block, mark, star, draw })
    }
}

/// What the client's screen showed when it acted: targets were interpolated between
/// `from_tick` and `from_tick + 1` at `alpha / 256`. Drives server-side rewind.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct ViewState {
    pub from_tick: u32,
    pub alpha: u8,
}

/// Client-measured network quality, sent about once a second (`ClientPacket::report`) so the server's playtest
/// telemetry can include numbers only the client has.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ClientReport {
    /// Smoothed round-trip time in ms (12 bits on the wire, `PING_UNKNOWN` = not measured yet).
    pub ping_ms: u16,
    /// Reconcile corrections the client has applied since it connected, modulo 65536.
    pub corrections: u16,
}

/// `ClientReport::ping_ms` before the first measurement.
pub const PING_UNKNOWN: u16 = 0xFFF;

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ClientPacket {
    pub seq: u16,
    /// Newest snapshot tick the client holds; server uses it as the next delta baseline.
    pub ack_snapshot_tick: u32,
    /// Highest contiguous reliable event seq applied (walls / obstacles).
    pub ack_event_seq: u32,
    pub view: ViewState,
    /// About once a second: client-side ping and correction count (telemetry only; never trusted for gameplay).
    pub report: Option<ClientReport>,
    /// Newest first, contiguous ticks (`intents[i].tick == intents[0].tick - i`), 1..=MAX_REDUNDANCY entries.
    pub intents: Vec<PlayerIntent>,
}

impl ClientPacket {
    pub fn encode(&self) -> Result<Vec<u8>, DecodeError> {
        let n = self.intents.len();
        if n == 0 || n > MAX_REDUNDANCY {
            return Err(DecodeError::Malformed);
        }
        let newest = self.intents[0].tick;
        if self.intents.iter().enumerate().any(|(i, it)| it.tick != newest.wrapping_sub(i as u32)) {
            return Err(DecodeError::Malformed);
        }
        let mut w = BitWriter::new();
        w.write_bits(self.seq as u32, 16);
        w.write_varint(self.ack_snapshot_tick);
        w.write_varint(self.ack_event_seq);
        // view tick trails the ack by a few ticks: zigzag delta is ~1 byte
        w.write_zigzag(self.view.from_tick.wrapping_sub(self.ack_snapshot_tick) as i32);
        w.write_bits(self.view.alpha as u32, 8);
        w.write_bool(self.report.is_some());
        if let Some(r) = self.report {
            w.write_bits(r.ping_ms.min(PING_UNKNOWN) as u32, 12);
            w.write_bits(r.corrections as u32, 16);
        }
        w.write_bits((n - 1) as u32, 4);
        w.write_varint(newest);
        self.intents[0].write_body(&mut w);
        for i in 1..n {
            let same = self.intents[i].same_body(&self.intents[i - 1]);
            w.write_bool(same);
            if !same {
                self.intents[i].write_body(&mut w);
            }
        }
        Ok(w.finish())
    }

    pub fn decode(buf: &[u8]) -> DecodeResult<Self> {
        let mut r = BitReader::new(buf);
        let seq = r.read_bits(16)? as u16;
        let ack_snapshot_tick = r.read_varint()?;
        let ack_event_seq = r.read_varint()?;
        let view_delta = r.read_zigzag()?;
        let alpha = r.read_bits(8)? as u8;
        let report = if r.read_bool()? { Some(ClientReport { ping_ms: r.read_bits(12)? as u16, corrections: r.read_bits(16)? as u16 }) } else { None };
        let n = r.read_bits(4)? as usize + 1;
        let newest = r.read_varint()?;
        if (n as u32) > newest.saturating_add(1) {
            return Err(DecodeError::Malformed);
        }
        let mut intents = Vec::with_capacity(n);
        intents.push(PlayerIntent::read_body(&mut r, newest)?);
        for i in 1..n {
            let tick = newest - i as u32;
            if r.read_bool()? {
                let prev = intents[i - 1];
                intents.push(PlayerIntent { tick, ..prev });
            } else {
                intents.push(PlayerIntent::read_body(&mut r, tick)?);
            }
        }
        Ok(Self {
            seq,
            ack_snapshot_tick,
            ack_event_seq,
            view: ViewState { from_tick: ack_snapshot_tick.wrapping_add(view_delta as u32), alpha },
            report,
            intents,
        })
    }
}

/// Server-side per-client input queue. Orders by tick, drops duplicates/stale, and when a tick's input is
/// missing repeats the last held state with one-shot edges cleared (no phantom dashes).
#[derive(Default)]
pub struct IntentQueue {
    pending: VecDeque<(PlayerIntent, ViewState)>,
    last_executed: Option<PlayerIntent>,
    /// What the client's screen showed when it produced the intent most recently returned by `take_for_tick`
    /// (`from_tick == 0` = unknown, no rewind). Per-intent: a packet's view belongs to its newest intent, and
    /// older redundant intents were made `i` ticks earlier, so their view is `i` ticks older. Using the packet
    /// view for all of them would rewind too little by about the client's input lead.
    pub latest_view: ViewState,
    /// Ticks whose real intent has arrived: bit `i` = tick `got_top - i` (a 64-tick window). A tick the server had
    /// to synthesise (repeat after loss) stays a gap until the real intent shows up, even late.
    got: u64,
    got_top: u32,
    /// Edges of intents that arrived after their tick was simulated: merged, once, into the next simulated tick.
    carry: Carry,
    /// Smallest `newest intent tick - server tick` of the packets since `take_margin` (ticks of headroom; negative
    /// = late). Reported to the client so it can size its intent lead.
    margin_min: Option<i32>,
    /// Intents dropped for being more than `MAX_FUTURE_TICKS` ahead of the server clock (cumulative).
    pub rejected_future: u32,
    /// Ticks the server had to synthesise (repeat the last input) because the real intent had not arrived (cumulative).
    pub repeated_ticks: u32,
}

/// One-shot actions of a late intent, carried to the next tick (bitwise OR / last build wall wins).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Carry {
    buttons: u8,
    ultimate: bool,
    build: bool,
    build_wall: bool,
    zip: bool,
    mark: bool,
    star: u8,
}

impl Carry {
    fn is_empty(&self) -> bool {
        *self == Carry::default()
    }

    fn of(it: &PlayerIntent) -> Carry {
        Carry { buttons: it.buttons.0 & Buttons::EDGES, ultimate: it.ultimate, build: it.build, build_wall: it.build && it.build_wall, zip: it.zip, mark: it.mark && it.buttons.has(Buttons::FIRE), star: if it.buttons.has(Buttons::FIRE) { it.star } else { 0 } }
    }

    fn merge(&mut self, o: Carry) {
        self.buttons |= o.buttons;
        self.ultimate |= o.ultimate;
        if o.build {
            self.build = true;
            self.build_wall = o.build_wall;
        }
        self.zip |= o.zip;
        self.mark |= o.mark;
        if o.star != 0 {
            self.star = o.star;
        }
    }

    fn apply(&self, it: &mut PlayerIntent) {
        it.buttons.0 |= self.buttons;
        it.ultimate |= self.ultimate;
        if self.build {
            it.build = true;
            it.build_wall = self.build_wall;
        }
        it.zip |= self.zip;
        it.mark |= self.mark;
        if it.star == 0 {
            it.star = self.star;
        }
    }
}

impl IntentQueue {
    const MAX_QUEUED: usize = 32;
    /// Gaps older than this many ticks behind the newest received tick are given up on (`complete_tick`).
    const GAP_WINDOW: u32 = 32;

    /// `server_tick` = tick about to be simulated. Returns how many intents were rejected as too far in the
    /// future (flood / clock-skew cheat); caller feeds that into the anti-cheat strike counter.
    pub fn push_packet(&mut self, pkt: &ClientPacket, server_tick: u32) -> u32 {
        let ceiling = server_tick.saturating_add(MAX_FUTURE_TICKS);
        let mut rejected = 0;
        // oldest first so `pending` stays sorted
        let newest = pkt.intents.first().map_or(0, |i| i.tick);
        let margin = (newest as i64 - server_tick as i64).clamp(-64, 64) as i32;
        self.margin_min = Some(self.margin_min.map_or(margin, |m| m.min(margin)));
        for it in pkt.intents.iter().rev() {
            if it.tick > ceiling {
                self.rejected_future = self.rejected_future.saturating_add(1);
                if it.tick > server_tick.saturating_add(STRIKE_FUTURE_TICKS) {
                    rejected += 1;
                }
                continue;
            }
            // a view of tick 0 means "none"; stays none. Otherwise age it by how much older this intent is.
            let view = if pkt.view.from_tick == 0 {
                ViewState::default()
            } else {
                ViewState { from_tick: pkt.view.from_tick.saturating_sub(newest.wrapping_sub(it.tick)), alpha: pkt.view.alpha }
            };
            self.push(*it, view);
        }
        // a packet carrying out-of-window ticks has no trustworthy view time either
        if rejected > 0 {
            for (_, v) in self.pending.iter_mut().filter(|(it, _)| it.tick > server_tick) {
                *v = ViewState::default();
            }
        }
        rejected
    }

    fn mark_got(&mut self, tick: u32) -> bool {
        // returns true when this tick was not marked before
        if self.got == 0 && self.got_top == 0 {
            // first intent of the stream: nothing before it was ever expected, so no phantom gaps behind it
            self.got_top = tick;
            self.got = u64::MAX;
            return true;
        }
        if tick > self.got_top {
            let shift = tick - self.got_top;
            self.got = if shift >= 64 { 0 } else { self.got << shift };
            self.got_top = tick;
            self.got |= 1;
            return true;
        }
        let back = self.got_top - tick;
        if back >= 64 {
            return false; // too old to track: treat as already seen
        }
        let bit = 1u64 << back;
        let fresh = self.got & bit == 0;
        self.got |= bit;
        fresh
    }

    fn push(&mut self, it: PlayerIntent, view: ViewState) {
        let fresh = self.mark_got(it.tick);
        if self.last_executed.map_or(false, |l| it.tick <= l.tick) {
            // too late to execute on its own tick. If the server already simulated this tick with a real intent it
            // is a duplicate; if the tick was synthesised (lost) its one-shot actions were never applied: carry them
            if fresh {
                self.carry.merge(Carry::of(&it));
            }
            return;
        }
        match self.pending.binary_search_by_key(&it.tick, |p| p.0.tick) {
            Ok(_) => {}
            Err(i) => self.pending.insert(i, (it, view)),
        }
        while self.pending.len() > Self::MAX_QUEUED {
            self.pending.pop_front();
        }
    }

    /// Highest tick `c` such that the real intent of every tick up to `c` has arrived (within the last
    /// `GAP_WINDOW` ticks; older gaps are abandoned). The client may stop re-sending intents <= `c`: all their
    /// edges have been applied or carried. 0 = nothing received yet.
    pub fn complete_tick(&self) -> u32 {
        if self.got == 0 {
            return 0;
        }
        let oldest = self.got_top.saturating_sub(Self::GAP_WINDOW.min(63));
        // first missing tick above `oldest`
        for t in oldest..=self.got_top {
            if self.got & (1u64 << (self.got_top - t)) == 0 {
                return t - 1;
            }
        }
        self.got_top
    }

    /// Smallest arrival margin since the last call (ticks of headroom of the newest intent over the server clock).
    pub fn take_margin(&mut self) -> Option<i32> {
        self.margin_min.take()
    }

    /// Tick of the intent most recently simulated for this player (executed or repeated after a lost packet).
    /// This is the `acked_intent_tick` the client reconciles against.
    pub fn last_executed_tick(&self) -> Option<u32> {
        self.last_executed.map(|l| l.tick)
    }

    /// Input to simulate on `tick`. `None` until the client has sent anything.
    pub fn take_for_tick(&mut self, tick: u32) -> Option<PlayerIntent> {
        while self.pending.front().map_or(false, |p| p.0.tick < tick) {
            self.pending.pop_front();
        }
        let mut it = if self.pending.front().map_or(false, |p| p.0.tick == tick) {
            let (it, view) = self.pending.pop_front().unwrap();
            self.latest_view = view;
            Some(it)
        } else {
            // lost tick: repeat state; the view keeps advancing with the (extrapolated) client clock
            if self.last_executed.is_some() {
                self.repeated_ticks = self.repeated_ticks.saturating_add(1);
            }
            if self.latest_view.from_tick != 0 {
                self.latest_view.from_tick += 1;
            }
            self.last_executed.map(|l| PlayerIntent {
                tick,
                buttons: Buttons(l.buttons.0 & !Buttons::EDGES),
                ultimate: false,
                build: false, build_wall: false, build_ramp: false,
                zip: false,
                mark: false,
                star: 0,
                ..l
            })
        };
        if let Some(i) = it.as_mut() {
            // late edges ride on the next simulated tick, exactly once
            if !self.carry.is_empty() {
                self.carry.apply(i);
                self.carry = Carry::default();
            }
            self.last_executed = Some(*i);
        }
        it
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(tick: u32, buttons: u8, x: i8) -> PlayerIntent {
        PlayerIntent { tick, buttons: Buttons(buttons), move_x: x, move_z: -x / 2, aim_yaw: 1234 + tick as u16, aim_pitch: 2048, ultimate: false, build: false, build_wall: false, build_ramp: false, zip: false, block: false, mark: false, star: 0, draw: false }
    }

    #[test]
    fn roundtrip_with_redundancy() {
        let pkt = ClientPacket {
            seq: 65000,
            ack_snapshot_tick: 120_000,
            ack_event_seq: 77,
            view: ViewState { from_tick: 119_994, alpha: 200 },
            report: Some(ClientReport { ping_ms: 87, corrections: 65_531 }),
            intents: vec![
                intent(120_010, Buttons::DASH | Buttons::JUMP_HELD, 127),
                intent(120_009, Buttons::JUMP_HELD, 127),
                intent(120_008, Buttons::JUMP_HELD, 127),
                intent(120_007, 0, -127),
            ],
        };
        let bytes = pkt.encode().unwrap();
        assert!(bytes.len() <= 46, "packet too fat: {}", bytes.len()); // this one also carries the once-a-second report (+29 bits)
        assert_eq!(ClientPacket::decode(&bytes).unwrap(), pkt);
    }

    fn pkt(intents: Vec<PlayerIntent>) -> ClientPacket {
        ClientPacket { intents, ..Default::default() }
    }

    fn zipped(tick: u32) -> PlayerIntent {
        PlayerIntent { zip: true, buttons: Buttons(Buttons::DASH), ..intent(tick, 0, 0) }
    }

    /// A button edge whose tick was already simulated as a repeat (its packets were late or lost) is applied once,
    /// on the next simulated tick, and re-sent copies of it are ignored.
    #[test]
    fn late_edge_is_applied_exactly_once_on_the_next_tick() {
        let mut q = IntentQueue::default();
        q.push_packet(&pkt(vec![intent(4, 0, 0), intent(3, 0, 0)]), 3);
        assert!(q.take_for_tick(3).is_some() && q.take_for_tick(4).is_some());
        // tick 5 never arrives in time: the server repeats tick 4 with the edges cleared
        let rep = q.take_for_tick(5).unwrap();
        assert!(!rep.zip && !rep.buttons.has(Buttons::DASH));
        // now the real tick 5 (with a zip + dash) shows up together with tick 6
        q.push_packet(&pkt(vec![intent(6, 0, 0), zipped(5)]), 6);
        let t6 = q.take_for_tick(6).unwrap();
        assert!(t6.zip && t6.buttons.has(Buttons::DASH), "the late edge rides on tick 6");
        // the same edge again in later packets (the client re-sends until acked) must not fire a second time
        q.push_packet(&pkt(vec![intent(8, 0, 0), intent(7, 0, 0), intent(6, 0, 0), zipped(5)]), 7);
        assert!(!q.take_for_tick(7).unwrap().zip);
        assert!(!q.take_for_tick(8).unwrap().zip);
    }

    /// An edge in a tick that was executed for real is a duplicate when it arrives again.
    #[test]
    fn duplicate_of_an_executed_edge_never_refires() {
        let mut q = IntentQueue::default();
        q.push_packet(&pkt(vec![zipped(10)]), 10);
        assert!(q.take_for_tick(10).unwrap().zip);
        q.push_packet(&pkt(vec![intent(11, 0, 0), zipped(10)]), 11);
        assert!(!q.take_for_tick(11).unwrap().zip);
    }

    /// `complete_tick` stops at the first tick whose real intent has not arrived and moves on when it does.
    #[test]
    fn complete_tick_tracks_the_first_gap() {
        let mut q = IntentQueue::default();
        assert_eq!(q.complete_tick(), 0);
        q.push_packet(&pkt(vec![intent(102, 0, 0), intent(101, 0, 0), intent(100, 0, 0)]), 100);
        assert_eq!(q.complete_tick(), 102);
        q.push_packet(&pkt(vec![intent(105, 0, 0), intent(104, 0, 0)]), 100); // 103 lost
        assert_eq!(q.complete_tick(), 102, "gap at 103");
        q.push_packet(&pkt(vec![intent(106, 0, 0), intent(105, 0, 0), intent(104, 0, 0), intent(103, 0, 0)]), 100);
        assert_eq!(q.complete_tick(), 106);
        // an ancient gap is given up on (window), so a stuck client does not hold the ack back forever
        q.push_packet(&pkt(vec![intent(200, 0, 0)]), 190);
        assert_eq!(q.complete_tick(), 200 - 32 - 1, "ticks 107..=199 never came: the gap window starts at 168");
    }

    /// Arrival margin = newest intent tick minus the server tick about to be simulated, minimum since last taken.
    #[test]
    fn margin_is_the_minimum_headroom_since_last_taken() {
        let mut q = IntentQueue::default();
        assert_eq!(q.take_margin(), None);
        q.push_packet(&pkt(vec![intent(110, 0, 0)]), 100); // 10 ticks ahead
        q.push_packet(&pkt(vec![intent(111, 0, 0)]), 110); // 1 ahead
        q.push_packet(&pkt(vec![intent(112, 0, 0)]), 116); // 4 late
        assert_eq!(q.take_margin(), Some(-4));
        assert_eq!(q.take_margin(), None);
    }

    #[test]
    fn long_redundancy_windows_roundtrip() {
        let intents: Vec<PlayerIntent> = (0..MAX_REDUNDANCY as u32).map(|i| intent(500 - i, if i % 5 == 0 { Buttons::DASH } else { 0 }, i as i8)).collect();
        let p = ClientPacket { seq: 3, ack_snapshot_tick: 490, ack_event_seq: 1, view: ViewState { from_tick: 488, alpha: 9 }, report: None, intents };
        let bytes = p.encode().unwrap();
        assert_eq!(ClientPacket::decode(&bytes).unwrap(), p);
        assert!(bytes.len() < 200, "16-intent window: {} B", bytes.len());
    }

    #[test]
    fn rejects_garbage() {
        let pkt = ClientPacket { intents: vec![intent(5, 0, 0)], ..Default::default() };
        let bytes = pkt.encode().unwrap();
        assert!(ClientPacket::decode(&bytes[..bytes.len() - 2]).is_err());
        let bad = ClientPacket { intents: vec![intent(5, 0, 0), intent(9, 0, 0)], ..Default::default() };
        assert!(bad.encode().is_err());
    }

    #[test]
    fn move_vec_clamped() {
        let it = PlayerIntent { move_x: 127, move_z: 127, ..Default::default() };
        let (x, z) = it.move_vec();
        assert!((x * x + z * z).sqrt() <= 1.0 + 1e-5);
    }

    #[test]
    fn queue_dedupes_and_clears_edges_on_gap() {
        let mut q = IntentQueue::default();
        q.push_packet(&ClientPacket {
            intents: vec![intent(11, Buttons::DASH, 10), intent(10, 0, 10)],
            ..Default::default()
        }, 10);
        assert_eq!(q.take_for_tick(10).unwrap().buttons.0, 0);
        assert!(q.take_for_tick(11).unwrap().buttons.has(Buttons::DASH));
        // tick 12 lost: repeat without the dash edge
        let rep = q.take_for_tick(12).unwrap();
        assert!(!rep.buttons.has(Buttons::DASH));
        assert_eq!(rep.move_x, 10);
        // late duplicate of tick 11 ignored
        q.push_packet(&ClientPacket { intents: vec![intent(11, Buttons::DASH, 10)], ..Default::default() }, 13);
        assert!(!q.take_for_tick(13).unwrap().buttons.has(Buttons::DASH));
    }

    #[test]
    fn wall_cling_and_pitch_roundtrip() {
        let mut it = intent(50, Buttons::WALL_CLING | Buttons::FIRE | Buttons::SLICE, 0);
        it.aim_pitch = PITCH_MAX;
        let pkt = ClientPacket { intents: vec![it], ..Default::default() };
        let back = ClientPacket::decode(&pkt.encode().unwrap()).unwrap();
        assert_eq!(back, pkt);
        assert!(back.intents[0].buttons.has(Buttons::WALL_CLING));
        assert_eq!(Buttons::EDGES & (Buttons::WALL_CLING | Buttons::SLIDE), 0, "cling/slide are held, not edges");
        let s = PlayerIntent { tick: 1, buttons: Buttons(Buttons::SLIDE), ..Default::default() };
        let p = ClientPacket { intents: vec![s], ..Default::default() };
        assert!(ClientPacket::decode(&p.encode().unwrap()).unwrap().intents[0].buttons.has(Buttons::SLIDE));
    }

    #[test]
    fn oversize_pitch_is_clamped_on_the_wire() {
        let it = PlayerIntent { tick: 1, aim_pitch: u16::MAX, ..Default::default() };
        let pkt = ClientPacket { intents: vec![it], ..Default::default() };
        let back = ClientPacket::decode(&pkt.encode().unwrap()).unwrap();
        assert_eq!(back.intents[0].aim_pitch, PITCH_MAX);
    }

    #[test]
    fn world_move_rotates_with_yaw() {
        let it = PlayerIntent { move_z: 127, ..Default::default() };
        let (x, z) = it.world_move();
        assert!(x.abs() < 1e-5 && (z + 1.0).abs() < 1e-5, "yaw 0 forward = -Z");
        let it = PlayerIntent { move_z: 127, aim_yaw: 16384, ..Default::default() };
        let (x, z) = it.world_move();
        assert!((x + 1.0).abs() < 1e-4 && z.abs() < 1e-4, "quarter turn forward = -X");
        let it = PlayerIntent { move_x: 127, ..Default::default() };
        assert!((it.world_move().0 - 1.0).abs() < 1e-5, "strafe right = +X");
    }

    #[test]
    fn future_flood_cannot_evict_legit_inputs() {
        let mut q = IntentQueue::default();
        q.push_packet(&ClientPacket { intents: vec![intent(101, Buttons::FIRE, 5), intent(100, 0, 5)], ..Default::default() }, 100);
        for base in 0..30u32 {
            let pkt = ClientPacket { intents: vec![intent(10_000 + base, 0, 0)], ..Default::default() };
            assert_eq!(q.push_packet(&pkt, 100), 1);
        }
        assert!(q.take_for_tick(100).is_some());
        assert!(q.take_for_tick(101).unwrap().buttons.has(Buttons::FIRE));
    }

    #[test]
    fn view_is_per_intent_not_per_packet() {
        let mut q = IntentQueue::default();
        // newest intent 105 saw tick 100; redundant 104/103 were made 1/2 ticks earlier
        q.push_packet(
            &ClientPacket {
                view: ViewState { from_tick: 100, alpha: 64 },
                intents: vec![intent(105, 0, 0), intent(104, 0, 0), intent(103, 0, 0)],
                ..Default::default()
            },
            103,
        );
        q.take_for_tick(103);
        assert_eq!(q.latest_view, ViewState { from_tick: 98, alpha: 64 });
        q.take_for_tick(104);
        assert_eq!(q.latest_view.from_tick, 99);
        q.take_for_tick(105);
        assert_eq!(q.latest_view.from_tick, 100);
        q.take_for_tick(106); // lost: view advances
        assert_eq!(q.latest_view.from_tick, 101);
    }

    #[test]
    fn missing_view_stays_missing() {
        let mut q = IntentQueue::default();
        q.push_packet(&ClientPacket { intents: vec![intent(5, 0, 0)], ..Default::default() }, 5);
        q.take_for_tick(5);
        assert_eq!(q.latest_view, ViewState::default());
    }

    #[test]
    fn default_intent_looks_level() {
        assert_eq!(PlayerIntent::default().aim_pitch, PITCH_LEVEL);
        assert!((super::super::quant::dq_pitch(PITCH_LEVEL)).abs() < 0.001);
    }

    #[test]
    fn last_executed_tick_tracks_taken_ticks() {
        let mut q = IntentQueue::default();
        assert_eq!(q.last_executed_tick(), None);
        q.push_packet(&ClientPacket { intents: vec![intent(7, 0, 0)], ..Default::default() }, 7);
        q.take_for_tick(7);
        assert_eq!(q.last_executed_tick(), Some(7));
        q.take_for_tick(8); // lost: repeated
        assert_eq!(q.last_executed_tick(), Some(8));
    }

    #[test]
    fn ultimate_roundtrips_and_is_an_edge() {
        let mut it = intent(30, 0, 0);
        it.ultimate = true;
        let pkt = ClientPacket { intents: vec![it, intent(29, 0, 0)], ..Default::default() };
        let back = ClientPacket::decode(&pkt.encode().unwrap()).unwrap();
        assert_eq!(back, pkt);
        assert!(back.intents[0].ultimate && !back.intents[1].ultimate);
        let mut q = IntentQueue::default();
        q.push_packet(&ClientPacket { intents: vec![it], ..Default::default() }, 30);
        assert!(q.take_for_tick(30).unwrap().ultimate);
        assert!(!q.take_for_tick(31).unwrap().ultimate, "lost-packet repeat must not re-fire the ultimate");
    }

    #[test]
    fn build_roundtrips_and_is_an_edge() {
        let mut it = intent(40, 0, 0);
        it.build = true;
        let pkt = ClientPacket { intents: vec![it, intent(39, 0, 0)], ..Default::default() };
        let back = ClientPacket::decode(&pkt.encode().unwrap()).unwrap();
        assert_eq!(back, pkt);
        assert!(back.intents[0].build && !back.intents[1].build);
        let mut q = IntentQueue::default();
        q.push_packet(&ClientPacket { intents: vec![it], ..Default::default() }, 40);
        assert!(q.take_for_tick(40).unwrap().build);
        assert!(!q.take_for_tick(41).unwrap().build, "lost-packet repeat must not place a second block");
    }

    #[test]
    fn zip_roundtrips_and_is_an_edge() {
        let mut it = intent(60, 0, 0);
        it.zip = true;
        let pkt = ClientPacket { intents: vec![it, intent(59, 0, 0)], ..Default::default() };
        let back = ClientPacket::decode(&pkt.encode().unwrap()).unwrap();
        assert_eq!(back, pkt);
        assert!(back.intents[0].zip && !back.intents[1].zip);
        let mut q = IntentQueue::default();
        q.push_packet(&ClientPacket { intents: vec![it], ..Default::default() }, 60);
        assert!(q.take_for_tick(60).unwrap().zip);
        assert!(!q.take_for_tick(61).unwrap().zip, "lost-packet repeat must not fire a second zipline");
    }
}

#[cfg(test)]
mod build_kind_tests {
    use super::*;

    #[test]
    fn piece_kind_bits_roundtrip_and_cost_nothing_on_idle_intents() {
        let base = PlayerIntent { tick: 50, ..Default::default() };
        let variants = [
            base,
            PlayerIntent { build: true, ..base },
            PlayerIntent { build: true, build_wall: true, ..base },
            PlayerIntent { build: true, build_ramp: true, ..base },
        ];
        for it in variants {
            let pkt = ClientPacket { intents: vec![it], ..Default::default() };
            assert_eq!(ClientPacket::decode(&pkt.encode().unwrap()).unwrap(), pkt, "{it:?}");
        }
        let idle = ClientPacket { intents: vec![base], ..Default::default() }.encode().unwrap().len();
        let wall = ClientPacket { intents: vec![variants[2]], ..Default::default() }.encode().unwrap().len();
        assert!(wall <= idle + 1, "a build press costs at most one extra byte ({idle} -> {wall})");
    }
}
