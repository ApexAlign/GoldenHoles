//! Star variants (docs/stars.md): the throwable a star throw sends is the standard star or the flash star, both drawing
//! on the same energy pool. Stars are a close-range tool (`combat.shuriken.range`). Pure data and helpers here; the
//! flight lives in `shuriken.rs`, the flash white-out in `Server::star_pass` (`star_pass.rs`).

/// Which star a throw is. Wire value = discriminant (2 bits in the intent and in a snapshot disc's flags; 1 and 3 were
/// the barrel and the canister before v23 and now read as `Standard`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum StarKind {
    /// The classic neon shuriken: 1 energy, damage, ricochets.
    #[default]
    Standard = 0,
    /// Bursts on impact or after a short fuse: whites out and dazes anyone who faces it.
    Flash = 2,
}

impl StarKind {
    pub const ALL: [StarKind; 2] = [StarKind::Standard, StarKind::Flash];

    pub fn from_u8(v: u8) -> Self {
        match v & 3 {
            2 => Self::Flash,
            _ => Self::Standard,
        }
    }

    /// The other kind (mouse wheel / Q).
    pub fn next(self) -> Self {
        match self {
            Self::Standard => Self::Flash,
            Self::Flash => Self::Standard,
        }
    }

    pub fn prev(self) -> Self {
        self.next()
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Standard => "STAR",
            Self::Flash => "FLASH",
        }
    }
}

/// `combat.star.*` (Balance). Standard star numbers stay in `combat.shuriken.*`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StarCfg {
    /// Flash: energy cost, fuse in the air (s), reach (m), white-out length at full strength (s), half-angle (degrees) of
    /// the cone around a victim's view in which the burst blinds them (no line of sight needed), the radius within which
    /// the thrower is blinded too (only when facing it), the extra damage a dazed player takes (0.25 = +25 %) and the
    /// fraction of its normal turn rate a dazed player's aim keeps.
    pub flash_cost: f32,
    pub flash_fuse_s: f32,
    pub flash_radius: f32,
    pub flash_secs: f32,
    pub flash_cone_deg: f32,
    pub flash_self_radius: f32,
    pub flash_daze_damage: f32,
    pub flash_daze_turn: f32,
}

impl Default for StarCfg {
    fn default() -> Self {
        Self {
            flash_cost: 2.0,
            flash_fuse_s: 0.5,
            flash_radius: 14.0,
            flash_secs: 3.5,
            flash_cone_deg: 100.0,
            flash_self_radius: 6.0,
            flash_daze_damage: 0.25,
            flash_daze_turn: 0.45,
        }
    }
}

impl StarCfg {
    /// Energy charges one throw of `kind` costs (`standard` = `combat.shuriken.energy_cost`).
    pub fn cost(&self, kind: StarKind, standard: f32) -> f32 {
        match kind {
            StarKind::Standard => standard,
            StarKind::Flash => self.flash_cost,
        }
    }
}

/// How hard a flash burst at `burst` whites out a viewer at `eye` looking along unit `fwd`, 0 = not at all.
/// Facing scales it (looking straight at the burst = full, at the edge of the cone = a fifth), distance scales it
/// (point blank = full, at `flash_radius` = a fifth). No line of sight is needed: the glare floods round corners.
/// Pure, so the server and tests share it.
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
        assert_eq!(StarKind::Standard.next(), StarKind::Flash);
        assert_eq!(StarKind::Flash.next(), StarKind::Standard);
        for k in StarKind::ALL {
            assert_eq!(StarKind::from_u8(k as u8), k);
            assert_eq!(k.next().prev(), k);
        }
        // the retired barrel (1) and canister (3) wire values read as a plain star
        assert_eq!(StarKind::from_u8(1), StarKind::Standard);
        assert_eq!(StarKind::from_u8(3), StarKind::Standard);
    }

    #[test]
    fn flash_strength_scales_with_facing_and_distance() {
        let c = StarCfg::default();
        let fwd = [0.0, 0.0, -1.0];
        let eye = [0.0, 0.0, 0.0];
        let ahead_near = flash_strength(&c, eye, fwd, [0.0, 0.0, -3.0]);
        let ahead_far = flash_strength(&c, eye, fwd, [0.0, 0.0, -12.0]);
        let side = flash_strength(&c, eye, fwd, [-8.0, 0.0, -1.0]); // ~83 deg off axis: inside the 100 degree cone
        assert!(ahead_near > 0.7 && ahead_near > ahead_far && ahead_far > 0.0, "{ahead_near} {ahead_far}");
        assert!(side > 0.0 && side < ahead_near * 0.5, "the glare reaches past the sides: {side}");
        assert_eq!(flash_strength(&c, eye, fwd, [0.0, 0.0, 3.0]), 0.0, "behind: not blinded");
        assert_eq!(flash_strength(&c, eye, fwd, [0.0, 0.0, -15.0]), 0.0, "out of range");
    }
}
