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
    /// The bow's focus zoom (`zoom.rs`); the reticle ring, range readout and drop tick draw while it is above ~0.
    pub zoom: Option<crate::zoom::ZoomView>,
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
                    if kind == 0 { "YUMI: HOLD LEFT CLICK TO DRAW, RELEASE TO LOOSE".to_string() } else { "FUDA TAGS: Q THROWS, AGAIN DETONATES".to_string() }
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
        Some(PowerHud { view: view.power, tag_on_me, glint, callouts: self.callouts.clone(), time: self.time, zoom: Some(view.zoom).filter(|z| z.frac > 0.001 && view.death.is_none()) })
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
    if let Some(z) = &h.zoom {
        zoom_reticle(out, z, h.time, p, ax);
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
            let reloading = v.reload < 1.0;
            if !(v.up || v.draw > 0.0 || reloading) {
                return;
            }
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
            } else if v.up {
                arc(out, ax, RING_R, top, top + q, SEGS, col.a(0.16), 0.0);
            }
            out.push(Prim::Text { pos: (0.5 - 0.125 * ax, 0.5 + 0.03), text: if reloading { "YUMI RELOADING".into() } else { "YUMI".into() }, size: 0.014, color: col.a(0.95), align: Align::Right });
            reload_bar(out, v, col, ax);
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

/// Hip-fire ring radius (screen-height fraction) the zoom reticle starts from, and the radius it tightens to at focus.
pub const ZOOM_RING_HIP: f32 = 0.075;
pub const ZOOM_RING_FOCUS: f32 = 0.018;

/// Screen-height offset below the centre of the arrow-drop tick: `tan(drop) / tan(fov / 2)` of the half height.
pub fn drop_tick_offset(z: &crate::zoom::ZoomView) -> f32 {
    (0.5 * z.drop_rad.tan() / (z.fov_deg.to_radians() * 0.5).tan()).clamp(0.0, 0.45)
}

/// The zoom's Tron look: a thin neon ring that tightens as the draw zooms in (and settles still at focus), a small range
/// readout to the aimed point, a drop-compensation tick under the centre and a faint edge vignette. No scope mask.
fn zoom_reticle(out: &mut Vec<Prim>, z: &crate::zoom::ZoomView, time: f32, p: &Palette, ax: f32) {
    let f = z.frac.clamp(0.0, 1.0);
    let col = if z.focus { Rgba(1.0, 0.95, 0.55, 1.0) } else { p.cyan };
    let r = ZOOM_RING_HIP + (ZOOM_RING_FOCUS - ZOOM_RING_HIP) * f;
    // the ring breathes while the aim is still settling and holds dead still at focus
    let sway = if z.focus { 0.0 } else { f * 0.004 };
    let c = (0.5 + sway * (time * 1.7).sin() * ax, 0.5 + sway * (time * 2.3).cos());
    out.push(Prim::Vignette { start: 0.6, max_a: 0.2 * f, tint: p.bg.a(1.0) });
    out.push(Prim::Ring { c, r, color: col.a(0.25 + 0.6 * f), fill: false, glow: if z.focus { 0.005 } else { 0.002 } });
    if z.focus {
        // four hairlines reach in from the ring: the steady-aim cue
        let (a, b) = (r + 0.012, r + 0.032);
        for (dx, dy) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
            out.push(Prim::Line { a: (0.5 + dx * a * ax, 0.5 + dy * a), b: (0.5 + dx * b * ax, 0.5 + dy * b), color: col.a(0.8), glow: 0.003 });
        }
    }
    // range readout to the right of the ring, under the draw-state label height
    let text = match z.range {
        Some(m) => format!("{:.0} M", m),
        None => "--- M".to_string(),
    };
    out.push(Prim::Text { pos: (0.5 + (r + 0.02) * ax, 0.5 + 0.006), text, size: 0.016, color: col.a(0.35 + 0.6 * f), align: Align::Left });
    // arrow drop at that range: a tick below the centre, where the arrow lands when loosed along the crosshair
    let y = 0.5 + drop_tick_offset(z);
    let w = 0.012 * ax;
    out.push(Prim::Line { a: (0.5 - w, y), b: (0.5 + w, y), color: col.a(0.3 + 0.6 * f), glow: 0.002 });
    out.push(Prim::Line { a: (0.5, y), b: (0.5, y + 0.008), color: col.a(0.3 + 0.6 * f), glow: 0.0 });
}

/// The bow's reload bar under the crosshair: a track with a gold fill that grows over the 2.2 s and brightens when ready.
fn reload_bar(out: &mut Vec<Prim>, v: &PowerView, col: Rgba, ax: f32) {
    let (cx, y) = (0.5, 0.5 + RING_R + 0.022);
    let w = 0.06 * ax;
    let ready = v.reload >= 1.0;
    out.push(Prim::Line { a: (cx - w, y), b: (cx + w, y), color: col.a(0.22), glow: 0.0 });
    let fill = v.reload.clamp(0.0, 1.0);
    out.push(Prim::Line { a: (cx - w, y), b: (cx - w + 2.0 * w * fill, y), color: (if ready { Rgba(1.0, 0.95, 0.55, 1.0) } else { col }).a(if ready { 0.5 } else { 0.95 }), glow: if ready { 0.0 } else { 0.003 } });
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

    fn yumi_view(draw: f32, reload: f32) -> PowerView {
        let cfg = PowerCfg::default();
        let mut s = PowerState::default();
        s.cooldown = cfg.yumi.reload_s * (1.0 - reload);
        let mut v = PowerView::of(&s, &cfg, 0, 0).unwrap();
        v.up = true;
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
            draw(&mut out, &state(yumi_view(d, 1.0)), &p, 1.0);
            (out.iter().filter(|x| matches!(x, Prim::Line { .. })).count(), texts(&out))
        };
        let (idle, t0) = lines(0.0);
        let (half, t1) = lines(0.5);
        let (full, t2) = lines(1.0);
        assert!(half >= idle && full > 0, "{idle} {half} {full}");
        assert!(!t0.iter().any(|t| t == "RELEASE") && !t1.iter().any(|t| t == "RELEASE"));
        assert!(t2.iter().any(|t| t == "RELEASE"));
        assert!(t0.iter().any(|t| t == "YUMI"), "{t0:?}");
    }

    fn zoom_hud(z: crate::zoom::ZoomView) -> Vec<Prim> {
        let mut s = state(yumi_view(z.frac, 1.0));
        s.power.as_mut().unwrap().zoom = Some(z);
        let mut out = Vec::new();
        draw(&mut out, &s, &Palette::TRON, 1.0);
        out
    }

    fn zv(frac: f32, range: Option<f32>) -> crate::zoom::ZoomView {
        let cfg = shinobi_sim::power::YumiCfg::default();
        let fov = shinobi_sim::power::zoom_fov_deg(90.0, frac, &cfg);
        crate::zoom::ZoomView { frac, fov_deg: fov, focus: frac >= 1.0, range, drop_rad: shinobi_sim::power::arrow_drop_angle(range.unwrap_or(150.0), &cfg) }
    }

    fn ring_r(out: &[Prim]) -> f32 {
        out.iter().find_map(|x| if let Prim::Ring { r, .. } = x { Some(*r) } else { None }).unwrap()
    }

    #[test]
    fn the_zoom_ring_tightens_with_the_zoom_and_the_readout_names_the_range() {
        let (hip, half, full) = (zoom_hud(zv(0.1, Some(40.0))), zoom_hud(zv(0.5, Some(40.0))), zoom_hud(zv(1.0, Some(142.4))));
        assert!(ring_r(&hip) > ring_r(&half) && ring_r(&half) > ring_r(&full), "{} {} {}", ring_r(&hip), ring_r(&half), ring_r(&full));
        assert!((ring_r(&full) - ZOOM_RING_FOCUS).abs() < 1e-6);
        assert!(texts(&full).iter().any(|t| t == "142 M"), "{:?}", texts(&full));
        assert!(texts(&zoom_hud(zv(0.5, None))).iter().any(|t| t == "--- M"));
        let vig = |o: &[Prim]| o.iter().find_map(|x| if let Prim::Vignette { max_a, .. } = x { Some(*max_a) } else { None }).unwrap();
        assert!(vig(&full) > vig(&half) && vig(&full) <= crate::hud::VIGNETTE_MAX_A, "faint");
        // no zoom: none of it
        let mut s = state(yumi_view(0.0, 1.0));
        s.power.as_mut().unwrap().zoom = None;
        let mut out = Vec::new();
        draw(&mut out, &s, &Palette::TRON, 1.0);
        assert!(!out.iter().any(|x| matches!(x, Prim::Ring { .. } | Prim::Vignette { .. })));
    }

    #[test]
    fn the_drop_tick_sits_below_the_centre_and_sinks_with_range() {
        let near = drop_tick_offset(&zv(1.0, Some(50.0)));
        let far = drop_tick_offset(&zv(1.0, Some(200.0)));
        assert!(near > 0.0 && far > near && far < 0.45, "{near} {far}");
        // 200 m at 30 deg FOV: 4 m/s^2 over 1.82 s = 6.6 m = 1.9 deg = about 0.062 of the screen height
        assert!((far - 0.062).abs() < 0.01, "{far}");
        // the same drop reads bigger on screen the tighter the FOV
        let wide = drop_tick_offset(&crate::zoom::ZoomView { fov_deg: 90.0, ..zv(1.0, Some(200.0)) });
        assert!(far > wide * 2.0);
    }

    #[test]
    fn focus_holds_the_ring_still_and_hip_zoom_sways_it() {
        let c = |out: &[Prim]| out.iter().find_map(|x| if let Prim::Ring { c, .. } = x { Some(*c) } else { None }).unwrap();
        let at = |frac: f32, t: f32| {
            let mut s = state(yumi_view(frac, 1.0));
            s.power.as_mut().unwrap().time = t;
            s.power.as_mut().unwrap().zoom = Some(zv(frac, None));
            let mut out = Vec::new();
            draw(&mut out, &s, &Palette::TRON, 1.0);
            c(&out)
        };
        assert_eq!(at(1.0, 0.3), (0.5, 0.5));
        assert_eq!(at(1.0, 1.9), (0.5, 0.5));
        assert_ne!(at(0.6, 0.3), at(0.6, 1.9));
    }

    #[test]
    fn the_reload_bar_fills_over_the_reload_and_the_label_says_so() {
        let p = Palette::TRON;
        let bar = |reload: f32| {
            let mut out = Vec::new();
            draw(&mut out, &state(yumi_view(0.0, reload)), &p, 1.0);
            let lines: Vec<(f32, f32)> = out.iter().filter_map(|x| if let Prim::Line { a, b, color, .. } = x { if (a.1 - (0.5 + RING_R + 0.022)).abs() < 1e-6 && (color.3 > 0.4) { Some((a.0, b.0)) } else { None } } else { None }).collect();
            (texts(&out), lines)
        };
        let (t, l) = bar(0.25);
        assert!(t.iter().any(|x| x == "YUMI RELOADING"), "{t:?}");
        let (t2, l2) = bar(0.75);
        assert!(t2.iter().any(|x| x == "YUMI RELOADING"));
        let w = |l: &Vec<(f32, f32)>| l.iter().map(|(a, b)| b - a).fold(0.0, f32::max);
        assert!(w(&l2) > w(&l), "the fill grows: {} -> {}", w(&l), w(&l2));
        let (t3, _) = bar(1.0);
        assert!(t3.iter().any(|x| x == "YUMI"), "{t3:?}");
        // a bow that is neither up, drawn nor reloading draws nothing
        let mut v = yumi_view(0.0, 1.0);
        v.up = false;
        let mut out = Vec::new();
        draw(&mut out, &state(v), &p, 1.0);
        assert!(out.is_empty(), "{out:?}");
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
                callouts: vec![Callout { text: "ECHO FOX TOOK THE YUMI".into(), age: 0.5, kind: 0, mine: false }, Callout { text: "FUDA TAGS: Q THROWS".into(), age: CALLOUT_SECS - 0.1, kind: 1, mine: true }],
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
