//! Star-energy readout, low-guard colours and the contextual hint line. Pure data in, draw list out (tested).
//! `HudBridge` owns a `StarFx` and a `HintState` and fills `HudState::star` / `HudState::hint` from them.

use crate::hud::{Align, HudState, Palette, Prim, Rgba};

/// Guard energy fraction under which the guard ring / bar pulse red.
pub const GUARD_LOW: f32 = 0.25;
pub const LOW_RED: Rgba = Rgba(1.0, 0.12, 0.15, 1.0);
/// Seconds the refill flash lasts.
pub const REFILL_FLASH: f32 = 0.5;
/// Seconds the refused-throw shake lasts.
pub const REFUSE_FLASH: f32 = 0.35;
/// A hint is shown at most this many times (counted across runs, stored in the settings).
pub const HINT_MAX_SHOWS: u8 = 3;
/// Seconds one hint stays up.
pub const HINT_SECS: f32 = 3.5;
const PIPS: usize = 6;
/// Arc radius (outside the crosshair ring) and pip radius, screen fractions.
const PIP_R: f32 = 0.105;
const PIP_SIZE: f32 = 0.0075;

/// What the HUD needs to draw the star pool.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct StarHud {
    /// Charges now (fractional while one is recharging), pool size, locked out (needs 2 back).
    pub charges: f32,
    pub max: f32,
    pub locked: bool,
    /// MARK charge 0..=1 (throw held).
    pub charging: f32,
    /// Seconds since the pool refilled / unlocked (None = no flash).
    pub flash: Option<f32>,
    /// Seconds since a throw was refused.
    pub refused: Option<f32>,
    /// Seconds since the pool last dropped or filled (a full pool dims after a while).
    pub idle: f32,
}

/// Tracks the edges of the predicted pool: refill flash, refusal flash, idle time.
#[derive(Clone, Copy, Debug, Default)]
pub struct StarFx {
    prev: Option<(f32, bool)>,
    flash: Option<f32>,
    refused: Option<f32>,
    idle: f32,
}

impl StarFx {
    /// Once per frame with the client's predicted pool (`RenderView::star`).
    pub fn update(&mut self, dt: f32, star: Option<(f32, f32, bool)>, charging: f32, refused: bool) -> Option<StarHud> {
        let age = |a: &mut Option<f32>, lim: f32| *a = a.map(|t| t + dt).filter(|t| *t < lim);
        age(&mut self.flash, REFILL_FLASH);
        age(&mut self.refused, REFUSE_FLASH);
        self.idle += dt;
        if refused {
            self.refused = Some(0.0);
        }
        let Some((c, max, locked)) = star else {
            self.prev = None;
            return None;
        };
        if let Some((pc, plocked)) = self.prev {
            let full = |v: f32| v + 0.01 >= max;
            let filled = full(c) && !full(pc);
            if filled || (plocked && !locked) {
                self.flash = Some(0.0);
            }
            // regeneration is a steady climb: only a drop (a throw) or the pool reaching full counts as activity
            if c < pc - 0.001 || filled {
                self.idle = 0.0;
            }
            if !full(c) {
                self.idle = self.idle.min(1.0);
            }
        }
        self.prev = Some((c, locked));
        Some(StarHud { charges: c, max, locked, charging, flash: self.flash, refused: self.refused, idle: self.idle })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HintId {
    OutOfStars,
    ShieldLow,
    /// Placeholder: the heavy-cut agent calls `HudBridge::hint(HintId::HeavyCut)`.
    HeavyCut,
    /// Placeholder: the grapple agent calls `HudBridge::hint(HintId::HookYank)`.
    HookYank,
}

pub const HINT_COUNT: usize = 4;

impl HintId {
    pub const ALL: [HintId; HINT_COUNT] = [HintId::OutOfStars, HintId::ShieldLow, HintId::HeavyCut, HintId::HookYank];
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn text(self) -> &'static str {
        match self {
            HintId::OutOfStars => "OUT OF STARS - WAIT FOR 2 TO RECHARGE",
            HintId::ShieldLow => "SHIELD LOW - LOWER IT TO RECOVER",
            HintId::HeavyCut => "HOLD KATANA FOR A HEAVY CUT",
            HintId::HookYank => "HOOK A TARGET AND PULL IN TO YANK",
        }
    }
}

/// Which hints were shown how many times, and the one on screen now.
#[derive(Clone, Copy, Debug, Default)]
pub struct HintState {
    counts: [u8; HINT_COUNT],
    active: Option<(HintId, f32)>,
}

impl HintState {
    pub fn with_counts(counts: [u8; HINT_COUNT]) -> Self {
        Self { counts, active: None }
    }
    pub fn counts(&self) -> [u8; HINT_COUNT] {
        self.counts
    }
    /// Adopt saved counts (keeps the larger so a live count never goes backwards).
    pub fn load(&mut self, saved: [u8; HINT_COUNT]) {
        for (c, s) in self.counts.iter_mut().zip(saved) {
            *c = (*c).max(s);
        }
    }
    /// Ask to show `id`. Shown only if it was seen fewer than `HINT_MAX_SHOWS` times and nothing else is up.
    /// Re-triggering the hint that is already on screen keeps it up without counting again.
    pub fn trigger(&mut self, id: HintId) -> bool {
        if let Some((a, _)) = self.active {
            if a == id {
                self.active = Some((id, 0.0));
            }
            return false;
        }
        let c = &mut self.counts[id.index()];
        if *c >= HINT_MAX_SHOWS {
            return false;
        }
        *c += 1;
        self.active = Some((id, 0.0));
        true
    }
    pub fn tick(&mut self, dt: f32) {
        self.active = self.active.map(|(i, t)| (i, t + dt)).filter(|(_, t)| *t < HINT_SECS);
    }
    /// The hint to draw: id and seconds since it appeared.
    pub fn current(&self) -> Option<(HintId, f32)> {
        self.active
    }
}

/// Pip centre `i` of `n` on the arc right of the crosshair.
fn pip_pos(i: usize, n: usize, ax: f32) -> (f32, f32) {
    let span = 1.3f32; // radians, centred on 3 o'clock
    let a = if n > 1 { -span * 0.5 + span * i as f32 / (n - 1) as f32 } else { 0.0 };
    let r = PIP_R;
    (0.5 + r * a.cos() * ax, 0.5 + r * a.sin())
}

/// Star pips right of the crosshair, the MARK charge arc, and the contextual hint under the crosshair.
pub fn draw(out: &mut Vec<Prim>, s: &HudState, p: &Palette, ax: f32) {
    if s.dead {
        return;
    }
    if let Some(st) = &s.star {
        star_pips(out, st, p, ax);
    }
    if let Some((id, t)) = s.hint {
        let fade = ((HINT_SECS - t) / 0.5).clamp(0.0, 1.0) * (t / 0.2).clamp(0.0, 1.0);
        out.push(Prim::Text { pos: (0.5, 0.5 + 0.17), text: id.text().into(), size: 0.02, color: p.gold.a(0.95 * fade), align: Align::Center });
    }
}

fn star_pips(out: &mut Vec<Prim>, st: &StarHud, p: &Palette, ax: f32) {
    let n = (st.max.round() as usize).clamp(1, PIPS);
    let idle_full = st.charges + 0.01 >= st.max && st.charging <= 0.0 && st.idle > 1.5 && st.flash.is_none();
    let dim = if idle_full { 0.5 } else { 1.0 };
    let base = if st.locked { p.magenta } else { p.gold };
    let shake = st.refused.map_or(0.0, |t| (1.0 - t / REFUSE_FLASH) * 0.004 * (t * 70.0).sin());
    for i in 0..n {
        let (x, y) = pip_pos(i, n, ax);
        let c = (x + shake, y);
        let f = (st.charges - i as f32).clamp(0.0, 1.0);
        // the 2nd charge is the unlock threshold: mark it while locked out
        let marker = st.locked && i == 1;
        out.push(Prim::Ring { c, r: PIP_SIZE, color: if marker { p.magenta.a(0.9) } else { base.a(0.3 * dim) }, fill: false, glow: if marker { 0.006 } else { 0.0 } });
        if f > 0.0 {
            let col = if st.refused.is_some() { LOW_RED } else { base };
            let a = (if st.locked { 0.4 } else { 0.95 }) * dim * (0.4 + 0.6 * f);
            out.push(Prim::Ring { c, r: PIP_SIZE * (0.35 + 0.65 * f), color: col.a(a), fill: true, glow: if st.locked { 0.0 } else { 0.004 } });
        }
    }
    if let Some(t) = st.flash {
        let u = (t / REFILL_FLASH).clamp(0.0, 1.0);
        out.push(Prim::Ring { c: (0.5 + PIP_R * ax, 0.5), r: 0.03 + 0.05 * u, color: p.cyan.a(0.8 * (1.0 - u)), fill: false, glow: 0.008 });
    }
    if st.locked {
        let k = 0.6 + 0.4 * (st.idle * 6.0).sin().abs();
        let (x, y) = pip_pos(n - 1, n, ax);
        out.push(Prim::Text { pos: (x + 0.018 * ax, y + 0.006), text: "RECHARGE".into(), size: 0.012, color: p.magenta.a(0.7 * k), align: Align::Left });
    }
    if st.charging > 0.02 {
        // MARK charge: a ring of segments closing around the crosshair
        let segs = 16;
        let on = (st.charging.clamp(0.0, 1.0) * segs as f32).ceil() as usize;
        for k in 0..on.min(segs) {
            let q = std::f32::consts::TAU / segs as f32;
            let (a0, a1) = (k as f32 * q - 1.5708, (k as f32 + 0.7) * q - 1.5708);
            let r = 0.036;
            out.push(Prim::Line { a: (0.5 + r * a0.cos() * ax, 0.5 + r * a0.sin()), b: (0.5 + r * a1.cos() * ax, 0.5 + r * a1.sin()), color: p.orange.a(0.9), glow: 0.005 });
        }
    }
}

/// 0..1 pulse for the low-guard warning (red under `GUARD_LOW`).
pub fn guard_pulse(time: f32) -> f32 {
    0.5 + 0.5 * (time * 14.0).sin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_show_at_most_n_times_and_one_at_a_time() {
        let mut h = HintState::default();
        for _ in 0..HINT_MAX_SHOWS {
            assert!(h.trigger(HintId::OutOfStars));
            assert!(!h.trigger(HintId::ShieldLow), "one hint at a time");
            h.tick(HINT_SECS + 0.1);
            assert!(h.current().is_none());
        }
        assert!(!h.trigger(HintId::OutOfStars), "shown N times already");
        assert!(h.trigger(HintId::ShieldLow));
        assert_eq!(h.counts(), [HINT_MAX_SHOWS, 1, 0, 0]);
    }

    #[test]
    fn retriggering_the_live_hint_keeps_it_up_without_counting() {
        let mut h = HintState::default();
        assert!(h.trigger(HintId::HeavyCut));
        h.tick(3.0);
        assert!(!h.trigger(HintId::HeavyCut));
        assert_eq!(h.current().unwrap().1, 0.0);
        assert_eq!(h.counts()[HintId::HeavyCut.index()], 1);
    }

    #[test]
    fn refill_flashes_and_a_full_idle_pool_dims() {
        let mut fx = StarFx::default();
        let s = fx.update(0.016, Some((5.5, 6.0, false)), 0.0, false).unwrap();
        assert!(s.flash.is_none());
        let s = fx.update(0.016, Some((6.0, 6.0, false)), 0.0, false).unwrap();
        assert_eq!(s.flash, Some(0.0));
        let s = fx.update(2.0, Some((6.0, 6.0, false)), 0.0, false).unwrap();
        assert!(s.flash.is_none() && s.idle > 1.5);
        let s = fx.update(0.016, Some((2.0, 6.0, true)), 0.0, true).unwrap();
        assert!(s.refused.is_some() && s.locked);
        let s = fx.update(0.016, Some((2.0, 6.0, false)), 0.0, false).unwrap();
        assert!(s.flash.is_some(), "unlock flashes");
        assert!(fx.update(0.016, None, 0.0, false).is_none());
    }

    #[test]
    fn pips_one_pair_per_charge_and_fewer_prims_when_empty() {
        let p = Palette::TRON;
        let n = |c: f32, locked| {
            let mut out = Vec::new();
            let s = HudState { star: Some(StarHud { charges: c, max: 6.0, locked, ..Default::default() }), ..Default::default() };
            draw(&mut out, &s, &p, 1.0);
            out.len()
        };
        assert_eq!(n(6.0, false), 12);
        assert!(n(0.0, true) < 12 && n(0.0, true) >= 6);
        assert!(n(3.5, false) > n(0.0, false));
    }
}
