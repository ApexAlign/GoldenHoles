//! CPU rasterizer for HUD draw lists: tessellated triangles alpha-blended over a flat test background, for
//! layout inspection and golden checks without a GPU. Glow, scanlines, grids and the vignette shader are not
//! reproduced (vignette is approximated as an edge darkening of the tint), so this shows geometry, colour and
//! legibility of what the HUD draws, not the final bloom.

use crate::hud::{vignette_alpha, Prim};
use crate::hud_geom::tessellate;

/// Test backdrops: a dark void and a bright sky-like scene (worst cases for legibility).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backdrop {
    Dark,
    Bright,
}

fn backdrop(b: Backdrop, x: f32, y: f32) -> [f32; 3] {
    match b {
        Backdrop::Dark => [0.01 + 0.04 * y, 0.015 + 0.03 * y, 0.04 + 0.08 * y],
        // horizon glow with a striped "wall" band across the middle
        Backdrop::Bright => {
            let sky = [0.55 - 0.2 * y, 0.35 + 0.1 * y, 0.6 - 0.1 * y];
            let band = if (0.42..0.62).contains(&y) { 0.25 * (((x * 24.0) as i32 % 2) as f32) } else { 0.0 };
            [sky[0] + band, sky[1] + band, sky[2] + band]
        }
    }
}

/// Render `prims` at `w` x `h` to packed RGB8.
pub fn render(prims: &[Prim], w: u32, h: u32, bg: Backdrop) -> Vec<u8> {
    let aspect = w as f32 / h as f32;
    let mut img: Vec<[f32; 3]> = (0..w * h).map(|i| backdrop(bg, (i % w) as f32 / w as f32, (i / w) as f32 / h as f32)).collect();
    // vignettes first (screen-space shade toward their tint), in draw order
    for p in prims {
        if let Prim::Vignette { start, max_a, tint } = p {
            for y in 0..h {
                for x in 0..w {
                    let (nx, ny) = (x as f32 / w as f32 * 2.0 - 1.0, y as f32 / h as f32 * 2.0 - 1.0);
                    let r = (nx * nx + ny * ny).sqrt() / std::f32::consts::SQRT_2;
                    let a = vignette_alpha(r, *start, *max_a);
                    if a > 0.0 {
                        let px = &mut img[(y * w + x) as usize];
                        for (c, t) in px.iter_mut().zip([tint.0, tint.1, tint.2]) {
                            *c = *c * (1.0 - a) + t.min(1.0) * a;
                        }
                    }
                }
            }
        }
    }
    let g = tessellate(prims, aspect);
    for tri in g.idx.chunks_exact(3) {
        let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let to = |p: [f32; 2]| (p[0] * w as f32, p[1] * h as f32);
        let (pa, pb, pc) = (to(g.pos[a]), to(g.pos[b]), to(g.pos[c]));
        let (min_x, max_x) = (pa.0.min(pb.0).min(pc.0).floor().max(0.0), pa.0.max(pb.0).max(pc.0).ceil().min(w as f32 - 1.0));
        let (min_y, max_y) = (pa.1.min(pb.1).min(pc.1).floor().max(0.0), pa.1.max(pb.1).max(pc.1).ceil().min(h as f32 - 1.0));
        let area = (pb.0 - pa.0) * (pc.1 - pa.1) - (pb.1 - pa.1) * (pc.0 - pa.0);
        if area.abs() < 1e-9 {
            continue;
        }
        // flat colour per triangle (the tessellator colours whole primitives uniformly)
        let col = g.col[a];
        for y in min_y as i32..=max_y as i32 {
            for x in min_x as i32..=max_x as i32 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let w0 = ((pb.0 - px) * (pc.1 - py) - (pb.1 - py) * (pc.0 - px)) / area;
                let w1 = ((pc.0 - px) * (pa.1 - py) - (pc.1 - py) * (pa.0 - px)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 >= -0.02 && w1 >= -0.02 && w2 >= -0.02 {
                    let d = &mut img[(y as u32 * w + x as u32) as usize];
                    let al = col[3].clamp(0.0, 1.0);
                    for (ch, s) in d.iter_mut().zip([col[0], col[1], col[2]]) {
                        *ch = *ch * (1.0 - al) + s.min(1.0) * al;
                    }
                }
            }
        }
    }
    img.iter().flat_map(|p| p.iter().map(|v| (v.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8)).collect()
}

/// Binary PPM (P6) bytes.
pub fn ppm(rgb: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    out.extend_from_slice(rgb);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hud::{Rect, Rgba};

    #[test]
    fn a_rect_lands_where_it_should_and_alpha_blends() {
        let prims = [Prim::Rect { r: Rect::new(0.25, 0.25, 0.5, 0.5), color: Rgba(1.0, 0.0, 0.0, 1.0), glow: 0.0 }];
        let (w, h) = (80, 40);
        let img = render(&prims, w, h, Backdrop::Dark);
        let at = |x: u32, y: u32| &img[((y * w + x) * 3) as usize..((y * w + x) * 3 + 3) as usize];
        assert_eq!(at(40, 20), &[255, 0, 0], "inside the rect");
        assert!(at(2, 2)[0] < 60, "outside stays backdrop");
        assert_eq!(&ppm(&img, w, h)[..11], b"P6\n80 40\n25");
    }
}
