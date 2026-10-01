//! Reliable-over-unreliable event stream for structural state (walls, obstacles) and low-rate gameplay events
//! (kills, respawns, match flow, pickups). High-rate hits are NOT here: they ride the unreliable own block.
//! Wire tag is 4 bits (v3).
//! Server re-sends every unacked event in each snapshot datagram, oldest first, within a byte budget.
//! Client applies strictly in sequence order and acks the highest contiguous seq.

use std::collections::VecDeque;

use shinobi_proto::bits::{BitReader, BitWriter, DecodeError, DecodeResult};
use super::obstacle::{MotionPath, ObstacleDesc, ObstacleId};
use super::walls::{WallId, WallSpawn};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetEvent {
    WallSpawn(WallSpawn),
    WallKill { id: WallId, tick: u32 },
    /// Also used to retarget an existing obstacle.
    ObstacleSpawn(ObstacleDesc),
    ObstacleDespawn { id: ObstacleId, tick: u32 },
    /// `killer` 0 = none (obstacle, self-kill). `kind` = `shinobi_proto::gameplay::KIND_*`.
    Kill { tick: u32, killer: u16, victim: u16, kind: u8 },
    Respawn { tick: u32, player: u16 },
    /// `phase` = `gameplay::PHASE_*`; fires when the whole-seconds number changes.
    MatchCountdown { tick: u32, phase: u8, seconds: u8 },
    MatchStart { tick: u32, map: u32 },
    /// `winner` 0 = draw; `reason` 0 frag limit, 1 time limit.
    MatchEnd { tick: u32, winner: u16, reason: u8 },
    /// Clear ALL walls, obstacles and pickups; fresh spawn events for the new map follow.
    MapChange { tick: u32, map: u32 },
    PickupSpawned { tick: u32, id: u32, pos: [i32; 3], kind: u8 },
    PickupTaken { tick: u32, id: u32, player: u16 },
    /// Wrist-grapple hooked (or re-hooked): `anchor` in 1/512 m. Draw a rope from the player's hand to it.
    RopeAttach { tick: u32, player: u16, anchor: [i32; 3] },
    /// The player's rope is gone (released, expired, died, left).
    RopeRelease { tick: u32, player: u16 },
}

fn write_v3(w: &mut BitWriter, v: [i32; 3], base: [i32; 3]) {
    for i in 0..3 {
        w.write_zigzag(v[i].wrapping_sub(base[i]));
    }
}

fn read_v3(r: &mut BitReader, base: [i32; 3]) -> DecodeResult<[i32; 3]> {
    Ok([
        base[0].wrapping_add(r.read_zigzag()?),
        base[1].wrapping_add(r.read_zigzag()?),
        base[2].wrapping_add(r.read_zigzag()?),
    ])
}

impl NetEvent {
    pub fn write(&self, w: &mut BitWriter) {
        match self {
            Self::WallSpawn(s) => {
                w.write_bits(0, 4);
                w.write_varint(s.id);
                w.write_bits(s.owner as u32, 8);
                w.write_varint(s.spawn_tick);
                write_v3(w, s.a, [0; 3]);
                write_v3(w, s.b, s.a); // segments are short: delta from `a`
            }
            Self::WallKill { id, tick } => {
                w.write_bits(1, 4);
                w.write_varint(*id);
                w.write_varint(*tick);
            }
            Self::ObstacleSpawn(d) => {
                w.write_bits(2, 4);
                w.write_bits(d.id as u32, 16);
                w.write_bits(d.kind as u32, 8);
                w.write_varint(d.start_tick);
                w.write_varint(d.end_tick);
                for h in d.half {
                    w.write_bits(h as u32, 16);
                }
                match d.path {
                    MotionPath::Linear { origin, vel } => {
                        w.write_bool(false);
                        write_v3(w, origin, [0; 3]);
                        write_v3(w, vel, [0; 3]);
                    }
                    MotionPath::PingPong { a, b, period } => {
                        w.write_bool(true);
                        write_v3(w, a, [0; 3]);
                        write_v3(w, b, a);
                        w.write_bits(period as u32, 16);
                    }
                }
            }
            Self::ObstacleDespawn { id, tick } => {
                w.write_bits(3, 4);
                w.write_bits(*id as u32, 16);
                w.write_varint(*tick);
            }
            Self::Kill { tick, killer, victim, kind } => {
                w.write_bits(4, 4);
                w.write_varint(*tick);
                w.write_bits(*killer as u32, 16);
                w.write_bits(*victim as u32, 16);
                w.write_bits(*kind as u32 & 7, 3);
            }
            Self::Respawn { tick, player } => {
                w.write_bits(5, 4);
                w.write_varint(*tick);
                w.write_bits(*player as u32, 16);
            }
            Self::MatchCountdown { tick, phase, seconds } => {
                w.write_bits(6, 4);
                w.write_varint(*tick);
                w.write_bits(*phase as u32 & 3, 2);
                w.write_bits(*seconds as u32, 8);
            }
            Self::MatchStart { tick, map } => {
                w.write_bits(7, 4);
                w.write_varint(*tick);
                w.write_varint(*map);
            }
            Self::MatchEnd { tick, winner, reason } => {
                w.write_bits(8, 4);
                w.write_varint(*tick);
                w.write_bits(*winner as u32, 16);
                w.write_bits(*reason as u32 & 3, 2);
            }
            Self::MapChange { tick, map } => {
                w.write_bits(9, 4);
                w.write_varint(*tick);
                w.write_varint(*map);
            }
            Self::PickupSpawned { tick, id, pos, kind } => {
                w.write_bits(10, 4);
                w.write_varint(*tick);
                w.write_varint(*id);
                write_v3(w, *pos, [0; 3]);
                w.write_bits(*kind as u32, 8);
            }
            Self::PickupTaken { tick, id, player } => {
                w.write_bits(11, 4);
                w.write_varint(*tick);
                w.write_varint(*id);
                w.write_bits(*player as u32, 16);
            }
            Self::RopeAttach { tick, player, anchor } => {
                w.write_bits(12, 4);
                w.write_varint(*tick);
                w.write_bits(*player as u32, 16);
                write_v3(w, *anchor, [0; 3]);
            }
            Self::RopeRelease { tick, player } => {
                w.write_bits(13, 4);
                w.write_varint(*tick);
                w.write_bits(*player as u32, 16);
            }
        }
    }

    pub fn read(r: &mut BitReader) -> DecodeResult<Self> {
        Ok(match r.read_bits(4)? {
            0 => {
                let id = r.read_varint()?;
                let owner = r.read_bits(8)? as u8;
                let spawn_tick = r.read_varint()?;
                let a = read_v3(r, [0; 3])?;
                let b = read_v3(r, a)?;
                Self::WallSpawn(WallSpawn { id, owner, spawn_tick, a, b })
            }
            1 => Self::WallKill { id: r.read_varint()?, tick: r.read_varint()? },
            2 => {
                let id = r.read_bits(16)? as u16;
                let kind = r.read_bits(8)? as u8;
                let start_tick = r.read_varint()?;
                let end_tick = r.read_varint()?;
                let half = [r.read_bits(16)? as u16, r.read_bits(16)? as u16, r.read_bits(16)? as u16];
                let path = if !r.read_bool()? {
                    MotionPath::Linear { origin: read_v3(r, [0; 3])?, vel: read_v3(r, [0; 3])? }
                } else {
                    let a = read_v3(r, [0; 3])?;
                    let b = read_v3(r, a)?;
                    MotionPath::PingPong { a, b, period: r.read_bits(16)? as u16 }
                };
                Self::ObstacleSpawn(ObstacleDesc { id, kind, start_tick, end_tick, half, path })
            }
            3 => Self::ObstacleDespawn { id: r.read_bits(16)? as u16, tick: r.read_varint()? },
            4 => Self::Kill {
                tick: r.read_varint()?,
                killer: r.read_bits(16)? as u16,
                victim: r.read_bits(16)? as u16,
                kind: r.read_bits(3)? as u8,
            },
            5 => Self::Respawn { tick: r.read_varint()?, player: r.read_bits(16)? as u16 },
            6 => {
                let tick = r.read_varint()?;
                let phase = r.read_bits(2)? as u8;
                if phase > 2 {
                    return Err(DecodeError::Malformed);
                }
                Self::MatchCountdown { tick, phase, seconds: r.read_bits(8)? as u8 }
            }
            7 => Self::MatchStart { tick: r.read_varint()?, map: r.read_varint()? },
            8 => Self::MatchEnd { tick: r.read_varint()?, winner: r.read_bits(16)? as u16, reason: r.read_bits(2)? as u8 },
            9 => Self::MapChange { tick: r.read_varint()?, map: r.read_varint()? },
            10 => Self::PickupSpawned {
                tick: r.read_varint()?,
                id: r.read_varint()?,
                pos: read_v3(r, [0; 3])?,
                kind: r.read_bits(8)? as u8,
            },
            11 => Self::PickupTaken { tick: r.read_varint()?, id: r.read_varint()?, player: r.read_bits(16)? as u16 },
            12 => Self::RopeAttach { tick: r.read_varint()?, player: r.read_bits(16)? as u16, anchor: read_v3(r, [0; 3])? },
            13 => Self::RopeRelease { tick: r.read_varint()?, player: r.read_bits(16)? as u16 },
            _ => return Err(DecodeError::Malformed),
        })
    }
}

const MAX_PER_DATAGRAM: usize = 64;

/// Server side, one per client.
#[derive(Default)]
pub struct EventOutbox {
    next_seq: u32, // last assigned
    unacked: VecDeque<(u32, NetEvent)>,
}

impl EventOutbox {
    pub fn push(&mut self, ev: NetEvent) -> u32 {
        self.next_seq += 1;
        self.unacked.push_back((self.next_seq, ev));
        self.next_seq
    }

    pub fn ack(&mut self, seq: u32) {
        while self.unacked.front().map_or(false, |(s, _)| *s <= seq) {
            self.unacked.pop_front();
        }
    }

    pub fn pending(&self) -> usize {
        self.unacked.len()
    }

    /// Writes as many pending events as fit in `budget_bytes`. High priority: call before entity data.
    pub fn write_pending(&self, w: &mut BitWriter, budget_bytes: usize) {
        let mut scratch = BitWriter::new();
        let mut count = 0;
        for (_, ev) in self.unacked.iter().take(MAX_PER_DATAGRAM) {
            ev.write(&mut scratch);
            if (scratch.bit_len() + 7) / 8 > budget_bytes {
                break;
            }
            count += 1;
        }
        w.write_varint(count as u32);
        if count == 0 {
            return;
        }
        w.write_varint(self.unacked[0].0);
        for (_, ev) in self.unacked.iter().take(count) {
            ev.write(w);
        }
    }
}

/// Client side.
pub struct EventInbox {
    next_expected: u32,
}

impl Default for EventInbox {
    fn default() -> Self {
        Self { next_expected: 1 }
    }
}

impl EventInbox {
    /// Highest contiguous seq applied; echo this as `ClientPacket::ack_event_seq`.
    pub fn ack(&self) -> u32 {
        self.next_expected - 1
    }

    pub fn read(&mut self, r: &mut BitReader, mut apply: impl FnMut(NetEvent)) -> DecodeResult<()> {
        let count = r.read_varint()? as usize;
        if count > MAX_PER_DATAGRAM {
            return Err(DecodeError::Malformed);
        }
        if count == 0 {
            return Ok(());
        }
        let first = r.read_varint()?;
        for i in 0..count as u32 {
            let ev = NetEvent::read(r)?;
            let seq = first.wrapping_add(i);
            if seq == self.next_expected {
                self.next_expected += 1;
                apply(ev);
            } // else duplicate (seq < next) or gap (will be re-sent)
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(id: u32) -> NetEvent {
        NetEvent::WallSpawn(WallSpawn { id, owner: 3, spawn_tick: 1000 + id, a: [10, 0, -5], b: [200, 0, 90] })
    }

    #[test]
    fn rope_events_roundtrip_and_stay_small() {
        let evs = [
            NetEvent::RopeAttach { tick: 123_456, player: 7, anchor: [-20_000, 5_120, 33_333] },
            NetEvent::RopeRelease { tick: 123_500, player: 7 },
        ];
        let mut w = BitWriter::new();
        for e in &evs {
            e.write(&mut w);
        }
        let bytes = w.finish();
        assert!(bytes.len() <= 20, "attach + release = {} B", bytes.len());
        let mut r = BitReader::new(&bytes);
        for e in &evs {
            assert_eq!(&NetEvent::read(&mut r).unwrap(), e);
        }
    }

    #[test]
    fn resend_until_acked_survives_loss() {
        let mut out = EventOutbox::default();
        let mut inbox = EventInbox::default();
        let mut got = vec![];
        for i in 1..=5 {
            out.push(wall(i));
        }
        out.push(NetEvent::ObstacleDespawn { id: 7, tick: 99 });

        // datagram 1 lost entirely; datagram 2 delivered but truncated by a tiny budget
        let mut w = BitWriter::new();
        out.write_pending(&mut w, 1024);
        let _lost = w.finish();

        let mut w = BitWriter::new();
        out.write_pending(&mut w, 20);
        let b = w.finish();
        inbox.read(&mut BitReader::new(&b), |e| got.push(e)).unwrap();
        assert!(!got.is_empty() && got.len() < 6);
        out.ack(inbox.ack());

        // duplicate delivery of the old datagram is harmless
        inbox.read(&mut BitReader::new(&b), |e| got.push(e)).unwrap();

        let mut w = BitWriter::new();
        out.write_pending(&mut w, 1024);
        let b = w.finish();
        inbox.read(&mut BitReader::new(&b), |e| got.push(e)).unwrap();
        out.ack(inbox.ack());
        assert_eq!(got.len(), 6);
        assert_eq!(got[0], wall(1));
        assert_eq!(out.pending(), 0);
    }

    #[test]
    fn gameplay_events_roundtrip_and_unknown_tag_rejected() {
        let evs = [
            NetEvent::Kill { tick: 90_000, killer: 0, victim: 12, kind: 4 },
            NetEvent::Kill { tick: 1, killer: 65_535, victim: 2, kind: 0 },
            NetEvent::Respawn { tick: 7, player: 3 },
            NetEvent::MatchCountdown { tick: 600, phase: 0, seconds: 10 },
            NetEvent::MatchStart { tick: 601, map: 2 },
            NetEvent::MatchEnd { tick: 99_999, winner: 0, reason: 1 },
            NetEvent::MapChange { tick: 100_500, map: 3 },
            NetEvent::PickupSpawned { tick: 8, id: 70_000, pos: [-512, 384, 2048], kind: 0 },
            NetEvent::PickupTaken { tick: 9, id: 70_000, player: 5 },
        ];
        for ev in evs {
            let mut w = BitWriter::new();
            ev.write(&mut w);
            let b = w.finish();
            assert_eq!(NetEvent::read(&mut BitReader::new(&b)).unwrap(), ev, "{ev:?}");
        }
        // tag 15 does not exist (and tag 3-bit-era garbage must not decode as an event)
        let mut w = BitWriter::new();
        w.write_bits(15, 4);
        let b = w.finish();
        assert!(NetEvent::read(&mut BitReader::new(&b)).is_err());
        // match countdown with an impossible phase is rejected
        let mut w = BitWriter::new();
        w.write_bits(6, 4);
        w.write_varint(1);
        w.write_bits(3, 2);
        w.write_bits(5, 8);
        assert!(NetEvent::read(&mut BitReader::new(&w.finish())).is_err());
    }

    #[test]
    fn obstacle_event_roundtrip() {
        let ev = NetEvent::ObstacleSpawn(ObstacleDesc {
            id: 42,
            kind: 2,
            start_tick: 5000,
            end_tick: 0,
            half: [64, 32, 64],
            path: MotionPath::PingPong { a: [-100, 0, 7], b: [900, 0, 7], period: 120 },
        });
        let mut w = BitWriter::new();
        ev.write(&mut w);
        let b = w.finish();
        assert_eq!(NetEvent::read(&mut BitReader::new(&b)).unwrap(), ev);
    }
}
