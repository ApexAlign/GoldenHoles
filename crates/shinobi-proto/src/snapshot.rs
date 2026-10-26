//! Server -> client entity snapshots, delta-compressed against the newest snapshot the client acked.
//!
//! Position residuals are taken against a velocity-extrapolated baseline (integer math, bit-exact on both
//! sides), so steady running or dashing costs ~1 byte per axis.

use super::bits::{BitReader, BitWriter, DecodeError, DecodeResult};
use super::TICK_HZ;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum MoveKind {
    #[default]
    Grounded = 0,
    Airborne = 1,
    WallRun = 2,
    /// Dash or roll; `FLAG_ROLL` tells which.
    Evading = 3,
}

impl MoveKind {
    fn from_bits(b: u32) -> Self {
        match b & 3 {
            0 => Self::Grounded,
            1 => Self::Airborne,
            2 => Self::WallRun,
            _ => Self::Evading,
        }
    }
}

pub const FLAG_INVULN: u8 = 1 << 0;
pub const FLAG_ROLL: u8 = 1 << 1;
pub const FLAG_DEAD: u8 = 1 << 2;

/// `EntityState::fx` bits (combat state other players can see).
/// Shield up (the block / parry stance).
pub const FX_BLOCK: u8 = 1 << 0;
/// The strike window is open: hits on this player right now deal bonus damage (recovering from a swing, or
/// staggered after a parry). The targeting triangle pulses on it.
pub const FX_OPEN: u8 = 1 << 1;
/// Staggered: this player's attack was just parried; they cannot act for a moment.
pub const FX_STAGGER: u8 = 1 << 2;
/// Marked by a MARK star: the thrower's next katana hit on this player does bonus damage.
pub const FX_MARK: u8 = 1 << 3;
/// Kinetic Battery aura is up: the katana glows so everyone can read the 1.2x threat.
pub const FX_AURA: u8 = 1 << 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct EntityState {
    /// 1/512 m
    pub pos: [i32; 3],
    /// 1/64 m/s
    pub vel: [i16; 3],
    pub yaw: u16,
    pub kind: MoveKind,
    pub flags: u8,
    /// `FX_*` combat-state bits (5).
    pub fx: u8,
}

/// `flags` bits 3..7 hold health LOST in 1/31 steps (0 = full health, so a default entity is healthy).
pub const HP_SHIFT: u32 = 3;
pub const HP_STEPS: u8 = 31;

pub fn hp_lost5(flags: u8) -> u8 {
    flags >> HP_SHIFT
}

/// 0.0..=1.0 (1/31 resolution; exact health for the owner travels in the own block).
pub fn hp_frac(flags: u8) -> f32 {
    1.0 - hp_lost5(flags) as f32 / HP_STEPS as f32
}

/// Replaces the health bits of `flags`.
pub fn with_hp_frac(flags: u8, frac: f32) -> u8 {
    let lost = ((1.0 - frac.clamp(0.0, 1.0)) * HP_STEPS as f32).round() as u8;
    (flags & ((1 << HP_SHIFT) - 1)) | (lost << HP_SHIFT)
}

impl EntityState {
    /// kind (2 bits) + flags (8 bits) + fx (5 bits), see `FLAG_*`, `hp_lost5` and `FX_*`.
    fn status_bits(&self) -> u32 {
        self.kind as u32 | ((self.flags as u32) << 2) | (((self.fx & 31) as u32) << 10)
    }
}

pub type EntityId = u16;

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Snapshot {
    pub tick: u32,
    /// Sorted by id, ids unique.
    pub entities: Vec<(EntityId, EntityState)>,
}

impl Snapshot {
    pub fn get(&self, id: EntityId) -> Option<&EntityState> {
        self.entities.binary_search_by_key(&id, |e| e.0).ok().map(|i| &self.entities[i].1)
    }
}

/// Where the baseline entity is expected after `dticks`, from its own velocity.
/// vel_u/64 m/s * dt/60 s * 512 u/m = vel_u * dt * 2/15.
fn predict_pos(base: &EntityState, dticks: u32) -> [i32; 3] {
    let f = |p: i32, v: i16| p.wrapping_add((v as i64 * dticks as i64 * 2 / 15) as i32);
    [f(base.pos[0], base.vel[0]), f(base.pos[1], base.vel[1]), f(base.pos[2], base.vel[2])]
}

const M_POS: u32 = 1;
const M_VEL: u32 = 2;
const M_YAW: u32 = 4;
const M_STATUS: u32 = 8;

const _: () = assert!(TICK_HZ == 60, "predict_pos constant assumes 60 Hz");

pub fn encode_delta(baseline: Option<&Snapshot>, cur: &Snapshot) -> Vec<u8> {
    let baseline = baseline.filter(|b| b.tick < cur.tick);
    let mut w = BitWriter::new();
    w.write_bool(baseline.is_some());
    match baseline {
        Some(b) => {
            w.write_varint(b.tick);
            w.write_varint(cur.tick - b.tick);
        }
        None => w.write_varint(cur.tick),
    }
    let dticks = baseline.map_or(0, |b| cur.tick - b.tick);

    // removals
    let removed: Vec<EntityId> = baseline
        .map(|b| b.entities.iter().map(|e| e.0).filter(|id| cur.get(*id).is_none()).collect())
        .unwrap_or_default();
    w.write_varint(removed.len() as u32);
    let mut prev = 0u32;
    for id in &removed {
        w.write_varint(*id as u32 - prev);
        prev = *id as u32;
    }

    w.write_varint(cur.entities.len() as u32);
    let mut prev = 0u32;
    for (i, (id, st)) in cur.entities.iter().enumerate() {
        w.write_varint(if i == 0 { *id as u32 } else { *id as u32 - prev - 1 });
        prev = *id as u32;

        let base = baseline.and_then(|b| b.get(*id));
        let (pred_pos, base_st) = match base {
            Some(b) => (predict_pos(b, dticks), *b),
            None => ([0; 3], EntityState::default()),
        };
        let mut mask = 0;
        if st.pos != pred_pos {
            mask |= M_POS;
        }
        if st.vel != base_st.vel {
            mask |= M_VEL;
        }
        if st.yaw != base_st.yaw {
            mask |= M_YAW;
        }
        if st.status_bits() != base_st.status_bits() {
            mask |= M_STATUS;
        }
        if base.is_some() {
            w.write_bool(mask != 0);
            if mask == 0 {
                continue;
            }
            w.write_bits(mask, 4);
        } else {
            mask = 0xf; // new entity: full state
        }
        if mask & M_POS != 0 {
            for a in 0..3 {
                w.write_zigzag(st.pos[a].wrapping_sub(pred_pos[a]));
            }
        }
        if mask & M_VEL != 0 {
            for a in 0..3 {
                w.write_zigzag(st.vel[a] as i32 - base_st.vel[a] as i32);
            }
        }
        if mask & M_YAW != 0 {
            w.write_zigzag(st.yaw.wrapping_sub(base_st.yaw) as i16 as i32);
        }
        if mask & M_STATUS != 0 {
            w.write_bits(st.status_bits(), 15);
        }
    }
    w.finish()
}

/// `lookup` resolves a baseline by tick (the client keeps its last few snapshots).
pub fn decode_delta<'a>(
    buf: &[u8],
    lookup: impl Fn(u32) -> Option<&'a Snapshot>,
) -> DecodeResult<Snapshot> {
    let mut r = BitReader::new(buf);
    let (tick, baseline) = if r.read_bool()? {
        let base_tick = r.read_varint()?;
        let d = r.read_varint()?;
        let base = lookup(base_tick).ok_or(DecodeError::BaselineMissing(base_tick))?;
        (base_tick.checked_add(d).ok_or(DecodeError::Malformed)?, Some(base))
    } else {
        (r.read_varint()?, None)
    };
    let dticks = baseline.map_or(0, |b| tick - b.tick);

    let n_removed = r.read_varint()?;
    let mut removed = Vec::new();
    let mut prev = 0u32;
    for _ in 0..n_removed.min(1024) {
        prev = prev.checked_add(r.read_varint()?).ok_or(DecodeError::Malformed)?;
        removed.push(prev as EntityId);
    }

    let n = r.read_varint()?;
    if n > u16::MAX as u32 {
        return Err(DecodeError::Malformed);
    }
    let mut entities = Vec::with_capacity(n as usize);
    let mut prev = 0u32;
    for i in 0..n {
        let d = r.read_varint()?;
        let id = if i == 0 { d } else { prev.checked_add(d).and_then(|v| v.checked_add(1)).ok_or(DecodeError::Malformed)? };
        if id > u16::MAX as u32 {
            return Err(DecodeError::Malformed);
        }
        prev = id;
        let base = baseline.and_then(|b| b.get(id as EntityId));
        let (pred_pos, mut st) = match base {
            Some(b) => (predict_pos(b, dticks), *b),
            None => ([0; 3], EntityState::default()),
        };
        st.pos = pred_pos;
        let mask = if base.is_some() {
            if r.read_bool()? { r.read_bits(4)? } else { 0 }
        } else {
            0xf
        };
        if mask & M_POS != 0 {
            for a in 0..3 {
                st.pos[a] = pred_pos[a].wrapping_add(r.read_zigzag()?);
            }
        }
        if mask & M_VEL != 0 {
            for a in 0..3 {
                let v = st.vel[a] as i32 + r.read_zigzag()?;
                st.vel[a] = i16::try_from(v).map_err(|_| DecodeError::Malformed)?;
            }
        }
        if mask & M_YAW != 0 {
            st.yaw = st.yaw.wrapping_add(r.read_zigzag()? as u16);
        }
        if mask & M_STATUS != 0 {
            let b = r.read_bits(15)?;
            st.kind = MoveKind::from_bits(b);
            st.flags = (b >> 2) as u8;
            st.fx = ((b >> 10) & 31) as u8;
        }
        entities.push((id as EntityId, st));
    }
    let _ = removed; // entities absent from `entities` are gone; list kept on the wire for fast client-side despawn hooks
    Ok(Snapshot { tick, entities })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(x: i32, vx: i16, yaw: u16) -> EntityState {
        EntityState { pos: [x, 512, -x], vel: [vx, 0, -vx], yaw, kind: MoveKind::Grounded, flags: 0, fx: 0 }
    }

    #[test]
    fn full_and_delta_roundtrip() {
        let a = Snapshot { tick: 100, entities: vec![(1, st(0, 576, 10)), (5, st(4000, 0, 0)), (9, st(-50, 10, 65535))] };
        let full = encode_delta(None, &a);
        assert_eq!(decode_delta(&full, |_| None).unwrap(), a);

        // 2 ticks later: entity 1 runs on its velocity (9 m/s = 576 u/s -> 19 u/tick*2), 5 dashes, 9 removed, 12 joins
        let mut e1 = st(0, 576, 10);
        e1.pos = predict_pos(&a.entities[0].1, 2);
        let mut e5 = st(4000, 0, 0);
        e5.vel = [1920, 0, 0];
        e5.kind = MoveKind::Evading;
        e5.flags = FLAG_INVULN;
        e5.yaw = 3;
        let b = Snapshot { tick: 102, entities: vec![(1, e1), (5, e5), (12, st(7, 0, 0))] };
        let delta = encode_delta(Some(&a), &b);
        let got = decode_delta(&delta, |t| (t == 100).then_some(&a)).unwrap();
        assert_eq!(got, b);
        assert!(delta.len() < full.len() + 4, "delta {} vs full {}", delta.len(), full.len());
    }

    #[test]
    fn steady_runner_is_cheap() {
        let mut a = Snapshot { tick: 10, entities: vec![] };
        let mut b = Snapshot { tick: 12, entities: vec![] };
        for id in 0..8u16 {
            let s = st(1000 * id as i32, 576, 100);
            a.entities.push((id, s));
            let mut n = s;
            n.pos = predict_pos(&s, 2);
            b.entities.push((id, n));
        }
        let delta = encode_delta(Some(&a), &b);
        // 8 entities, all unchanged relative to prediction: ~1 bit each + header
        assert!(delta.len() <= 16, "{}", delta.len());
        assert_eq!(decode_delta(&delta, |_| Some(&a)).unwrap(), b);
    }

    #[test]
    fn health_bits_roundtrip_through_delta() {
        let mut a = st(0, 0, 0);
        assert_eq!(hp_frac(a.flags), 1.0, "default flags = healthy");
        let base = Snapshot { tick: 1, entities: vec![(1, a)] };
        a.flags = with_hp_frac(FLAG_INVULN | FLAG_DEAD, 0.0);
        assert_eq!((hp_lost5(a.flags), a.flags & 7), (31, FLAG_INVULN | FLAG_DEAD));
        let next = Snapshot { tick: 2, entities: vec![(1, a)] };
        let d = encode_delta(Some(&base), &next);
        assert_eq!(decode_delta(&d, |_| Some(&base)).unwrap(), next);
        for f in [0.0, 0.25, 0.5, 0.99, 1.0] {
            assert!((hp_frac(with_hp_frac(0, f)) - f).abs() <= 0.5 / 31.0 + 1e-6);
        }
        assert_eq!(with_hp_frac(FLAG_ROLL, 1.0), FLAG_ROLL, "other flag bits survive");
    }

    #[test]
    fn missing_baseline_reported() {
        let a = Snapshot { tick: 1, entities: vec![(1, st(0, 0, 0))] };
        let b = Snapshot { tick: 3, entities: vec![(1, st(1, 0, 0))] };
        let d = encode_delta(Some(&a), &b);
        assert_eq!(decode_delta(&d, |_| None), Err(DecodeError::BaselineMissing(1)));
    }
}

/// Snapshot entity ids at or above this are in-flight shurikens (yaw = owner id, vel = direction * speed), not players.
/// `flags` of such an entity: bits 0..2 bounce count, bits 2..4 the star kind (`StarKind`: 0 standard, 2 flash). (Landed
/// barrel / canister world objects, ids from 0x5000, are gone since v23.)
pub const PROJECTILE_ID_BASE: EntityId = 0x4000;
/// Scale of a surface normal / aim direction packed into a snapshot entity's `vel` (tags, the drawn-bow glint).
pub const STUCK_NORMAL_SPEED: f32 = 8.0;

/// Power-weapon entities (docs/powerups.md), ids from `POWER_ID_BASE` (stable for the object's life, `uid & 0xFF`):
/// * `ARROW_ID_BASE..`: a yumi arrow. `yaw` = owner id, `vel` = its velocity, `flags` bit 0 = turned around by a parry.
/// * `TAG_ID_BASE..`: a fuda tag. `yaw` = owner id. Flying (`TAG_FLYING`): `vel` is its velocity. Stuck: `vel` = surface normal
///   * `STUCK_NORMAL_SPEED`, or with `TAG_ON_PLAYER` `vel[0]` = the victim's id; `TAG_ARMED`; `fx` = fuse left in 1/15ths.
/// * `DRAW_ID_BASE..` (+ the drawer's id & 0xFF): a bow held out. `pos` = eye, `vel` = aim * `STUCK_NORMAL_SPEED`, `yaw` =
///   owner id, `fx` = draw progress in 1/15ths. Others see the glint of the aim line from it.
pub const POWER_ID_BASE: EntityId = 0x6000;
pub const ARROW_ID_BASE: EntityId = 0x6000;
pub const TAG_ID_BASE: EntityId = 0x6100;
pub const DRAW_ID_BASE: EntityId = 0x6200;
pub const POWER_ID_END: EntityId = 0x6300;
pub const ARROW_REFLECTED: u8 = 1 << 0;
/// `flags` bit 1 of an arrow entity: a Tether Arrow (yanks its victim toward the shooter).
pub const ARROW_TETHER: u8 = 1 << 1;
pub const TAG_FLYING: u8 = 1 << 0;
pub const TAG_ON_PLAYER: u8 = 1 << 1;
pub const TAG_ARMED: u8 = 1 << 2;

/// What a snapshot entity id is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityClass {
    Player,
    Disc,
    Arrow,
    Tag,
    Draw,
}

pub fn entity_class(id: EntityId) -> EntityClass {
    match id {
        i if i < PROJECTILE_ID_BASE => EntityClass::Player,
        i if i < POWER_ID_BASE => EntityClass::Disc,
        i if i < TAG_ID_BASE => EntityClass::Arrow,
        i if i < DRAW_ID_BASE => EntityClass::Tag,
        _ => EntityClass::Draw,
    }
}

/// Pack a disc's `flags`: bounce count (0..=3) and star kind (0..=3).
pub fn disc_flags(bounces: u8, kind: u8) -> u8 {
    (bounces.min(3)) | ((kind & 3) << 2)
}

/// Star kind (`StarKind` wire value) of a projectile entity's `flags`.
pub fn disc_kind(flags: u8) -> u8 {
    (flags >> 2) & 3
}
