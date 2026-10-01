//! Server-authoritative netcode: std-only (no serde), bit-packed, 60 Hz sim.
//!
//! client -> server: `intent::ClientPacket` (redundant inputs + ack + view time)
//! server -> client: `snapshot` deltas (30 Hz default) + reliable `events` stream (Tron walls, neon obstacles)
//! lag comp:         `lagcomp` (pose history, rewind, swept hit)
//! client:           `interp` (jitter-adaptive buffer), `reconcile` (predict + replay + visual smoothing)

pub mod bits;
pub mod events;
pub mod intent;
pub mod interp;
pub mod lagcomp;
pub mod obstacle;
pub mod quant;
pub mod reconcile;
pub mod snapshot;
pub mod walls;

pub const TICK_HZ: u32 = 60;
pub const TICK_DT: f32 = 1.0 / TICK_HZ as f32;
/// Snapshot every N ticks (30 Hz): halves cellular bandwidth; interpolation hides it.
pub const SNAPSHOT_DIVISOR: u32 = 2;

pub fn secs_to_ticks(s: f32) -> u32 {
    (s * TICK_HZ as f32).round() as u32
}
