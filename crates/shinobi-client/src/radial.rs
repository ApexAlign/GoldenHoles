//! Radial build menu (hold N / M): two sectors, FLOOR on the left and WALL on the right, picked with the free
//! cursor. Releasing the key commits the highlighted piece (`ModeOut.radial_commit`); a release inside the
//! dead zone cancels. Pure geometry + draw list.

use crate::hud::{Align, BuildSlot, Palette, Prim};

const C: (f32, f32) = (0.5, 0.5);
const R_OUT: f32 = 0.17;
const R_DEAD: f32 = 0.03;
const ASPECT: f32 = 0.5625;

/// Piece under a normalized cursor position (y down), or `None` inside the dead zone. Three 120 degree sectors,
/// angle measured clockwise from straight up: RAMP on top, WALL lower right, FLOOR lower left.
pub fn pick(cursor: (f32, f32)) -> Option<BuildSlot> {
    // x is corrected to screen-height units so the dead zone is a circle
    let dx = (cursor.0 - C.0) / ASPECT;
    let dy = cursor.1 - C.1;
    if dx * dx + dy * dy < R_DEAD * R_DEAD {
        return None;
    }
    let deg = dx.atan2(-dy).to_degrees().rem_euclid(360.0);
    Some(if deg >= 300.0 || deg < 60.0 {
        BuildSlot::Ramp
    } else if deg < 180.0 {
        BuildSlot::Wall
    } else {
        BuildSlot::Floor
    })
}

/// Sector centre (screen position) and label for a slot.
fn sector(slot: BuildSlot) -> ((f32, f32), &'static str) {
    let (deg, label) = match slot {
        BuildSlot::Ramp => (0.0f32, "N  RAMP"),
        BuildSlot::Wall => (120.0, "M  WALL"),
        BuildSlot::Floor => (240.0, "N  FLOOR"),
    };
    let (s, c) = deg.to_radians().sin_cos();
    ((C.0 + s * R_OUT * 0.58 * ASPECT, C.1 - c * R_OUT * 0.58), label)
}

/// Draw the menu with `hover` highlighted and `current` (the selected piece) marked.
pub fn draw(hover: Option<BuildSlot>, current: BuildSlot, p: &Palette) -> Vec<Prim> {
    let mut out = Vec::new();
    out.push(Prim::Rect { r: crate::hud::Rect::new(0.0, 0.0, 1.0, 1.0), color: p.bg.a(0.35), glow: 0.0 });
    out.push(Prim::Ring { c: C, r: R_OUT, color: p.cyan.a(0.8), fill: false, glow: 0.006 });
    out.push(Prim::Ring { c: C, r: R_DEAD, color: p.cyan.a(0.4), fill: false, glow: 0.0 });
    // sector dividers at 60, 180 and 300 degrees
    for deg in [60.0f32, 180.0, 300.0] {
        let (s, c) = deg.to_radians().sin_cos();
        out.push(Prim::Line {
            a: (C.0 + s * R_DEAD * ASPECT, C.1 - c * R_DEAD),
            b: (C.0 + s * R_OUT * ASPECT, C.1 - c * R_OUT),
            color: p.cyan.a(0.5),
            glow: 0.0,
        });
    }
    for slot in [BuildSlot::Ramp, BuildSlot::Wall, BuildSlot::Floor] {
        let ((cx, cy), label) = sector(slot);
        let on = hover == Some(slot);
        let col = if on { p.gold } else if slot == current { p.cyan } else { p.text };
        if on {
            out.push(Prim::Ring { c: (cx, cy), r: R_OUT * 0.36, color: p.gold.a(0.18), fill: true, glow: 0.01 });
        }
        out.push(Prim::Text { pos: (cx, cy - 0.005), text: label.into(), size: 0.026, color: col.a(if on { 1.0 } else { 0.75 }), align: Align::Center });
        // piece preview under the label: flat slab, tilted slab, upright slab
        let (w, h) = match slot {
            BuildSlot::Floor => (0.05, 0.012),
            BuildSlot::Wall => (0.012, 0.05),
            BuildSlot::Ramp => (0.0, 0.0),
        };
        if slot == BuildSlot::Ramp {
            out.push(Prim::Line { a: (cx - 0.025 * ASPECT * 1.5, cy + 0.045), b: (cx + 0.025 * ASPECT * 1.5, cy + 0.022), color: col.a(0.85), glow: 0.004 });
        } else {
            out.push(Prim::Rect { r: crate::hud::Rect::new(cx - w * 0.5, cy + 0.04 - h * 0.5, w, h), color: col.a(0.8), glow: 0.004 });
        }
    }
    out.push(Prim::Text { pos: (C.0, C.1 + R_OUT + 0.05), text: "RELEASE TO PRINT".into(), size: 0.02, color: p.text.a(0.6), align: Align::Center });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_splits_into_three_sectors_with_a_circular_dead_zone() {
        assert_eq!(pick((0.5, 0.5)), None);
        assert_eq!(pick((0.51, 0.5)), None, "inside the dead zone");
        assert_eq!(pick((0.5, 0.3)), Some(BuildSlot::Ramp), "straight up");
        assert_eq!(pick((0.62, 0.62)), Some(BuildSlot::Wall), "lower right");
        assert_eq!(pick((0.38, 0.62)), Some(BuildSlot::Floor), "lower left");
        assert_eq!(pick((0.7, 0.45)), Some(BuildSlot::Wall), "right and a little up is still wall (angle ~ 80)");
        assert_eq!(pick((0.2, 0.45)), Some(BuildSlot::Floor), "left and a little up is floor (angle ~ 280)");
        assert_eq!(pick((0.62, 0.3)), Some(BuildSlot::Ramp), "up and right within 60 degrees of vertical");
    }

    #[test]
    fn draw_labels_all_three_pieces_and_highlights_the_hover() {
        let p = Palette::TRON;
        let labels = |h| draw(h, BuildSlot::Floor, &p).iter().filter_map(|x| if let Prim::Text { text, .. } = x { Some(text.clone()) } else { None }).collect::<Vec<_>>();
        for want in ["N  FLOOR", "N  RAMP", "M  WALL"] {
            assert!(labels(None).contains(&want.to_string()), "{want}");
        }
        let fills = |h| draw(h, BuildSlot::Floor, &p).iter().filter(|x| matches!(x, Prim::Ring { fill: true, .. })).count();
        assert_eq!(fills(None), 0);
        assert_eq!(fills(Some(BuildSlot::Ramp)), 1);
    }
}
