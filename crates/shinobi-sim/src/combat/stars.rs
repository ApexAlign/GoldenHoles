//! Star variants (docs/stars.md): the throwable that right-click sends is chosen from four kinds, all drawing on the
//! same energy pool. Pure data and helpers here; the flight / landing lives in `shuriken.rs`, the world effects
//! (explosions, flash white-out) in `Server::star_pass` (`star_pass.rs`).

/// Which star a throw is. Wire value = discriminant (2 bits in the intent and in a snapshot disc's flags).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum StarKind {
    /// The classic neon shuriken: 1 energy, damage, ricochets.
    #[default]
    Standard = 0,
    /// A thrown explosive barrel: heavy lob, bounces, rests, then goes off (or earlier when hit / destroyed).
    Barrel = 1,
    /// Bursts on impact or after a short fuse: whites out and dazes anyone who faces it.
    Flash = 2,
    /// A gas canister: fast, sticks to the surface or player it hits, short fuse, smaller blast.
    Canister = 3,
}

impl StarKind {
    pub const ALL: [StarKind; 4] = [StarKind::Standard, StarKind::Barrel, StarKind::Flash, StarKind::Canister];

    pub fn from_u8(v: u8) -> Self {
        match v & 3 {
            1 => Self::Barrel,
            2 => Self::Flash,
            3 => Self::Canister,
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
            Self::Barrel => "BARREL",
            Self::Flash => "FLASH",
            Self::Canister => "CANISTER",
        }
    }

    /// Barrels and canisters end up as a landed object (`Disc::stuck`) that counts down to its blast.
    pub fn sticks(self) -> bool {
        matches!(self, Self::Barrel | Self::Canister)
    }

    /// Barrels and canisters go off with a blast (`StarCfg::blast`).
    pub fn explosive(self) -> bool {
        matches!(self, Self::Barrel | Self::Canister)
    }
}

/// An explosion's numbers: radius (m), damage at the centre, the fraction of it left at the edge, knockback speed
/// (m/s, radial) and the upward launch (m/s), both scaled by the same falloff.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Blast {
    pub radius: f32,
    pub damage: f32,
    pub edge: f32,
    pub knock: f32,
    pub launch: f32,
}

impl Blast {
    /// Scale at `d` metres: linear from 1 at the centre to `edge` at the radius.
    pub fn falloff(&self, d: f32) -> f32 {
        let t = (d / self.radius.max(1e-3)).clamp(0.0, 1.0);
        1.0 - (1.0 - self.edge) * t
    }
}

/// `combat.star.*` (Balance). Standard star numbers stay in `combat.shuriken.*`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StarCfg {
    /// Barrel: energy cost, throw speed (m/s), gravity (m/s^2), bounce restitution (and `barrel_grip`: sideways speed kept per bounce), fuse after it comes to rest (s), and the
    /// blast: radius, centre damage, fraction left at the edge, knockback speed, launch speed. `barrel_hit_radius`: how
    /// close a star / arrow / katana cut must pass to set a barrel (or canister) off.
    pub barrel_cost: f32,
    pub barrel_speed: f32,
    pub barrel_gravity: f32,
    pub barrel_bounce: f32,
    /// Fraction of its sideways speed a barrel keeps at each bounce (friction).
    pub barrel_grip: f32,
    pub barrel_fuse_s: f32,
    pub barrel_radius: f32,
    pub barrel_damage: f32,
    pub barrel_edge: f32,
    pub barrel_knock: f32,
    pub barrel_launch: f32,
    pub barrel_hit_radius: f32,
    /// Canister: energy cost, throw speed, fuse after it sticks, blast radius / centre damage / edge fraction / knockback /
    /// launch.
    pub canister_cost: f32,
    pub canister_speed: f32,
    pub canister_fuse_s: f32,
    pub canister_radius: f32,
    pub canister_damage: f32,
    pub canister_edge: f32,
    pub canister_knock: f32,
    pub canister_launch: f32,
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
            barrel_cost: 3.0,
            barrel_speed: 22.0,
            barrel_gravity: 22.0,
            barrel_bounce: 0.3,
            barrel_grip: 0.6,
            barrel_fuse_s: 1.5,
            barrel_radius: 6.0,
            barrel_damage: 90.0,
            barrel_edge: 0.15,
            barrel_knock: 16.0,
            barrel_launch: 9.0,
            barrel_hit_radius: 0.9,
            canister_cost: 2.0,
            canister_speed: 55.0,
            canister_fuse_s: 1.2,
            canister_radius: 4.0,
            canister_damage: 60.0,
            canister_edge: 0.15,
            canister_knock: 22.0,
            canister_launch: 6.0,
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
            StarKind::Barrel => self.barrel_cost,
            StarKind::Flash => self.flash_cost,
            StarKind::Canister => self.canister_cost,
        }
    }

    /// Flight speed (m/s) of a throw of `kind` (`standard` = `combat.shuriken.speed`).
    pub fn speed(&self, kind: StarKind, standard: f32) -> f32 {
        match kind {
            StarKind::Barrel => self.barrel_speed,
            StarKind::Canister => self.canister_speed,
            _ => standard,
        }
    }

    /// Seconds from rest / sticking until a barrel or canister goes off; 0 for the other kinds.
    pub fn fuse(&self, kind: StarKind) -> f32 {
        match kind {
            StarKind::Barrel => self.barrel_fuse_s,
            StarKind::Canister => self.canister_fuse_s,
            _ => 0.0,
        }
    }

    /// The blast of an explosive kind.
    pub fn blast(&self, kind: StarKind) -> Option<Blast> {
        match kind {
            StarKind::Barrel => Some(Blast { radius: self.barrel_radius, damage: self.barrel_damage, edge: self.barrel_edge, knock: self.barrel_knock, launch: self.barrel_launch }),
            StarKind::Canister => Some(Blast { radius: self.canister_radius, damage: self.canister_damage, edge: self.canister_edge, knock: self.canister_knock, launch: self.canister_launch }),
            _ => None,
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
        let mut k = StarKind::Standard;
        for want in [StarKind::Barrel, StarKind::Flash, StarKind::Canister, StarKind::Standard] {
            k = k.next();
            assert_eq!(k, want);
        }
        for k in StarKind::ALL {
            assert_eq!(StarKind::from_u8(k as u8), k);
            assert_eq!(k.next().prev(), k);
        }
    }

    #[test]
    fn blast_falls_off_linearly_to_the_edge_fraction() {
        let b = StarCfg::default().blast(StarKind::Barrel).unwrap();
        assert!((b.falloff(0.0) - 1.0).abs() < 1e-6);
        assert!((b.falloff(b.radius) - b.edge).abs() < 1e-6);
        assert!(b.falloff(b.radius * 0.5) > b.edge && b.falloff(b.radius * 0.5) < 1.0);
        assert!(StarCfg::default().blast(StarKind::Flash).is_none());
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
