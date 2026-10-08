//! Wire protocol v3: handshake + server -> client datagram framing. THIS DOC IS THE SPEC; code follows it.
//!
//! # Handshake (stateless for strangers)
//! * `HELLO` client -> server, 13 bytes: `"GSHN" | VERSION u8 | cookie u64 LE`. First send uses cookie 0.
//! * `CHALLENGE` server -> client, 9 bytes: `[TAG_CHALLENGE 0x43] | cookie u64 LE`. Never larger than HELLO
//!   (no amplification). cookie = keyed hash(server secret, FULL source addr, coarse time bucket); the server
//!   keeps NO per-address state before the cookie comes back. Valid for the current and previous bucket.
//! * Client answers CHALLENGE with `HELLO` carrying that cookie (and keeps resending every ~100 ms, taking the
//!   newest cookie). A HELLO whose cookie verifies allocates the slot and gets `WELCOME`.
//! * `WELCOME` server -> client, 7 bytes: `[0x57] | player_id u16 LE | server_tick u32 LE` (unchanged).
//! * HELLO with another VERSION gets `KICK(BadVersion)`; known peers re-sending HELLO get `WELCOME` again.
//! * v18: custom characters (v17) and power weapons (yumi / fuda tag: snapshot entities, `NetEvent::PowerFx` = event 26) merged; both changed the wire.
//! * v17: `ClientPacket.look` (custom character, `character.rs`) rides the first packets after WELCOME until the server's
//!   `NetEvent::CharacterLook` echo arrives; the server re-validates it and replicates it to everyone.
//! * After WELCOME the client sends `ClientPacket`s (see `intent.rs`) and acks `snapshot_tick` / `event_seq`.
//!
//! # SNAPSHOT datagram (server -> client)
//! `[TAG_SNAPSHOT 0x53] | events_len u16 LE | own_len u16 LE | board_len u8 | events | own | board | snapshot`
//! * `events`: reliable event stream (`shinobi_sim::events`): `varint count`, `varint first_seq`, events.
//!   Highest priority; unacked events are re-sent every datagram (byte budgeted).
//! * `own`: owner-only, bit-packed, may be empty (`own_len 0` = nothing for this peer):
//!   `bit has_state` then, if set, `varint acked_intent_tick` + `OwnState` (sim `own_state.rs`, exact f32);
//!   then always (when own_len > 0): `varint hp_x10` (exact health x10, 0 = dead), `u4 n_hits` (max 15), then n x
//!   `varint other_id` (u16; 0 = none/hazard) | `u8 amount` (damage, clamped) | `u3 kind` | `bit taken`
//!   (0 = you dealt it -> hit marker, 1 = you took it -> damage direction) | `bit kill`.
//!   Hits are UNRELIABLE and capped (15 newest per datagram): loss only costs a marker. Dead players get
//!   `has_state = 0` (extras only). `kind`: 0 shuriken, 1 slash, 2 grid beam, 3 obstacle, 4 light wall.
//! * `board`: periodic scoreboard (every 60 ticks and on phase change), same for all peers, may be empty:
//!   `u2 phase` (0 warmup, 1 live, 2 intermission) | `varint map` | `bit has_left` [`varint ticks_left`] |
//!   `u16 winner` (0 = none) | `u8 n` | n x (`u8 id` | `varint kills` | `varint deaths` | `bit alive`).
//! * `snapshot`: delta-compressed entities (`snapshot.rs`). Entity `flags` now also carry health:
//!   bits 0..2 = invuln / roll / dead, bits 3..7 = health LOST in 1/31 steps (0 = full health, so a default
//!   `flags` is a healthy entity). NPC entities use ids >= 0x8000 (players are 1..=255).
//!
//! # Reliable events added in v3 (`shinobi_sim::events::NetEvent`, 4-bit tag)
//! `Kill{tick,killer,victim,kind}`, `Respawn{tick,player}`, `MatchCountdown{tick,phase,seconds}`,
//! `MatchStart{tick,map}`, `MatchEnd{tick,winner,reason}`, `MapChange{tick,map}` (clear ALL walls, obstacles and
//! pickups, then expect fresh spawn events), `PickupSpawned{tick,id,pos,kind}`, `PickupTaken{tick,id,player}`,
//! plus the v1 `WallSpawn`, `WallKill`, `ObstacleSpawn`, `ObstacleDespawn`. High-rate hits are NOT here.
//!
//! # KICK
//! `[0x4B] | reason u8` (see `KickReason`), sent several times on departure and to departed addresses.

use super::bits::{DecodeError, DecodeResult};

pub const MAGIC: [u8; 4] = *b"GSHN";
/// 3: cookie handshake, snapshot datagram with own/board sections, gameplay events. See the module doc.
pub const VERSION: u8 = 18;
pub const TAG_WELCOME: u8 = 0x57;
pub const TAG_SNAPSHOT: u8 = 0x53;
pub const TAG_KICK: u8 = 0x4B;
pub const TAG_CHALLENGE: u8 = 0x43;
pub const HELLO_LEN: usize = 13;
pub const CHALLENGE_LEN: usize = 9;
/// Client retransmit policy: send HELLO (with the newest cookie) every this often until WELCOME...
pub const HELLO_RETRY_MS: u64 = 100;
/// ...and give up (`connect_failed`) after this long without WELCOME.
pub const CONNECT_TIMEOUT_MS: u64 = 5000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hello {
    pub version: u8,
    /// 0 on the first send; otherwise the cookie from the server's CHALLENGE.
    pub cookie: u64,
}

pub fn encode_hello(cookie: u64) -> [u8; HELLO_LEN] {
    let mut out = [0u8; HELLO_LEN];
    out[..4].copy_from_slice(&MAGIC);
    out[4] = VERSION;
    out[5..].copy_from_slice(&cookie.to_le_bytes());
    out
}

/// Structural parse; the caller compares `version` (a mismatch deserves `KICK(BadVersion)`, not silence).
pub fn decode_hello(buf: &[u8]) -> DecodeResult<Hello> {
    if buf.len() != HELLO_LEN || buf[..4] != MAGIC {
        return Err(DecodeError::Malformed);
    }
    Ok(Hello { version: buf[4], cookie: u64::from_le_bytes(buf[5..].try_into().unwrap()) })
}

pub fn is_hello(buf: &[u8]) -> bool {
    decode_hello(buf).is_ok()
}

pub fn encode_challenge(cookie: u64) -> [u8; CHALLENGE_LEN] {
    let mut out = [0u8; CHALLENGE_LEN];
    out[0] = TAG_CHALLENGE;
    out[1..].copy_from_slice(&cookie.to_le_bytes());
    out
}

pub fn decode_challenge(buf: &[u8]) -> DecodeResult<u64> {
    match buf {
        [TAG_CHALLENGE, rest @ ..] if rest.len() == 8 => Ok(u64::from_le_bytes(rest.try_into().unwrap())),
        _ => Err(DecodeError::Malformed),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Welcome {
    pub player_id: u16,
    /// Server tick at send time: seeds the client's clock estimate.
    pub server_tick: u32,
}

impl Welcome {
    pub fn encode(&self) -> [u8; 7] {
        let t = self.server_tick.to_le_bytes();
        let i = self.player_id.to_le_bytes();
        [TAG_WELCOME, i[0], i[1], t[0], t[1], t[2], t[3]]
    }

    pub fn decode(buf: &[u8]) -> DecodeResult<Self> {
        match buf {
            [TAG_WELCOME, i0, i1, t0, t1, t2, t3] => Ok(Self {
                player_id: u16::from_le_bytes([*i0, *i1]),
                server_tick: u32::from_le_bytes([*t0, *t1, *t2, *t3]),
            }),
            _ => Err(DecodeError::Malformed),
        }
    }
}

/// Why the server dropped you. Wire: `[TAG_KICK][reason u8]`. The server sends it a few times (UDP) to the
/// departing address, and again to later datagrams from that address while it is banned / recently gone.
/// Unknown reason bytes decode as `Other` so older clients still show *something*.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum KickReason {
    Other = 0,
    /// Anti-cheat strikes (impossible movement, aim slew, future-tick spam).
    Cheat = 1,
    /// Packet rate over the per-peer cap.
    Flood = 2,
    /// No packets for the idle timeout.
    Timeout = 3,
    /// HELLO refused: no free slot.
    ServerFull = 4,
    /// HELLO carried a different protocol VERSION: update the client.
    BadVersion = 5,
}

impl KickReason {
    pub fn from_byte(b: u8) -> Self {
        match b {
            1 => Self::Cheat,
            2 => Self::Flood,
            3 => Self::Timeout,
            4 => Self::ServerFull,
            5 => Self::BadVersion,
            _ => Self::Other,
        }
    }

    /// Short text for the HUD ("kicked: {reason}").
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Other => "disconnected",
            Self::Cheat => "cheat detection",
            Self::Flood => "packet flood",
            Self::Timeout => "timed out",
            Self::ServerFull => "server full",
            Self::BadVersion => "client out of date",
        }
    }
}

pub fn encode_kick(reason: KickReason) -> [u8; 2] {
    [TAG_KICK, reason as u8]
}

pub fn decode_kick(buf: &[u8]) -> DecodeResult<KickReason> {
    match buf {
        [TAG_KICK, r] => Ok(KickReason::from_byte(*r)),
        _ => Err(DecodeError::Malformed),
    }
}

/// Snapshot datagram: `[TAG_SNAPSHOT][events_len u16 LE][own_len u16 LE][board_len u8][events][own][board][snapshot]`.
/// `own` and `board` are opaque here (see `gameplay.rs` and the sim's `OwnState`); either may be empty.
pub fn frame_snapshot_v3(events: &[u8], own: &[u8], board: &[u8], snapshot: &[u8]) -> Vec<u8> {
    assert!(events.len() <= u16::MAX as usize && own.len() <= u16::MAX as usize && board.len() <= u8::MAX as usize);
    let mut out = Vec::with_capacity(6 + events.len() + own.len() + board.len() + snapshot.len());
    out.push(TAG_SNAPSHOT);
    out.extend_from_slice(&(events.len() as u16).to_le_bytes());
    out.extend_from_slice(&(own.len() as u16).to_le_bytes());
    out.push(board.len() as u8);
    out.extend_from_slice(events);
    out.extend_from_slice(own);
    out.extend_from_slice(board);
    out.extend_from_slice(snapshot);
    out
}

/// Returns `(events, own, board, snapshot)`.
pub fn split_snapshot_v3(buf: &[u8]) -> DecodeResult<(&[u8], &[u8], &[u8], &[u8])> {
    match buf {
        [TAG_SNAPSHOT, e0, e1, o0, o1, bl, rest @ ..] => {
            let (n, m, k) = (u16::from_le_bytes([*e0, *e1]) as usize, u16::from_le_bytes([*o0, *o1]) as usize, *bl as usize);
            if rest.len() < n + m + k {
                return Err(DecodeError::Truncated);
            }
            let (events, rest) = rest.split_at(n);
            let (own, rest) = rest.split_at(m);
            let (board, snapshot) = rest.split_at(k);
            Ok((events, own, board, snapshot))
        }
        _ => Err(DecodeError::Malformed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_codecs() {
        let h = encode_hello(0xDEAD_BEEF_0123_4567);
        assert_eq!(decode_hello(&h), Ok(Hello { version: VERSION, cookie: 0xDEAD_BEEF_0123_4567 }));
        assert!(is_hello(&encode_hello(0)));
        assert!(!is_hello(b"GSHN\x03"), "old 5-byte HELLO is not a v3 HELLO");
        let mut other = encode_hello(1);
        other[4] = VERSION + 1;
        assert_eq!(decode_hello(&other).unwrap().version, VERSION + 1, "version mismatch is parseable, not silent");
        let c = encode_challenge(42);
        assert_eq!(decode_challenge(&c), Ok(42));
        assert!(decode_challenge(&c[..8]).is_err());
        // anti-amplification: a CHALLENGE is never larger than the HELLO that provoked it
        assert!(CHALLENGE_LEN <= HELLO_LEN);
    }

    #[test]
    fn welcome_kick_and_snapshot_frame() {
        let w = Welcome { player_id: 513, server_tick: 123_456 };
        assert_eq!(Welcome::decode(&w.encode()), Ok(w));
        for r in [KickReason::Cheat, KickReason::Flood, KickReason::Timeout, KickReason::ServerFull, KickReason::BadVersion] {
            assert_eq!(decode_kick(&encode_kick(r)), Ok(r));
        }
        assert_eq!(decode_kick(&[TAG_KICK, 200]), Ok(KickReason::Other));
        assert!(decode_kick(&[TAG_KICK]).is_err());

        let f = frame_snapshot_v3(&[1, 2, 3], &[7, 7, 7, 7], &[5], &[9, 9]);
        assert_eq!(split_snapshot_v3(&f), Ok((&[1u8, 2, 3][..], &[7u8, 7, 7, 7][..], &[5u8][..], &[9u8, 9][..])));
        let big_own = vec![1u8; 300]; // own_len is u16: no silent truncation
        let f = frame_snapshot_v3(&[], &big_own, &[], &[4]);
        assert_eq!(split_snapshot_v3(&f).unwrap().1.len(), 300);
        assert!(split_snapshot_v3(&f[..10]).is_err());
        assert!(split_snapshot_v3(&[TAG_WELCOME, 0, 0]).is_err());
    }
}
