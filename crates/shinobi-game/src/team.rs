//! Team colours for the 8v8 mode: Cyan vs Orange (magenta stays hazards / pickups). Single source of truth so ally vs
//! enemy reads the same on bodies, discs, trails, sparks and flashes.
//!
//! The team id per player will come from `RenderView` once Network puts it on the wire; until then `LocalTeam` is a
//! resource (default Cyan) and `remote_team` is a stub (parity of the entity id). Swap `remote_team` for the real
//! lookup and everything downstream follows.

use bevy::prelude::*;

use shinobi_client::hud::{Palette, Rgba};

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

/// Our own team (stub: Cyan until the server tells us).
#[derive(Resource, Default, Clone, Copy)]
pub struct LocalTeam(pub Team);

/// Team of another player. STUB: alternates by id until the wire carries it.
pub fn remote_team(id: u16) -> Team {
    if id % 2 == 0 {
        Team::Cyan
    } else {
        Team::Orange
    }
}

pub struct TeamPlugin;

impl Plugin for TeamPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LocalTeam>();
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
        assert_ne!(remote_team(2), remote_team(3));
    }

    #[test]
    fn hdr_scales_the_palette_colour() {
        let p = Palette::TRON;
        let c = Team::Orange.hdr(&p, 2.0);
        assert!((c[0] - p.orange.0 * 2.0).abs() < 1e-6);
    }
}
