//! Per-local-player movement state, sent only to its owner (not in the shared entity snapshot).
//!
//! Bit-exact by design: positions, velocities and directions are raw f32, timers are whole sim ticks (the sim
//! only ever decrements them by `TICK_DT`). Restoring this and replaying the buffered intents therefore
//! reproduces the server's continued run bit-for-bit, so the reconciler only ever fires on a REAL divergence
//! (lost input, server-only event). A quantised blob cannot do that: the KCC's contact response (wall-run,
//! landings, autostep) amplifies millimetre-sized state errors into centimetre-sized position errors.
//!
//! Timers: cooldowns/buffers/wall time are `ceil(t / dt)` ticks, which makes every future `t > 0` test come out
//! identical to the server's; dash/roll and slide durations are the exact number of ticks already elapsed,
//! rebuilt with the same f32 operations the sim runs.
//!
//! Wire layout (bits), `OwnState::write`:
//!   pos 3x32 | vel 3x32 | facing 2x32 | kind 2 (0 grounded, 1 airborne, 2 wall-run) | double_used 1 |
//!   was_grounded 1 | has_evasion 1 | has_slide 1 | timers 6x8 (coyote, jump_buf, dash_cd, wall_cd, slide_cd, slice_cd) |
//!   [wall-run: normal 2x32, tangent 2x32, ticks_left 8] [evasion: roll 1, elapsed 8, dir 2x32]
//!   [slide: dir 2x32, speed 32, elapsed 8]
//!   = 310 bits base (39 bytes); +136 in a wall-run, +73 in a dash/roll, +104 in a slide.

use shinobi_proto::bits::{BitReader, BitWriter, DecodeError, DecodeResult};
use shinobi_proto::TICK_DT;

use crate::character::{Cfg, CharacterSnapshot, Evasion, MoveState, SlideState};
use crate::rapier3d::prelude::*;

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct OwnState {
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    /// 0 grounded, 1 airborne, 2 wall-run.
    pub kind: u8,
    pub double_used: bool,
    pub was_grounded: bool,
    /// coyote, jump_buf, dash_cd, wall_cd, slide_cd, slice_cd (katana): ticks remaining (ceil).
    pub timers: [u8; 6],
    /// Horizontal (x, z).
    pub facing: [f32; 2],
    pub wall: Option<WallQ>,
    pub evasion: Option<EvasionQ>,
    pub slide: Option<SlideQ>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WallQ {
    pub normal: [f32; 2],
    pub tangent: [f32; 2],
    /// Ticks remaining (ceil).
    pub ticks_left: u8,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct EvasionQ {
    pub roll: bool,
    /// Ticks already elapsed.
    pub elapsed: u8,
    pub dir: [f32; 2],
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SlideQ {
    pub dir: [f32; 2],
    pub speed: f32,
    /// Ticks already elapsed.
    pub elapsed: u8,
}

/// Ticks remaining, rounded up (the 1e-3 tick guard absorbs f32 fuzz so a timer of exactly k ticks stays k).
fn q_t(t: f32) -> u8 {
    (t / TICK_DT - 1e-3).ceil().clamp(0.0, 255.0) as u8
}
fn dq_t(v: u8) -> f32 {
    v as f32 * TICK_DT
}
/// Whole ticks elapsed of a timer that counted `total` down to `left`.
fn q_elapsed(total: f32, left: f32) -> u8 {
    ((total - left) / TICK_DT).round().clamp(0.0, 255.0) as u8
}
fn xz(v: Vector<Real>) -> [f32; 2] {
    [v.x, v.z]
}
fn v3(a: [f32; 2]) -> Vector<Real> {
    vector![a[0], 0.0, a[1]]
}

impl OwnState {
    pub fn from_snapshot(s: &CharacterSnapshot) -> Self {
        let (kind, wall) = match s.state {
            MoveState::Grounded => (0, None),
            MoveState::Airborne => (1, None),
            MoveState::WallRun { normal, tangent, time_left } => {
                (2, Some(WallQ { normal: xz(normal), tangent: xz(tangent), ticks_left: q_t(time_left) }))
            }
        };
        Self {
            pos: [s.pos.x, s.pos.y, s.pos.z],
            vel: [s.vel.x, s.vel.y, s.vel.z],
            kind,
            double_used: s.double_used,
            was_grounded: s.was_grounded,
            timers: [q_t(s.coyote), q_t(s.jump_buf), q_t(s.dash_cd), q_t(s.wall_cd), q_t(s.slide_cd), q_t(s.slice_cd)],
            facing: xz(s.facing),
            wall,
            evasion: s.evasion.map(|e| EvasionQ { roll: e.roll, elapsed: q_elapsed(e.total, e.left), dir: xz(e.dir) }),
            slide: s.slide.map(|l| SlideQ { dir: xz(l.dir), speed: l.speed, elapsed: q_elapsed(0.0, -l.time) }),
        }
    }

    /// `pos` overrides `self.pos` (the caller may pair the blob with a different source); pass `self.pos` normally.
    pub fn to_snapshot(&self, pos: Vector<Real>, cfg: &Cfg) -> CharacterSnapshot {
        let state = match (self.kind, self.wall) {
            (2, Some(w)) => MoveState::WallRun { normal: v3(w.normal), tangent: v3(w.tangent), time_left: dq_t(w.ticks_left) },
            (0, _) => MoveState::Grounded,
            _ => MoveState::Airborne,
        };
        CharacterSnapshot {
            pos,
            vel: vector![self.vel[0], self.vel[1], self.vel[2]],
            state,
            evasion: self.evasion.map(|e| {
                let total = if e.roll { cfg.roll_time } else { cfg.dash_time };
                let mut left = total;
                for _ in 0..e.elapsed {
                    left -= TICK_DT; // same f32 sequence the sim runs
                }
                Evasion { roll: e.roll, left, total, dir: v3(e.dir) }
            }),
            slide: self.slide.map(|l| {
                let mut time = 0.0;
                for _ in 0..l.elapsed {
                    time += TICK_DT;
                }
                SlideState { dir: v3(l.dir), speed: l.speed, time }
            }),
            facing: v3(self.facing),
            was_grounded: self.was_grounded,
            double_used: self.double_used,
            coyote: dq_t(self.timers[0]),
            jump_buf: dq_t(self.timers[1]),
            dash_cd: dq_t(self.timers[2]),
            wall_cd: dq_t(self.timers[3]),
            slide_cd: dq_t(self.timers[4]),
            slice_cd: dq_t(self.timers[5]),
        }
    }

    pub fn write(&self, w: &mut BitWriter) {
        let f = |w: &mut BitWriter, v: f32| w.write_bits(v.to_bits(), 32);
        for v in self.pos.into_iter().chain(self.vel).chain(self.facing) {
            f(w, v);
        }
        w.write_bits(self.kind as u32, 2);
        w.write_bool(self.double_used);
        w.write_bool(self.was_grounded);
        w.write_bool(self.evasion.is_some());
        w.write_bool(self.slide.is_some());
        for t in self.timers {
            w.write_bits(t as u32, 8);
        }
        if self.kind == 2 {
            let wall = self.wall.unwrap_or(WallQ { normal: [0.0; 2], tangent: [0.0; 2], ticks_left: 0 });
            for v in wall.normal.into_iter().chain(wall.tangent) {
                f(w, v);
            }
            w.write_bits(wall.ticks_left as u32, 8);
        }
        if let Some(e) = self.evasion {
            w.write_bool(e.roll);
            w.write_bits(e.elapsed as u32, 8);
            for v in e.dir {
                f(w, v);
            }
        }
        if let Some(l) = self.slide {
            for v in l.dir {
                f(w, v);
            }
            f(w, l.speed);
            w.write_bits(l.elapsed as u32, 8);
        }
    }

    pub fn read(r: &mut BitReader) -> DecodeResult<Self> {
        fn f(r: &mut BitReader) -> DecodeResult<f32> {
            let v = f32::from_bits(r.read_bits(32)?);
            if v.is_finite() { Ok(v) } else { Err(DecodeError::Malformed) }
        }
        fn f2(r: &mut BitReader) -> DecodeResult<[f32; 2]> {
            Ok([f(r)?, f(r)?])
        }
        let pos = [f(r)?, f(r)?, f(r)?];
        let vel = [f(r)?, f(r)?, f(r)?];
        let facing = f2(r)?;
        let kind = r.read_bits(2)? as u8;
        if kind > 2 {
            return Err(DecodeError::Malformed);
        }
        let double_used = r.read_bool()?;
        let was_grounded = r.read_bool()?;
        let has_ev = r.read_bool()?;
        let has_sl = r.read_bool()?;
        let mut timers = [0u8; 6];
        for t in &mut timers {
            *t = r.read_bits(8)? as u8;
        }
        let wall = if kind == 2 {
            Some(WallQ { normal: f2(r)?, tangent: f2(r)?, ticks_left: r.read_bits(8)? as u8 })
        } else {
            None
        };
        let evasion = if has_ev {
            Some(EvasionQ { roll: r.read_bool()?, elapsed: r.read_bits(8)? as u8, dir: f2(r)? })
        } else {
            None
        };
        let slide = if has_sl {
            Some(SlideQ { dir: f2(r)?, speed: f(r)?, elapsed: r.read_bits(8)? as u8 })
        } else {
            None
        };
        Ok(Self { pos, vel, kind, double_used, was_grounded, timers, facing, wall, evasion, slide })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> OwnState {
        OwnState {
            pos: [1.5, -0.25, 1e6],
            vel: [-12.34, 0.007, 30.0],
            kind: 2,
            double_used: true,
            was_grounded: false,
            timers: [1, 2, 255, 0, 99, 7],
            facing: [0.6, -0.8],
            wall: Some(WallQ { normal: [-1.0, 0.0], tangent: [0.0, -1.0], ticks_left: 130 }),
            evasion: Some(EvasionQ { roll: true, elapsed: 50, dir: [0.0, 1.0] }),
            slide: Some(SlideQ { dir: [1.0, 0.0], speed: 11.25, elapsed: 200 }),
        }
    }

    #[test]
    fn wire_roundtrip_is_exact_and_sized_as_documented() {
        for o in [full(), OwnState::default(), OwnState { kind: 1, ..Default::default() }] {
            let mut w = BitWriter::new();
            o.write(&mut w);
            let expect = 310
                + if o.kind == 2 { 136 } else { 0 }
                + if o.evasion.is_some() { 73 } else { 0 }
                + if o.slide.is_some() { 104 } else { 0 };
            assert_eq!(w.bit_len(), expect);
            let bytes = w.finish();
            assert_eq!(OwnState::read(&mut BitReader::new(&bytes)).unwrap(), o);
        }
    }

    /// The wire frame prefixes the own block with a u8 length (max 255 B). Wall-run, dash/roll and slide cannot
    /// actually coexist (a dash ends a wall-run, a slide needs the ground and no evasion), so the theoretical
    /// all-flags blob below is an upper bound on anything the sim can produce.
    #[test]
    fn size_fits_the_u8_frame_prefix_and_typical_is_small() {
        let bytes = |o: &OwnState| {
            let mut w = BitWriter::new();
            o.write(&mut w);
            w.finish().len()
        };
        assert_eq!(bytes(&full()), 78, "theoretical worst: all optional blocks present");
        assert!(bytes(&full()) <= 255);
        let typical = OwnState { kind: 0, ..Default::default() };
        assert_eq!(bytes(&typical), 39, "grounded / airborne, nothing else active");
        assert!(bytes(&typical) <= 64);
        let wall_run = OwnState { kind: 2, wall: full().wall, ..Default::default() };
        assert_eq!(bytes(&wall_run), 56);
    }

    #[test]
    fn rejects_bad_kind_non_finite_and_truncation() {
        let mut w = BitWriter::new();
        full().write(&mut w);
        let bytes = w.finish();
        assert_eq!(OwnState::read(&mut BitReader::new(&bytes[..10])), Err(DecodeError::Truncated));

        let mut bad_kind = bytes.clone();
        bad_kind[32] |= 0b11; // kind bits sit right after 8 floats (256 bits); 3 is illegal
        assert_eq!(OwnState::read(&mut BitReader::new(&bad_kind)), Err(DecodeError::Malformed));

        let mut nan = bytes.clone();
        nan[0..4].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
        assert_eq!(OwnState::read(&mut BitReader::new(&nan)), Err(DecodeError::Malformed));
    }

    #[test]
    fn timer_ticks_preserve_every_future_positive_test() {
        for t in [0.0f32, 0.004, 0.0167, 0.12, 0.5, 0.6] {
            let q = dq_t(q_t(t));
            for m in 0..40 {
                assert_eq!(t - m as f32 * TICK_DT > 1e-6, q - m as f32 * TICK_DT > 1e-6, "t {t} m {m}");
            }
        }
    }
}
