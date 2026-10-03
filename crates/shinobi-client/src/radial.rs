//! Radial build menu (hold N / M): two sectors, FLOOR on the left and WALL on the right, picked with the free
//! cursor. Releasing the key commits the highlighted piece (`ModeOut.radial_commit`); a release inside the
//! dead zone cancels. Pure geometry + draw list.

use crate::hud::{Align, BuildSlot, Palette, Prim};

const C: (f32, f32) = (0.5, 0.5);
const R_OUT: f32 = 0.17;
const R_DEAD: f32 = 0.03;
const ASPECT: f32 = 0.5625;

/// Piece under a normalized cursor position (y down), or `None` inside the dead zone.
pub fn pick(cursor: (f32, f32)) -> Option<BuildSlot> {
    // x is corrected to screen-height units so the dead zone is a circle
    let dx = (cursor.0 - C.0) / ASPECT;
    let dy = cursor.1 - C.1;
    if dx * dx + dy * dy < R_DEAD * R_DEAD {
        return None;
    }
    Some(if dx < 0.0 { BuildSlot::Floor } else { BuildSlot::Wall })
}

/// Draw the menu with `hover` highlighted and `current` (the selected piece) marked.
pub fn draw(hover: Option<BuildSlot>, current: BuildSlot, p: &Palette) -> Vec<Prim> {
    let mut out = Vec::new();
    out.push(Prim::Rect { r: crate::hud::Rect::new(0.0, 0.0, 1.0, 1.0), color: p.bg.a(0.35), glow: 0.0 });
    out.push(Prim::Ring { c: C, r: R_OUT, color: p.cyan.a(0.8), fill: false, glow: 0.006 });
    out.push(Prim::Ring { c: C, r: R_DEAD, color: p.cyan.a(0.4), fill: false, glow: 0.0 });
    out.push(Prim::Line { a: (C.0, C.1 - R_OUT), b: (C.0, C.1 - R_DEAD), color: p.cyan.a(0.5), glow: 0.0 });
    out.push(Prim::Line { a: (C.0, C.1 + R_DEAD), b: (C.0, C.1 + R_OUT), color: p.cyan.a(0.5), glow: 0.0 });
    for (slot, sx, label) in [(BuildSlot::Floor, -1.0f32, "N  FLOOR"), (BuildSlot::Wall, 1.0, "M  WALL")] {
        let on = hover == Some(slot);
        let col = if on { p.gold } else if slot == current { p.cyan } else { p.text };
        let cx = C.0 + sx * R_OUT * 0.55 * ASPECT;
        if on {
            // lit half-disc marker: a ring at the sector centre
            out.push(Prim::Ring { c: (cx, C.1), r: R_OUT * 0.55, color: p.gold.a(0.18), fill: true, glow: 0.01 });
        }
        out.push(Prim::Text { pos: (cx, C.1 + 0.008), text: label.into(), size: 0.03, color: col.a(if on { 1.0 } else { 0.75 }), align: Align::Center });
        // piece preview glyph under the label: slab (floor) or upright slab (wall)
        let (w, h) = if slot == BuildSlot::Floor { (0.05, 0.012) } else { (0.012, 0.05) };
        out.push(Prim::Rect { r: crate::hud::Rect::new(cx - w * 0.5, C.1 + 0.05 - h * 0.5, w, h), color: col.a(0.8), glow: 0.004 });
    }
    out.push(Prim::Text { pos: (C.0, C.1 + R_OUT + 0.05), text: "RELEASE TO PRINT".into(), size: 0.02, color: p.text.a(0.6), align: Align::Center });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_splits_left_right_with_a_circular_dead_zone() {
        assert_eq!(pick((0.5, 0.5)), None);
        assert_eq!(pick((0.51, 0.5)), None, "inside the dead zone");
        assert_eq!(pick((0.3, 0.5)), Some(BuildSlot::Floor));
        assert_eq!(pick((0.7, 0.4)), Some(BuildSlot::Wall));
        // vertical offset beyond the dead radius still resolves by side
        assert_eq!(pick((0.49, 0.8)), Some(BuildSlot::Floor));
    }

    #[test]
    fn draw_labels_both_pieces_and_highlights_the_hover() {
        let p = Palette::TRON;
        let labels = |h| draw(h, BuildSlot::Floor, &p).iter().filter_map(|x| if let Prim::Text { text, .. } = x { Some(text.clone()) } else { None }).collect::<Vec<_>>();
        assert!(labels(None).contains(&"N  FLOOR".to_string()) && labels(None).contains(&"M  WALL".to_string()));
        let fills = |h| draw(h, BuildSlot::Floor, &p).iter().filter(|x| matches!(x, Prim::Ring { fill: true, .. })).count();
        assert_eq!(fills(None), 0);
        assert_eq!(fills(Some(BuildSlot::Wall)), 1);
    }
}
