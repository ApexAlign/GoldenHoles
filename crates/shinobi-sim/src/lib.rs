//! Authoritative simulation. The only crate that steps Rapier.
//! Colliders here are primitives (capsules, cuboids); high-poly visual meshes never reach this crate.

pub use rapier3d;

pub mod anticheat;
pub mod balance;
pub mod block;
pub mod blocks;
pub mod character;
pub mod combat;
pub mod enemies;
pub mod events;
pub mod hazards;
pub mod lagcomp;
pub mod match_state;
pub mod level;
pub mod movement;
pub mod obstacle;
pub mod pvp;
pub mod separation;
pub mod spawn;
pub mod team;
pub mod own_state;
pub mod pickups;
pub mod tick;
pub mod walls;
