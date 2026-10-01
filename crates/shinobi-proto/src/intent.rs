//! Client -> server input packets.
//!
//! Each datagram carries the newest intent plus up to `MAX_REDUNDANCY - 1` older ones (contiguous ticks),
//! so a single lost cellular packet never drops an input or a button edge. The server de-duplicates by tick.

use std::collections::VecDeque;

use super::bits::{BitReader, BitWriter, DecodeError, DecodeResult};
use super::quant::dq_axis;

pub const MAX_REDUNDANCY: usize = 4;
/// Wire width of `aim_pitch` (-90..90 deg over 4096 steps = 0.044 deg/step).
pub const PITCH_BITS: u32 = 12;
pub const PITCH_MAX: u16 = (1 << PITCH_BITS) - 1;
/// Code for pitch 0 (level).
pub const PITCH_LEVEL: u16 = 1 << (PITCH_BITS - 1);
/// Newest intent may lead the server clock by at most this many ticks (12 = 200 ms, covers clock drift and the
/// redundancy window). Anything further is a flood/time-skip attempt and is dropped.
pub const MAX_FUTURE_TICKS: u32 = 12;

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
    /// One-tick edge: fire the zipline gun at the aimed hookable anchor (server validates range, line of sight,
    /// cooldown, line cap). Own wire bit, cleared on lost-packet repeat.
    pub zip: bool,
}

/// Neutral intent: no buttons, no stick, looking level (pitch code 2048 = 0 deg, not straight down).
impl Default for PlayerIntent {
    fn default() -> Self {
        Self { tick: 0, buttons: Buttons(0), move_x: 0, move_z: 0, aim_yaw: 0, aim_pitch: PITCH_LEVEL, ultimate: false, build: false, zip: false }
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
            && self.zip == o.zip
    }

    fn write_body(&self, w: &mut BitWriter) {
        w.write_bits(self.buttons.0 as u32 & Buttons::MASK as u32, 8);
        w.write_bits(self.move_x as u8 as u32, 8);
        w.write_bits(self.move_z as u8 as u32, 8);
        w.write_bits(self.aim_yaw as u32, 16);
        w.write_bits(self.aim_pitch.min(PITCH_MAX) as u32, PITCH_BITS);
        w.write_bool(self.ultimate);
        w.write_bool(self.build);
        w.write_bool(self.zip);
    }

    fn read_body(r: &mut BitReader, tick: u32) -> DecodeResult<Self> {
        Ok(Self {
            tick,
            buttons: Buttons(r.read_bits(8)? as u8),
            move_x: r.read_bits(8)? as u8 as i8,
            move_z: r.read_bits(8)? as u8 as i8,
            aim_yaw: r.read_bits(16)? as u16,
            aim_pitch: r.read_bits(PITCH_BITS)? as u16,
            ultimate: r.read_bool()?,
            build: r.read_bool()?,
            zip: r.read_bool()?,
        })
    }
}

/// What the client's screen showed when it acted: targets were interpolated between
/// `from_tick` and `from_tick + 1` at `alpha / 256`. Drives server-side rewind.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct ViewState {
    pub from_tick: u32,
    pub alpha: u8,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ClientPacket {
    pub seq: u16,
    /// Newest snapshot tick the client holds; server uses it as the next delta baseline.
    pub ack_snapshot_tick: u32,
    /// Highest contiguous reliable event seq applied (walls / obstacles).
    pub ack_event_seq: u32,
    pub view: ViewState,
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
        w.write_bits((n - 1) as u32, 2);
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
        let n = r.read_bits(2)? as usize + 1;
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
}

impl IntentQueue {
    const MAX_QUEUED: usize = 16;

    /// `server_tick` = tick about to be simulated. Returns how many intents were rejected as too far in the
    /// future (flood / clock-skew cheat); caller feeds that into the anti-cheat strike counter.
    pub fn push_packet(&mut self, pkt: &ClientPacket, server_tick: u32) -> u32 {
        let ceiling = server_tick.saturating_add(MAX_FUTURE_TICKS);
        let mut rejected = 0;
        // oldest first so `pending` stays sorted
        let newest = pkt.intents.first().map_or(0, |i| i.tick);
        for it in pkt.intents.iter().rev() {
            if it.tick > ceiling {
                rejected += 1;
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

    fn push(&mut self, it: PlayerIntent, view: ViewState) {
        if self.last_executed.map_or(false, |l| it.tick <= l.tick) {
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
        let it = if self.pending.front().map_or(false, |p| p.0.tick == tick) {
            let (it, view) = self.pending.pop_front().unwrap();
            self.latest_view = view;
            Some(it)
        } else {
            // lost tick: repeat state; the view keeps advancing with the (extrapolated) client clock
            if self.latest_view.from_tick != 0 {
                self.latest_view.from_tick += 1;
            }
            self.last_executed.map(|l| PlayerIntent {
                tick,
                buttons: Buttons(l.buttons.0 & !Buttons::EDGES),
                ultimate: false,
                build: false,
                zip: false,
                ..l
            })
        };
        if it.is_some() {
            self.last_executed = it;
        }
        it
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(tick: u32, buttons: u8, x: i8) -> PlayerIntent {
        PlayerIntent { tick, buttons: Buttons(buttons), move_x: x, move_z: -x / 2, aim_yaw: 1234 + tick as u16, aim_pitch: 2048, ultimate: false, build: false, zip: false }
    }

    #[test]
    fn roundtrip_with_redundancy() {
        let pkt = ClientPacket {
            seq: 65000,
            ack_snapshot_tick: 120_000,
            ack_event_seq: 77,
            view: ViewState { from_tick: 119_994, alpha: 200 },
            intents: vec![
                intent(120_010, Buttons::DASH | Buttons::JUMP_HELD, 127),
                intent(120_009, Buttons::JUMP_HELD, 127),
                intent(120_008, Buttons::JUMP_HELD, 127),
                intent(120_007, 0, -127),
            ],
        };
        let bytes = pkt.encode().unwrap();
        assert!(bytes.len() <= 40, "packet too fat: {}", bytes.len());
        assert_eq!(ClientPacket::decode(&bytes).unwrap(), pkt);
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
