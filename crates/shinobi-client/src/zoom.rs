//! The yumi's focused ZOOM (docs/powerups.md, "Zoom"): client-only camera state. The FOV narrows with the draw only
//! while the focus button (RMB) is held (`power::zoom_of_draw`), look speed follows the FOV, a hit on you drops it.
//! Hip-fire never zooms. The damage / reload tradeoff is the sim's (`power::hip_*`).
use shinobi_sim::power::{self, YumiCfg};

/// Zoom rises at most this fast (1/s) so a snapped-back view eases in; it falls at once (a hit, a release).
pub const RISE_PER_S: f32 = 5.0;

/// Drop tick distance when nothing is aimed at (m).
pub const DEFAULT_RANGE: f32 = 150.0;

/// What the HUD / view model / camera need of the zoom for one frame.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct ZoomView {
    /// 0 = hip-fire, 1 = fully zoomed.
    pub frac: f32,
    /// The camera's vertical FOV right now (degrees).
    pub fov_deg: f32,
    /// Fully drawn and not flinching: the focus state (tight reticle, steady aim).
    pub focus: bool,
    /// Metres to the aimed point (level geometry or a player); `None` = open sky.
    pub range: Option<f32>,
    /// Arrow drop below the aim at that range (radians).
    pub drop_rad: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ZoomState {
    pub zoom: f32,
    /// Seconds the zoom stays dropped after a hit.
    pub hurt: f32,
    last_hp: Option<f32>,
}

impl ZoomState {
    /// One sim tick. `draw_frac` 0..=1 (0 when not drawing), `zoom_held` the focus button (RMB), `hp` the server's
    /// exact health if known.
    pub fn tick(&mut self, dt: f32, draw_frac: f32, zoom_held: bool, hp: Option<f32>, cfg: &YumiCfg) {
        if let (Some(prev), Some(now)) = (self.last_hp, hp) {
            if now < prev - 0.05 {
                self.hurt = cfg.zoom_hurt_s;
                self.zoom = 0.0;
            }
        }
        self.last_hp = hp;
        self.hurt = (self.hurt - dt).max(0.0);
        let target = if self.hurt > 0.0 || !zoom_held { 0.0 } else { power::zoom_of_draw(draw_frac, cfg) };
        self.zoom = if target < self.zoom { target } else { (self.zoom + RISE_PER_S * dt).min(target) };
    }

    /// The FOV (degrees) for a base FOV.
    pub fn fov(&self, base: f32, cfg: &YumiCfg) -> f32 {
        power::zoom_fov_deg(base, self.zoom, cfg)
    }

    /// Look-speed multiplier: the bow's slow turn while drawn, never faster than the FOV ratio asks for.
    pub fn turn_k(&self, drawing: bool, base: f32, cfg: &YumiCfg) -> f32 {
        if !drawing {
            return 1.0;
        }
        cfg.turn_mult.min(power::fov_turn_scale(base, self.fov(base, cfg)))
    }

    pub fn view(&self, base: f32, full: bool, range: Option<f32>, cfg: &YumiCfg) -> ZoomView {
        ZoomView {
            frac: self.zoom,
            fov_deg: self.fov(base, cfg),
            focus: full && self.hurt <= 0.0 && self.zoom > 0.95,
            range,
            drop_rad: power::arrow_drop_angle(range.unwrap_or(DEFAULT_RANGE), cfg),
        }
    }
}

/// Distance along the ray `o + t d` (`d` unit) to the nearest of the given sphere centres within `radius` of the ray,
/// closer than `max`: how the readout ranges a player in the crosshair.
pub fn ray_sphere_range(o: [f32; 3], d: [f32; 3], centre: [f32; 3], radius: f32, max: f32) -> Option<f32> {
    let v = [centre[0] - o[0], centre[1] - o[1], centre[2] - o[2]];
    let t = v[0] * d[0] + v[1] * d[1] + v[2] * d[2];
    if t < 0.0 || t > max {
        return None;
    }
    let c = [v[0] - d[0] * t, v[1] - d[1] * t, v[2] - d[2] * t];
    ((c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt() <= radius).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(z: &mut ZoomState, secs: f32, draw: f32, held: bool, hp: Option<f32>) {
        let cfg = YumiCfg::default();
        for _ in 0..(secs * 60.0) as u32 {
            z.tick(1.0 / 60.0, draw, held, hp, &cfg);
        }
    }

    #[test]
    fn only_a_focus_held_draw_zooms_to_the_target_fov() {
        let cfg = YumiCfg::default();
        let mut z = ZoomState::default();
        run(&mut z, 1.0, 1.0, false, Some(100.0));
        assert_eq!(z.fov(90.0, &cfg), 90.0, "hip-fire: a full draw does not zoom");
        assert!(!z.view(90.0, true, Some(80.0), &cfg).focus);
        run(&mut z, 0.3, cfg.tap_s / cfg.draw_s, true, Some(100.0));
        assert_eq!(z.fov(90.0, &cfg), 90.0, "inside the tap: no zoom even focused");
        run(&mut z, 0.5, 0.6, true, Some(100.0));
        let part = z.fov(90.0, &cfg);
        assert!(part < 90.0 && part > 30.0, "{part}");
        run(&mut z, 1.0, 1.0, true, Some(100.0));
        assert!((z.fov(90.0, &cfg) - 30.0).abs() < 1e-3);
        assert!(z.view(90.0, true, Some(80.0), &cfg).focus);
        run(&mut z, 0.1, 1.0, false, Some(100.0));
        assert_eq!(z.zoom, 0.0, "releasing focus drops it at once");
        run(&mut z, 0.1, 0.0, false, Some(100.0));
        assert_eq!(z.zoom, 0.0, "release drops it at once");
    }

    #[test]
    fn damage_drops_the_zoom_and_it_eases_back() {
        let cfg = YumiCfg::default();
        let mut z = ZoomState::default();
        run(&mut z, 1.0, 1.0, true, Some(100.0));
        assert!(z.zoom > 0.99);
        run(&mut z, 1.0 / 60.0, 1.0, true, Some(70.0));
        assert_eq!(z.zoom, 0.0);
        assert!(!z.view(90.0, true, None, &cfg).focus);
        run(&mut z, 0.2, 1.0, true, Some(70.0));
        assert_eq!(z.zoom, 0.0, "still flinching");
        run(&mut z, 0.5, 1.0, true, Some(70.0));
        assert!(z.zoom > 0.9, "back in: {}", z.zoom);
    }

    #[test]
    fn look_speed_follows_the_fov() {
        let cfg = YumiCfg::default();
        let mut z = ZoomState::default();
        assert_eq!(z.turn_k(false, 90.0, &cfg), 1.0);
        assert_eq!(z.turn_k(true, 90.0, &cfg), cfg.turn_mult, "hip draw: the plain bow slow-down");
        run(&mut z, 1.5, 1.0, true, Some(100.0));
        let k = z.turn_k(true, 90.0, &cfg);
        assert!((k - 15f32.to_radians().tan()).abs() < 1e-3 && k < cfg.turn_mult, "{k}");
    }

    #[test]
    fn a_player_in_the_crosshair_ranges_and_one_beside_it_does_not() {
        let o = [0.0, 1.5, 0.0];
        let d = [0.0, 0.0, -1.0];
        let r = ray_sphere_range(o, d, [0.2, 1.4, -120.0], 0.6, 300.0).unwrap();
        assert!((r - 120.0).abs() < 0.01);
        assert!(ray_sphere_range(o, d, [3.0, 1.4, -120.0], 0.6, 300.0).is_none());
        assert!(ray_sphere_range(o, d, [0.0, 1.5, 20.0], 0.6, 300.0).is_none(), "behind");
    }
}
