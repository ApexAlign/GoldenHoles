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

impl Tier {
    /// The next cheaper tier, if any.
    pub fn lower(self) -> Option<Tier> {
        match self {
            Tier::High => Some(Tier::Medium),
            Tier::Medium => Some(Tier::Low),
            Tier::Low => None,
        }
    }

    /// Starting tier for a GPU class: integrated GPUs start at Medium (they share memory bandwidth with the CPU and
    /// cannot hold 60 fps at 1080p with full bloom + CRT), software rasterisers at Low.
    pub fn for_device(kind: DeviceKind) -> Tier {
        match kind {
            DeviceKind::Discrete => Tier::High,
            DeviceKind::Integrated | DeviceKind::Other => Tier::Medium,
            DeviceKind::Software => Tier::Low,
        }
    }
}

/// GPU class, from the adapter's device type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Discrete,
    Integrated,
    Software,
    Other,
}

impl DeviceKind {
    /// Parses wgpu's `DeviceType` debug name (`DiscreteGpu`, `IntegratedGpu`, `Cpu`, `VirtualGpu`, `Other`).
    pub fn from_wgpu_name(name: &str) -> DeviceKind {
        match name {
            "DiscreteGpu" => DeviceKind::Discrete,
            "IntegratedGpu" => DeviceKind::Integrated,
            "Cpu" => DeviceKind::Software,
            _ => DeviceKind::Other,
        }
    }
}

/// Watches frame times and says when to drop a tier: p95 frame time above `budget_ms` over a `window_s` window.
/// After a drop it waits `cooldown_s` (the new tier needs time to settle, and the window restarts empty).
#[derive(Clone, Debug)]
pub struct FrameGovernor {
    pub budget_ms: f32,
    pub window_s: f32,
    pub cooldown_s: f32,
    samples: Vec<f32>,
    span: f32,
    cooldown: f32,
}

impl FrameGovernor {
    pub fn new() -> Self {
        Self { budget_ms: 16.6, window_s: 2.0, cooldown_s: 4.0, samples: Vec::new(), span: 0.0, cooldown: 0.0 }
    }

    /// Ignore frames for `secs` (shader compilation and window setup hitch the first seconds).
    pub fn warmup(&mut self, secs: f32) {
        self.cooldown = secs;
    }

    /// Feed one frame (`dt_s` seconds). Returns the window's p95 (ms) when it is over budget at the end of a window.
    pub fn push(&mut self, dt_s: f32) -> Option<f32> {
        if self.cooldown > 0.0 {
            self.cooldown -= dt_s;
            return None;
        }
        self.samples.push(dt_s * 1000.0);
        self.span += dt_s;
        if self.span < self.window_s {
            return None;
        }
        let mut v = std::mem::take(&mut self.samples);
        self.span = 0.0;
        v.sort_by(|a, b| a.total_cmp(b));
        let p95 = v[((v.len() as f32 * 0.95) as usize).min(v.len() - 1)];
        if p95 > self.budget_ms * 1.05 {
            self.cooldown = self.cooldown_s;
            Some(p95)
        } else {
            None
        }
    }
}

impl Default for FrameGovernor {
    fn default() -> Self {
        Self::new()
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
    fn device_class_picks_the_starting_tier() {
        assert_eq!(Tier::for_device(DeviceKind::from_wgpu_name("DiscreteGpu")), Tier::High);
        assert_eq!(Tier::for_device(DeviceKind::from_wgpu_name("IntegratedGpu")), Tier::Medium);
        assert_eq!(Tier::for_device(DeviceKind::from_wgpu_name("Cpu")), Tier::Low);
        assert_eq!(Tier::for_device(DeviceKind::from_wgpu_name("???")), Tier::Medium);
    }

    #[test]
    fn tiers_step_down_to_low_and_stop() {
        assert_eq!(Tier::High.lower(), Some(Tier::Medium));
        assert_eq!(Tier::Medium.lower(), Some(Tier::Low));
        assert_eq!(Tier::Low.lower(), None);
    }

    fn run(gov: &mut FrameGovernor, ms: f32, secs: f32) -> Vec<f32> {
        let n = (secs * 1000.0 / ms) as usize;
        (0..n).filter_map(|_| gov.push(ms / 1000.0)).collect()
    }

    #[test]
    fn governor_ignores_a_steady_60_and_flags_a_slow_window() {
        let mut g = FrameGovernor::new();
        assert!(run(&mut g, 16.6, 6.0).is_empty(), "locked 60 is fine");
        let v = run(&mut g, 25.0, 2.5);
        assert_eq!(v.len(), 1);
        assert!(v[0] > 20.0);
    }

    #[test]
    fn governor_one_slow_frame_in_a_window_is_not_enough_but_many_are() {
        let mut g = FrameGovernor::new();
        let mut hits = 0;
        for i in 0..130 {
            // one 100 ms hitch among 16.6 ms frames: p95 stays in budget
            hits += g.push(if i == 40 { 0.1 } else { 0.0166 }).is_some() as u32;
        }
        assert_eq!(hits, 0);
        let mut g = FrameGovernor::new();
        let mut hits = 0;
        for i in 0..130 {
            // every 10th frame is 60 ms: >5% of frames slow -> p95 over budget
            hits += g.push(if i % 10 == 0 { 0.06 } else { 0.0166 }).is_some() as u32;
        }
        assert_eq!(hits, 1);
    }

    #[test]
    fn governor_waits_out_a_cooldown_after_a_drop() {
        let mut g = FrameGovernor::new();
        assert_eq!(run(&mut g, 30.0, 2.2).len(), 1);
        assert!(run(&mut g, 30.0, 3.0).is_empty(), "cooldown (4 s) swallows the next frames");
        assert_eq!(run(&mut g, 30.0, 4.5).len(), 1, "then it measures again");
    }

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
