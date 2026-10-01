//! Cosmetic motion trail: turns a short history of positions into thin ribbon geometry. Purely visual: the game
//! has no persistent trail walls and nothing here touches the sim or any collider. The history is recorded by the
//! shell from interpolated positions (see `shinobi-game/src/walls.rs`).
//!
//! Two outputs, same math as `shaders/lightwall.wgsl` / `fx::lightwall_alpha_age`:
//!   * `wall_samples` -> compact (pos, tick) list for the storage-buffer path (vertex shader builds the quad strip)
//!   * `wall_strip`   -> ready triangle-strip vertices for backends without vertex-stage storage buffers

use shinobi_proto::TICK_DT;

use crate::fx::{lightwall_alpha_age, LightWallCfg};

/// One recorded position (feet/hip anchor) and when it was recorded, seconds on the shell's clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrailPoint {
    pub pos: [f32; 3],
    pub born: f32,
}

/// Matches `struct Sample` in lightwall.wgsl (16 B: vec3 + u32).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WallSample {
    pub pos: [f32; 3],
    pub tick: u32,
}

/// One strip vertex: bottom then top per sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WallVertex {
    pub pos: [f32; 3],
    /// 0 = foot, 1 = top edge (hot rim).
    pub v: f32,
    /// Already includes fade + server-time pulse; 0 = cull.
    pub alpha: f32,
    /// 1 = just recorded (head), 0 = about to expire (tail). Drives the hot-to-cool colour ramp.
    pub heat: f32,
}

fn ticks(born: f32) -> u32 {
    (born / TICK_DT).round().max(0.0) as u32
}

/// Live samples, oldest -> newest, across both ring slices. Expired points are dropped.
pub fn wall_samples(slices: (&[TrailPoint], &[TrailPoint]), now: f32, life_s: f32) -> Vec<WallSample> {
    slices
        .0
        .iter()
        .chain(slices.1.iter())
        .filter(|p| now - p.born <= life_s)
        .map(|p| WallSample { pos: p.pos, tick: ticks(p.born) })
        .collect()
}

/// Triangle strip (2 vertices per live sample). Fewer than 2 live samples -> empty (nothing to span).
pub fn wall_strip(
    slices: (&[TrailPoint], &[TrailPoint]),
    now: f32,
    height: f32,
    cfg: &LightWallCfg,
    server_time: f64,
) -> Vec<WallVertex> {
    let live: Vec<_> = slices.0.iter().chain(slices.1.iter()).filter(|p| now - p.born <= cfg.life_s).collect();
    if live.len() < 2 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(live.len() * 2);
    for p in live {
        let alpha = lightwall_alpha_age(cfg, now - p.born, ticks(p.born), server_time);
        let base = p.pos;
        let heat = (1.0 - (now - p.born) / cfg.life_s).clamp(0.0, 1.0);
        // taper: full height at the head, thinning to a point at the tail (a comet streak, not a wall)
        let h = height * (0.15 + 0.85 * heat);
        out.push(WallVertex { pos: base, v: 0.0, alpha, heat });
        out.push(WallVertex { pos: [base[0], base[1] + h, base[2]], v: 1.0, alpha, heat });
    }
    out
}

/// Colour for a ribbon vertex: tail `cool` -> head `hot`, brighter on the top rim. Returns linear RGB (HDR, for bloom).
pub fn ribbon_color(cool: [f32; 3], hot: [f32; 3], heat: f32, rim: bool) -> [f32; 3] {
    let k = if rim { 1.6 } else { 0.35 };
    let t = heat.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (cool[i] + (hot[i] - cool[i]) * t) * k)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f32, born: f32) -> TrailPoint {
        TrailPoint { pos: [x, 0.5, 0.0], born }
    }

    fn cfg() -> LightWallCfg {
        LightWallCfg { pulse_depth: 0.0, ..Default::default() }
    }

    #[test]
    fn strip_spans_both_ring_slices_in_order() {
        let a = [pt(0.0, 9.0), pt(1.0, 9.1)];
        let b = [pt(2.0, 9.2)];
        let s = wall_strip((&a, &b), 9.3, 1.6, &cfg(), 0.0);
        assert_eq!(s.len(), 6);
        let xs: Vec<_> = s.iter().step_by(2).map(|v| v.pos[0]).collect();
        assert_eq!(xs, vec![0.0, 1.0, 2.0]);
        assert_eq!((s[0].v, s[1].v), (0.0, 1.0));
        // ages 0.3 / 0.2 / 0.1 of 4 s life -> nearly full height at the newest sample
        let h = |i: usize| s[i * 2 + 1].pos[1] - s[i * 2].pos[1];
        assert!(h(0) < h(1) && h(1) < h(2), "taper: tail thinner than head");
        assert!(h(2) <= 1.6 && h(2) > 1.5);
    }

    #[test]
    fn heat_runs_from_tail_to_head_and_colour_ramps() {
        let a = [pt(0.0, 3.0), pt(1.0, 3.9)];
        let s = wall_strip((&a, &[]), 4.0, 1.0, &cfg(), 0.0);
        assert!(s[0].heat < s[2].heat && s[2].heat > 0.9);
        let cool = ribbon_color([0.0, 0.1, 1.0], [1.0, 0.1, 0.5], 0.0, true);
        let hot = ribbon_color([0.0, 0.1, 1.0], [1.0, 0.1, 0.5], 1.0, true);
        assert_eq!(cool[0], 0.0);
        assert!((hot[0] - 1.6).abs() < 1e-6);
        assert!(ribbon_color([1.0; 3], [1.0; 3], 0.5, false)[0] < ribbon_color([1.0; 3], [1.0; 3], 0.5, true)[0]);
    }

    #[test]
    fn single_or_no_sample_emits_nothing() {
        let a = [pt(0.0, 1.0)];
        assert!(wall_strip((&a, &[]), 1.0, 1.6, &cfg(), 0.0).is_empty());
        assert!(wall_strip((&[], &[]), 1.0, 1.6, &cfg(), 0.0).is_empty());
    }

    #[test]
    fn older_segments_fade_and_expired_are_dropped() {
        let c = cfg(); // life 4 s, fade from 2.4 s
        let pts = [pt(0.0, 0.0), pt(1.0, 1.5), pt(2.0, 3.0), pt(3.0, 3.9)];
        let now = 4.5; // ages 4.5(expired), 3.0, 1.5, 0.6
        let s = wall_strip((&pts, &[]), now, 1.0, &c, 0.0);
        assert_eq!(s.len(), 6, "oldest expired -> 3 samples");
        let al: Vec<_> = s.iter().step_by(2).map(|v| v.alpha).collect();
        assert!(al[0] < al[1] && al[1] <= al[2] + 1e-6, "alpha rises toward the newest: {al:?}");
        assert!((al[2] - c.max_alpha).abs() < 1e-5, "fresh sample at full alpha");
        assert!(al[0] < c.max_alpha * 0.75, "3.0 s old sample already fading (~0.68 of max)");
    }

    #[test]
    fn samples_carry_sim_ticks_for_the_gpu_path() {
        let a = [pt(0.0, 1.0)];
        let b = [pt(1.0, 1.0 + 10.0 * TICK_DT)];
        let s = wall_samples((&a, &b), 1.2, 4.0);
        assert_eq!(s.len(), 2);
        assert_eq!(s[1].tick - s[0].tick, 10);
        assert_eq!(wall_samples((&a, &b), 9.0, 4.0).len(), 0);
    }

    #[test]
    fn cpu_alpha_equals_gpu_formula_inputs() {
        // ribbon vertex alpha must equal the shared fx formula for the same age/tick/time
        let c = LightWallCfg::default();
        let a = [pt(0.0, 2.0)];
        let b = [pt(1.0, 2.5)];
        let st = 7.25;
        let s = wall_strip((&a, &b), 3.0, 1.0, &c, st);
        assert_eq!(s[0].alpha, lightwall_alpha_age(&c, 1.0, ticks(2.0), st));
    }
}
