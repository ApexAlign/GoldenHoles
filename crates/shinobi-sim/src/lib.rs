//! Authoritative simulation. The only crate that steps Rapier.
//! Colliders here are primitives (capsules, cuboids); high-poly visual meshes never reach this crate.

pub use rapier3d;

pub mod anticheat;
pub mod balance;
pub mod block;
pub mod blocks;
pub mod breaks;
pub mod character;
pub mod cloak;
pub mod combat;
pub mod enemies;
pub mod env_events;
pub mod events;
pub mod features;
pub mod hazards;
pub mod hash;
pub mod lagcomp;
pub mod katana_moves;
pub mod melee;
pub mod match_state;
pub mod level;
pub mod movement;
pub mod obstacle;
pub mod pvp;
pub mod separation;
pub mod shield;
pub mod spawn;
pub mod star_pass;
pub mod team;
pub mod own_state;
pub mod pickups;
pub mod power;
pub mod power_pass;
pub mod tick;
pub mod walls;
