//! HUD of the power weapons (docs/powerups.md), in its own file so the energy pips stay untouched: the draw ring around the
//! crosshair with the arrow count and carry timer, the fuda tag readout, the "TAG ON YOU" countdown, the faint
//! "someone is aiming a bow at you" frame, and the pickup call-outs. Pure data in, draw list out (tested).
//! `HudBridge` owns a `PowerHudFx` and fills `HudState::power` from it.

use crate::client_game::RenderView;
use crate::hud::{Align, HudState, Palette, Prim, Rgba};
use crate::power_view::{PowerView, PowerWorld};
use shinobi_sim::events::NetEvent;
use shinobi_sim::power::PowerKind;
use shinobi_sim::power_pass::{FX_PICKUP_TAG, FX_PICKUP_YUMI};

/// Seconds a call-out stays on screen.
pub const CALLOUT_SECS: f32 = 4.5;
/// Radius of the draw ring (screen-height fraction, outside the MARK ring at 0.036 and inside the star pips at 0.105).
const RING_R: f32 = 0.062;
const SEGS: usize = 28;
/// A bow drawn within this many metres of our head, aimed at us, glints on our screen.
pub const GLINT_RADIUS: f32 = 2.2;

#[derive(Clone, Debug, PartialEq)]
pub struct Callout {
    pub text: String,
    pub age: f32,
    /// 0 = bow, 1 = tags; own pickups are drawn brighter.
    pub kind: u8,
    pub mine: bool,
}

/// A fuda tag stuck to us.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TagOnMe {
    /// Seconds until it goes off.
    pub secs: f32,
    pub armed: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PowerHud {
    pub view: Option<PowerView>,
    pub tag_on_me: Option<TagOnMe>,
    /// 0..=1: how squarely a drawn bow of someone else points at us.
    pub glint: f32,
    pub callouts: Vec<Callout>,
    pub time: f32,
}

/// Keeps the call-outs and the clocks between frames.
#[derive(Clone, Debug, Default)]
pub struct PowerHudFx {
    callouts: Vec<Callout>,
    time: f32,
}

/// Distance from `p` to the ray `o + t * d` for t >= 0 (`d` unit), or None when `p` is behind the origin.
fn ray_miss(o: [f32; 3], d: [f32; 3], p: [f32; 3]) -> Option<f32> {
    let v = [p[0] - o[0], p[1] - o[1], p[2] - o[2]];
    let t = v[0] * d[0] + v[1] * d[1] + v[2] * d[2];
    if t < 0.0 {
        return None;
    }
    let c = [v[0] - d[0] * t, v[1] - d[1] * t, v[2] - d[2] * t];
    Some((c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt())
}

/// How strongly the bows of others point at a head at `eye`: 0..=1.
pub fn glint_at(world: &PowerWorld, me: u16, eye: [f32; 3]) -> f32 {
    world
        .draws
        .iter()
        .filter(|d| d.owner != me)
        .filter_map(|d| ray_miss(d.pos, d.dir, eye).map(|m| (1.0 - m / GLINT_RADIUS).clamp(0.0, 1.0) * (0.35 + 0.65 * d.frac)))
        .fold(0.0, f32::max)
}

impl PowerHudFx {
    /// Once per rendered frame.
    pub fn update(&mut self, dt: f32, view: &RenderView, local_id: u16, name: impl Fn(u16) -> String) -> Option<PowerHud> {
        self.time += dt;
        for c in &mut self.callouts {
            c.age += dt;
        }
        self.callouts.retain(|c| c.age < CALLOUT_SECS);
        for e in &view.events {
            if let NetEvent::PowerFx { what, owner, .. } = e {
                let (kind, text) = match *what {
                    FX_PICKUP_YUMI => (0, "THE YUMI"),
                    FX_PICKUP_TAG => (1, "FUDA TAGS"),
                    _ => continue,
                };
                let mine = *owner == local_id;
                let line = if mine {
                    if kind == 0 { "YUMI: HOLD RIGHT CLICK TO DRAW, RELEASE TO LOOSE".to_string() } else { "FUDA TAGS: RIGHT CLICK THROWS, AGAIN DETONATES".to_string() }
                } else {
                    format!("{} TOOK {}", name(*owner), text)
                };
                self.callouts.push(Callout { text: line, age: 0.0, kind, mine });
                if self.callouts.len() > 3 {
                    self.callouts.remove(0);
                }
            }
        }
        let tag_on_me = view.power_world.tag_on(local_id).map(|t| TagOnMe { secs: t.fuse_left * shinobi_sim::power::TagCfg::default().fuse_s, armed: t.armed });
        let glint = if view.death.is_some() { 0.0 } else { glint_at(&view.power_world, local_id, view.eye) };
        if view.power.is_none() && tag_on_me.is_none() && glint <= 0.0 && self.callouts.is_empty() {
            return None;
        }
        Some(PowerHud { view: view.power, tag_on_me, glint, callouts: self.callouts.clone(), time: self.time })
    }
}

fn arc(out: &mut Vec<Prim>, ax: f32, r: f32, from: f32, to: f32, segs: usize, color: Rgba, glow: f32) {
    for k in 0..segs {
        let (a0, a1) = (from + (to - from) * k as f32 / segs as f32, from + (to - from) * (k as f32 + 0.8) / segs as f32);
        out.push(Prim::Line { a: (0.5 + r * a0.cos() * ax, 0.5 + r * a0.sin()), b: (0.5 + r * a1.cos() * ax, 0.5 + r * a1.sin()), color, glow });
    }
}

/// Tint of a weapon.
pub fn kind_colour(k: PowerKind, p: &Palette) -> Rgba {
    match k {
        PowerKind::Yumi => p.gold,
        PowerKind::Tag => p.orange,
        PowerKind::None => p.cyan,
    }
}

/// Everything of the power HUD.
pub fn draw(out: &mut Vec<Prim>, s: &HudState, p: &Palette, ax: f32) {
    let Some(h) = &s.power else { return };
    if s.dead {
        return;
    }
    // call-outs, top centre under the match banner
    for (i, c) in h.callouts.iter().enumerate() {
        let fade = ((CALLOUT_SECS - c.age) / 0.6).clamp(0.0, 1.0) * (c.age / 0.15).clamp(0.0, 1.0);
        let col = if c.kind == 0 { p.gold } else { p.orange };
        out.push(Prim::Text { pos: (0.5, 0.13 + 0.03 * i as f32), text: c.text.clone(), size: if c.mine { 0.016 } else { 0.015 }, color: col.a((if c.mine { 1.0 } else { 0.85 }) * fade), align: Align::Center });
    }
    // a bow drawn on us: a faint frame + a small note
    if h.glint > 0.02 {
        let pulse = 0.6 + 0.4 * (h.time * 12.0).sin().abs();
        let a = (0.18 + 0.5 * h.glint) * pulse;
        let col = p.magenta.a(a);
        for (x0, y0, x1, y1) in [(0.02, 0.02, 0.12, 0.02), (0.02, 0.02, 0.02, 0.12), (0.98, 0.02, 0.88, 0.02), (0.98, 0.02, 0.98, 0.12), (0.02, 0.98, 0.12, 0.98), (0.02, 0.98, 0.02, 0.88), (0.98, 0.98, 0.88, 0.98), (0.98, 0.98, 0.98, 0.88)] {
            out.push(Prim::Line { a: (x0, y0), b: (x1, y1), color: col, glow: 0.004 });
        }
        if h.glint > 0.45 {
            out.push(Prim::Text { pos: (0.5, 0.22), text: "BOW DRAWN ON YOU".into(), size: 0.014, color: p.magenta.a(a), align: Align::Center });
        }
    }
    // a tag stuck to us
    if let Some(t) = h.tag_on_me {
        let blink = if t.secs < 0.8 { 0.5 + 0.5 * (h.time * 24.0).sin().abs() } else { 0.7 + 0.3 * (h.time * 8.0).sin().abs() };
        let col = Rgba(1.0, 0.12, 0.15, 1.0).a(blink);
        out.push(Prim::Text { pos: (0.5, 0.27), text: format!("TAG ON YOU {:.1}", t.secs.max(0.0)), size: 0.026, color: col, align: Align::Center });
        out.push(Prim::Text { pos: (0.5, 0.30), text: if t.armed { "ARMED - CUT IT OFF WITH A SWING".into() } else { "ARMING - CUT IT OFF WITH A SWING".into() }, size: 0.0125, color: p.text.a(0.8), align: Align::Center });
        let u = (t.secs / shinobi_sim::power::TagCfg::default().fuse_s).clamp(0.0, 1.0);
        arc(out, ax, 0.05, -std::f32::consts::FRAC_PI_2, -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * u, SEGS, col, 0.006);
    }
    let Some(v) = &h.view else { return };
    let col = kind_colour(v.kind, p);
    match v.kind {
        PowerKind::Yumi => {
            // the draw ring closes clockwise from 12 o'clock; full = bright gold and a second ring
            let q = std::f32::consts::TAU;
            let top = -std::f32::consts::FRAC_PI_2;
            if v.draw > 0.0 {
                let c = if v.full { Rgba(1.0, 0.95, 0.55, 1.0) } else { col };
                arc(out, ax, RING_R, top, top + q * v.draw, SEGS, c.a(if v.full { 1.0 } else { 0.9 }), if v.full { 0.008 } else { 0.004 });
                if v.full {
                    out.push(Prim::Ring { c: (0.5, 0.5), r: RING_R + 0.012, color: c.a(0.5 + 0.4 * (h.time * 14.0).sin().abs()), fill: false, glow: 0.006 });
                    out.push(Prim::Text { pos: (0.5, 0.5 + RING_R + 0.045), text: "RELEASE".into(), size: 0.014, color: c.a(0.95), align: Align::Center });
                }
            } else {
                arc(out, ax, RING_R, top, top + q, SEGS, col.a(0.16), 0.0);
            }
            // arrows left of the crosshair, mirror of the star pips
            let n = v.ammo_max.max(1) as usize;
            for i in 0..n {
                let a = std::f32::consts::PI + (i as f32 - (n as f32 - 1.0) * 0.5) * 0.28;
                let c = (0.5 + 0.105 * a.cos() * ax, 0.5 + 0.105 * a.sin());
                let full = i < v.ammo as usize;
                out.push(Prim::Line { a: (c.0 - 0.009 * ax, c.1 + 0.006), b: (c.0 + 0.009 * ax, c.1 - 0.006), color: col.a(if full { 0.95 } else { 0.22 }), glow: if full { 0.003 } else { 0.0 } });
                out.push(Prim::Ring { c: (c.0 + 0.009 * ax, c.1 - 0.006), r: 0.0035, color: col.a(if full { 0.95 } else { 0.22 }), fill: full, glow: 0.0 });
            }
            out.push(Prim::Text { pos: (0.5 - 0.125 * ax, 0.5 + 0.03), text: format!("YUMI {}", v.ammo), size: 0.014, color: col.a(0.95), align: Align::Right });
            carry_bar(out, v, col, ax);
        }
        PowerKind::Tag => {
            let n = v.ammo_max.max(1) as usize;
            for i in 0..n {
                let a = std::f32::consts::PI + (i as f32 - (n as f32 - 1.0) * 0.5) * 0.3;
                let c = (0.5 + 0.105 * a.cos() * ax, 0.5 + 0.105 * a.sin());
                let full = i < v.ammo as usize;
                // a paper tag: a small rect with a glyph stroke
                out.push(Prim::Rect { r: crate::hud::Rect { x: c.0 - 0.005 * ax, y: c.1 - 0.009, w: 0.010 * ax, h: 0.018 }, color: col.a(if full { 0.85 } else { 0.18 }), glow: if full { 0.003 } else { 0.0 } });
            }
            let label = if v.ammo > 0 { format!("FUDA {}", v.ammo) } else if v.live_tags > 0 { "DETONATE".to_string() } else { "FUDA".to_string() };
            out.push(Prim::Text { pos: (0.5 - 0.125 * ax, 0.5 + 0.03), text: label, size: 0.014, color: col.a(0.95), align: Align::Right });
            if v.live_tags > 0 {
                let armed = v.armed_tags > 0;
                let k = if armed && v.ammo == 0 { 0.6 + 0.4 * (h.time * 10.0).sin().abs() } else { 0.7 };
                out.push(Prim::Text { pos: (0.5 - 0.125 * ax, 0.5 + 0.052), text: format!("{} LIVE{}", v.live_tags, if armed && v.ammo == 0 { " - CLICK TO BLOW" } else { "" }), size: 0.012, color: p.text.a(k), align: Align::Right });
            }
            if v.ammo > 0 {
                carry_bar(out, v, col, ax);
            }
        }
        PowerKind::None => {}
    }
}

/// The thin carry-time bar under the label; it flashes in its last five seconds.
fn carry_bar(out: &mut Vec<Prim>, v: &PowerView, col: Rgba, ax: f32) {
    let (x1, y) = (0.5 - 0.125 * ax, 0.5 + 0.038);
    let w = 0.07 * ax;
    out.push(Prim::Line { a: (x1 - w, y), b: (x1, y), color: col.a(0.2), glow: 0.0 });
    let low = v.left_secs < 5.0;
    let k = if low { 0.6 + 0.4 * (v.left_secs * 9.0).sin().abs() } else { 1.0 };
    out.push(Prim::Line { a: (x1 - w * v.left.clamp(0.0, 1.0), y), b: (x1, y), color: (if low { Rgba(1.0, 0.2, 0.2, 1.0) } else { col }).a(0.9 * k), glow: 0.002 });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::power_view::DrawObj;
    use shinobi_sim::power::{PowerCfg, PowerState};

    fn state(view: PowerView) -> HudState {
        HudState { power: Some(PowerHud { view: Some(view), time: 0.3, ..Default::default() }), ..Default::default() }
    }

    fn yumi_view(draw: f32, ammo: u8) -> PowerView {
        let cfg = PowerCfg::default();
        let mut s = PowerState::default();
        s.grant(PowerKind::Yumi, ammo, 0.0, &cfg);
        let mut v = PowerView::of(&s, &cfg, 0, 0).unwrap();
        v.draw = draw;
        v.full = draw >= 1.0;
        v
    }

    fn texts(out: &[Prim]) -> Vec<String> {
        out.iter().filter_map(|p| if let Prim::Text { text, .. } = p { Some(text.clone()) } else { None }).collect()
    }

    #[test]
    fn the_draw_ring_grows_with_the_draw_and_says_release_when_full() {
        let p = Palette::TRON;
        let lines = |d: f32| {
            let mut out = Vec::new();
            draw(&mut out, &state(yumi_view(d, 3)), &p, 1.0);
            (out.iter().filter(|x| matches!(x, Prim::Line { .. })).count(), texts(&out))
        };
        let (idle, t0) = lines(0.0);
        let (half, t1) = lines(0.5);
        let (full, t2) = lines(1.0);
        assert!(half >= idle && full > 0, "{idle} {half} {full}");
        assert!(!t0.iter().any(|t| t == "RELEASE") && !t1.iter().any(|t| t == "RELEASE"));
        assert!(t2.iter().any(|t| t == "RELEASE"));
        assert!(t0.iter().any(|t| t == "YUMI 3"), "{t0:?}");
    }

    #[test]
    fn fewer_arrows_dim_the_pips_and_the_label_follows() {
        let p = Palette::TRON;
        let mut out = Vec::new();
        draw(&mut out, &state(yumi_view(0.0, 1)), &p, 1.0);
        assert!(texts(&out).iter().any(|t| t == "YUMI 1"));
        let bright = out.iter().filter(|x| matches!(x, Prim::Line { color, .. } if color.3 > 0.9)).count();
        let mut out3 = Vec::new();
        draw(&mut out3, &state(yumi_view(0.0, 3)), &p, 1.0);
        let bright3 = out3.iter().filter(|x| matches!(x, Prim::Line { color, .. } if color.3 > 0.9)).count();
        assert!(bright3 > bright);
    }

    #[test]
    fn a_tag_on_you_counts_down_and_a_spent_bag_offers_the_detonate_prompt() {
        let p = Palette::TRON;
        let s = HudState { power: Some(PowerHud { tag_on_me: Some(TagOnMe { secs: 1.2, armed: true }), ..Default::default() }), ..Default::default() };
        let mut out = Vec::new();
        draw(&mut out, &s, &p, 1.0);
        assert!(texts(&out).iter().any(|t| t == "TAG ON YOU 1.2"), "{:?}", texts(&out));
        assert!(texts(&out).iter().any(|t| t.starts_with("ARMED")));
        let cfg = PowerCfg::default();
        let mut v = PowerView::of(&PowerState::default(), &cfg, 1, 1).unwrap();
        v.kind = PowerKind::Tag;
        v.ammo = 0;
        v.ammo_max = 2;
        let mut out = Vec::new();
        draw(&mut out, &state(v), &p, 1.0);
        let t = texts(&out);
        assert!(t.iter().any(|x| x == "DETONATE") && t.iter().any(|x| x.contains("CLICK TO BLOW")), "{t:?}");
    }

    #[test]
    fn a_bow_aimed_at_your_head_glints_one_aimed_past_you_does_not() {
        let world = PowerWorld { draws: vec![DrawObj { pos: [0.0, 1.5, 0.0], dir: [0.0, 0.0, -1.0], owner: 5, frac: 1.0 }], ..Default::default() };
        let on = glint_at(&world, 1, [0.0, 1.5, -30.0]);
        let off = glint_at(&world, 1, [6.0, 1.5, -30.0]);
        let behind = glint_at(&world, 1, [0.0, 1.5, 30.0]);
        assert!(on > 0.9 && off == 0.0 && behind == 0.0, "{on} {off} {behind}");
        assert_eq!(glint_at(&world, 5, [0.0, 1.5, -30.0]), 0.0, "your own bow never glints at you");
        let s = HudState { power: Some(PowerHud { glint: on, time: 0.1, ..Default::default() }), ..Default::default() };
        let mut out = Vec::new();
        draw(&mut out, &s, &Palette::TRON, 1.0);
        assert!(texts(&out).iter().any(|t| t == "BOW DRAWN ON YOU"));
    }

    #[test]
    fn callouts_name_who_took_the_weapon_and_fade() {
        let p = Palette::TRON;
        let s = HudState {
            power: Some(PowerHud {
                callouts: vec![Callout { text: "ECHO FOX TOOK THE YUMI".into(), age: 0.5, kind: 0, mine: false }, Callout { text: "FUDA TAGS: RIGHT CLICK THROWS".into(), age: CALLOUT_SECS - 0.1, kind: 1, mine: true }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut out = Vec::new();
        draw(&mut out, &s, &p, 1.0);
        assert_eq!(texts(&out).len(), 2);
        let alphas: Vec<f32> = out.iter().filter_map(|x| if let Prim::Text { color, .. } = x { Some(color.3) } else { None }).collect();
        assert!(alphas[0] > 0.8 && alphas[1] < 0.2, "{alphas:?}");
    }
}
