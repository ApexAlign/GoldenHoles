//! "CLICK TO PLAY" start panel with the controls list, shown while the pointer is not locked.
//! Renderer-agnostic `hud::Prim` list in normalized screen space, like `hud` and `screens`.

use crate::hud::{Align, Palette, Prim, Rect};

pub const TITLE: &str = "GOLDEN SHINOBI";

/// Window title for the OS title bar.
pub fn window_title(locked: bool) -> &'static str {
    if locked { "Golden Shinobi" } else { "Golden Shinobi - click the window to play" }
}

const CONTROLS: [(&str, &str); 18] = [
    ("W A S D", "MOVE"),
    ("MOUSE", "AIM"),
    ("SPACE", "JUMP / DOUBLE JUMP"),
    ("HOLD W AT WALL", "WALL RUN"),
    ("SHIFT", "DASH"),
    ("C", "SLIDE"),
    ("E", "GRAPPLE"),
    ("N / M", "BUILD FLOOR OR RAMP / WALL"),
    ("TAB", "SCOREBOARD"),
    ("BACKQUOTE", "CONSOLE"),
    ("F1 / F3", "CONTROLS STRIP / PERF"),
    ("LEFT CLICK", "KATANA SLICE"),
    ("RIGHT CLICK", "THROW SHURIKEN"),
    ("HOLD RIGHT CLICK", "MARK STAR (2 CHARGES, +40% CUT)"),
    ("STAR ENERGY", "6 CHARGES; EMPTY = WAIT FOR 2"),
    ("HOLD CTRL / F / V", "BLOCK, PARRY (VIEW LOCKS)"),
    ("MIDDLE / BACK BUTTON", "BLOCK (MOUSE)"),
    ("ESC", "RELEASE MOUSE"),
];

pub fn click_to_play(p: &Palette, time: f32) -> Vec<Prim> {
    let mut out = Vec::new();
    out.push(Prim::Rect { r: Rect::new(0.0, 0.0, 1.0, 1.0), color: p.bg.a(0.55), glow: 0.0 });
    let r = Rect::new(0.26, 0.08, 0.48, 0.84);
    out.push(Prim::Rect { r, color: p.bg.a(0.85), glow: 0.0 });
    let (x1, y1) = (r.x + r.w, r.y + r.h);
    for (a, b) in [((r.x, r.y), (x1, r.y)), ((x1, r.y), (x1, y1)), ((x1, y1), (r.x, y1)), ((r.x, y1), (r.x, r.y))] {
        out.push(Prim::Line { a, b, color: p.cyan.a(0.85), glow: 0.008 });
    }
    out.push(Prim::Text { pos: (0.5, 0.18), text: TITLE.into(), size: 0.06, color: p.cyan, align: Align::Center });
    let pulse = 0.65 + 0.35 * (time * 3.0).sin().abs();
    out.push(Prim::Text { pos: (0.5, 0.27), text: "CLICK TO PLAY".into(), size: 0.05, color: p.orange.a(pulse), align: Align::Center });
    for (i, (key, what)) in CONTROLS.iter().enumerate() {
        let y = 0.35 + i as f32 * 0.031;
        out.push(Prim::Text { pos: (0.30, y), text: (*key).into(), size: 0.026, color: p.cyan, align: Align::Left });
        out.push(Prim::Text { pos: (0.70, y), text: (*what).into(), size: 0.026, color: p.text, align: Align::Right });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(prims: &[Prim]) -> Vec<String> {
        prims.iter().filter_map(|p| if let Prim::Text { text, .. } = p { Some(text.clone()) } else { None }).collect()
    }

    #[test]
    fn panel_says_click_to_play_and_lists_every_control() {
        let t = texts(&click_to_play(&Palette::TRON, 0.0));
        assert!(t.iter().any(|s| s == "CLICK TO PLAY") && t.iter().any(|s| s == TITLE));
        for must in ["MOVE", "AIM", "DASH", "WALL RUN", "RELEASE MOUSE", "THROW SHURIKEN"] {
            assert!(t.iter().any(|s| s == must), "missing {must}");
        }
    }

    #[test]
    fn panel_stays_inside_the_screen_and_titles_differ() {
        for p in click_to_play(&Palette::TRON, 1.0) {
            if let Prim::Text { pos, .. } = p {
                assert!((0.0..=1.0).contains(&pos.0) && (0.0..=1.0).contains(&pos.1));
            }
        }
        assert_ne!(window_title(true), window_title(false));
        assert!(window_title(false).contains("click"));
    }
}
