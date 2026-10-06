//! "First 30 seconds" controls card: the keys a new player needs, shown at the left edge, each row fading out after its
//! first use, the whole card after `LIFETIME` seconds of play. Pure state (tested); `ClientGame` feeds it, `hud` draws it.

use crate::hud::{Align, Palette, Prim};

/// Seconds of play after which the whole card has faded out.
pub const LIFETIME: f32 = 30.0;
const FADE: f32 = 1.5;
const ROW_FADE: f32 = 0.6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Use {
    Move,
    Jump,
    Dash,
    Slide,
    Grapple,
    Build,
    Katana,
    Shuriken,
    Block,
}

const ROWS: [(Use, &str, &str); 9] = [
    (Use::Move, "WASD", "MOVE"),
    (Use::Jump, "SPACE", "JUMP"),
    (Use::Dash, "SHIFT", "DASH"),
    (Use::Slide, "C", "SLIDE"),
    (Use::Grapple, "E", "GRAPPLE"),
    (Use::Build, "N M", "BUILD"),
    (Use::Katana, "LMB", "KATANA"),
    (Use::Shuriken, "RMB", "THROW + LOCK"),
    (Use::Block, "CTRL / F", "BLOCK + PARRY"),
];

/// One drawn row: key text, action text, opacity 0..1.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlRow {
    pub key: &'static str,
    pub label: &'static str,
    pub alpha: f32,
}

#[derive(Clone, Debug, Default)]
pub struct ControlsCard {
    /// Seconds of play so far (counted only while in the game).
    played: f32,
    /// Seconds since each row's first use (None = not used yet).
    used: [Option<f32>; 9],
    /// F1: show every row at full strength until toggled off again.
    pinned: bool,
}

impl ControlsCard {
    pub fn mark(&mut self, u: Use) {
        let i = ROWS.iter().position(|r| r.0 == u).unwrap();
        if self.used[i].is_none() {
            self.used[i] = Some(0.0);
        }
    }

    /// Call once per sim tick while the player is in the game.
    pub fn tick(&mut self, dt: f32) {
        self.played += dt;
        for u in self.used.iter_mut().flatten() {
            *u += dt;
        }
    }

    /// F1: bring the strip back (all rows, no fading) or put it away again.
    pub fn toggle_pin(&mut self) {
        self.pinned = !self.pinned;
    }

    pub fn finished(&self) -> bool {
        !self.pinned && (self.played >= LIFETIME + FADE || self.used.iter().all(|u| u.map_or(false, |t| t >= ROW_FADE)))
    }

    /// Rows still worth drawing (alpha > 0), with their opacity.
    pub fn rows(&self) -> Vec<ControlRow> {
        if self.pinned {
            return ROWS.iter().map(|r| ControlRow { key: r.1, label: r.2, alpha: 1.0 }).collect();
        }
        let global = ((LIFETIME + FADE - self.played) / FADE).clamp(0.0, 1.0);
        ROWS.iter()
            .zip(self.used.iter())
            .filter_map(|(r, used)| {
                let row = used.map_or(1.0, |t| (1.0 - t / ROW_FADE).clamp(0.0, 1.0));
                let a = global * row;
                (a > 0.01).then_some(ControlRow { key: r.1, label: r.2, alpha: a })
            })
            .collect()
    }
}

/// Draw list for the strip: a compact two-line block above the chin, centred in the clear band (x 0.26 to 0.74, y 0.75
/// to 0.80), four `KEY ACTION` entries per line. Rows that were used (faded out) drop away and the rest re-flow.
pub fn prims(rows: &[ControlRow], p: &Palette) -> Vec<Prim> {
    let mut out = Vec::new();
    if rows.is_empty() {
        return out;
    }
    let (x0, col_w, y0, dy) = (0.26f32, 0.12f32, 0.76f32, 0.03f32);
    for (i, r) in rows.iter().enumerate() {
        let (col, line) = ((i % 4) as f32, (i / 4) as f32);
        let (x, y) = (x0 + col * col_w, y0 + line * dy);
        let key_w = r.key.chars().count() as f32 * 0.0088 + 0.008;
        out.push(Prim::Text { pos: (x, y), text: r.key.into(), size: 0.016, color: p.gold.a(0.95 * r.alpha), align: Align::Left });
        out.push(Prim::Text { pos: (x + key_w, y), text: r.label.into(), size: 0.015, color: p.text.a(0.8 * r.alpha), align: Align::Left });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_rows_show_at_first_and_the_whole_card_fades_after_30_s() {
        let mut c = ControlsCard::default();
        assert_eq!(c.rows().len(), 9);
        assert!(c.rows().iter().all(|r| r.alpha == 1.0));
        for _ in 0..(29.0 * 60.0) as u32 {
            c.tick(1.0 / 60.0);
        }
        assert_eq!(c.rows().len(), 9, "still up at 29 s");
        for _ in 0..(3.0 * 60.0) as u32 {
            c.tick(1.0 / 60.0);
        }
        assert!(c.rows().is_empty() && c.finished(), "gone after {LIFETIME} s + fade");
    }

    #[test]
    fn a_row_fades_after_its_first_use_and_only_that_row() {
        let mut c = ControlsCard::default();
        c.mark(Use::Grapple);
        for _ in 0..12 {
            c.tick(1.0 / 60.0);
        }
        let rows = c.rows();
        let g = rows.iter().find(|r| r.key == "E").unwrap();
        assert!(g.alpha < 1.0 && g.alpha > 0.0, "fading: {}", g.alpha);
        assert!(rows.iter().filter(|r| r.key != "E").all(|r| r.alpha == 1.0));
        for _ in 0..60 {
            c.tick(1.0 / 60.0);
        }
        assert_eq!(c.rows().len(), 8, "the used row is gone");
        // using it again changes nothing
        c.mark(Use::Grapple);
        assert_eq!(c.rows().len(), 8);
    }

    #[test]
    fn using_every_key_finishes_the_card_early() {
        let mut c = ControlsCard::default();
        for u in [Use::Move, Use::Jump, Use::Dash, Use::Slide, Use::Grapple, Use::Build, Use::Katana, Use::Shuriken, Use::Block] {
            c.mark(u);
        }
        for _ in 0..60 {
            c.tick(1.0 / 60.0);
        }
        assert!(c.finished() && c.rows().is_empty());
    }

    #[test]
    fn the_strip_sits_low_in_the_clear_band_and_f1_brings_it_back() {
        let mut c = ControlsCard::default();
        for pr in prims(&c.rows(), &Palette::TRON) {
            if let Prim::Text { pos, .. } = pr {
                assert!(pos.0 >= 0.26 && pos.0 <= 0.74 && pos.1 > 0.7 && pos.1 < 0.84, "{pos:?}");
            }
        }
        // finish it, then F1 shows everything again at full strength, F1 again hides it
        for u in [Use::Move, Use::Jump, Use::Dash, Use::Slide, Use::Grapple, Use::Build, Use::Katana, Use::Shuriken, Use::Block] {
            c.mark(u);
        }
        for _ in 0..60 {
            c.tick(1.0 / 60.0);
        }
        assert!(c.finished() && c.rows().is_empty());
        c.toggle_pin();
        assert!(!c.finished() && c.rows().len() == 9 && c.rows().iter().all(|r| r.alpha == 1.0));
        c.toggle_pin();
        assert!(c.rows().is_empty());
    }
}
