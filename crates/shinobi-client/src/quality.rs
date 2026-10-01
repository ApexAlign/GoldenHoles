//! Quality tiers: which expensive effects run and how aggressively the scene resolution may drop.
//! Renderer-agnostic policy; a backend reads it once at startup (and again if the player changes tier).
//!
//! | tier   | bloom            | CRT lens/scanlines | dynamic resolution | light-wall points |
//! |--------|------------------|--------------------|--------------------|-------------------|
//! | High   | desktop preset   | on                 | off (native)       | 256               |
//! | Medium | mobile preset    | on                 | 0.70 .. 1.0        | 128               |
//! | Low    | off              | off                | 0.55 .. 1.0        | 64                |

use crate::fx::DynResCfg;
use crate::shaders::BloomSettings;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tier {
    #[default]
    High,
    Medium,
    Low,
}

impl Tier {
    pub fn parse(s: &str) -> Option<Tier> {
        match s.to_ascii_lowercase().as_str() {
            "high" | "desktop" => Some(Tier::High),
            "medium" | "med" | "mid" => Some(Tier::Medium),
            "low" | "mobile" => Some(Tier::Low),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Quality {
    pub tier: Tier,
    /// `None` = no bloom pass at all.
    pub bloom: Option<BloomSettings>,
    /// Barrel/chromatic/scanline/vignette composite (also carries the katana sweep warp).
    pub crt: bool,
    /// `None` = always render at native resolution.
    pub dyn_res: Option<DynResCfg>,
    /// Cap on recorded points per light-wall.
    pub wall_points: usize,
}

impl Quality {
    pub fn from_tier(tier: Tier) -> Self {
        // Frame time is measured from the display clock, which is vsync-quantised: a locked 60 Hz frame is
        // ~16.7 ms whether or not there is headroom. So "headroom" = "not over budget"; the slow recovery
        // (90 frames) turns that into periodic upward probing, and 3 over-budget frames drop it again.
        let dr = |min: f32| DynResCfg { min, up_ms: 17.4, ..DynResCfg::default() };
        match tier {
            Tier::High => Self { tier, bloom: Some(BloomSettings::DESKTOP), crt: true, dyn_res: None, wall_points: 256 },
            Tier::Medium => Self { tier, bloom: Some(BloomSettings::MOBILE), crt: true, dyn_res: Some(dr(0.70)), wall_points: 128 },
            Tier::Low => Self { tier, bloom: None, crt: false, dyn_res: Some(dr(0.55)), wall_points: 64 },
        }
    }
}

impl Default for Quality {
    fn default() -> Self {
        Self::from_tier(Tier::High)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_get_progressively_cheaper() {
        let (h, m, l) = (Quality::from_tier(Tier::High), Quality::from_tier(Tier::Medium), Quality::from_tier(Tier::Low));
        assert!(h.dyn_res.is_none() && h.crt && h.bloom == Some(BloomSettings::DESKTOP));
        assert_eq!(m.bloom, Some(BloomSettings::MOBILE));
        assert!(m.bloom.unwrap().base_scale < h.bloom.unwrap().base_scale, "mobile bloom runs at lower res");
        assert!(l.bloom.is_none() && !l.crt);
        assert!(h.wall_points > m.wall_points && m.wall_points > l.wall_points);
        assert!(m.dyn_res.unwrap().min > l.dyn_res.unwrap().min);
    }

    #[test]
    fn mobile_dynres_can_recover_under_vsync() {
        // a vsync-locked 16.7 ms frame must count as headroom, otherwise the scale never climbs back
        let c = Quality::from_tier(Tier::Medium).dyn_res.unwrap();
        assert!(16.7 < c.up_ms && c.up_ms < c.down_ms);
    }

    #[test]
    fn parse_names() {
        assert_eq!(Tier::parse("LOW"), Some(Tier::Low));
        assert_eq!(Tier::parse("mobile"), Some(Tier::Low));
        assert_eq!(Tier::parse("med"), Some(Tier::Medium));
        assert_eq!(Tier::parse("ultra"), None);
        assert_eq!(Tier::default(), Tier::High);
    }
}
