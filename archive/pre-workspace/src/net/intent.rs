//! Client -> server input packets.
//!
//! Each datagram carries the newest intent plus up to `MAX_REDUNDANCY - 1` older ones (contiguous ticks),
//! so a single lost cellular packet never drops an input or a button edge. The server de-duplicates by tick.

use std::collections::VecDeque;

use rapier3d::prelude::Vector;

use super::bits::{BitReader, BitWriter, DecodeError, DecodeResult};
use super::quant::dq_axis;
use crate::character::PlayerInput;

pub const MAX_REDUNDANCY: usize = 4;

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Buttons(pub u8);

impl Buttons {
    pub const JUMP_PRESSED: u8 = 1 << 0;
    pub const JUMP_HELD: u8 = 1 << 1;
    pub const DASH: u8 = 1 << 2;
    pub const ROLL: u8 = 1 << 3;
    pub const SLICE: u8 = 1 << 4;
    pub const FIRE: u8 = 1 << 5;
    pub const MASK: u8 = 0x3f;
    /// One-tick edges; must not be re-fired when the server repeats an intent.
    pub const EDGES: u8 = Self::JUMP_PRESSED | Self::DASH | Self::ROLL | Self::SLICE | Self::FIRE;

    pub fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

/// One simulation tick of player input. Everything is quantised to what the wire carries,
/// so the client predicts from exactly the values the server will simulate.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct PlayerIntent {
    /// Server tick this input is meant to execute on.
    pub tick: u32,
    pub buttons: Buttons,
    /// Stick in -127..127, world-space X / Z (camera-relative mapping happens client-side before quantising).
    pub move_x: i8,
    pub move_z: i8,
    /// Aim / facing yaw, 0..65535 = 0..2pi.
    pub aim_yaw: u16,
}

impl PlayerIntent {
    /// Move vector with length clamped to 1 (a hostile client can send the (-127,-127) corner).
    pub fn move_vec(&self) -> (f32, f32) {
        let (x, z) = (dq_axis(self.move_x), dq_axis(self.move_z));
        let len = (x * x + z * z).sqrt();
        if len > 1.0 { (x / len, z / len) } else { (x, z) }
    }

    pub fn to_player_input(&self) -> PlayerInput {
        let (x, z) = self.move_vec();
        PlayerInput {
            move_dir: Vector::new(x, 0.0, z),
            jump_pressed: self.buttons.has(Buttons::JUMP_PRESSED),
            jump_held: self.buttons.has(Buttons::JUMP_HELD),
            dash_pressed: self.buttons.has(Buttons::DASH),
            roll_pressed: self.buttons.has(Buttons::ROLL),
        }
    }

    fn same_body(&self, o: &Self) -> bool {
        self.buttons == o.buttons
            && self.move_x == o.move_x
            && self.move_z == o.move_z
            && self.aim_yaw == o.aim_yaw
    }

    fn write_body(&self, w: &mut BitWriter) {
        w.write_bits(self.buttons.0 as u32 & Buttons::MASK as u32, 6);
        w.write_bits(self.move_x as u8 as u32, 8);
        w.write_bits(self.move_z as u8 as u32, 8);
        w.write_bits(self.aim_yaw as u32, 16);
    }

    fn read_body(r: &mut BitReader, tick: u32) -> DecodeResult<Self> {
        Ok(Self {
            tick,
            buttons: Buttons(r.read_bits(6)? as u8),
            move_x: r.read_bits(8)? as u8 as i8,
            move_z: r.read_bits(8)? as u8 as i8,
            aim_yaw: r.read_bits(16)? as u16,
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
    pending: VecDeque<PlayerIntent>,
    last_executed: Option<PlayerIntent>,
    /// Newest view reported with any accepted packet.
    pub latest_view: ViewState,
}

impl IntentQueue {
    const MAX_QUEUED: usize = 16;

    pub fn push_packet(&mut self, pkt: &ClientPacket) {
        self.latest_view = pkt.view;
        // oldest first so `pending` stays sorted
        for it in pkt.intents.iter().rev() {
            self.push(*it);
        }
    }

    fn push(&mut self, it: PlayerIntent) {
        if self.last_executed.map_or(false, |l| it.tick <= l.tick) {
            return;
        }
        match self.pending.binary_search_by_key(&it.tick, |p| p.tick) {
            Ok(_) => {}
            Err(i) => self.pending.insert(i, it),
        }
        while self.pending.len() > Self::MAX_QUEUED {
            self.pending.pop_front();
        }
    }

    /// Input to simulate on `tick`. `None` until the client has sent anything.
    pub fn take_for_tick(&mut self, tick: u32) -> Option<PlayerIntent> {
        while self.pending.front().map_or(false, |p| p.tick < tick) {
            self.pending.pop_front();
        }
        let it = if self.pending.front().map_or(false, |p| p.tick == tick) {
            self.pending.pop_front()
        } else {
            self.last_executed.map(|l| PlayerIntent {
                tick,
                buttons: Buttons(l.buttons.0 & !Buttons::EDGES),
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
        PlayerIntent { tick, buttons: Buttons(buttons), move_x: x, move_z: -x / 2, aim_yaw: 1234 + tick as u16 }
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
        assert!(bytes.len() <= 32, "packet too fat: {}", bytes.len());
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
        });
        assert_eq!(q.take_for_tick(10).unwrap().buttons.0, 0);
        assert!(q.take_for_tick(11).unwrap().buttons.has(Buttons::DASH));
        // tick 12 lost: repeat without the dash edge
        let rep = q.take_for_tick(12).unwrap();
        assert!(!rep.buttons.has(Buttons::DASH));
        assert_eq!(rep.move_x, 10);
        // late duplicate of tick 11 ignored
        q.push_packet(&ClientPacket { intents: vec![intent(11, Buttons::DASH, 10)], ..Default::default() });
        assert!(!q.take_for_tick(13).unwrap().buttons.has(Buttons::DASH));
    }
}
