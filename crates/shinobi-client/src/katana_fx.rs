//! Client cues for the katana moves (docs/melee.md, "Katana moves"): the heavy-cut charge ring on the lock triangle,
//! the air-cut slam shock-wave ring, the whirl arc, the counter flash and the execute flash, plus the contextual hint
//! ids for the controls card / hint system. Pure data in, `Prim`s out; the shell feeds `KatanaFx` from predicted
//! `SlashKind`s, the charge mirror and confirmed hits, the HUD draws `KatanaFx::cue()`.

use crate::hud::{Align, Palette, Prim, Rgba};
use shinobi_sim::combat::SlashKind;
use std::f32::consts::TAU;

/// Seconds each one-shot cue stays on screen.
pub const SHOCK_S: f32 = 0.55;
pub const WHIRL_S: f32 = 0.30;
pub const COUNTER_S: f32 = 0.35;
pub const EXECUTE_S: f32 = 0.50;
pub const COMBO_S: f32 = 0.80;
/// How long after our shield parried a blow a hit of ours counts as the counter cut (`melee.counter_window_s`).
pub const COUNTER_WINDOW_S: f32 = 0.25;

/// Live cue state. `charge` is a level (0 idle .. 1 full); the rest are ages (seconds since the cue started, `None` = off).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct KatanaFx {
    pub charge: f32,
    pub shock: Option<f32>,
    pub whirl: Option<f32>,
    pub counter: Option<f32>,
    pub execute: Option<f32>,
    /// A named combo landed (`KatanaCombo::label`): its age and the callout text.
    pub combo: Option<(f32, &'static str)>,
}

/// What the HUD needs per frame: the state plus where to draw (the lock triangle, else the crosshair).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct KatanaCue {
    pub fx: KatanaFx,
    /// Normalized screen position of the lock triangle (None = no lock: cues centre on the crosshair).
    pub at: Option<(f32, f32)>,
    /// Width / height, to keep arcs round.
    pub aspect: f32,
}

impl KatanaFx {
    /// A predicted or confirmed cut of this kind started now.
    pub fn on_slash(&mut self, kind: SlashKind) {
        match kind {
            SlashKind::Whirl => self.whirl = Some(0.0),
            SlashKind::Counter => self.counter = Some(0.0),
            // the dive flashes the shock ring; the real shock-wave goes off when the slammed victim lands
            SlashKind::Air => self.shock = Some(0.0),
            SlashKind::Heavy | SlashKind::Light => {}
        }
        self.charge = 0.0;
    }

    /// A cut completed a named combo (TWIN FANG, BREAKER, ...): flash its name.
    pub fn on_combo(&mut self, combo: shinobi_sim::combat::KatanaCombo) {
        if let Some(label) = combo.label() {
            self.combo = Some((0.0, label));
        }
    }

    /// A confirmed slash kill that the server marked as a bonus hit is an execute.
    pub fn on_confirmed_hit(&mut self, kill: bool, bonus: bool) {
        if kill && bonus {
            self.execute = Some(0.0);
        }
    }

    pub fn tick(&mut self, dt: f32, charge: f32) {
        self.charge = charge.clamp(0.0, 1.0);
        for (v, life) in [(&mut self.shock, SHOCK_S), (&mut self.whirl, WHIRL_S), (&mut self.counter, COUNTER_S), (&mut self.execute, EXECUTE_S)] {
            *v = v.map(|a| a + dt).filter(|a| *a < life);
        }
        self.combo = self.combo.map(|(a, l)| (a + dt, l)).filter(|(a, _)| *a < COMBO_S);
    }

    pub fn active(&self) -> bool {
        self.charge > 0.0 || self.shock.is_some() || self.whirl.is_some() || self.counter.is_some() || self.execute.is_some() || self.combo.is_some()
    }
}

#[allow(clippy::too_many_arguments)]
fn arc(out: &mut Vec<Prim>, c: (f32, f32), r: f32, aspect: f32, a0: f32, sweep: f32, color: Rgba, glow: f32) {
    let n = ((sweep.abs() / TAU) * 40.0).ceil().max(2.0) as usize;
    let pt = |t: f32| (c.0 + t.cos() * r / aspect.max(0.1), c.1 + t.sin() * r);
    for i in 0..n {
        let (t0, t1) = (a0 + sweep * i as f32 / n as f32, a0 + sweep * (i + 1) as f32 / n as f32);
        out.push(Prim::Line { a: pt(t0), b: pt(t1), color, glow });
    }
}

/// Draw list for the cues; empty when nothing is going on.
pub fn prims(cue: &KatanaCue, p: &Palette) -> Vec<Prim> {
    let mut out = Vec::new();
    let f = &cue.fx;
    let c = cue.at.unwrap_or((0.5, 0.5));
    let aspect = if cue.aspect > 0.0 { cue.aspect } else { 16.0 / 9.0 };
    // heavy charge: a ring around the lock triangle that fills clockwise from the top, gold and pulsing when full
    if f.charge > 0.02 {
        let full = f.charge >= 0.999;
        let col = if full { p.gold } else { p.cyan };
        let r = 0.062;
        out.push(Prim::Ring { c, r, color: col.a(0.18), fill: false, glow: 0.0 });
        arc(&mut out, c, r, aspect, -TAU / 4.0, TAU * f.charge, col.a(0.95), 0.006);
        if full {
            out.push(Prim::Ring { c, r: r + 0.012, color: p.gold.a(0.55), fill: false, glow: 0.008 });
            out.push(Prim::Text { pos: (c.0, c.1 - r - 0.045), text: "HEAVY".into(), size: 0.016, color: p.gold, align: Align::Center });
        }
    }
    // slam shock-wave: expanding rings
    if let Some(a) = f.shock {
        let u = a / SHOCK_S;
        out.push(Prim::Ring { c, r: 0.04 + 0.26 * u.sqrt(), color: p.cyan.a(0.9 * (1.0 - u)), fill: false, glow: 0.01 });
        out.push(Prim::Ring { c, r: 0.02 + 0.18 * u, color: p.magenta.a(0.6 * (1.0 - u)), fill: false, glow: 0.006 });
    }
    // whirl: a sweeping arc that goes all the way round
    if let Some(a) = f.whirl {
        let u = (a / WHIRL_S).clamp(0.0, 1.0);
        arc(&mut out, c, 0.11, aspect, -TAU / 4.0, TAU * u, p.cyan.a(1.0 - 0.6 * u), 0.008);
        arc(&mut out, c, 0.095, aspect, -TAU / 4.0 + 0.4, TAU * u * 0.8, p.magenta.a(0.7 * (1.0 - u)), 0.005);
    }
    // counter flash: a bright gold bar through the target and the word
    if let Some(a) = f.counter {
        let u = (a / COUNTER_S).clamp(0.0, 1.0);
        let w = 0.16 * (1.0 - 0.5 * u);
        out.push(Prim::Line { a: (c.0 - w / aspect, c.1 + w * 0.5), b: (c.0 + w / aspect, c.1 - w * 0.5), color: p.gold.a(1.0 - u), glow: 0.012 });
        out.push(Prim::Text { pos: (c.0, c.1 + 0.11), text: "COUNTER".into(), size: 0.022, color: p.gold.a(1.0 - u), align: Align::Center });
    }
    // execute flash: a magenta wash, a gold ring and the word
    if let Some(a) = f.execute {
        let u = (a / EXECUTE_S).clamp(0.0, 1.0);
        out.push(Prim::Vignette { start: 0.25, max_a: 0.7 * (1.0 - u), tint: p.magenta });
        out.push(Prim::Ring { c, r: 0.03 + 0.12 * u, color: p.gold.a(1.0 - u), fill: false, glow: 0.012 });
        out.push(Prim::Text { pos: (c.0, c.1 + 0.12), text: "EXECUTE".into(), size: 0.026, color: p.magenta.a(1.0 - 0.5 * u), align: Align::Center });
    }
    // named combo: the name pops in large, rises a little and fades (above the counter / execute words)
    if let Some((a, label)) = f.combo {
        let u = (a / COMBO_S).clamp(0.0, 1.0);
        let pop = 1.0 + 0.35 * (1.0 - (u * 6.0).min(1.0));
        out.push(Prim::Text { pos: (c.0, c.1 - 0.13 - 0.03 * u), text: label.into(), size: 0.024 * pop, color: p.gold.a(1.0 - u * u), align: Align::Center });
    }
    out
}

/// Contextual hint ids for the katana moves. The hint system (agent-hud-energy, `hud_energy.rs`) can map these to text
/// and cooldowns; nothing here depends on it.
pub const HINT_HEAVY: &str = "katana.heavy";
pub const HINT_AIR: &str = "katana.air";
pub const HINT_COUNTER: &str = "katana.counter";
pub const HINT_WHIRL: &str = "katana.whirl";
pub const HINT_EXECUTE: &str = "katana.execute";

#[derive(Clone, Copy, Debug, Default)]
pub struct HintCtx {
    /// A counter window is open (a parry just landed).
    pub counter_open: bool,
    /// A locked target is stunned and at low hp.
    pub execute_ready: bool,
    /// Airborne with a locked target in reach below.
    pub airborne_target: bool,
    /// The local player just lowered the shield or is sliding, with someone close.
    pub whirl_ready: bool,
    /// A cut just connected: holding the button now charges the heavy cut.
    pub just_cut: bool,
}

/// Hint ids that apply now, in priority order (the caller shows the first it is allowed to).
pub fn hint_ids(c: &HintCtx) -> Vec<&'static str> {
    let mut v = Vec::new();
    if c.execute_ready {
        v.push(HINT_EXECUTE);
    }
    if c.counter_open {
        v.push(HINT_COUNTER);
    }
    if c.whirl_ready {
        v.push(HINT_WHIRL);
    }
    if c.airborne_target {
        v.push(HINT_AIR);
    }
    if c.just_cut {
        v.push(HINT_HEAVY);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_start_on_their_slash_kind_and_expire() {
        let mut f = KatanaFx::default();
        assert!(!f.active());
        f.on_slash(SlashKind::Whirl);
        assert!(f.whirl.is_some() && f.active());
        f.tick(WHIRL_S + 0.01, 0.0);
        assert!(f.whirl.is_none() && !f.active());
        f.on_confirmed_hit(true, false);
        assert!(f.execute.is_none(), "a plain kill is not an execute");
        f.on_confirmed_hit(true, true);
        assert!(f.execute.is_some());
    }

    #[test]
    fn charge_draws_a_ring_that_grows_with_it() {
        let p = Palette::default();
        let at = |c: f32| prims(&KatanaCue { fx: KatanaFx { charge: c, ..Default::default() }, at: Some((0.5, 0.5)), aspect: 1.78 }, &p).len();
        assert_eq!(at(0.0), 0);
        assert!(at(0.9) > at(0.3), "more of the ring is drawn");
        assert!(prims(&KatanaCue { fx: KatanaFx { charge: 1.0, ..Default::default() }, at: None, aspect: 1.78 }, &p).iter().any(|q| matches!(q, Prim::Text { text, .. } if text == "HEAVY")));
    }

    #[test]
    fn hints_come_in_priority_order() {
        let h = hint_ids(&HintCtx { counter_open: true, just_cut: true, execute_ready: true, ..Default::default() });
        assert_eq!(h, vec![HINT_EXECUTE, HINT_COUNTER, HINT_HEAVY]);
        assert!(hint_ids(&HintCtx::default()).is_empty());
    }
}
