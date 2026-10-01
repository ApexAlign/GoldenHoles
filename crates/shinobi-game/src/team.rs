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
    pub fn other(self) -> Team {
        match self {
            Team::Cyan => Team::Orange,
            Team::Orange => Team::Cyan,
        }
    }

    /// Base colour from the shared palette.
    pub fn color(self, p: &Palette) -> Rgba {
        match self {
            Team::Cyan => p.cyan,
            Team::Orange => p.orange,
        }
    }

    /// HDR rgb (for bloom): palette colour times `gain`.
    pub fn hdr(self, p: &Palette, gain: f32) -> [f32; 3] {
        let c = self.color(p);
        [c.0 * gain, c.1 * gain, c.2 * gain]
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

/// Our own team (Cyan until the server tells us / outside team modes). Kept fresh by `sync_local`.
#[derive(Resource, Default, Clone, Copy)]
pub struct LocalTeam(pub Team);

/// Team of another player; `None` outside team modes (callers fall back to the neutral hostile colour).
pub fn remote_team(net: &Net, id: u16) -> Option<Team> {
    net.0.game.net.team_of(id).and_then(from_wire)
}

fn sync_local(net: Res<Net>, mut local: ResMut<LocalTeam>) {
    if let Some(t) = net.0.game.net.my_team().and_then(from_wire) {
        local.0 = t;
    }
}

pub struct TeamPlugin;

impl Plugin for TeamPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LocalTeam>().add_systems(Update, sync_local);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teams_are_distinct_and_symmetric() {
        let p = Palette::TRON;
        assert_ne!(Team::Cyan.color(&p), Team::Orange.color(&p));
        assert_eq!(Team::Cyan.other().other(), Team::Cyan);
        assert_eq!(from_wire(0), Some(Team::Cyan));
        assert_eq!(from_wire(1), Some(Team::Orange));
        assert_eq!(from_wire(9), None);
    }

    #[test]
    fn hdr_scales_the_palette_colour() {
        let p = Palette::TRON;
        let c = Team::Orange.hdr(&p, 2.0);
        assert!((c[0] - p.orange.0 * 2.0).abs() < 1e-6);
    }
}
