//! Team colours for the 8v8 mode: Cyan vs Orange (magenta stays hazards / pickups). Single source of truth so ally vs
//! enemy reads the same on bodies, discs, trails, sparks and flashes.
//!
//! Team ids come from `NetClient::team_of` / `my_team` (Network). Outside team modes there are no teams: remotes keep
//! the neutral magenta hostile colour and the local team defaults to Cyan.

use bevy::prelude::*;

use shinobi_client::hud::{Palette, Rgba};

use crate::net::Net;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Team {
    #[default]
    Cyan,
    Orange,
}

impl Team {
    /// Base colour from the shared palette.
    pub fn color(self, p: &Palette) -> Rgba {
        match self {
            Team::Cyan => p.cyan,
            Team::Orange => p.orange,
        }
    }
}

/// Wire team id (`shinobi_sim::team::TEAM_*`) -> `Team`.
pub fn from_wire(t: u8) -> Option<Team> {
    match t {
        shinobi_sim::team::TEAM_CYAN => Some(Team::Cyan),
        shinobi_sim::team::TEAM_ORANGE => Some(Team::Orange),
        _ => None,
    }
}

/// Distinct neon colour per player id (free-for-all). Same table the HUD uses (`colour::player_rgba`), so a player's
/// body, trail, rope and leaderboard row always match.
pub fn neon(id: u16) -> [f32; 3] {
    let c = shinobi_client::colour::player_rgba(id);
    [c.0, c.1, c.2]
}

/// A player's colour (unit scale, multiply by a gain for HDR): the team colour in team modes, else their own neon.
pub fn colour_of(net: &Net, id: u16) -> [f32; 3] {
    match remote_team(net, id) {
        Some(t) => {
            let c = t.color(&Palette::TRON);
            [c.0, c.1, c.2]
        }
        None => neon(id),
    }
}

/// Our own colour (cyan until the server assigns an id).
#[derive(Resource, Clone, Copy)]
pub struct LocalColour(pub [f32; 3]);

impl Default for LocalColour {
    fn default() -> Self {
        let c = Palette::TRON.cyan;
        Self([c.0, c.1, c.2])
    }
}

/// Quantised key so colours can index a material cache.
pub fn key(c: [f32; 3]) -> u32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    q(c[0]) << 16 | q(c[1]) << 8 | q(c[2])
}

/// Team of another player; `None` outside team modes.
pub fn remote_team(net: &Net, id: u16) -> Option<Team> {
    net.0.game.net.team_of(id).and_then(from_wire)
}

fn sync_local(net: Res<Net>, mut local: ResMut<LocalColour>) {
    if let Some(id) = net.0.own_id() {
        local.0 = colour_of(&net, id);
    }
}

pub struct TeamPlugin;

impl Plugin for TeamPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LocalColour>().add_systems(Update, sync_local);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teams_are_distinct_and_symmetric() {
        let p = Palette::TRON;
        assert_ne!(Team::Cyan.color(&p), Team::Orange.color(&p));
        assert_eq!(from_wire(0), Some(Team::Cyan));
        assert_eq!(from_wire(1), Some(Team::Orange));
        assert_eq!(from_wire(9), None);
    }

    #[test]
    fn neon_colours_are_bright_and_distinct() {
        for id in 0..32u16 {
            let c = neon(id);
            assert!(c.iter().cloned().fold(0.0, f32::max) > 0.6, "bright: {c:?}");
        }
        let keys: std::collections::HashSet<u32> = (0..16u16).map(|i| key(neon(i))).collect();
        assert_eq!(keys.len(), 16, "16 players get 16 different colours");
    }

}
