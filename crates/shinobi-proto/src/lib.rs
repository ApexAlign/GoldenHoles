//! Server-authoritative netcode: std-only (no serde), bit-packed, 60 Hz sim.
//!
//! client -> server: `intent::ClientPacket` (alias `PlayerIntentPacket`; redundant inputs + ack + view time)
//! server -> client: `snapshot` deltas (30 Hz default) + reliable `events` stream (Tron walls, neon obstacles)
//! (lag comp, walls, obstacles, events live in `shinobi-sim`; interp/reconcile in `shinobi-client`)

pub mod bits;
pub mod character;
pub mod colour;
pub mod datagram;
pub mod gameplay;
pub mod intent;
pub mod json;
pub mod quant;
pub mod snapshot;

pub const TICK_HZ: u32 = 60;
pub const TICK_DT: f32 = 1.0 / TICK_HZ as f32;
/// Snapshot every N ticks (30 Hz): halves cellular bandwidth; interpolation hides it.
pub const SNAPSHOT_DIVISOR: u32 = 2;

pub fn secs_to_ticks(s: f32) -> u32 {
    (s * TICK_HZ as f32).round() as u32
}
