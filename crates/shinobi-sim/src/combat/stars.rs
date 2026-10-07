//! Star variants (docs/stars.md): the throwable that right-click sends is chosen from four kinds, all drawing on the
//! same energy pool. Pure data and helpers here; the flight / landing lives in `shuriken.rs`, the world effects
//! (mine burst, flash white-out, smoke lock-break) in `Server::star_pass` (`tick.rs`).

/// Which star a throw is. Wire value = discriminant (2 bits in the intent and in a snapshot disc's flags).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum StarKind {
    /// The classic neon shuriken: 1 energy, damage, ricochets.
    #[default]
    Standard = 0,
    /// Sticks where it lands, arms, bursts when an enemy walks close.
    Mine = 1,
    /// Bursts on impact or after a short fuse: whites out enemies who face it.
    Flash = 2,
    /// Lands and releases a cloud that breaks lock-on onto anyone inside.
    Smoke = 3,
}

impl StarKind {
    pub const ALL: [StarKind; 4] = [StarKind::Standard, StarKind::Mine, StarKind::Flash, StarKind::Smoke];

    pub fn from_u8(v: u8) -> Self {
        match v & 3 {
            1 => Self::Mine,
            2 => Self::Flash,
            3 => Self::Smoke,
            _ => Self::Standard,
        }
    }

    /// Next kind in the cycle (mouse wheel up / Q).
    pub fn next(self) -> Self {
        Self::from_u8(self as u8 + 1)
    }

    pub fn prev(self) -> Self {
        Self::from_u8(self as u8 + 3)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Standard => "STAR",
            Self::Mine => "MINE",
            Self::Flash => "FLASH",
            Self::Smoke => "SMOKE",
        }
    }

    /// Mines and smoke stick where they land; the other kinds are gone when they land.
    pub fn sticks(self) -> bool {
        matches!(self, Self::Mine | Self::Smoke)
    }
}

/// `combat.star.*` (Balance). Standard star numbers stay in `combat.shuriken.*`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StarCfg {
    /// Mine: energy cost, seconds after landing until it is armed, trigger distance (an enemy this close sets it off),
    /// burst radius, burst damage, knockback speed (m/s), mines alive per player (a new one replaces the oldest), life (s),
    /// and how close a shot star / katana cut must pass to disarm it.
    pub mine_cost: f32,
    pub mine_arm_s: f32,
    pub mine_trigger_radius: f32,
    pub mine_burst_radius: f32,
    pub mine_damage: f32,
    pub mine_knock: f32,
    pub mine_max: u32,
    pub mine_life_s: f32,
    pub mine_disarm_radius: f32,
    /// Flash: energy cost, fuse in the air (s), reach (m), white-out length at full strength (s), half-angle (degrees) of
    /// the cone around an enemy's view in which the burst blinds them.
    pub flash_cost: f32,
    pub flash_fuse_s: f32,
    pub flash_radius: f32,
    pub flash_secs: f32,
    pub flash_cone_deg: f32,
    /// Smoke: energy cost, how long the cloud lasts (s), cloud radius (m).
    pub smoke_cost: f32,
    pub smoke_secs: f32,
    pub smoke_radius: f32,
}

impl Default for StarCfg {
    fn default() -> Self {
        Self {
            mine_cost: 3.0,
            mine_arm_s: 0.6,
            mine_trigger_radius: 3.0,
            mine_burst_radius: 4.0,
            mine_damage: 25.0,
            mine_knock: 9.0,
            mine_max: 2,
            mine_life_s: 20.0,
            mine_disarm_radius: 0.9,
            flash_cost: 2.0,
            flash_fuse_s: 0.5,
            flash_radius: 12.0,
            flash_secs: 1.2,
            flash_cone_deg: 70.0,
            smoke_cost: 2.0,
            smoke_secs: 5.0,
            smoke_radius: 4.0,
        }
    }
}

impl StarCfg {
    /// Energy charges one throw of `kind` costs (`standard` = `combat.shuriken.energy_cost`).
    pub fn cost(&self, kind: StarKind, standard: f32) -> f32 {
        match kind {
            StarKind::Standard => standard,
            StarKind::Mine => self.mine_cost,
            StarKind::Flash => self.flash_cost,
            StarKind::Smoke => self.smoke_cost,
        }
    }

    /// Seconds a landed object of this kind stays (mines, smoke); 0 for the others.
    pub fn life(&self, kind: StarKind) -> f32 {
        match kind {
            StarKind::Mine => self.mine_life_s,
            StarKind::Smoke => self.smoke_secs,
            _ => 0.0,
        }
    }
}

/// How hard a flash burst at `burst` whites out a viewer at `eye` looking along unit `fwd`, 0 = not at all.
/// Facing scales it (looking straight at the burst = full, at the edge of the cone = a fifth), distance scales it
/// (point blank = full, at `flash_radius` = a fifth). Pure, so the server and tests share it.
pub fn flash_strength(cfg: &StarCfg, eye: [f32; 3], fwd: [f32; 3], burst: [f32; 3]) -> f32 {
    let d = [burst[0] - eye[0], burst[1] - eye[1], burst[2] - eye[2]];
    let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if dist > cfg.flash_radius {
        return 0.0;
    }
    if dist < 1e-3 {
        return 1.0;
    }
    let facing = (d[0] * fwd[0] + d[1] * fwd[1] + d[2] * fwd[2]) / dist;
    let cos_edge = cfg.flash_cone_deg.to_radians().cos();
    if facing < cos_edge {
        return 0.0;
    }
    let f = ((facing - cos_edge) / (1.0 - cos_edge).max(1e-3)).clamp(0.0, 1.0);
    let near = (1.0 - dist / cfg.flash_radius.max(1e-3)).clamp(0.0, 1.0);
    (0.2 + 0.8 * f) * (0.2 + 0.8 * near)
}

/// A player's view direction from the quantised yaw/pitch the server keeps (yaw 0 faces -Z).
pub fn view_dir(yaw: f32, pitch: f32) -> [f32; 3] {
    let (sy, cy) = (libm::sinf(yaw), libm::cosf(yaw));
    let (sp, cp) = (libm::sinf(pitch), libm::cosf(pitch));
    [-sy * cp, sp, -cy * cp]
}

/// Is `p` inside a sphere of `radius` around `c`?
pub fn within(p: [f32; 3], c: [f32; 3], radius: f32) -> bool {
    let d = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
    d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= radius * radius
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_cycle_and_round_trip() {
        let mut k = StarKind::Standard;
        for want in [StarKind::Mine, StarKind::Flash, StarKind::Smoke, StarKind::Standard] {
            k = k.next();
            assert_eq!(k, want);
        }
        for k in StarKind::ALL {
            assert_eq!(StarKind::from_u8(k as u8), k);
            assert_eq!(k.next().prev(), k);
        }
    }

    #[test]
    fn flash_strength_scales_with_facing_and_distance() {
        let c = StarCfg::default();
        let fwd = [0.0, 0.0, -1.0];
        let eye = [0.0, 0.0, 0.0];
        let ahead_near = flash_strength(&c, eye, fwd, [0.0, 0.0, -3.0]);
        let ahead_far = flash_strength(&c, eye, fwd, [0.0, 0.0, -11.0]);
        let edge = flash_strength(&c, eye, fwd, [-8.0, 0.0, -3.0]); // ~69 deg off axis
        assert!(ahead_near > 0.7 && ahead_near > ahead_far && ahead_far > 0.0, "{ahead_near} {ahead_far}");
        assert!(edge > 0.0 && edge < ahead_near * 0.5, "{edge}");
        assert_eq!(flash_strength(&c, eye, fwd, [0.0, 0.0, 3.0]), 0.0, "behind: not blinded");
        assert_eq!(flash_strength(&c, eye, fwd, [0.0, 0.0, -13.0]), 0.0, "out of range");
    }
}
