//! Server-side PvP bots for the authoritative sim. See `fsm.rs` for the brain, `tickets.rs` for the off-screen attack limiter and `host.rs` for the 60 Hz driver.
//!
//! ```ignore
//! let mut host = BotHost::new(BotHostCfg::default());
//! host.add_bot(&mut server, 201, Archetype::Heavy, BotSkill::Normal);
//! loop { host.advance(&mut server, elapsed); }   // instead of server.frame(elapsed)
//! ```

pub mod fsm;
pub mod host;
pub mod nav;
pub mod power_bot;
pub mod rng;
pub mod skill;
pub mod tickets;
pub mod view;

pub use host::{BotHost, BotHostCfg};
pub use skill::{Archetype, BotSkill};
