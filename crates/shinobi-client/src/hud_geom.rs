//! Tessellates the HUD draw list (`hud::Prim`) into one triangle soup any backend can upload:
//! positions in normalized screen space (0,0 top-left, 1,1 bottom-right), straight-alpha vertex colours.
//! Handles `Rect`, `Line`, `Ring` and `Text` (5x7 bitmap font). `Vignette` and `Scanlines` are skipped
//! (the CRT composite already does both) and `Grid` is skipped for now.

use std::f32::consts::TAU;

use crate::font::{self, ADVANCE, GLYPH_H, GLYPH_W};
use crate::hud::{Align, Prim, Rect, Rgba};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Geom {
    pub pos: Vec<[f32; 2]>,
    pub col: Vec<[f32; 4]>,
    pub idx: Vec<u32>,
}

/// Hairline thickness in screen-height fractions.
const LINE_T: f32 = 0.0022;
const RING_SEGS: usize = 48;

impl Geom {
    fn quad(&mut self, p: [[f32; 2]; 4], c: Rgba) {
        let b = self.pos.len() as u32;
        for v in p {
            self.pos.push(v);
            self.col.push([c.0, c.1, c.2, c.3]);
        }
        self.idx.extend_from_slice(&[b, b + 1, b + 2, b, b + 2, b + 3]);
    }

    fn rect(&mut self, r: Rect, c: Rgba) {
        self.quad([[r.x, r.y], [r.x + r.w, r.y], [r.x + r.w, r.y + r.h], [r.x, r.y + r.h]], c);
    }

    /// Thin quad from `a` to `b`; thickness is in height fractions so it stays square on any aspect.
    fn line(&mut self, a: (f32, f32), b: (f32, f32), t: f32, aspect: f32, c: Rgba) {
        // work in height-normalised units so the normal is perpendicular on screen
        let (ax, ay, bx, by) = (a.0 * aspect, a.1, b.0 * aspect, b.1);
        let (dx, dy) = (bx - ax, by - ay);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-7 {
            return;
        }
        let (nx, ny) = (-dy / len * t * 0.5, dx / len * t * 0.5);
        let to = |x: f32, y: f32| [x / aspect, y];
        self.quad([to(ax + nx, ay + ny), to(bx + nx, by + ny), to(bx - nx, by - ny), to(ax - nx, ay - ny)], c);
    }

    fn ring(&mut self, cen: (f32, f32), r: f32, t: f32, fill: bool, aspect: f32, c: Rgba) {
        let pt = |a: f32, rad: f32| [cen.0 + a.cos() * rad / aspect, cen.1 + a.sin() * rad];
        for i in 0..RING_SEGS {
            let (a0, a1) = (i as f32 / RING_SEGS as f32 * TAU, (i + 1) as f32 / RING_SEGS as f32 * TAU);
            if fill {
                let b = self.pos.len() as u32;
                for p in [[cen.0, cen.1], pt(a0, r), pt(a1, r)] {
                    self.pos.push(p);
                    self.col.push([c.0, c.1, c.2, c.3]);
                }
                self.idx.extend_from_slice(&[b, b + 1, b + 2]);
            } else {
                let (ri, ro) = ((r - t * 0.5).max(0.0), r + t * 0.5);
                self.quad([pt(a0, ro), pt(a1, ro), pt(a1, ri), pt(a0, ri)], c);
            }
        }
    }

    fn text(&mut self, pos: (f32, f32), s: &str, size: f32, c: Rgba, align: Align, aspect: f32) {
        let px_h = size / GLYPH_H as f32; // one font pixel, screen-height fraction
        let px_w = px_h / aspect; // ...and in x units, square on screen
        let width = font::text_cols(s) as f32 * px_w;
        let x0 = match align {
            Align::Left => pos.0,
            Align::Center => pos.0 - width * 0.5,
            Align::Right => pos.0 - width,
        };
        let y0 = pos.1 - size; // baseline-left anchor: glyph top sits `size` above
        for (i, ch) in s.chars().enumerate() {
            let rows = font::glyph(ch);
            for (ry, row) in rows.iter().enumerate() {
                for cx in 0..GLYPH_W {
                    if row & (1 << (GLYPH_W - 1 - cx)) != 0 {
                        let x = x0 + (i * ADVANCE + cx) as f32 * px_w;
                        let y = y0 + ry as f32 * px_h;
                        self.rect(Rect::new(x, y, px_w, px_h), c);
                    }
                }
            }
        }
    }
}

/// `aspect` = screen width / height.
pub fn tessellate(prims: &[Prim], aspect: f32) -> Geom {
    let mut g = Geom::default();
    for p in prims {
        match p {
            Prim::Rect { r, color, .. } => g.rect(*r, *color),
            Prim::Line { a, b, color, .. } => g.line(*a, *b, LINE_T, aspect, *color),
            Prim::Ring { c, r, color, fill, .. } => g.ring(*c, *r, LINE_T, *fill, aspect, *color),
            Prim::Text { pos, text, size, color, align } => g.text(*pos, text, *size, *color, *align, aspect),
            Prim::Vignette { .. } | Prim::Scanlines { .. } | Prim::Grid { .. } => {}
        }
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hud::{Hud, HudState};

    const W: Rgba = Rgba(1.0, 1.0, 1.0, 1.0);

    #[test]
    fn rect_is_two_triangles_with_the_given_colour() {
        let g = tessellate(&[Prim::Rect { r: Rect::new(0.1, 0.2, 0.3, 0.4), color: Rgba(0.5, 0.6, 0.7, 0.8), glow: 0.0 }], 1.0);
        assert_eq!((g.pos.len(), g.idx.len()), (4, 6));
        assert_eq!(g.col[0], [0.5, 0.6, 0.7, 0.8]);
        assert_eq!(g.pos[2], [0.4, 0.6]);
    }

    #[test]
    fn ring_is_round_on_screen_at_any_aspect() {
        let aspect = 2.0;
        let g = tessellate(&[Prim::Ring { c: (0.5, 0.5), r: 0.1, color: W, fill: true, glow: 0.0 }], aspect);
        // every rim vertex is 0.1 screen-heights from the centre once x is scaled back by aspect
        for p in g.pos.iter().skip(1).step_by(3) {
            let d = (((p[0] - 0.5) * aspect).powi(2) + (p[1] - 0.5).powi(2)).sqrt();
            assert!((d - 0.1).abs() < 1e-4, "{d}");
        }
    }

    #[test]
    fn text_alignment_and_pixel_squareness() {
        let aspect = 16.0 / 9.0;
        let size = 0.07;
        let l = tessellate(&[Prim::Text { pos: (0.5, 0.5), text: "I".into(), size, color: W, align: Align::Left }], aspect);
        let c = tessellate(&[Prim::Text { pos: (0.5, 0.5), text: "I".into(), size, color: W, align: Align::Center }], aspect);
        let minx = |g: &Geom| g.pos.iter().map(|p| p[0]).fold(f32::MAX, f32::min);
        assert!(minx(&c) < minx(&l), "centered text starts left of the anchor");
        // first quad of the first lit pixel: square on screen
        let w = (l.pos[1][0] - l.pos[0][0]) * aspect;
        let h = l.pos[2][1] - l.pos[1][1];
        assert!((w - h).abs() < 1e-5, "font pixel is square: {w} vs {h}");
        assert!(l.pos.iter().all(|p| p[1] <= 0.5 + 1e-6), "glyph sits above the baseline");
    }

    #[test]
    fn skipped_prims_emit_nothing_and_real_hud_tessellates() {
        let g = tessellate(&[Prim::Vignette { start: 0.5, max_a: 0.5, tint: W }, Prim::Scanlines { count: 10, scroll: 0.0, a: 0.1 }], 1.0);
        assert!(g.pos.is_empty() && g.idx.is_empty());
        let s = HudState { hp: 0.7, dash_ready: 1.0, shuriken_ready: 0.5, ..Default::default() };
        let g = tessellate(&Hud::build(&s), 16.0 / 9.0);
        assert!(g.idx.len() > 60, "crosshair + bars + labels produce geometry: {}", g.idx.len());
        assert!(g.idx.iter().all(|&i| (i as usize) < g.pos.len()));
        assert_eq!(g.pos.len(), g.col.len());
    }
}
