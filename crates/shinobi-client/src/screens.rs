//! Full-screen states as draw lists: connecting, disconnected, kicked, paused menu, scoreboard.
//! Renderer-agnostic like `hud` (same `Prim`s, normalized coords, `Palette` colours). Each screen is a dimmed
//! Tron panel over the game; the pause menu also exposes hit rects so touch / mouse can click it.

use shinobi_proto::datagram::KickReason;

use crate::settings::{self as cfg, Settings};
use crate::hud::{scoreboard, Align, HudState, Palette, Prim, Rect, ScoreRow};
use crate::netclient::{ConnState, NetClient};

/// Seconds without a snapshot before a connected client shows "connection lost".
pub const SILENCE_TIMEOUT_S: f32 = 4.0;
/// Reasons are user/server text: clip so a long one can't run off the panel.
pub const REASON_MAX_CHARS: usize = 56;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Resume,
    Settings,
    Scoreboard,
    Disconnect,
}

impl MenuItem {
    pub const ALL: [MenuItem; 4] = [Self::Resume, Self::Settings, Self::Scoreboard, Self::Disconnect];
    fn label(self) -> &'static str {
        match self {
            Self::Resume => "RESUME",
            Self::Settings => "SETTINGS",
            Self::Scoreboard => "SCOREBOARD",
            Self::Disconnect => "DISCONNECT",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Screen {
    InGame,
    Connecting { elapsed_s: f32 },
    Disconnected { reason: String },
    /// Server dropped us. Terminal: the app must build a fresh `NetClient` to reconnect, and only once
    /// `retry_in_s` has reached 0 (cheat / flood bans last ~10 s server-side).
    Kicked { reason: String, retry_in_s: f32 },
    Paused { selected: usize },
    Scoreboard { rows: Vec<ScoreRow> },
}

impl Screen {
    /// Connection-driven screen. `silence_s` = seconds since the last snapshot arrived, `since_kick_s` =
    /// seconds since `net.kicked()` first became `Some` (both from the platform clock). A kick wins over
    /// everything else.
    pub fn for_net(net: &NetClient, connecting_for_s: f32, silence_s: f32, since_kick_s: f32) -> Screen {
        if let Some(r) = net.kicked() {
            return Screen::kicked(r, since_kick_s);
        }
        match net.state() {
            ConnState::Connecting => Screen::Connecting { elapsed_s: connecting_for_s },
            ConnState::Connected { .. } if silence_s > SILENCE_TIMEOUT_S => {
                Screen::Disconnected { reason: "CONNECTION LOST".into() }
            }
            ConnState::Connected { .. } => Screen::InGame,
        }
    }
}

/// Server-side ban length we should wait out before retrying.
/// One line of plain language under the kick reason (reasons are `KickReason::as_str` uppercased).
pub fn kick_hint(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "CHEAT DETECTION" => "THE SERVER FLAGGED YOUR MOVEMENT",
        "PACKET FLOOD" => "TOO MANY PACKETS WERE SENT",
        "SERVER FULL" => "NO FREE SLOT: TRY AGAIN",
        "TIMED OUT" => "THE SERVER HEARD NOTHING FOR TOO LONG",
        "CLIENT OUT OF DATE" => "UPDATE THE GAME TO CONTINUE",
        _ => return None,
    })
}

pub fn kick_cooldown_s(r: KickReason) -> f32 {
    match r {
        KickReason::Cheat | KickReason::Flood => 10.0,
        KickReason::ServerFull => 3.0,
        KickReason::Timeout | KickReason::Other | KickReason::BadVersion => 0.0,
    }
}

impl Screen {
    pub fn kicked(r: KickReason, since_kick_s: f32) -> Screen {
        Screen::Kicked { reason: r.as_str().to_uppercase(), retry_in_s: (kick_cooldown_s(r) - since_kick_s).max(0.0) }
    }
}

fn clip(s: &str) -> String {
    let t: String = s.chars().filter(|c| !c.is_control()).collect();
    if t.chars().count() <= REASON_MAX_CHARS {
        t
    } else {
        let mut c: String = t.chars().take(REASON_MAX_CHARS - 1).collect();
        c.push('\u{2026}');
        c
    }
}

const PANEL: Rect = Rect { x: 0.28, y: 0.30, w: 0.44, h: 0.40 };

fn menu_rect(i: usize) -> Rect {
    Rect::new(PANEL.x + 0.04, 0.40 + i as f32 * 0.07, PANEL.w - 0.08, 0.05)
}

/// Menu item under a normalized point (mouse click / tap), if any.
pub fn menu_hit(x: f32, y: f32) -> Option<MenuItem> {
    MenuItem::ALL.iter().enumerate().find_map(|(i, m)| {
        let r = menu_rect(i);
        (x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h).then_some(*m)
    })
}

/// Keyboard navigation for the pause menu (wraps).
pub fn menu_step(selected: usize, down: bool) -> usize {
    let n = MenuItem::ALL.len();
    if down { (selected + 1) % n } else { (selected + n - 1) % n }
}

fn dim(out: &mut Vec<Prim>, p: &Palette, a: f32) {
    out.push(Prim::Rect { r: Rect::new(0.0, 0.0, 1.0, 1.0), color: p.bg.a(a), glow: 0.0 });
    out.push(Prim::Grid { r: Rect::new(0.0, 0.0, 1.0, 1.0), cell: 0.05, color: p.grid.a(0.22) });
}

fn panel(out: &mut Vec<Prim>, r: Rect, edge: crate::hud::Rgba, p: &Palette) {
    out.push(Prim::Rect { r, color: p.bg.a(0.85), glow: 0.0 });
    let (x1, y1) = (r.x + r.w, r.y + r.h);
    for (a, b) in [((r.x, r.y), (x1, r.y)), ((x1, r.y), (x1, y1)), ((x1, y1), (r.x, y1)), ((r.x, y1), (r.x, r.y))] {
        out.push(Prim::Line { a, b, color: edge.a(0.85), glow: 0.008 });
    }
}

fn title(out: &mut Vec<Prim>, text: &str, y: f32, color: crate::hud::Rgba) {
    out.push(Prim::Text { pos: (0.5, y), text: text.into(), size: 0.06, color, align: Align::Center });
}

/// Draw a screen. `time` = seconds, only for animation; output is deterministic for a given input.
/// The `Scoreboard` screen carries its own rows so it also works while paused.
pub fn draw(screen: &Screen, p: &Palette, time: f32) -> Vec<Prim> {
    let mut out = Vec::new();
    match screen {
        Screen::InGame => {}
        Screen::Connecting { elapsed_s } => {
            dim(&mut out, p, 0.9);
            panel(&mut out, PANEL, p.cyan, p);
            let dots = ".".repeat((time * 2.0) as usize % 4);
            title(&mut out, &format!("CONNECTING{dots}"), 0.48, p.cyan);
            // scanning bar: a bright block sweeping a dim track
            let track = Rect::new(PANEL.x + 0.04, 0.56, PANEL.w - 0.08, 0.012);
            out.push(Prim::Rect { r: track, color: p.cyan.a(0.15), glow: 0.0 });
            let block = 0.1;
            let x = track.x + (track.w - block * track.w) * (0.5 - 0.5 * (time * 2.4).cos());
            out.push(Prim::Rect { r: Rect::new(x, track.y, track.w * block, track.h), color: p.cyan.a(0.95), glow: 0.01 });
            out.push(Prim::Text { pos: (0.5, 0.64), text: format!("{:.0}S", elapsed_s.max(0.0)), size: 0.03, color: p.text.a(0.6), align: Align::Center });
        }
        Screen::Disconnected { reason } => {
            dim(&mut out, p, 0.92);
            panel(&mut out, PANEL, p.magenta, p);
            title(&mut out, "DISCONNECTED", 0.43, p.magenta);
            out.push(Prim::Text { pos: (0.5, 0.52), text: clip(reason), size: 0.032, color: p.text.a(0.9), align: Align::Center });
            out.push(Prim::Text { pos: (0.5, 0.62), text: "PRESS ENTER TO RECONNECT".into(), size: 0.028, color: p.orange.a(0.6 + 0.4 * (time * 3.0).sin().abs()), align: Align::Center });
            out.push(Prim::Text { pos: (0.5, 0.67), text: "ESC TO QUIT".into(), size: 0.022, color: p.text.a(0.6), align: Align::Center });
        }
        Screen::Kicked { reason, retry_in_s } => {
            dim(&mut out, p, 0.94);
            panel(&mut out, PANEL, p.orange, p);
            title(&mut out, "KICKED", 0.43, p.orange);
            out.push(Prim::Text { pos: (0.5, 0.51), text: clip(reason), size: 0.036, color: p.orange.a(1.0), align: Align::Center });
            if let Some(h) = kick_hint(reason) {
                out.push(Prim::Text { pos: (0.5, 0.565), text: h.into(), size: 0.022, color: p.text.a(0.8), align: Align::Center });
            }
            let (msg, col) = if *retry_in_s > 0.0 {
                (format!("RETRY IN {}S", retry_in_s.ceil() as u32), p.orange.a(0.8))
            } else {
                ("PRESS ENTER TO RECONNECT".into(), p.cyan.a(0.6 + 0.4 * (time * 3.0).sin().abs()))
            };
            out.push(Prim::Text { pos: (0.5, 0.62), text: msg, size: 0.028, color: col, align: Align::Center });
            out.push(Prim::Text { pos: (0.5, 0.67), text: "ESC TO QUIT".into(), size: 0.022, color: p.text.a(0.6), align: Align::Center });
        }
        Screen::Paused { selected } => {
            dim(&mut out, p, 0.6);
            panel(&mut out, PANEL, p.cyan, p);
            out.push(Prim::Text { pos: (0.5, 0.375), text: "PAUSED".into(), size: 0.045, color: p.cyan, align: Align::Center });
            for (i, m) in MenuItem::ALL.iter().enumerate() {
                let r = menu_rect(i);
                let sel = i == *selected;
                if sel {
                    out.push(Prim::Rect { r, color: p.cyan.a(0.18), glow: 0.008 });
                    out.push(Prim::Line { a: (r.x, r.y), b: (r.x, r.y + r.h), color: p.orange, glow: 0.008 });
                }
                let c = if *m == MenuItem::Disconnect { p.magenta } else { p.text };
                out.push(Prim::Text { pos: (0.5, r.y + r.h * 0.72), text: m.label().into(), size: 0.032, color: c.a(if sel { 1.0 } else { 0.7 }), align: Align::Center });
            }
        }
        Screen::Scoreboard { rows } => {
            dim(&mut out, p, 0.45);
            let s = HudState { scoreboard: rows.clone(), show_scoreboard: true, ..Default::default() };
            scoreboard(&mut out, &s, p);
        }
    }
    out
}

// ---------- settings screen ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingRow {
    MouseSens,
    AimPadSens,
    StickRadius,
    StickDeadzone,
    Fov,
    FxIntensity,
    CrtWarp,
    InvertY,
    ReducedMotion,
    HeadBob,
    LeftHanded,
}

impl SettingRow {
    pub const ALL: [SettingRow; 11] = [
        Self::MouseSens,
        Self::AimPadSens,
        Self::StickRadius,
        Self::StickDeadzone,
        Self::Fov,
        Self::FxIntensity,
        Self::CrtWarp,
        Self::InvertY,
        Self::ReducedMotion,
        Self::HeadBob,
        Self::LeftHanded,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::MouseSens => "SENS CM/360",
            Self::AimPadSens => "AIM PAD SENS",
            Self::StickRadius => "STICK SIZE",
            Self::StickDeadzone => "STICK DEADZONE",
            Self::Fov => "FOV",
            Self::FxIntensity => "FX INTENSITY",
            Self::CrtWarp => "CRT WARP",
            Self::InvertY => "INVERT Y",
            Self::ReducedMotion => "REDUCED MOTION",
            Self::HeadBob => "HEAD BOB",
            Self::LeftHanded => "LEFT HANDED",
        }
    }

    /// Sensitivities are felt as ratios, so their sliders are logarithmic: equal steps change the turn speed by an equal
    /// percentage and the low end (where aim is precise) gets as much room as the high end.
    pub fn log_scale(self) -> bool {
        matches!(self, Self::MouseSens | Self::AimPadSens)
    }

    /// Keyboard step in slider fraction: fine for sensitivities (about 1 percent of turn speed per press), coarse else.
    pub fn key_step(self) -> f32 {
        if self.log_scale() { 0.01 } else { 0.05 }
    }

    /// `(min, max, default)` for slider rows, `None` for toggles.
    fn range(self) -> Option<(f32, f32, f32)> {
        Some(match self {
            Self::MouseSens => cfg::MOUSE_SENS,
            Self::AimPadSens => cfg::AIM_PAD_SENS,
            Self::StickRadius => cfg::STICK_RADIUS,
            Self::StickDeadzone => cfg::STICK_DEADZONE,
            Self::Fov => cfg::FOV,
            Self::FxIntensity => cfg::FX_INTENSITY,
            Self::CrtWarp => cfg::CRT_WARP,
            _ => return None,
        })
    }

    fn slider_value(self, s: &Settings) -> Option<f32> {
        Some(match self {
            Self::MouseSens => s.mouse_sens,
            Self::AimPadSens => s.aim_pad_sens,
            Self::StickRadius => s.stick_radius_frac,
            Self::StickDeadzone => s.stick_deadzone,
            Self::Fov => s.fov_deg,
            Self::FxIntensity => s.fx_intensity,
            Self::CrtWarp => s.crt_warp,
            _ => return None,
        })
    }

    fn toggle_value(self, s: &Settings) -> Option<bool> {
        Some(match self {
            Self::InvertY => s.invert_y,
            Self::ReducedMotion => s.reduced_motion,
            Self::HeadBob => s.head_bob,
            Self::LeftHanded => s.left_handed,
            _ => return None,
        })
    }
}

const SET_PANEL: Rect = Rect { x: 0.22, y: 0.07, w: 0.56, h: 0.84 };
const SET_ROW_H: f32 = 0.058;
const SET_TRACK_X: f32 = 0.47;
const SET_TRACK_W: f32 = 0.18;

fn setting_rect(i: usize) -> Rect {
    Rect::new(SET_PANEL.x + 0.02, SET_PANEL.y + 0.1 + i as f32 * SET_ROW_H, SET_PANEL.w - 0.04, SET_ROW_H - 0.008)
}

/// Slider position 0..=1 of a row (`None` for toggles).
pub fn setting_frac(s: &Settings, row: SettingRow) -> Option<f32> {
    let (lo, hi, _) = row.range()?;
    let v = row.slider_value(s)?;
    Some(if row.log_scale() { ((v / lo).ln() / (hi / lo).ln()).clamp(0.0, 1.0) } else { ((v - lo) / (hi - lo)).clamp(0.0, 1.0) })
}

/// Value readout: percent for 0..1 sliders, degrees for FOV, plain number for sensitivities, ON/OFF for toggles.
pub fn setting_text(s: &Settings, row: SettingRow) -> String {
    if let Some(b) = row.toggle_value(s) {
        return if b { "ON" } else { "OFF" }.into();
    }
    let v = row.slider_value(s).unwrap_or(0.0);
    match row {
        SettingRow::Fov => format!("{:.0} DEG", v),
        // mouse sensitivity reads as centimetres of mouse travel per full turn (smaller = faster), at an assumed 800 DPI
        SettingRow::MouseSens => format!("{:.1}", cfg::cm_per_360(v)),
        SettingRow::AimPadSens => format!("{:.4}", v),
        _ => format!("{:.0}%", v * 100.0),
    }
}

/// Row + slider fraction under a normalized point (mouse / touch), if any. Sliders respond over the whole
/// right part of the row so a drag that wanders vertically keeps working; the fraction is clamped to 0..=1.
pub fn settings_hit(x: f32, y: f32) -> Option<(SettingRow, f32)> {
    SettingRow::ALL.iter().enumerate().find_map(|(i, row)| {
        let r = setting_rect(i);
        let inside = x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h;
        inside.then(|| (*row, ((x - SET_TRACK_X) / SET_TRACK_W).clamp(0.0, 1.0)))
    })
}

/// Apply a click / drag. Sliders take `frac` (only when the pointer is over the track or beyond it); toggles
/// flip when `click` is true (a drag across them must not flicker). Result is always clamped.
pub fn settings_apply(s: &mut Settings, row: SettingRow, frac: f32, click: bool) {
    if let Some((lo, hi, _)) = row.range() {
        let f = frac.clamp(0.0, 1.0);
        let v = if row.log_scale() { lo * (hi / lo).powf(f) } else { lo + f * (hi - lo) };
        match row {
            SettingRow::MouseSens => s.mouse_sens = v,
            SettingRow::AimPadSens => s.aim_pad_sens = v,
            SettingRow::StickRadius => s.stick_radius_frac = v,
            SettingRow::StickDeadzone => s.stick_deadzone = v,
            SettingRow::Fov => s.fov_deg = v,
            SettingRow::FxIntensity => s.fx_intensity = v,
            SettingRow::CrtWarp => s.crt_warp = v,
            _ => {}
        }
    } else if click {
        match row {
            SettingRow::InvertY => s.invert_y = !s.invert_y,
            SettingRow::ReducedMotion => s.reduced_motion = !s.reduced_motion,
            SettingRow::HeadBob => s.head_bob = !s.head_bob,
            SettingRow::LeftHanded => s.left_handed = !s.left_handed,
            _ => {}
        }
    }
    *s = s.clamped();
}

/// Settings screen draw list. `selected` = keyboard / hover row. Opened from the pause menu's SETTINGS item.
pub fn draw_settings(s: &Settings, selected: usize, p: &Palette) -> Vec<Prim> {
    let mut out = Vec::new();
    dim(&mut out, p, 0.7);
    panel(&mut out, SET_PANEL, p.cyan, p);
    out.push(Prim::Text { pos: (0.5, SET_PANEL.y + 0.065), text: "SETTINGS".into(), size: 0.045, color: p.cyan, align: Align::Center });
    for (i, row) in SettingRow::ALL.iter().enumerate() {
        let r = setting_rect(i);
        let sel = i == selected;
        if sel {
            out.push(Prim::Rect { r, color: p.cyan.a(0.14), glow: 0.006 });
            out.push(Prim::Line { a: (r.x, r.y), b: (r.x, r.y + r.h), color: p.orange, glow: 0.006 });
        }
        let ty = r.y + r.h * 0.7;
        out.push(Prim::Text { pos: (r.x + 0.02, ty), text: row.label().into(), size: 0.03, color: p.text.a(if sel { 1.0 } else { 0.75 }), align: Align::Left });
        match setting_frac(s, *row) {
            Some(f) => {
                let ty_track = r.y + r.h * 0.5;
                out.push(Prim::Rect { r: Rect::new(SET_TRACK_X, ty_track - 0.004, SET_TRACK_W, 0.008), color: p.cyan.a(0.15), glow: 0.0 });
                out.push(Prim::Rect { r: Rect::new(SET_TRACK_X, ty_track - 0.004, SET_TRACK_W * f, 0.008), color: p.cyan.a(0.9), glow: 0.006 });
                out.push(Prim::Ring { c: (SET_TRACK_X + SET_TRACK_W * f, ty_track), r: 0.012, color: p.orange, fill: true, glow: 0.008 });
                out.push(Prim::Text { pos: (r.x + r.w - 0.01, ty), text: setting_text(s, *row), size: 0.028, color: p.orange.a(0.9), align: Align::Right });
            }
            None => {
                let on = row.toggle_value(s).unwrap_or(false);
                let c = if on { p.cyan } else { p.magenta };
                out.push(Prim::Text { pos: (r.x + r.w - 0.01, ty), text: setting_text(s, *row), size: 0.03, color: c.a(0.95), align: Align::Right });
            }
        }
    }
    out.push(Prim::Text { pos: (0.5, SET_PANEL.y + SET_PANEL.h - 0.02), text: "ARROWS ADJUST   RIGHT = FASTER   ESC BACK".into(), size: 0.02, color: p.text.a(0.5), align: Align::Center });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use shinobi_proto::datagram::Welcome;

    fn texts(v: &[Prim]) -> Vec<String> {
        v.iter().filter_map(|p| if let Prim::Text { text, .. } = p { Some(text.clone()) } else { None }).collect()
    }

    #[test]
    fn each_screen_shows_its_title_and_ingame_draws_nothing() {
        let p = Palette::TRON;
        assert!(draw(&Screen::InGame, &p, 0.0).is_empty());
        let cases = [
            (Screen::Connecting { elapsed_s: 2.0 }, "CONNECTING"),
            (Screen::Disconnected { reason: "x".into() }, "DISCONNECTED"),
            (Screen::Kicked { reason: "x".into(), retry_in_s: 0.0 }, "KICKED"),
            (Screen::Paused { selected: 0 }, "PAUSED"),
        ];
        for (s, t) in cases {
            assert!(texts(&draw(&s, &p, 0.0)).iter().any(|x| x.starts_with(t)), "{t}");
        }
    }

    #[test]
    fn kick_reason_is_clipped_and_control_chars_stripped() {
        let long = format!("ANTI-CHEAT: {}\n\u{7}", "speed ".repeat(40));
        let t = texts(&draw(&Screen::Kicked { reason: long, retry_in_s: 0.0 }, &Palette::TRON, 0.0));
        let r = t.iter().find(|x| x.starts_with("ANTI-CHEAT")).unwrap();
        assert_eq!(r.chars().count(), REASON_MAX_CHARS);
        assert!(r.ends_with('\u{2026}') && !r.contains('\n'));
        let short = texts(&draw(&Screen::Kicked { reason: "SPEEDHACK".into(), retry_in_s: 0.0 }, &Palette::TRON, 0.0));
        assert!(short.contains(&"SPEEDHACK".to_string()));
    }

    #[test]
    fn connecting_animates_but_is_deterministic_per_time() {
        let p = Palette::TRON;
        let s = Screen::Connecting { elapsed_s: 1.0 };
        assert_eq!(draw(&s, &p, 0.3), draw(&s, &p, 0.3));
        assert_ne!(draw(&s, &p, 0.0), draw(&s, &p, 0.4));
    }

    #[test]
    fn pause_menu_hit_test_and_keyboard_wrap() {
        for (i, m) in MenuItem::ALL.iter().enumerate() {
            let r = menu_rect(i);
            assert_eq!(menu_hit(r.x + r.w * 0.5, r.y + r.h * 0.5), Some(*m));
        }
        assert_eq!(menu_hit(0.05, 0.05), None);
        assert_eq!(menu_hit(0.5, 0.395), None, "gap above the first row");
        assert_eq!(menu_step(0, false), 3);
        assert_eq!(menu_step(3, true), 0);
        // selected row draws the highlight
        let with = draw(&Screen::Paused { selected: 2 }, &Palette::TRON, 0.0).len();
        let none = draw(&Screen::Paused { selected: 99 }, &Palette::TRON, 0.0).len();
        assert!(with > none);
    }

    #[test]
    fn scoreboard_screen_lists_rows() {
        let rows = vec![ScoreRow { name: "Ronin".into(), kills: 3, deaths: 1, ping_ms: 20, local: true, alive: true, team: None, colour: None }];
        let t = texts(&draw(&Screen::Scoreboard { rows }, &Palette::TRON, 0.0));
        assert!(t.contains(&"Ronin".to_string()) && t.contains(&"PLAYER".to_string()));
    }

    #[test]
    fn for_net_maps_connection_state() {
        let mut net = NetClient::default();
        assert_eq!(Screen::for_net(&net, 1.5, 0.0, 0.0), Screen::Connecting { elapsed_s: 1.5 });
        net.on_datagram(&Welcome { player_id: 1, server_tick: 0 }.encode(), 0.0).unwrap();
        assert_eq!(Screen::for_net(&net, 0.0, 1.0, 0.0), Screen::InGame);
        assert!(matches!(Screen::for_net(&net, 0.0, SILENCE_TIMEOUT_S + 0.1, 0.0), Screen::Disconnected { .. }));
    }

    #[test]
    fn kick_cooldown_counts_down_then_offers_reconnect() {
        let p = Palette::TRON;
        let k = |since| Screen::kicked(KickReason::Cheat, since);
        assert_eq!(k(0.0), Screen::Kicked { reason: "CHEAT DETECTION".into(), retry_in_s: 10.0 });
        assert!(texts(&draw(&k(2.5), &p, 0.0)).contains(&"RETRY IN 8S".to_string()), "ceil of 7.5");
        assert!(texts(&draw(&k(10.0), &p, 0.0)).contains(&"PRESS ENTER TO RECONNECT".to_string()));
        assert_eq!(k(99.0), Screen::Kicked { reason: "CHEAT DETECTION".into(), retry_in_s: 0.0 });
        // a timeout kick has no ban: reconnect immediately
        assert!(matches!(Screen::kicked(KickReason::Timeout, 0.0), Screen::Kicked { retry_in_s, .. } if retry_in_s == 0.0));
        assert_eq!(kick_cooldown_s(KickReason::ServerFull), 3.0);
    }

    #[test]
    fn settings_rows_hit_and_sliders_span_min_to_max() {
        for (i, row) in SettingRow::ALL.iter().enumerate() {
            let r = setting_rect(i);
            let (hit, _) = settings_hit(r.x + 0.05, r.y + r.h * 0.5).unwrap();
            assert_eq!(hit, *row);
        }
        assert_eq!(settings_hit(0.02, 0.5), None, "outside the panel");
        let mut s = Settings::default();
        settings_apply(&mut s, SettingRow::Fov, 0.0, false);
        assert_eq!(s.fov_deg, cfg::FOV.0);
        settings_apply(&mut s, SettingRow::Fov, 1.0, false);
        assert_eq!(s.fov_deg, cfg::FOV.1);
        settings_apply(&mut s, SettingRow::Fov, 7.0, false);
        assert_eq!(s.fov_deg, cfg::FOV.1, "overshoot clamps");
        // pointer left of the track pins to min, right of it pins to max
        let i = SettingRow::ALL.iter().position(|r| *r == SettingRow::MouseSens).unwrap();
        let y = setting_rect(i).y + 0.02;
        assert_eq!(settings_hit(0.3, y).unwrap().1, 0.0);
        assert_eq!(settings_hit(SET_TRACK_X + SET_TRACK_W + 0.05, y).unwrap().1, 1.0);
    }

    #[test]
    fn every_slider_row_round_trips_through_frac() {
        let mut s = Settings::default();
        for row in SettingRow::ALL.iter().filter(|r| r.range().is_some()) {
            for f in [0.0, 0.25, 0.5, 1.0] {
                settings_apply(&mut s, *row, f, false);
                assert!((setting_frac(&s, *row).unwrap() - f).abs() < 1e-4, "{row:?} {f}");
            }
        }
        assert_eq!(s, s.clamped());
    }

    #[test]
    fn toggles_flip_on_click_only_and_cover_the_new_camera_options() {
        let mut s = Settings::default();
        for (row, get) in [
            (SettingRow::InvertY, (|s: &Settings| s.invert_y) as fn(&Settings) -> bool),
            (SettingRow::ReducedMotion, |s| s.reduced_motion),
            (SettingRow::HeadBob, |s| s.head_bob),
            (SettingRow::LeftHanded, |s| s.left_handed),
        ] {
            let before = get(&s);
            settings_apply(&mut s, row, 0.9, false);
            assert_eq!(get(&s), before, "drag must not flip {row:?}");
            settings_apply(&mut s, row, 0.0, true);
            assert_eq!(get(&s), !before);
        }
        assert_eq!(setting_text(&s, SettingRow::LeftHanded), "ON");
    }

    #[test]
    fn settings_screen_lists_every_row_with_values_and_a_selection_highlight() {
        let p = Palette::TRON;
        let s = Settings { fov_deg: 100.0, ..Default::default() };
        let out = draw_settings(&s, 4, &p);
        let t = texts(&out);
        for row in SettingRow::ALL {
            assert!(t.contains(&row.label().to_string()), "{row:?}");
        }
        assert!(t.contains(&"100 DEG".to_string()) && t.contains(&"SETTINGS".to_string()));
        assert!(draw_settings(&s, 4, &p).len() > draw_settings(&s, 99, &p).len());
        // all geometry stays on screen
        for q in &out {
            if let Some(b) = crate::hud::bounds(q) {
                assert!(b.x >= 0.0 && b.y >= 0.0 && b.x + b.w <= 1.0 && b.y + b.h <= 1.0, "{b:?}");
            }
        }
    }

    #[test]
    fn kicked_and_disconnected_screens_explain_and_offer_reconnect_and_quit() {
        let p = Palette::TRON;
        let t = texts(&draw(&Screen::Kicked { reason: "CHEAT DETECTION".into(), retry_in_s: 4.2 }, &p, 0.0));
        for want in ["KICKED", "CHEAT DETECTION", "THE SERVER FLAGGED YOUR MOVEMENT", "RETRY IN 5S", "ESC TO QUIT"] {
            assert!(t.contains(&want.to_string()), "{want}");
        }
        let t = texts(&draw(&Screen::Kicked { reason: "PACKET FLOOD".into(), retry_in_s: 0.0 }, &p, 0.0));
        assert!(t.contains(&"PRESS ENTER TO RECONNECT".to_string()) && t.contains(&"ESC TO QUIT".to_string()));
        let t = texts(&draw(&Screen::Disconnected { reason: "CONNECTION LOST".into() }, &p, 0.0));
        assert!(t.contains(&"PRESS ENTER TO RECONNECT".to_string()) && t.contains(&"ESC TO QUIT".to_string()));
        assert_eq!(kick_hint("SOMETHING ELSE"), None);
        assert!(kick_hint("CLIENT OUT OF DATE").is_some());
    }

    #[test]
    fn sensitivity_slider_is_logarithmic_reads_in_cm_per_360_and_fov_is_80_to_110() {
        let mut s = Settings::default();
        // ends and a round trip through the log mapping
        settings_apply(&mut s, SettingRow::MouseSens, 0.0, false);
        assert!((s.mouse_sens - cfg::MOUSE_SENS.0).abs() < 1e-7);
        settings_apply(&mut s, SettingRow::MouseSens, 1.0, false);
        assert!((s.mouse_sens - cfg::MOUSE_SENS.1).abs() < 1e-6);
        settings_apply(&mut s, SettingRow::MouseSens, 0.5, false);
        let mid = s.mouse_sens;
        assert!((mid - (cfg::MOUSE_SENS.0 * cfg::MOUSE_SENS.1).sqrt()).abs() < 1e-6, "midpoint is the geometric mean");
        assert!((setting_frac(&s, SettingRow::MouseSens).unwrap() - 0.5).abs() < 1e-4);
        // equal key steps = equal ratio, anywhere on the slider
        let ratio = |at: f32| {
            let mut t = Settings::default();
            settings_apply(&mut t, SettingRow::MouseSens, at, false);
            let before = t.mouse_sens;
            settings_apply(&mut t, SettingRow::MouseSens, at + SettingRow::MouseSens.key_step(), false);
            t.mouse_sens / before
        };
        assert!((ratio(0.2) - ratio(0.7)).abs() < 1e-3, "{} {}", ratio(0.2), ratio(0.7));
        assert!(SettingRow::MouseSens.key_step() < SettingRow::Fov.key_step(), "sensitivity steps are fine");
        // default 0.0012 rad/count at 800 DPI is about 17 cm per turn; the readout agrees and faster = smaller number
        let d = Settings::default();
        let cm: f32 = setting_text(&d, SettingRow::MouseSens).parse().unwrap();
        assert!((cm - 16.7).abs() < 0.3, "{cm}");
        let mut fast = d;
        fast.mouse_sens *= 2.0;
        assert!(setting_text(&fast, SettingRow::MouseSens).parse::<f32>().unwrap() < cm);
        // FOV slider spans exactly 80..110
        settings_apply(&mut s, SettingRow::Fov, 0.0, false);
        assert_eq!(s.fov_deg, 80.0);
        settings_apply(&mut s, SettingRow::Fov, 1.0, false);
        assert_eq!(s.fov_deg, 110.0);
    }
}
