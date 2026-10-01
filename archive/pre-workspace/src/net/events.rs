//! Reliable-over-unreliable event stream for structural state (walls, obstacles).
//! Server re-sends every unacked event in each snapshot datagram, oldest first, within a byte budget.
//! Client applies strictly in sequence order and acks the highest contiguous seq.

use std::collections::VecDeque;

use super::bits::{BitReader, BitWriter, DecodeError, DecodeResult};
use super::obstacle::{MotionPath, ObstacleDesc, ObstacleId};
use super::walls::{WallId, WallSpawn};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetEvent {
    WallSpawn(WallSpawn),
    WallKill { id: WallId, tick: u32 },
    /// Also used to retarget an existing obstacle.
    ObstacleSpawn(ObstacleDesc),
    ObstacleDespawn { id: ObstacleId, tick: u32 },
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
                w.write_bits(0, 2);
                w.write_varint(s.id);
                w.write_bits(s.owner as u32, 8);
                w.write_varint(s.spawn_tick);
                write_v3(w, s.a, [0; 3]);
                write_v3(w, s.b, s.a); // segments are short: delta from `a`
            }
            Self::WallKill { id, tick } => {
                w.write_bits(1, 2);
                w.write_varint(*id);
                w.write_varint(*tick);
            }
            Self::ObstacleSpawn(d) => {
                w.write_bits(2, 2);
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
                w.write_bits(3, 2);
                w.write_bits(*id as u32, 16);
                w.write_varint(*tick);
            }
        }
    }

    pub fn read(r: &mut BitReader) -> DecodeResult<Self> {
        Ok(match r.read_bits(2)? {
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
            _ => Self::ObstacleDespawn { id: r.read_bits(16)? as u16, tick: r.read_varint()? },
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
