//! Reliable-over-unreliable event stream for structural state (walls, obstacles) and low-rate gameplay events
//! (kills, respawns, match flow, pickups). High-rate hits are NOT here: they ride the unreliable own block.
//! Wire tag is 6 bits (v26).
//! Server re-sends every unacked event in each snapshot datagram, oldest first, within a byte budget.
//! Client applies strictly in sequence order and acks the highest contiguous seq.

use std::collections::VecDeque;

use shinobi_proto::bits::{BitReader, BitWriter, DecodeError, DecodeResult};
use super::obstacle::{MotionPath, ObstacleDesc, ObstacleId};
use super::walls::{WallId, WallSpawn};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum InjuryRegion {
    Torso = 0,
    Head = 1,
    Neck = 2,
    ArmL = 3,
    ArmR = 4,
    LegL = 5,
    LegR = 6,
    Pelvis = 7,
}

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
    /// `player` is on `team` (`team::TEAM_*`); `TEAM_NONE` = left the match. Sent to everyone on join / switch, and
    /// replayed to late joiners, so every client always knows every player's team (own team included).
    TeamAssign { player: u16, team: u8 },
    /// Team kills (cyan, orange) whenever either changes, and when a team match ends: `result` = `TEAM_RESULT_*` (running, who won, or sudden death in progress).
    TeamScore { tick: u32, score: [u16; 2], result: u8 },
    /// A player-built piece appeared in `cell` (grid cell, centre = (cell + 0.5) * block size). `kind`: 0 floor,
    /// 1 wall, 2 ramp; `axis`: a wall's normal (0 = X, 1 = Z) or the direction a ramp rises toward (0 +X, 1 +Z, 2 -X, 3 -Z); `scale_q`: size in 1/16ths (16 = 1.0). Replayed to late joiners.
    BlockPlaced { owner: u16, cell: [i32; 3], kind: u8, axis: u8, scale_q: u8 },
    /// The server's build rules (sent to every joiner): clients predict placement with exactly these.
    BlockRules(crate::blocks::BlockRules),
    /// The block in `cell` is gone (evicted by its owner's cap, or its owner left).
    BlockRemoved { cell: [i32; 3] },
    /// The piece in `cell` was destroyed: it de-rezzes (dissolves into pixels) instead of just vanishing.
    BlockDerez { cell: [i32; 3] },
    /// Owner only: the build press was refused, `BLOCK_REFUSED_*`.
    BlockRefused { why: u8 },
    /// The piece in `cell` is hurt but standing: `hp_q` = remaining health in 1/255ths (255 = untouched, 1 = nearly
    /// gone), for cracking visuals. At most one per cell per server frame; hurt pieces are replayed to late joiners.
    BlockDamaged { cell: [i32; 3], hp_q: u8 },
    /// The match rules (sent to every joiner): kills to win (free-for-all), team kills to win (team deathmatch) and
    /// the time limit in seconds, so the HUD can show "FIRST TO n" without guessing the server's config, and the
    /// respawn delay in ticks (the death sequence and its countdown fill exactly that).
    MatchRules { frag_limit: u16, team_frag_limit: u16, time_limit_secs: u16, respawn_ticks: u16 },
    /// A star variant did something worth an effect and a sound at `pos` (docs/stars.md): `what` = `STAR_FX_*`, thrown by
    /// `owner`. The landed objects themselves ride in the snapshot.
    StarFx { tick: u32, what: u8, owner: u16, pos: [i32; 3] },
    /// A flash star whited `victim` out: `strength_q` / 255, for `ticks` sim ticks. Everyone gets it (cheap, rare); only
    /// the victim's client draws the white-out.
    StarBlind { tick: u32, victim: u16, strength_q: u8, ticks: u16 },
    /// `player`'s custom look (`shinobi_proto::character`, already validated by the server). Sent to everyone on join
    /// (catch-up) and whenever it changes; stock-look players send none.
    CharacterLook { player: u16, wire: shinobi_proto::character::Wire },
    /// A power weapon (yumi / fuda tag) moment worth an effect and a sound at `pos`: `what` = `power_pass::FX_*`, by `owner`
    /// (pickup call-outs name who took it). The arrows and tags themselves ride in the snapshot.
    PowerFx { tick: u32, what: u8, owner: u16, pos: [i32; 3] },
    /// Destructible level box `idx` (index of the `dbox` line among the level's boxes, `breaks.rs`) broke. `cascade`: it
    /// fell because what held it up broke. Everyone's predicted world removes the collider; the renderer shatters it.
    WorldBreak { idx: u16, cascade: bool },
    /// Box `idx` re-formed (whole again, collider back).
    WorldRez { idx: u16 },
    /// Box `idx` took damage and stands: `hp_q` remaining in 1/255ths (cracks).
    WorldCrack { idx: u16, hp_q: u8 },
    /// Environmental event (`env_events.rs`): `what` = `ENV_*`, `id` names a lightning bolt (mark and strike share it), `pos`
    /// the bolt's strike point (quantized like every position), `aux` ticks: warning length.
    EnvFx { what: u8, id: u8, pos: [i32; 3], aux: u16 },
    /// `player`'s stealth cloak (`cloak.rs`, docs/melee.md "Cloak") came up (`on` true) or ended / broke (`on` false).
    /// Rare (a few per minute per player), so a reliable event rather than a per-snapshot flag; every client can see
    /// the shimmer regardless of distance, so this is fair game to read.
    CloakFx { tick: u32, player: u16, on: bool },
    /// Directed injury/gore: broadcast to all for client-side blood, wounds, limbing and dismemberment.
    InjuryFx {
        tick: u32,
        attacker: u16,
        victim: u16,
        damage_q: u8,
        kind: u8,
        region: u8,
        point: [i32; 3],
        blocked: bool,
        sever: u8,
    },
}

/// `NetEvent::EnvFx::what`.
pub const ENV_BOLT_MARK: u8 = 3; // (0..=2 were the quake cues, removed; ids are not reused)
pub const ENV_BOLT_STRIKE: u8 = 4;

/// `NetEvent::StarFx::what`.
/// A flash star burst (0, 1, 2, 4, 5, 6 were the barrel and canister cues before v23).
pub const STAR_FX_FLASH_BURST: u8 = 3;

pub const BLOCK_REFUSED_OCCUPIED: u8 = 0;
pub const BLOCK_REFUSED_BLOCKED: u8 = 1;
pub const BLOCK_REFUSED_COOLDOWN: u8 = 2;

/// `TeamAssign.team` for a player who left.
pub const TEAM_NONE: u8 = 3;
pub const TEAM_RESULT_RUNNING: u8 = 0;
pub const TEAM_RESULT_CYAN: u8 = 1;
pub const TEAM_RESULT_ORANGE: u8 = 2;
pub const TEAM_RESULT_DRAW: u8 = 3;
/// Tied at the end: sudden death is running (first team kill wins).
pub const TEAM_RESULT_SUDDEN: u8 = 4;

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
                w.write_bits(0, 6);
                w.write_varint(s.id);
                w.write_bits(s.owner as u32, 8);
                w.write_varint(s.spawn_tick);
                write_v3(w, s.a, [0; 3]);
                write_v3(w, s.b, s.a); // segments are short: delta from `a`
            }
            Self::WallKill { id, tick } => {
                w.write_bits(1, 6);
                w.write_varint(*id);
                w.write_varint(*tick);
            }
            Self::ObstacleSpawn(d) => {
                w.write_bits(2, 6);
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
                w.write_bits(3, 6);
                w.write_bits(*id as u32, 16);
                w.write_varint(*tick);
            }
            Self::Kill { tick, killer, victim, kind } => {
                w.write_bits(4, 6);
                w.write_varint(*tick);
                w.write_bits(*killer as u32, 16);
                w.write_bits(*victim as u32, 16);
                w.write_bits(*kind as u32 & 7, 3);
            }
            Self::Respawn { tick, player } => {
                w.write_bits(5, 6);
                w.write_varint(*tick);
                w.write_bits(*player as u32, 16);
            }
            Self::MatchCountdown { tick, phase, seconds } => {
                w.write_bits(6, 6);
                w.write_varint(*tick);
                w.write_bits(*phase as u32 & 3, 2);
                w.write_bits(*seconds as u32, 8);
            }
            Self::MatchStart { tick, map } => {
                w.write_bits(7, 6);
                w.write_varint(*tick);
                w.write_varint(*map);
            }
            Self::MatchEnd { tick, winner, reason } => {
                w.write_bits(8, 6);
                w.write_varint(*tick);
                w.write_bits(*winner as u32, 16);
                w.write_bits(*reason as u32 & 3, 2);
            }
            Self::MapChange { tick, map } => {
                w.write_bits(9, 6);
                w.write_varint(*tick);
                w.write_varint(*map);
            }
            Self::PickupSpawned { tick, id, pos, kind } => {
                w.write_bits(10, 6);
                w.write_varint(*tick);
                w.write_varint(*id);
                write_v3(w, *pos, [0; 3]);
                w.write_bits(*kind as u32, 8);
            }
            Self::PickupTaken { tick, id, player } => {
                w.write_bits(11, 6);
                w.write_varint(*tick);
                w.write_varint(*id);
                w.write_bits(*player as u32, 16);
            }
            Self::RopeAttach { tick, player, anchor } => {
                w.write_bits(12, 6);
                w.write_varint(*tick);
                w.write_bits(*player as u32, 16);
                write_v3(w, *anchor, [0; 3]);
            }
            Self::RopeRelease { tick, player } => {
                w.write_bits(13, 6);
                w.write_varint(*tick);
                w.write_bits(*player as u32, 16);
            }
            Self::TeamAssign { player, team } => {
                w.write_bits(14, 6);
                w.write_bits(*player as u32, 16);
                w.write_bits(*team as u32 & 3, 2);
            }
            Self::TeamScore { tick, score, result } => {
                w.write_bits(15, 6);
                w.write_varint(*tick);
                w.write_varint(score[0] as u32);
                w.write_varint(score[1] as u32);
                w.write_bits(*result as u32 & 7, 3);
            }
            Self::BlockPlaced { owner, cell, kind, axis, scale_q } => {
                w.write_bits(16, 6);
                w.write_bits(*owner as u32, 16);
                write_v3(w, *cell, [0; 3]);
                w.write_bits(*kind as u32 & 3, 2);
                w.write_bits(*axis as u32 & 3, 2);
                w.write_bits(*scale_q as u32, 8);
            }
            Self::MatchRules { frag_limit, team_frag_limit, time_limit_secs, respawn_ticks } => {
                w.write_bits(22, 6);
                w.write_varint(*frag_limit as u32);
                w.write_varint(*team_frag_limit as u32);
                w.write_varint(*time_limit_secs as u32);
                w.write_varint(*respawn_ticks as u32);
            }
            Self::BlockRules(r) => {
                w.write_bits(20, 6);
                for v in [r.thickness_mm, r.range_dm, r.cooldown_ticks, r.scale_per_kill_milli, r.scale_cap_centi, r.max_per_owner] {
                    w.write_varint(v as u32);
                }
            }
            Self::BlockDerez { cell } => {
                w.write_bits(19, 6);
                write_v3(w, *cell, [0; 3]);
            }
            Self::BlockRemoved { cell } => {
                w.write_bits(17, 6);
                write_v3(w, *cell, [0; 3]);
            }
            Self::BlockDamaged { cell, hp_q } => {
                w.write_bits(21, 6);
                write_v3(w, *cell, [0; 3]);
                w.write_bits(*hp_q as u32, 8);
            }
            Self::BlockRefused { why } => {
                w.write_bits(18, 6);
                w.write_bits(*why as u32 & 3, 2);
            }
            Self::StarFx { tick, what, owner, pos } => {
                w.write_bits(23, 6);
                w.write_varint(*tick);
                w.write_bits(*what as u32 & 7, 3);
                w.write_bits(*owner as u32, 16);
                write_v3(w, *pos, [0; 3]);
            }
            Self::StarBlind { tick, victim, strength_q, ticks } => {
                w.write_bits(24, 6);
                w.write_varint(*tick);
                w.write_bits(*victim as u32, 16);
                w.write_bits(*strength_q as u32, 8);
                w.write_bits(*ticks as u32, 16);
            }
            Self::CharacterLook { player, wire } => {
                w.write_bits(25, 6);
                w.write_bits(*player as u32, 16);
                for b in wire {
                    w.write_bits(*b as u32, 8);
                }
            }
            Self::PowerFx { tick, what, owner, pos } => {
                w.write_bits(26, 6);
                w.write_varint(*tick);
                w.write_bits(*what as u32 & 31, 5);
                w.write_bits(*owner as u32, 16);
                write_v3(w, *pos, [0; 3]);
            }
            Self::WorldBreak { idx, cascade } => {
                w.write_bits(27, 6);
                w.write_bits(*idx as u32, 16);
                w.write_bits(*cascade as u32, 1);
            }
            Self::WorldRez { idx } => {
                w.write_bits(28, 6);
                w.write_bits(*idx as u32, 16);
            }
            Self::WorldCrack { idx, hp_q } => {
                w.write_bits(29, 6);
                w.write_bits(*idx as u32, 16);
                w.write_bits(*hp_q as u32, 8);
            }
            Self::EnvFx { what, id, pos, aux } => {
                w.write_bits(31, 6);
                w.write_bits(*what as u32 & 7, 3);
                w.write_bits(*id as u32, 8);
                write_v3(w, *pos, [0; 3]);
                w.write_bits(*aux as u32, 16);
            }
            Self::CloakFx { tick, player, on } => {
                w.write_bits(30, 6);
                w.write_varint(*tick);
                w.write_bits(*player as u32, 16);
                w.write_bool(*on);
            }
            Self::InjuryFx { tick, attacker, victim, damage_q, kind, region, point, blocked, sever } => {
                w.write_bits(32, 6);
                w.write_varint(*tick);
                w.write_bits(*attacker as u32, 16);
                w.write_bits(*victim as u32, 16);
                w.write_bits(*damage_q as u32, 8);
                w.write_bits(*kind as u32 & 15, 4);
                w.write_bits(*region as u32 & 7, 3);
                write_v3(w, *point, [0; 3]);
                w.write_bool(*blocked);
                w.write_bits(*sever as u32 & 15, 4);
            }
        }
    }

    pub fn read(r: &mut BitReader) -> DecodeResult<Self> {
        Ok(match r.read_bits(6)? {
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
            14 => Self::TeamAssign { player: r.read_bits(16)? as u16, team: r.read_bits(2)? as u8 },
            15 => {
                let tick = r.read_varint()?;
                let score = [
                    u16::try_from(r.read_varint()?).map_err(|_| DecodeError::Malformed)?,
                    u16::try_from(r.read_varint()?).map_err(|_| DecodeError::Malformed)?,
                ];
                Self::TeamScore { tick, score, result: r.read_bits(3)? as u8 }
            }
            16 => Self::BlockPlaced {
                owner: r.read_bits(16)? as u16,
                cell: read_v3(r, [0; 3])?,
                kind: r.read_bits(2)? as u8,
                axis: r.read_bits(2)? as u8,
                scale_q: r.read_bits(8)? as u8,
            },
            19 => Self::BlockDerez { cell: read_v3(r, [0; 3])? },
            20 => {
                let mut v = [0u16; 6];
                for x in &mut v {
                    *x = u16::try_from(r.read_varint()?).map_err(|_| DecodeError::Malformed)?;
                }
                Self::BlockRules(crate::blocks::BlockRules {
                    thickness_mm: v[0],
                    range_dm: v[1],
                    cooldown_ticks: v[2],
                    scale_per_kill_milli: v[3],
                    scale_cap_centi: v[4],
                    max_per_owner: v[5],
                })
            }
            17 => Self::BlockRemoved { cell: read_v3(r, [0; 3])? },
            18 => Self::BlockRefused { why: r.read_bits(2)? as u8 },
            22 => {
                let mut v = [0u16; 4];
                for x in &mut v {
                    *x = u16::try_from(r.read_varint()?).map_err(|_| DecodeError::Malformed)?;
                }
                Self::MatchRules { frag_limit: v[0], team_frag_limit: v[1], time_limit_secs: v[2], respawn_ticks: v[3] }
            }
            21 => Self::BlockDamaged { cell: read_v3(r, [0; 3])?, hp_q: r.read_bits(8)? as u8 },
            23 => Self::StarFx { tick: r.read_varint()?, what: r.read_bits(3)? as u8, owner: r.read_bits(16)? as u16, pos: read_v3(r, [0; 3])? },
            25 => {
                let player = r.read_bits(16)? as u16;
                let mut wire = [0u8; shinobi_proto::character::WIRE_LEN];
                for b in wire.iter_mut() {
                    *b = r.read_bits(8)? as u8;
                }
                Self::CharacterLook { player, wire }
            }
            24 => Self::StarBlind { tick: r.read_varint()?, victim: r.read_bits(16)? as u16, strength_q: r.read_bits(8)? as u8, ticks: r.read_bits(16)? as u16 },
            26 => Self::PowerFx { tick: r.read_varint()?, what: r.read_bits(5)? as u8, owner: r.read_bits(16)? as u16, pos: read_v3(r, [0; 3])? },
            27 => Self::WorldBreak { idx: r.read_bits(16)? as u16, cascade: r.read_bits(1)? == 1 },
            28 => Self::WorldRez { idx: r.read_bits(16)? as u16 },
            29 => Self::WorldCrack { idx: r.read_bits(16)? as u16, hp_q: r.read_bits(8)? as u8 },
            31 => Self::EnvFx { what: r.read_bits(3)? as u8, id: r.read_bits(8)? as u8, pos: read_v3(r, [0; 3])?, aux: r.read_bits(16)? as u16 },
            30 => Self::CloakFx { tick: r.read_varint()?, player: r.read_bits(16)? as u16, on: r.read_bool()? },
            32 => Self::InjuryFx {
                tick: r.read_varint()?,
                attacker: r.read_bits(16)? as u16,
                victim: r.read_bits(16)? as u16,
                damage_q: r.read_bits(8)? as u8,
                kind: r.read_bits(4)? as u8,
                region: r.read_bits(3)? as u8,
                point: read_v3(r, [0; 3])?,
                blocked: r.read_bool()?,
                sever: r.read_bits(4)? as u8,
            },
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
    fn block_events_roundtrip_and_stay_small() {
        let evs = [
            NetEvent::BlockPlaced { owner: 7, cell: [-12, 3, 40], kind: 1, axis: 1, scale_q: 24 },
            NetEvent::BlockDerez { cell: [-12, 3, 40] },
            NetEvent::BlockRules(crate::blocks::BlockCfg::default().rules()),
            NetEvent::BlockRemoved { cell: [-12, 3, 40] },
            NetEvent::BlockDamaged { cell: [-12, 3, 40], hp_q: 117 },
            NetEvent::BlockRefused { why: BLOCK_REFUSED_COOLDOWN },
        ];
        let mut w = BitWriter::new();
        for e in &evs {
            e.write(&mut w);
        }
        let bytes = w.finish();
        assert!(bytes.len() <= 32, "{} B for place + derez + rules + remove + damaged + refuse", bytes.len());
        let mut r = BitReader::new(&bytes);
        for e in &evs {
            assert_eq!(&NetEvent::read(&mut r).unwrap(), e);
        }
    }

    #[test]
    fn team_events_roundtrip() {
        let evs = [
            NetEvent::TeamAssign { player: 201, team: 1 },
            NetEvent::TeamAssign { player: 9, team: TEAM_NONE },
            NetEvent::TeamScore { tick: 99_999, score: [49, 50], result: TEAM_RESULT_ORANGE },
            NetEvent::MatchRules { frag_limit: 10, team_frag_limit: 50, time_limit_secs: 360, respawn_ticks: 150 },
            NetEvent::TeamScore { tick: 100_000, score: [50, 50], result: TEAM_RESULT_SUDDEN },
        ];
        let mut w = BitWriter::new();
        for e in &evs {
            e.write(&mut w);
        }
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        for e in &evs {
            assert_eq!(&NetEvent::read(&mut r).unwrap(), e);
        }
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
            NetEvent::InjuryFx {
                tick: 9_001,
                attacker: 7,
                victim: 12,
                damage_q: 34,
                kind: shinobi_proto::gameplay::KIND_SLASH,
                region: InjuryRegion::ArmL as u8,
                point: [-20_000, 5_120, 33_333],
                blocked: false,
                sever: 0,
            },
            NetEvent::InjuryFx {
                tick: 1,
                attacker: 0,
                victim: 65_535,
                damage_q: 255,
                kind: shinobi_proto::gameplay::KIND_TAG,
                region: InjuryRegion::Head as u8,
                point: [0, 0, 0],
                blocked: true,
                sever: 3,
            },
        ];
        for ev in evs {
            let mut w = BitWriter::new();
            ev.write(&mut w);
            let b = w.finish();
            assert_eq!(NetEvent::read(&mut BitReader::new(&b)).unwrap(), ev, "{ev:?}");
        }
        // tag 63 does not exist (garbage must not decode as an event)
        let mut w = BitWriter::new();
        w.write_bits(63, 6);
        let b = w.finish();
        assert!(NetEvent::read(&mut BitReader::new(&b)).is_err());
        // match countdown with an impossible phase is rejected
        let mut w = BitWriter::new();
        w.write_bits(6, 6);
        w.write_varint(1);
        w.write_bits(3, 2);
        w.write_bits(5, 8);
        assert!(NetEvent::read(&mut BitReader::new(&w.finish())).is_err());
    }

    #[test]
    fn star_events_roundtrip() {
        let evs = [
            NetEvent::StarFx { tick: 70_000, what: STAR_FX_FLASH_BURST, owner: 9, pos: [-20_000, 512, 33_333] },
            NetEvent::StarFx { tick: 70_001, what: STAR_FX_FLASH_BURST, owner: 65_535, pos: [0, 0, 0] },
            NetEvent::StarBlind { tick: 70_002, victim: 3, strength_q: 255, ticks: 72 },
            NetEvent::CharacterLook { player: 7, wire: shinobi_proto::character::preset(1).to_wire() },
            NetEvent::PowerFx { tick: 70_003, what: 13, owner: 4, pos: [-20_000, 512, 33_333] },
            NetEvent::WorldBreak { idx: 65_535, cascade: true },
            NetEvent::WorldBreak { idx: 0, cascade: false },
            NetEvent::WorldRez { idx: 300 },
            NetEvent::WorldCrack { idx: 12, hp_q: 77 },
            NetEvent::EnvFx { what: ENV_BOLT_MARK, id: 0, pos: [0, 0, 0], aux: 150 },
            NetEvent::EnvFx { what: ENV_BOLT_MARK, id: 255, pos: [-20_000, 512, 33_333], aux: 120 },
            NetEvent::EnvFx { what: ENV_BOLT_STRIKE, id: 9, pos: [1, -2, 3], aux: 0 },
            NetEvent::CloakFx { tick: 70_004, player: 6, on: true },
            NetEvent::CloakFx { tick: 70_005, player: 65_535, on: false },
        ];
        let mut w = BitWriter::new();
        for e in &evs {
            e.write(&mut w);
        }
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        for e in &evs {
            assert_eq!(&NetEvent::read(&mut r).unwrap(), e);
        }
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
