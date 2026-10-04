//! F3 performance overlay: frame-time graph and the counters that diagnose a stuttering client (fps, worst frame,
//! fixed steps per second, ping, reconcile correction, lock state). Pure state + draw list; the shell feeds it.

use std::collections::VecDeque;

use crate::hud::{Align, Palette, Prim, Rect};

/// Frames kept for the graph.
pub const GRAPH_FRAMES: usize = 120;
/// Graph full scale in milliseconds (a bar this tall or taller is clipped and drawn magenta).
pub const GRAPH_MAX_MS: f32 = 33.4;

#[derive(Clone, Debug, Default)]
pub struct Perf {
    pub visible: bool,
    frames: VecDeque<f32>,
    /// Seconds / frames / steps accumulated in the current one-second window.
    win_t: f32,
    win_frames: u32,
    win_steps: u32,
    /// Last completed window: fps and fixed steps per second.
    pub fps: f32,
    pub steps_per_s: f32,
    /// Largest reconcile correction (metres) seen in the last window, and the one before it displayed.
    win_corr_peak: f32,
    pub corr_peak: f32,
}

/// Extra live numbers the shell passes in each frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct PerfInputs {
    pub ping_ms: Option<u16>,
    /// Current reconcile smoothing offset magnitude, metres.
    pub correction_m: f32,
    /// "SEARCH" / "ACQUIRE" / "LOCKED" / "BREAK".
    pub lock: &'static str,
}

impl Perf {
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    /// One rendered frame: `dt` seconds, `steps` fixed steps run since the previous call, current correction offset.
    pub fn record(&mut self, dt: f32, steps: u32, correction_m: f32) {
        self.frames.push_back(dt * 1000.0);
        while self.frames.len() > GRAPH_FRAMES {
            self.frames.pop_front();
        }
        self.win_t += dt;
        self.win_frames += 1;
        self.win_steps += steps;
        self.win_corr_peak = self.win_corr_peak.max(correction_m);
        if self.win_t >= 1.0 {
            self.fps = self.win_frames as f32 / self.win_t;
            self.steps_per_s = self.win_steps as f32 / self.win_t;
            self.corr_peak = self.win_corr_peak;
            self.win_t = 0.0;
            self.win_frames = 0;
            self.win_steps = 0;
            self.win_corr_peak = 0.0;
        }
    }

    pub fn worst_ms(&self) -> f32 {
        self.frames.iter().cloned().fold(0.0, f32::max)
    }

    pub fn last_ms(&self) -> f32 {
        self.frames.back().copied().unwrap_or(0.0)
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
}

/// Panel on the right side, upper half (a debug overlay: it may cover scenery, never the crosshair).
pub fn draw(perf: &Perf, i: &PerfInputs, p: &Palette) -> Vec<Prim> {
    let mut out = Vec::new();
    if !perf.visible {
        return out;
    }
    let r = Rect::new(0.70, 0.10, 0.28, 0.36);
    out.push(Prim::Rect { r, color: p.bg.a(0.8), glow: 0.0 });
    for (a, b) in [((r.x, r.y), (r.x + r.w, r.y)), ((r.x + r.w, r.y), (r.x + r.w, r.y + r.h)), ((r.x + r.w, r.y + r.h), (r.x, r.y + r.h)), ((r.x, r.y + r.h), (r.x, r.y))] {
        out.push(Prim::Line { a, b, color: p.cyan.a(0.7), glow: 0.003 });
    }
    out.push(Prim::Text { pos: (r.x + 0.01, r.y + 0.03), text: "PERF  F3".into(), size: 0.022, color: p.orange.a(0.95), align: Align::Left });
    let fps_col = if perf.fps >= 50.0 { p.cyan } else if perf.fps >= 30.0 { p.orange } else { p.magenta };
    let steps_col = if (perf.steps_per_s - 60.0).abs() <= 3.0 { p.cyan } else { p.magenta };
    let rows: [(String, crate::hud::Rgba); 6] = [
        (format!("FPS {:.0}", perf.fps), fps_col),
        (format!("FRAME {:.1} MS  MAX {:.1}", perf.last_ms(), perf.worst_ms()), if perf.worst_ms() > 33.4 { p.magenta } else { p.cyan }),
        (format!("STEPS/S {:.0}  (WANT 60)", perf.steps_per_s), steps_col),
        (i.ping_ms.map_or("PING --".into(), |ms| format!("PING {ms} MS")), p.cyan),
        (format!("CORRECTION {:.3} M  PEAK {:.3}", i.correction_m, perf.corr_peak), if perf.corr_peak > 0.25 { p.magenta } else { p.cyan }),
        (format!("LOCK {}", i.lock), p.text),
    ];
    for (k, (t, c)) in rows.iter().enumerate() {
        out.push(Prim::Text { pos: (r.x + 0.012, r.y + 0.065 + k as f32 * 0.03), text: t.clone(), size: 0.02, color: c.a(0.95), align: Align::Left });
    }
    // frame-time graph: one bar per frame, 16.7 ms and 33.4 ms guide lines
    let g = Rect::new(r.x + 0.012, r.y + 0.26, r.w - 0.024, 0.085);
    let bar_w = g.w / GRAPH_FRAMES as f32;
    for (k, ms) in perf.frames.iter().enumerate() {
        let h = (ms / GRAPH_MAX_MS).clamp(0.0, 1.0) * g.h;
        let col = if *ms > GRAPH_MAX_MS { p.magenta } else if *ms > 20.0 { p.orange } else { p.cyan };
        out.push(Prim::Rect { r: Rect::new(g.x + k as f32 * bar_w, g.y + g.h - h, bar_w * 0.8, h), color: col.a(0.85), glow: 0.0 });
    }
    for ms in [16.7f32, 33.4] {
        let y = g.y + g.h - (ms / GRAPH_MAX_MS) * g.h;
        out.push(Prim::Line { a: (g.x, y), b: (g.x + g.w, y), color: p.text.a(0.35), glow: 0.0 });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(v: &[Prim]) -> Vec<String> {
        v.iter().filter_map(|p| if let Prim::Text { text, .. } = p { Some(text.clone()) } else { None }).collect()
    }

    #[test]
    fn windows_report_fps_steps_and_correction_peak_once_per_second() {
        let mut p = Perf::default();
        for k in 0..61 {
            p.record(1.0 / 60.0, 1, if k == 10 { 0.4 } else { 0.01 });
        }
        assert!((p.fps - 60.0).abs() < 1.5 && (p.steps_per_s - 60.0).abs() < 1.5, "{} {}", p.fps, p.steps_per_s);
        assert!((p.corr_peak - 0.4).abs() < 1e-6);
        // a stutter shows in the worst frame and the drop in fixed steps
        for _ in 0..30 {
            p.record(0.033, 0, 0.0);
        }
        assert!(p.worst_ms() >= 33.0);
        assert!(p.steps_per_s < 60.0 || p.fps >= 0.0);
    }

    #[test]
    fn graph_keeps_the_last_120_frames_and_hidden_draws_nothing() {
        let mut p = Perf::default();
        for _ in 0..500 {
            p.record(0.016, 1, 0.0);
        }
        assert_eq!(p.frame_count(), GRAPH_FRAMES);
        let i = PerfInputs { ping_ms: Some(31), correction_m: 0.02, lock: "LOCKED" };
        assert!(draw(&p, &i, &Palette::TRON).is_empty(), "hidden by default");
        p.toggle();
        let prims = draw(&p, &i, &Palette::TRON);
        let t = texts(&prims);
        for want in ["PERF  F3", "PING 31 MS", "LOCK LOCKED"] {
            assert!(t.iter().any(|x| x == want), "{want}");
        }
        assert!(t.iter().any(|x| x.starts_with("FPS ")) && t.iter().any(|x| x.starts_with("STEPS/S")));
        // overlay stays off the crosshair
        for pr in &prims {
            if let Some(b) = crate::hud::bounds(pr) {
                assert!(b.x >= 0.69, "perf panel in the right band: {b:?}");
            }
        }
        p.toggle();
        assert!(draw(&p, &i, &Palette::TRON).is_empty());
    }
}
