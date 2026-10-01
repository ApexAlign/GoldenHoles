//! Cybernetic helmet-visor HUD (renderer-agnostic).
//! Pure math: `Hud::build` turns gameplay state into a flat list of draw primitives in
//! normalized screen space (0,0 = top-left, 1,1 = bottom-right) that any backend can draw.
//!
//! Design rule: peripheral vision stays clear so fast projectiles stay trackable.
//! Content lives only in the "brow" (top) and "chin" (bottom) bands around a small center
//! reticle. The left/right `SIDE_CLEAR` columns hold nothing but hairline rails, and the
//! vignette is capped and zero inside `VIGNETTE_START`.

pub const SIDE_CLEAR: f32 = 0.22; // columns [0,SIDE_CLEAR] and [1-SIDE_CLEAR,1] stay content-free
pub const BROW_H: f32 = 0.07;
pub const CHIN_TOP: f32 = 0.86;
pub const VIGNETTE_START: f32 = 0.78; // radial distance (corner = 1) where the shade begins
pub const VIGNETTE_MAX_A: f32 = 0.22;
pub const RAIL_W: f32 = 0.0025; // hairline visor rail thickness

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub f32, pub f32, pub f32, pub f32);

impl Rgba {
    pub const CYAN: Rgba = Rgba(0.0, 0.94, 1.0, 1.0);
    pub const MAGENTA: Rgba = Rgba(1.0, 0.1, 0.65, 1.0);
    pub const AMBER: Rgba = Rgba(1.0, 0.72, 0.1, 1.0);
    pub const GOLD: Rgba = Rgba(1.0, 0.84, 0.3, 1.0);
    pub fn a(self, a: f32) -> Self {
        Rgba(self.0, self.1, self.2, a)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Prim {
    /// Filled rect; `glow` = bloom radius in screen fraction (0 = none).
    Rect { r: Rect, color: Rgba, glow: f32 },
    Line { a: (f32, f32), b: (f32, f32), color: Rgba, glow: f32 },
    /// Screen-edge radial shade; shader evaluates `vignette_alpha`.
    Vignette { start: f32, max_a: f32, tint: Rgba },
    /// Faint CRT scanlines: `count` lines, `scroll` in [0,1), alpha `a`.
    Scanlines { count: u32, scroll: f32, a: f32 },
}

/// Gameplay snapshot the HUD needs each frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct HudState {
    pub hp: f32,         // 0..=1
    pub dash_ready: f32, // 0..=1 (1 = ready)
    pub ult: f32,        // 0..=1 (1 = ready)
    pub wall_run: bool,
    pub invulnerable: bool,
    /// Seconds since last enemy hit landed (None = never / expired).
    pub since_hit: Option<f32>,
    /// Seconds since damage taken and its bearing: radians, 0 = ahead, +ve = clockwise.
    pub since_damage: Option<(f32, f32)>,
    pub time: f32,
}

pub const HIT_MARK_TIME: f32 = 0.18;
pub const DAMAGE_TIME: f32 = 0.6;

/// Radial shade in [0, max_a]; `r` = distance from center normalized so a corner = 1.
pub fn vignette_alpha(r: f32, start: f32, max_a: f32) -> f32 {
    if r <= start {
        return 0.0;
    }
    let t = ((r - start) / (1.0 - start)).clamp(0.0, 1.0);
    max_a * t * t * (3.0 - 2.0 * t) // smoothstep
}

pub struct Hud;

impl Hud {
    pub fn build(s: &HudState) -> Vec<Prim> {
        let mut out = Vec::with_capacity(48);
        let low_hp = s.hp < 0.3;
        // Low HP: slow amber->magenta pulse on the chrome, never a full-screen flash.
        let pulse = if low_hp { 0.75 + 0.25 * (s.time * 6.0).sin() } else { 1.0 };
        let chrome = if low_hp { Rgba::MAGENTA } else { Rgba::CYAN };
        let chrome = if s.invulnerable { Rgba::GOLD } else { chrome };

        out.push(Prim::Vignette { start: VIGNETTE_START, max_a: VIGNETTE_MAX_A, tint: chrome.a(1.0) });
        out.push(Prim::Scanlines { count: 240, scroll: (s.time * 0.05).fract(), a: 0.035 });

        // Visor rails: hairlines hugging the side edges, brow and chin.
        let rail = chrome.a(0.55 * pulse);
        for x in [RAIL_W * 0.5, 1.0 - RAIL_W * 0.5] {
            out.push(Prim::Line { a: (x, 0.12), b: (x, 0.88), color: rail, glow: 0.004 });
        }
        out.push(Prim::Line { a: (0.22, BROW_H), b: (0.78, BROW_H), color: rail, glow: 0.004 });
        // Chamfered corner ticks give the retro-arcade bezel feel.
        for (cx, dx) in [(SIDE_CLEAR, 1.0), (1.0 - SIDE_CLEAR, -1.0)] {
            out.push(Prim::Line { a: (cx, BROW_H), b: (cx - dx * 0.03, 0.0), color: rail, glow: 0.004 });
            out.push(Prim::Line { a: (cx, CHIN_TOP), b: (cx - dx * 0.03, 1.0), color: rail, glow: 0.004 });
        }
        out.push(Prim::Line { a: (SIDE_CLEAR, CHIN_TOP), b: (1.0 - SIDE_CLEAR, CHIN_TOP), color: rail, glow: 0.004 });

        // Chin: health (left of center), dash pips (center), ultimate (right of center).
        bar(&mut out, Rect::new(0.26, 0.92, 0.22, 0.014), s.hp, if low_hp { Rgba::MAGENTA } else { Rgba::CYAN }, pulse);
        bar(&mut out, Rect::new(0.52, 0.92, 0.22, 0.014), s.ult, Rgba::GOLD, 1.0);
        let dash_col = if s.dash_ready >= 1.0 { Rgba::CYAN } else { Rgba::AMBER };
        bar(&mut out, Rect::new(0.44, 0.955, 0.12, 0.008), s.dash_ready, dash_col, 1.0);

        // Brow: wall-run chevron only when relevant (keeps the top quiet).
        if s.wall_run {
            out.push(Prim::Rect { r: Rect::new(0.47, 0.03, 0.06, 0.008), color: Rgba::CYAN.a(0.8), glow: 0.006 });
        }

        // Center reticle: four short ticks with a large open gap so targets stay visible.
        let (cx, cy, gap, len) = (0.5, 0.5, 0.012, 0.014);
        let ret = Rgba::CYAN.a(0.85);
        out.push(Prim::Line { a: (cx - gap - len, cy), b: (cx - gap, cy), color: ret, glow: 0.003 });
        out.push(Prim::Line { a: (cx + gap, cy), b: (cx + gap + len, cy), color: ret, glow: 0.003 });
        out.push(Prim::Line { a: (cx, cy + gap), b: (cx, cy + gap + len), color: ret, glow: 0.003 });

        // Hit marker: X flash at center, fades fast.
        if let Some(t) = s.since_hit.filter(|t| *t < HIT_MARK_TIME) {
            let f = 1.0 - t / HIT_MARK_TIME;
            let c = Rgba::GOLD.a(f);
            let d = 0.008 + 0.012 * (1.0 - f);
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                out.push(Prim::Line {
                    a: (cx + sx * d, cy + sy * d),
                    b: (cx + sx * (d + 0.01), cy + sy * (d + 0.01)),
                    color: c,
                    glow: 0.004,
                });
            }
        }

        // Damage direction: short arc tick on a ring of radius 0.09 around center (inside clear zone).
        if let Some((t, bearing)) = s.since_damage.filter(|(t, _)| *t < DAMAGE_TIME) {
            let f = 1.0 - t / DAMAGE_TIME;
            let (r0, r1) = (0.085, 0.105);
            let (sn, cs) = bearing.sin_cos();
            out.push(Prim::Line {
                a: (cx + sn * r0 * 0.5625, cy - cs * r0), // 0.5625 ~ 9/16 aspect correction
                b: (cx + sn * r1 * 0.5625, cy - cs * r1),
                color: Rgba::MAGENTA.a(f),
                glow: 0.008,
            });
        }
        out
    }
}

fn bar(out: &mut Vec<Prim>, r: Rect, frac: f32, color: Rgba, pulse: f32) {
    let f = frac.clamp(0.0, 1.0);
    out.push(Prim::Rect { r, color: color.a(0.12), glow: 0.0 }); // track
    if f > 0.0 {
        out.push(Prim::Rect { r: Rect::new(r.x, r.y, r.w * f, r.h), color: color.a(0.9 * pulse), glow: 0.006 });
    }
}

/// Bounding box of a primitive (ignoring glow), for layout checks.
pub fn bounds(p: &Prim) -> Option<Rect> {
    match *p {
        Prim::Rect { r, .. } => Some(r),
        Prim::Line { a, b, .. } => {
            let (x0, x1) = (a.0.min(b.0), a.0.max(b.0));
            let (y0, y1) = (a.1.min(b.1), a.1.max(b.1));
            Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy() -> HudState {
        HudState {
            hp: 0.2,
            dash_ready: 0.5,
            ult: 1.0,
            wall_run: true,
            invulnerable: false,
            since_hit: Some(0.05),
            since_damage: Some((0.1, 1.2)),
            time: 3.0,
        }
    }

    #[test]
    fn peripheral_columns_hold_only_hairline_rails() {
        for s in [HudState::default(), busy()] {
            for p in Hud::build(&s) {
                let Some(b) = bounds(&p) else { continue };
                let in_side = b.x < SIDE_CLEAR - 1e-4 || b.x + b.w > 1.0 - SIDE_CLEAR + 1e-4;
                if in_side {
                    // only the thin vertical rails or the bezel ticks that touch the clear-zone edge
                    let rail = b.w <= RAIL_W + 1e-4;
                    let tick = b.x >= SIDE_CLEAR - 0.031 && b.x + b.w <= 1.0 - SIDE_CLEAR + 0.031;
                    assert!(rail || tick, "content in peripheral column: {b:?}");
                }
            }
        }
    }

    #[test]
    fn center_reticle_leaves_crosshair_gap_open() {
        for p in Hud::build(&busy()) {
            if let Some(b) = bounds(&p) {
                let near = (b.x - 0.5).abs() < 0.0115 && (b.y - 0.5).abs() < 0.0115
                    && (b.x + b.w - 0.5).abs() < 0.0115 && (b.y + b.h - 0.5).abs() < 0.0115;
                assert!(!near, "center obstructed: {b:?}");
            }
        }
    }

    #[test]
    fn vignette_clear_inside_and_capped() {
        assert_eq!(vignette_alpha(0.0, VIGNETTE_START, VIGNETTE_MAX_A), 0.0);
        assert_eq!(vignette_alpha(VIGNETTE_START, VIGNETTE_START, VIGNETTE_MAX_A), 0.0);
        let edge = vignette_alpha(1.0, VIGNETTE_START, VIGNETTE_MAX_A);
        assert!((edge - VIGNETTE_MAX_A).abs() < 1e-6);
        assert!(vignette_alpha(2.0, VIGNETTE_START, VIGNETTE_MAX_A) <= VIGNETTE_MAX_A);
    }

    #[test]
    fn transient_markers_expire() {
        let base = HudState { hp: 1.0, dash_ready: 1.0, ..Default::default() };
        let n0 = Hud::build(&base).len();
        let hit = HudState { since_hit: Some(0.01), ..base };
        assert!(Hud::build(&hit).len() > n0);
        let old = HudState { since_hit: Some(HIT_MARK_TIME + 0.1), since_damage: Some((DAMAGE_TIME + 0.1, 0.0)), ..base };
        assert_eq!(Hud::build(&old).len(), n0);
    }

    #[test]
    fn low_hp_and_iframes_retint_chrome_not_fill_screen() {
        let ps = Hud::build(&busy());
        for p in &ps {
            if let Prim::Vignette { max_a, .. } = p {
                assert!(*max_a <= VIGNETTE_MAX_A);
            }
        }
    }
}
