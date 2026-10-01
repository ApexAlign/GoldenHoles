//! Renderer-agnostic shader data: WGSL sources + the uniform/settings structs that feed them.
//! No GPU types. A backend (bevy today) uploads `to_array()` blocks and compiles `assemble()` output.
//! Colours come from `hud::Palette` (single source of truth); HDR gain is applied here, not in the palette.

use crate::hud::{Palette, Rgba};

pub const FX_COMMON: &str = include_str!("../shaders/fx_common.wgsl");
pub const TRON_SURFACE: &str = include_str!("../shaders/tron_surface.wgsl");
pub const TRON_HORIZON: &str = include_str!("../shaders/tron_horizon.wgsl");
pub const LIGHTWALL: &str = include_str!("../shaders/lightwall.wgsl");
pub const RIBBON_FX: &str = include_str!("../shaders/ribbon_fx.wgsl");
pub const CRT_COMPOSITE: &str = include_str!("../shaders/crt_composite.wgsl");

/// Pass source with the shared `Fx` block prepended (raw-wgpu path; bevy can import it instead).
pub fn assemble(pass_src: &str) -> String {
    format!("{FX_COMMON}\n{pass_src}")
}

fn hdr(c: Rgba, gain: f32) -> [f32; 4] {
    [c.0 * gain, c.1 * gain, c.2 * gain, 1.0]
}

/// `Mat` in tron_surface.wgsl (16 floats).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridMaterial {
    pub base: [f32; 4],
    pub glow: [f32; 4],
    pub cell: f32,
    pub major_every: f32,
    pub line_px: f32,
    pub edge_falloff: f32,
    pub edge_gain: f32,
}

impl GridMaterial {
    pub fn tron(p: &Palette) -> Self {
        Self {
            base: [p.bg.0, p.bg.1, p.bg.2, 1.0],
            glow: hdr(p.cyan, 1.4),
            cell: 2.0,
            major_every: 5.0,
            line_px: 0.7,
            edge_falloff: 0.06,
            edge_gain: 2.4,
        }
    }
    pub fn to_array(&self) -> [f32; 16] {
        [
            self.base[0], self.base[1], self.base[2], self.base[3],
            self.glow[0], self.glow[1], self.glow[2], self.glow[3],
            self.cell, self.major_every, self.line_px, 0.0,
            self.edge_falloff, self.edge_gain, 0.0, 0.0,
        ]
    }
}

/// `Sky` in tron_horizon.wgsl minus the matrix (caller prepends `inv_view_proj`, 16 floats).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HorizonParams {
    pub floor_y: f32,
    pub floor_col: [f32; 4],
    pub horizon_col: [f32; 4],
    pub cell: f32,
    pub major_every: f32,
    pub line_px: f32,
    pub fade_dist: f32,
    pub sky_top: [f32; 4],
}

impl HorizonParams {
    pub fn tron(p: &Palette) -> Self {
        Self {
            floor_y: 0.0,
            floor_col: hdr(p.cyan, 1.2),
            horizon_col: hdr(p.magenta, 1.1),
            cell: 2.0,
            major_every: 5.0,
            line_px: 0.7,
            fade_dist: 400.0,
            sky_top: [p.bg.0 * 0.6, p.bg.1 * 0.6, p.bg.2 * 1.4, 1.0],
        }
    }
    /// Floats after `inv_view_proj` in the `Sky` uniform: 5 x vec4.
    pub const FLOATS: usize = 20;

    /// Everything after `inv_view_proj` in the `Sky` uniform: cam_pos is supplied per frame.
    pub fn to_array(&self, cam_pos: [f32; 3]) -> [f32; Self::FLOATS] {
        let mut o = [0.0; Self::FLOATS];
        o[..3].copy_from_slice(&cam_pos);
        o[3] = self.floor_y;
        o[4..8].copy_from_slice(&self.floor_col);
        o[8..12].copy_from_slice(&self.horizon_col);
        o[12..16].copy_from_slice(&[self.cell, self.major_every, self.line_px, self.fade_dist]);
        o[16..20].copy_from_slice(&self.sky_top);
        o
    }
}

/// `Crt` in crt_composite.wgsl (12 floats).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrtSettings {
    pub scan_density: f32,
    pub scan_strength: f32,
    pub barrel_k: f32,
    pub chroma_px: f32,
    pub vignette_base: f32,
    pub chroma_falloff: f32,
    pub slash_width: f32,
    pub slash_intensity: f32,
    pub slash_col: [f32; 4],
}

impl CrtSettings {
    pub fn tron(p: &Palette) -> Self {
        Self {
            scan_density: 1.0,
            scan_strength: 0.06,
            barrel_k: 0.08,
            chroma_px: 1.8,
            vignette_base: 0.15,
            chroma_falloff: 2.0,
            slash_width: 0.035,
            slash_intensity: 0.8,
            slash_col: hdr(p.orange, 1.3),
        }
    }
    pub fn to_array(&self) -> [f32; 12] {
        [
            self.scan_density, self.scan_strength, self.barrel_k, self.chroma_px,
            self.vignette_base, self.chroma_falloff, self.slash_width, self.slash_intensity,
            self.slash_col[0], self.slash_col[1], self.slash_col[2], self.slash_col[3],
        ]
    }
}

/// Backend-neutral bloom request. Maps 1:1 onto bevy `Bloom` / a dual-kawase chain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BloomSettings {
    pub threshold: f32,
    pub knee: f32,
    pub intensity: f32,
    pub mips: u32,
    pub base_scale: f32,
}

impl BloomSettings {
    pub const DESKTOP: Self = Self { threshold: 0.8, knee: 0.4, intensity: 2.6, mips: 5, base_scale: 0.5 };
    pub const MOBILE: Self = Self { threshold: 0.9, knee: 0.4, intensity: 2.2, mips: 4, base_scale: 0.25 };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::FxUniforms;

    const ALL: [(&str, &str); 5] = [
        ("tron_surface", TRON_SURFACE),
        ("tron_horizon", TRON_HORIZON),
        ("lightwall", LIGHTWALL),
        ("ribbon_fx", RIBBON_FX),
        ("crt_composite", CRT_COMPOSITE),
    ];

    /// No WGSL validator offline: catch the cheap, common breakage (unbalanced delimiters, missing
    /// entry points, WGSL-forbidden shift/relational mixing).
    #[test]
    fn wgsl_structurally_sane() {
        for (name, src) in ALL.iter().chain([("fx_common", FX_COMMON)].iter().map(|x| x)) {
            for (o, c) in [('{', '}'), ('(', ')'), ('[', ']')] {
                let (no, nc) = (src.matches(o).count(), src.matches(c).count());
                assert_eq!(no, nc, "{name}: unbalanced {o}{c}");
            }
            assert!(!src.contains(">> 1u <") && !src.contains(">> 1u >"), "{name}: parenthesize shift");
        }
        assert!(TRON_SURFACE.contains("@vertex") && TRON_SURFACE.contains("@fragment"));
        assert!(TRON_HORIZON.contains("fn vs") && TRON_HORIZON.contains("fn fs"));
        assert!(LIGHTWALL.contains("fn vs") && LIGHTWALL.contains("fn fs"));
        assert!(RIBBON_FX.contains("fn fs_ribbon") && RIBBON_FX.contains("fn fs_disc"));
        assert!(CRT_COMPOSITE.contains("fn vs") && CRT_COMPOSITE.contains("fn fs"));
    }

    #[test]
    fn fx_block_matches_rust_layout() {
        // 4 vec4<f32> in WGSL == FxUniforms::FLOATS
        assert_eq!(FX_COMMON.matches("vec4<f32>,").count(), 4);
        assert_eq!(FxUniforms::FLOATS, 16);
        assert!(assemble(CRT_COMPOSITE).starts_with("// Shared by every shader"));
    }

    #[test]
    fn fwidth_never_inside_a_branch() {
        // fwidth/derivatives must run in uniform control flow; both grid shaders keep it in grid_line()
        // which is called unconditionally.
        for (n, s) in [("surface", TRON_SURFACE), ("horizon", TRON_HORIZON)] {
            let calls: Vec<_> = s.match_indices("grid_line(").collect();
            assert!(calls.len() >= 3, "{n}");
            assert!(!s.contains("if (dy <"), "{n}: floor branch must not wrap grid_line");
        }
    }

    /// Every `@group(g) @binding(b)` pair must be unique inside one shader: a duplicate is a pipeline
    /// creation error that only shows up at runtime on the GPU.
    #[test]
    fn wgsl_bindings_unique_per_shader() {
        for (name, src) in ALL {
            let mut seen = std::collections::HashSet::new();
            let mut rest = src;
            while let Some(i) = rest.find("@group(") {
                rest = &rest[i + "@group(".len()..];
                let g: u32 = rest[..rest.find(')').unwrap()].trim().parse().expect("group index");
                let j = rest.find("@binding(").expect("@group without @binding");
                let after = &rest[j + "@binding(".len()..];
                let b: u32 = after[..after.find(')').unwrap()].trim().parse().expect("binding index");
                assert!(seen.insert((g, b)), "{name}: duplicate @group({g}) @binding({b})");
            }
            assert!(!seen.is_empty(), "{name}: no bindings found");
        }
    }

    #[test]
    fn lightwall_wgsl_matches_cpu_formula() {
        // same formula text as fx::lightwall_alpha: tick phase 0.35, smoothstep fade from fade_start,
        // pulse = 1 - depth + depth * (0.5 + 0.5 sin)
        for needle in [
            "f32(s.tick) * 0.35",
            "W.life_s * W.fade_start",
            "1.0 - smoothstep(t0, W.life_s, age)",
            "1.0 - W.pulse_depth + W.pulse_depth * (0.5 + 0.5 * sin(phase))",
            "W.max_alpha * fade * pl",
        ] {
            assert!(LIGHTWALL.contains(needle), "lightwall.wgsl drifted from fx::lightwall_alpha: {needle}");
        }
    }

    #[test]
    fn slash_distorts_before_sampling() {
        let push = CRT_COMPOSITE.find("uv = uv + dist * 0.02 + slash_push").expect("push");
        let tap = CRT_COMPOSITE.find("textureSampleLevel(scene_tex").expect("tap");
        assert!(push < tap, "slash must warp uv before scene taps");
    }

    #[test]
    fn palette_drives_hdr_values() {
        let p = Palette::TRON;
        let m = GridMaterial::tron(&p);
        assert!(m.glow[2] > 1.0, "grid glow must exceed bloom threshold");
        assert!(BloomSettings::DESKTOP.threshold < m.glow[2]);
        assert_eq!(m.to_array().len(), 16);
        let h = HorizonParams::tron(&p).to_array([1.0, 2.0, 3.0]);
        assert_eq!(h.len(), 20, "5 x vec4 after inv_view_proj");
        assert_eq!(h[..4], [1.0, 2.0, 3.0, 0.0]);
        assert!(CrtSettings::tron(&p).to_array()[8] > 1.0);
    }
}
