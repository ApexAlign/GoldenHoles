//! Gameplay sections of the snapshot datagram (see `datagram.rs` for the layout): the owner-only block
//! (`OwnExtras`: exact health + unreliable hit markers around the sim's `OwnState`) and the periodic
//! scoreboard (`Board`). Plain structs with symmetric write/read so server and client share one codec.

use super::bits::{BitReader, BitWriter, DecodeError, DecodeResult};

/// Newest hits kept per datagram (the count field is 4 bits).
pub const MAX_HITS: usize = 15;
/// Rows kept in a scoreboard (its length field is a u8 byte count; ~7 bytes per row worst case, 255 byte cap).
pub const MAX_BOARD_ROWS: usize = 30;

pub const KIND_SHURIKEN: u8 = 0;
pub const KIND_SLASH: u8 = 1;
pub const KIND_GRID_BEAM: u8 = 2;
pub const KIND_OBSTACLE: u8 = 3;
pub const KIND_WALL: u8 = 4;
/// A mine star's burst.
pub const KIND_MINE: u8 = 5;
/// A yumi arrow / a fuda tag's blast (docs/powerups.md).
pub const KIND_ARROW: u8 = 6;
pub const KIND_TAG: u8 = 7;

/// One hit, owner-only and unreliable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HitMsg {
    /// Victim (when you dealt it), attacker (when you took it), 0 = hazard / none.
    pub other: u16,
    /// Damage, clamped to 255.
    pub amount: u8,
    pub kind: u8,
    /// false: you dealt it (hit marker); true: you took it (damage direction).
    pub taken: bool,
    /// The hit killed the victim.
    pub kill: bool,
    /// A headshot (shuriken at head height): draw the headshot marker.
    pub head: bool,
    /// The hit landed inside the target's strike window (bonus damage): draw the strike marker.
    pub bonus: bool,
}

/// The owner-only block. `OwnState` (sim crate) sits between `acked_tick` and the tail, so it is passed as a
/// closure: server `|w| own_state.write(w)`, client `OwnState::read`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OwnExtras {
    /// false for dead players (extras only): the client keeps predicting.
    pub has_state: bool,
    /// Newest intent tick the server executed for this player (only meaningful with `has_state`).
    pub acked_tick: u32,
    /// Exact health x10 (0 = dead).
    pub hp_x10: u16,
    /// Newest hits, oldest first. Each hit is resent in several datagrams (loss tolerance); `hit_seq` is the
    /// wrapping sequence of the LAST hit, earlier ones are consecutive below it, so the client dedups by seq.
    pub hits: Vec<HitMsg>,
    /// Only on the wire when `hits` is non-empty.
    pub hit_seq: u8,
    /// Highest intent tick such that the server has the real intent of every tick up to it (`IntentQueue::
    /// complete_tick`): the client re-sends button edges only until they are covered by this.
    pub intent_complete: u32,
    /// Smallest headroom (ticks) of the newest intent over the server clock since the previous block; negative =
    /// the intent arrived after its tick. The client sizes its intent lead from it.
    pub lead_margin: i8,
    /// `seq` of the newest client packet the server had received when this block was built, and how many ticks ago it
    /// arrived: the client subtracts the age from its own send-to-now time to measure the round trip.
    pub echo_seq: u16,
    pub echo_age: u8,
    /// Star energy: pool size in charges (0 = no pool, the readout is hidden), charges in 1/16ths, and the empty-pool
    /// lockout flag.
    pub star_max: u8,
    pub star_energy: u8,
    pub star_locked: bool,
    /// The power weapon in hand (`PowerKind` wire value, 0 = none), arrows / tags left, carry time left in 1/4 s.
    pub power_kind: u8,
    pub power_ammo: u8,
    pub power_left_q: u8,
}

/// `lead_margin` defaults to `LEAD_MARGIN_MAX`, which means "no measurement" (real margins are clamped to at most
/// `LEAD_MARGIN_MAX - 1`): a block built without margin data (tests, other tools) must not steer the client's lead.
impl Default for OwnExtras {
    fn default() -> Self {
        Self { has_state: false, acked_tick: 0, hp_x10: 0, hits: Vec::new(), hit_seq: 0, intent_complete: 0, lead_margin: LEAD_MARGIN_MAX, echo_seq: 0, echo_age: 0, star_max: 0, star_energy: 0, star_locked: false, power_kind: 0, power_ammo: 0, power_left_q: 0 }
    }
}

/// `lead_margin` is clamped to this range on the wire (5 bits).
pub const LEAD_MARGIN_MIN: i8 = -16;
pub const LEAD_MARGIN_MAX: i8 = 15;

impl OwnExtras {
    pub fn write(&self, w: &mut BitWriter, write_state: impl FnOnce(&mut BitWriter)) {
        w.write_bool(self.has_state);
        if self.has_state {
            w.write_varint(self.acked_tick);
            write_state(w);
        }
        w.write_varint(self.hp_x10 as u32);
        // trails / leads `acked_tick` by a few ticks: a zigzag delta is ~1 byte instead of a 3-byte tick
        w.write_zigzag(self.intent_complete.wrapping_sub(self.acked_tick) as i32);
        w.write_bits((self.lead_margin.clamp(LEAD_MARGIN_MIN, LEAD_MARGIN_MAX) as i32 - LEAD_MARGIN_MIN as i32) as u32, 5);
        w.write_bits(self.echo_seq as u32, 16);
        w.write_bits(self.echo_age as u32, 8);
        w.write_bits(self.star_max as u32, 4);
        if self.star_max > 0 {
            w.write_bits(self.star_energy as u32, 8);
            w.write_bool(self.star_locked);
        }
        w.write_bits(self.power_kind as u32 & 3, 2);
        if self.power_kind & 3 != 0 {
            w.write_bits(self.power_ammo as u32 & 15, 4);
            w.write_bits(self.power_left_q as u32, 8);
        }
        let n = self.hits.len().min(MAX_HITS);
        w.write_bits(n as u32, 4);
        if n > 0 {
            w.write_bits(self.hit_seq as u32, 8);
        }
        for h in &self.hits[self.hits.len() - n..] {
            w.write_varint(h.other as u32);
            w.write_bits(h.amount as u32, 8);
            w.write_bits(h.kind as u32 & 7, 3);
            w.write_bool(h.taken);
            w.write_bool(h.kill);
            w.write_bool(h.head);
            w.write_bool(h.bonus);
        }
    }

    /// `read_state` runs only when the block has state; its result is returned alongside.
    pub fn read<T>(
        r: &mut BitReader,
        read_state: impl FnOnce(&mut BitReader) -> DecodeResult<T>,
    ) -> DecodeResult<(Self, Option<T>)> {
        let has_state = r.read_bool()?;
        let (acked_tick, state) = if has_state {
            let t = r.read_varint()?;
            (t, Some(read_state(r)?))
        } else {
            (0, None)
        };
        let hp_x10 = u16::try_from(r.read_varint()?).map_err(|_| DecodeError::Malformed)?;
        let intent_complete = acked_tick.wrapping_add(r.read_zigzag()? as u32);
        let lead_margin = (r.read_bits(5)? as i32 + LEAD_MARGIN_MIN as i32) as i8;
        let echo_seq = r.read_bits(16)? as u16;
        let echo_age = r.read_bits(8)? as u8;
        let star_max = r.read_bits(4)? as u8;
        let (star_energy, star_locked) = if star_max > 0 { (r.read_bits(8)? as u8, r.read_bool()?) } else { (0, false) };
        let power_kind = r.read_bits(2)? as u8;
        let (power_ammo, power_left_q) = if power_kind != 0 { (r.read_bits(4)? as u8, r.read_bits(8)? as u8) } else { (0, 0) };
        let n = r.read_bits(4)? as usize;
        let hit_seq = if n > 0 { r.read_bits(8)? as u8 } else { 0 };
        let mut hits = Vec::with_capacity(n);
        for _ in 0..n {
            hits.push(HitMsg {
                other: u16::try_from(r.read_varint()?).map_err(|_| DecodeError::Malformed)?,
                amount: r.read_bits(8)? as u8,
                kind: r.read_bits(3)? as u8,
                taken: r.read_bool()?,
                kill: r.read_bool()?,
                head: r.read_bool()?,
                bonus: r.read_bool()?,
            });
        }
        Ok((Self { has_state, acked_tick, hp_x10, hits, hit_seq, intent_complete, lead_margin, echo_seq, echo_age, star_max, star_energy, star_locked, power_kind, power_ammo, power_left_q }, state))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BoardRow {
    pub id: u8,
    pub kills: u16,
    pub deaths: u16,
    pub alive: bool,
}

pub const PHASE_WARMUP: u8 = 0;
pub const PHASE_LIVE: u8 = 1;
pub const PHASE_INTERMISSION: u8 = 2;

/// Periodic scoreboard + match phase, identical for every peer.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Board {
    pub phase: u8,
    pub map: u32,
    pub ticks_left: Option<u32>,
    /// 0 = none / draw.
    pub winner: u16,
    pub rows: Vec<BoardRow>,
}

impl Board {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_bits(self.phase as u32 & 3, 2);
        w.write_varint(self.map);
        w.write_bool(self.ticks_left.is_some());
        if let Some(t) = self.ticks_left {
            w.write_varint(t);
        }
        w.write_bits(self.winner as u32, 16);
        let n = self.rows.len().min(MAX_BOARD_ROWS);
        w.write_bits(n as u32, 8);
        for row in &self.rows[..n] {
            w.write_bits(row.id as u32, 8);
            w.write_varint(row.kills as u32);
            w.write_varint(row.deaths as u32);
            w.write_bool(row.alive);
        }
        w.finish()
    }

    pub fn decode(buf: &[u8]) -> DecodeResult<Self> {
        let mut r = BitReader::new(buf);
        let phase = r.read_bits(2)? as u8;
        if phase > PHASE_INTERMISSION {
            return Err(DecodeError::Malformed);
        }
        let map = r.read_varint()?;
        let ticks_left = if r.read_bool()? { Some(r.read_varint()?) } else { None };
        let winner = r.read_bits(16)? as u16;
        let n = r.read_bits(8)? as usize;
        if n > MAX_BOARD_ROWS {
            return Err(DecodeError::Malformed);
        }
        let mut rows = Vec::with_capacity(n);
        for _ in 0..n {
            rows.push(BoardRow {
                id: r.read_bits(8)? as u8,
                kills: u16::try_from(r.read_varint()?).map_err(|_| DecodeError::Malformed)?,
                deaths: u16::try_from(r.read_varint()?).map_err(|_| DecodeError::Malformed)?,
                alive: r.read_bool()?,
            });
        }
        Ok(Self { phase, map, ticks_left, winner, rows })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_extras_roundtrip_with_and_without_state() {
        let hit = |other, taken| HitMsg { other, amount: 34, kind: KIND_SLASH, taken, kill: other == 7, head: other == 7 || taken, bonus: other == 7 };
        let full = OwnExtras { has_state: true, acked_tick: 123_456, hp_x10: 875, hits: vec![hit(7, false), hit(0, true)], hit_seq: 9, intent_complete: 123_450, lead_margin: -3, echo_seq: 65_000, echo_age: 3, star_max: 6, star_energy: 77, star_locked: true, power_kind: 2, power_ammo: 1, power_left_q: 200 };
        let mut w = BitWriter::new();
        full.write(&mut w, |w| {
            w.write_bits(0b1011, 4); // stand-in for OwnState
            w.write_varint(99);
        });
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let (got, st) = OwnExtras::read(&mut r, |r| Ok((r.read_bits(4)?, r.read_varint()?))).unwrap();
        assert_eq!(got, full);
        assert_eq!(st, Some((0b1011, 99)));

        // dead: extras only, state closure never runs
        let dead = OwnExtras { has_state: false, acked_tick: 0, hp_x10: 0, hits: vec![hit(3, true)], hit_seq: 255, intent_complete: 0, lead_margin: 15, echo_seq: 7, echo_age: 255, star_max: 0, star_energy: 0, star_locked: false, power_kind: 0, power_ammo: 0, power_left_q: 0 };
        let mut w = BitWriter::new();
        dead.write(&mut w, |_| panic!("no state when dead"));
        let bytes = w.finish();
        let (got, st): (_, Option<()>) = OwnExtras::read(&mut BitReader::new(&bytes), |_| panic!("no state")).unwrap();
        assert_eq!((got, st), (dead, None));
        assert!(bytes.len() <= 11, "dead-player block is tiny: {}", bytes.len()); // incl. the 3-byte ping echo
    }

    #[test]
    fn hit_list_keeps_newest_and_truncation_errors() {
        let hits: Vec<_> = (0..30u16).map(|i| HitMsg { other: i, amount: 1, kind: 0, taken: false, kill: false, head: false, bonus: false }).collect();
        let e = OwnExtras { has_state: false, acked_tick: 0, hp_x10: 1000, hits, hit_seq: 4, ..Default::default() };
        let mut w = BitWriter::new();
        e.write(&mut w, |_| {});
        let bytes = w.finish();
        let (got, _): (_, Option<()>) = OwnExtras::read(&mut BitReader::new(&bytes), |_| Ok(())).unwrap();
        assert_eq!(got.hits.len(), MAX_HITS);
        assert_eq!(got.hits.last().unwrap().other, 29, "newest hits survive the cap");
        assert!(OwnExtras::read(&mut BitReader::new(&bytes[..bytes.len() - 3]), |_| Ok::<(), _>(())).is_err());
    }

    #[test]
    fn board_roundtrip_and_validation() {
        let b = Board {
            phase: PHASE_LIVE,
            map: 3,
            ticks_left: Some(5400),
            winner: 0,
            rows: (1..=16u8).map(|id| BoardRow { id, kills: id as u16 * 3, deaths: id as u16, alive: id % 2 == 0 }).collect(),
        };
        let bytes = b.encode();
        assert!(bytes.len() < 200, "16-player board fits the u8 length: {}", bytes.len());
        assert_eq!(Board::decode(&bytes).unwrap(), b);
        let none = Board { phase: PHASE_WARMUP, ..Default::default() };
        assert_eq!(Board::decode(&none.encode()).unwrap(), none);
        assert!(Board::decode(&[0xff]).is_err());
    }
}
