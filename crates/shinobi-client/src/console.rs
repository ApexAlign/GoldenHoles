//! In-game console (backquote): typed commands, a scrolling log, and the draw list. Pure logic: the shell feeds
//! characters and Enter, and applies the returned settings.

use std::collections::VecDeque;

use crate::colour::player_rgba;
use crate::hud::{Align, KillEntry, Palette, Prim, Rect};
use crate::settings::{self, Settings};

pub const LOG_MAX: usize = 64;
pub const LINE_MAX: usize = 96;
const SHOWN: usize = 12;

#[derive(Clone, Debug, Default)]
pub struct Console {
    pub input: String,
    log: VecDeque<String>,
    history: Vec<String>,
}

impl Console {
    pub fn log(&self) -> impl Iterator<Item = &String> {
        self.log.iter()
    }

    pub fn push(&mut self, line: impl Into<String>) {
        self.log.push_back(line.into());
        while self.log.len() > LOG_MAX {
            self.log.pop_front();
        }
    }

    /// A typed character. Control characters and the backquote that toggles the console are ignored.
    pub fn type_char(&mut self, c: char) {
        if c.is_control() || c == '`' || self.input.chars().count() >= LINE_MAX {
            return;
        }
        self.input.push(c);
    }

    pub fn backspace(&mut self) {
        self.input.pop();
    }

    /// Run the typed line. `settings` is edited in place (the caller applies and saves it when the returned flag is
    /// true); `kills` is the recent kill log for `/log`; `me` is the local player id (for `/colour`).
    pub fn submit(&mut self, settings: &mut Settings, kills: &[KillEntry], me: u16) -> bool {
        let line = std::mem::take(&mut self.input);
        let line = line.trim();
        if line.is_empty() {
            return false;
        }
        self.push(format!("> {line}"));
        self.history.push(line.to_string());
        let (changed, out) = exec(line, settings, kills, me);
        for l in out {
            self.push(l);
        }
        if changed {
            *settings = settings.clamped();
        }
        changed
    }

    pub fn clear(&mut self) {
        self.log.clear();
    }
}

/// Execute one command. Returns `(settings changed, output lines)`.
pub fn exec(line: &str, s: &mut Settings, kills: &[KillEntry], me: u16) -> (bool, Vec<String>) {
    let mut it = line.trim_start_matches('/').split_whitespace();
    let cmd = it.next().unwrap_or("").to_ascii_lowercase();
    let arg = it.next();
    let num = |a: Option<&str>| a.and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite());
    match cmd.as_str() {
        "help" | "?" => (
            false,
            vec![
                "/sens <v>   mouse sensitivity".into(),
                "/fov <deg>  field of view".into(),
                "/colour     your neon colour".into(),
                "/log        recent kills".into(),
                "/clear      clear this log".into(),
            ],
        ),
        "sens" => match num(arg) {
            Some(v) => {
                s.mouse_sens = v;
                let c = s.clamped();
                (true, vec![format!("mouse sens = {:.4}", c.mouse_sens)])
            }
            None => (false, vec![format!("mouse sens = {:.4} (range {:.4}-{:.4})", s.mouse_sens, settings::MOUSE_SENS.0, settings::MOUSE_SENS.1)]),
        },
        "fov" => match num(arg) {
            Some(v) => {
                s.fov_deg = v;
                let c = s.clamped();
                (true, vec![format!("fov = {:.0}", c.fov_deg)])
            }
            None => (false, vec![format!("fov = {:.0} (range {:.0}-{:.0})", s.fov_deg, settings::FOV.0, settings::FOV.1)]),
        },
        "colour" | "color" => {
            let c = player_rgba(me);
            let hex = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            (false, vec![format!("your colour #{:02X}{:02X}{:02X}", hex(c.0), hex(c.1), hex(c.2))])
        }
        "log" => {
            if kills.is_empty() {
                return (false, vec!["no kills yet".into()]);
            }
            (false, kills.iter().rev().take(8).map(|k| if k.void { format!("{} fell", k.victim) } else { format!("{} > {}", k.killer, k.victim) }).collect())
        }
        "" => (false, vec![]),
        other => (false, vec![format!("unknown command '{other}' (try /help)")]),
    }
}

/// Terminal panel: lower-left of the central band (clear of the crosshair), log lines then the prompt.
pub fn draw(c: &Console, time: f32, p: &Palette) -> Vec<Prim> {
    let mut out = Vec::new();
    let r = Rect::new(0.24, 0.52, 0.52, 0.32);
    out.push(Prim::Rect { r, color: p.bg.a(0.82), glow: 0.0 });
    for (a, b) in [((r.x, r.y), (r.x + r.w, r.y)), ((r.x + r.w, r.y), (r.x + r.w, r.y + r.h)), ((r.x + r.w, r.y + r.h), (r.x, r.y + r.h)), ((r.x, r.y + r.h), (r.x, r.y))] {
        out.push(Prim::Line { a, b, color: p.cyan.a(0.8), glow: 0.005 });
    }
    out.push(Prim::Text { pos: (r.x + 0.01, r.y + 0.026), text: "CONSOLE  /help".into(), size: 0.02, color: p.orange.a(0.9), align: Align::Left });
    let lines: Vec<&String> = c.log.iter().rev().take(SHOWN).collect::<Vec<_>>().into_iter().rev().collect();
    for (i, l) in lines.iter().enumerate() {
        let user = l.starts_with("> ");
        out.push(Prim::Text { pos: (r.x + 0.012, r.y + 0.06 + i as f32 * 0.02), text: (*l).clone(), size: 0.017, color: if user { p.gold } else { p.cyan }.a(0.9), align: Align::Left });
    }
    let caret = if (time * 2.0) as i32 % 2 == 0 { "_" } else { " " };
    out.push(Prim::Text { pos: (r.x + 0.012, r.y + r.h - 0.012), text: format!("> {}{}", c.input, caret), size: 0.02, color: p.text.a(1.0), align: Align::Left });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(c: &mut Console, s: &mut Settings, line: &str) -> bool {
        for ch in line.chars() {
            c.type_char(ch);
        }
        c.submit(s, &[], 3)
    }

    #[test]
    fn sens_and_fov_set_clamp_and_report() {
        let (mut c, mut s) = (Console::default(), Settings::default());
        assert!(run(&mut c, &mut s, "/sens 0.005"));
        assert!((s.mouse_sens - 0.005).abs() < 1e-6);
        assert!(run(&mut c, &mut s, "/fov 500"));
        assert_eq!(s.fov_deg, settings::FOV.1, "clamped to the legal range");
        assert!(!run(&mut c, &mut s, "/fov abc"), "bad number changes nothing");
        assert!(c.log().any(|l| l.starts_with("fov = ")));
    }

    #[test]
    fn help_colour_log_clear_and_unknown() {
        let (mut c, mut s) = (Console::default(), Settings::default());
        run(&mut c, &mut s, "/help");
        assert!(c.log().any(|l| l.contains("/sens")));
        run(&mut c, &mut s, "/colour");
        assert!(c.log().any(|l| l.starts_with("your colour #") && l.len() == "your colour #RRGGBB".len()));
        run(&mut c, &mut s, "/nope");
        assert!(c.log().any(|l| l.contains("unknown command")));
        let k = KillEntry { killer: "A".into(), victim: "B".into(), weapon: shinobi_sim::combat::DamageKind::Slash, age: 0.0, involves_local: false, void: false, killer_team: None, victim_team: None, killer_colour: None, victim_colour: None };
        let (_, out) = exec("log", &mut s, &[k], 0);
        assert_eq!(out, vec!["A > B".to_string()]);
        c.clear();
        assert_eq!(c.log().count(), 0);
    }

    #[test]
    fn typing_ignores_control_and_backquote_and_caps_the_line() {
        let mut c = Console::default();
        for ch in "a`b\n\u{7}c".chars() {
            c.type_char(ch);
        }
        assert_eq!(c.input, "abc");
        c.backspace();
        assert_eq!(c.input, "ab");
        for _ in 0..200 {
            c.type_char('x');
        }
        assert_eq!(c.input.chars().count(), LINE_MAX);
    }

    #[test]
    fn log_is_bounded_and_the_panel_draws_prompt_and_stays_off_the_centre() {
        let mut c = Console::default();
        for i in 0..200 {
            c.push(format!("line {i}"));
        }
        assert_eq!(c.log().count(), LOG_MAX);
        c.input = "hi".into();
        let prims = draw(&c, 0.0, &Palette::TRON);
        assert!(prims.iter().any(|p| matches!(p, Prim::Text { text, .. } if text.starts_with("> hi"))));
        for p in &prims {
            if let Some(b) = crate::hud::bounds(p) {
                assert!(b.y >= 0.5 || b.y + b.h <= 0.5 - 0.04 || b.x > 0.6 || b.x + b.w < 0.4, "console covers the crosshair: {b:?}");
            }
        }
    }
}
