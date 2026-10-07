//! Player settings: clamped values, defaults, and a plain `key=value` text form (std only, no serde).
//!
//! The text format is forgiving on read so an old or hand-edited file never bricks the client: unknown keys
//! and bad values are skipped (returned as warnings) and the default stays in place. Non-finite floats are
//! rejected. Writing is stable (fixed key order) and round-trips exactly.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::camera::CameraFeel;
use crate::input::{Aim, TouchController};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    /// Mouse look, radians per device unit (Pointer Lock `movementX/Y`).
    pub mouse_sens: f32,
    /// Touch aim pad, radians per pixel of drag.
    pub aim_pad_sens: f32,
    /// Floating stick travel for full deflection, fraction of screen height.
    pub stick_radius_frac: f32,
    /// Stick deadzone, fraction of the stick radius.
    pub stick_deadzone: f32,
    /// Base vertical FOV, degrees (feeds `FpsCameraCfg::base_fov`).
    pub fov_deg: f32,
    pub invert_y: bool,
    /// Mirror the touch layout: buttons + aim pad on the left, stick on the right.
    pub left_handed: bool,
    /// Strength of camera/screen feedback (wall-run tilt, jump thrust, FOV kicks, shake, lens warp), 0..=1.
    pub fx_intensity: f32,
    /// Accessibility: no roll, thrust, FOV kicks, shake or lens warp, whatever `fx_intensity` says.
    pub reduced_motion: bool,
    /// Lens-warp (CRT pass) share of the FX intensity, 0..=1.
    pub crt_warp: f32,
    /// Subtle vertical camera bob while running. Off by default; scaled by the FX intensity, zero in reduced motion.
    pub head_bob: bool,
    /// HUD size multiplier (`HUD_SCALE` range). The shell multiplies it by 1.15 on windows 800 px tall or less.
    pub hud_scale: f32,
    /// Times each contextual hint was shown (`hud_energy::HintId` order); a hint stops after `HINT_MAX_SHOWS`.
    pub hints_seen: [u8; 4],
}

pub const MOUSE_SENS: (f32, f32, f32) = (0.0002, 0.02, 0.0012); // (min, max, default)
pub const AIM_PAD_SENS: (f32, f32, f32) = (0.001, 0.02, 0.006);
pub const STICK_RADIUS: (f32, f32, f32) = (0.06, 0.25, 0.13);
pub const STICK_DEADZONE: (f32, f32, f32) = (0.0, 0.5, 0.12);
pub const FOV: (f32, f32, f32) = (80.0, 110.0, 90.0);

/// Mouse DPI the cm/360 readout assumes (the pointer API gives counts, not distance).
pub const ASSUMED_DPI: f32 = 800.0;

/// Centimetres of mouse travel for a full turn at `sens` radians per count (at `ASSUMED_DPI`).
pub fn cm_per_360(sens: f32) -> f32 {
    (std::f32::consts::TAU / sens.max(1e-6)) / (ASSUMED_DPI / 2.54)
}
pub const FX_INTENSITY: (f32, f32, f32) = (0.0, 1.0, 1.0);
pub const CRT_WARP: (f32, f32, f32) = (0.0, 1.0, 1.0);
pub const HUD_SCALE: (f32, f32, f32) = (0.8, 1.4, 1.0);

/// Windows this tall (px) or shorter get the HUD enlarged by `SMALL_SCREEN_BOOST` on top of the setting.
pub const SMALL_SCREEN_HEIGHT: f32 = 800.0;
pub const SMALL_SCREEN_BOOST: f32 = 1.15;

/// The HUD scale actually applied: the setting, boosted on small windows.
pub fn effective_hud_scale(setting: f32, window_height_px: f32) -> f32 {
    let s = if setting.is_finite() { setting.clamp(HUD_SCALE.0, HUD_SCALE.1) } else { HUD_SCALE.2 };
    if window_height_px > 0.0 && window_height_px <= SMALL_SCREEN_HEIGHT { s * SMALL_SCREEN_BOOST } else { s }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mouse_sens: MOUSE_SENS.2,
            aim_pad_sens: AIM_PAD_SENS.2,
            stick_radius_frac: STICK_RADIUS.2,
            stick_deadzone: STICK_DEADZONE.2,
            fov_deg: FOV.2,
            invert_y: false,
            left_handed: false,
            fx_intensity: FX_INTENSITY.2,
            reduced_motion: false,
            crt_warp: CRT_WARP.2,
            head_bob: false,
            hud_scale: HUD_SCALE.2,
            hints_seen: [0; 4],
        }
    }
}

fn clamp((lo, hi, def): (f32, f32, f32), v: f32) -> f32 {
    if v.is_finite() { v.clamp(lo, hi) } else { def }
}

impl Settings {
    /// Pull every value into its legal range (NaN / inf -> default). Call after any UI edit.
    pub fn clamped(mut self) -> Self {
        self.mouse_sens = clamp(MOUSE_SENS, self.mouse_sens);
        self.aim_pad_sens = clamp(AIM_PAD_SENS, self.aim_pad_sens);
        self.stick_radius_frac = clamp(STICK_RADIUS, self.stick_radius_frac);
        self.stick_deadzone = clamp(STICK_DEADZONE, self.stick_deadzone);
        self.fov_deg = clamp(FOV, self.fov_deg);
        self.fx_intensity = clamp(FX_INTENSITY, self.fx_intensity);
        self.crt_warp = clamp(CRT_WARP, self.crt_warp);
        self.hud_scale = clamp(HUD_SCALE, self.hud_scale);
        self
    }

    /// Camera/FX strengths for `FpsCamera::feel` and the shell's `FxState` scaling.
    pub fn feel(&self) -> CameraFeel {
        let s = self.clamped();
        if s.reduced_motion {
            return CameraFeel::OFF;
        }
        let k = s.fx_intensity;
        CameraFeel { roll: k, thrust: k, fov_kick: k, shake: k, warp: k * s.crt_warp }
    }

    pub fn to_text(&self) -> String {
        let s = self.clamped();
        format!(
            "mouse_sens={}\naim_pad_sens={}\nstick_radius={}\nstick_deadzone={}\nfov={}\ninvert_y={}\nleft_handed={}\nfx_intensity={}\nreduced_motion={}\ncrt_warp={}\nhead_bob={}\nhud_scale={}\nhints_seen={},{},{},{}\n",
            s.mouse_sens, s.aim_pad_sens, s.stick_radius_frac, s.stick_deadzone, s.fov_deg, s.invert_y, s.left_handed,
            s.fx_intensity, s.reduced_motion, s.crt_warp, s.head_bob, s.hud_scale,
            s.hints_seen[0], s.hints_seen[1], s.hints_seen[2], s.hints_seen[3]
        )
    }

    /// Parse `key=value` lines (`#` comments, blank lines, surrounding spaces ok). Returns the settings and
    /// one warning per skipped line. Out-of-range numbers are clamped, not rejected.
    pub fn from_text(text: &str) -> (Self, Vec<String>) {
        let mut s = Self::default();
        let mut warn = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                warn.push(format!("line {}: expected key=value", i + 1));
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            let num = || v.parse::<f32>().ok().filter(|f| f.is_finite());
            let flag = || match v {
                "true" | "1" | "on" => Some(true),
                "false" | "0" | "off" => Some(false),
                _ => None,
            };
            let ok = match k {
                "mouse_sens" => num().map(|f| s.mouse_sens = f).is_some(),
                "aim_pad_sens" => num().map(|f| s.aim_pad_sens = f).is_some(),
                "stick_radius" => num().map(|f| s.stick_radius_frac = f).is_some(),
                "stick_deadzone" => num().map(|f| s.stick_deadzone = f).is_some(),
                "fov" => num().map(|f| s.fov_deg = f).is_some(),
                "invert_y" => flag().map(|b| s.invert_y = b).is_some(),
                "left_handed" => flag().map(|b| s.left_handed = b).is_some(),
                "fx_intensity" => num().map(|f| s.fx_intensity = f).is_some(),
                "reduced_motion" => flag().map(|b| s.reduced_motion = b).is_some(),
                "crt_warp" => num().map(|f| s.crt_warp = f).is_some(),
                "head_bob" => flag().map(|b| s.head_bob = b).is_some(),
                "hud_scale" => num().map(|f| s.hud_scale = f).is_some(),
                "hints_seen" => {
                    let v: Vec<u8> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    (v.len() == 4).then(|| s.hints_seen.copy_from_slice(&v)).is_some()
                }
                _ => {
                    warn.push(format!("line {}: unknown key '{k}'", i + 1));
                    continue;
                }
            };
            if !ok {
                warn.push(format!("line {}: bad value '{v}' for '{k}'", i + 1));
            }
        }
        (s.clamped(), warn)
    }

    // ---- persistence (std only) ----

    /// Load settings from `path`. Never fails: a missing file is the first run (defaults, no warning); an
    /// unreadable, non-UTF-8 or absurdly large file is ignored (defaults plus one warning) so a corrupt config
    /// can never keep the game from starting; a readable file goes through the forgiving `from_text`.
    pub fn load_from(path: &Path) -> (Self, Vec<String>) {
        const MAX_BYTES: u64 = 64 * 1024;
        let name = path.display();
        match std::fs::metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), Vec::new()),
            Err(e) => return (Self::default(), vec![format!("{name}: {e}; using defaults")]),
            Ok(m) if m.len() > MAX_BYTES => return (Self::default(), vec![format!("{name}: {} bytes is not a settings file; using defaults", m.len())]),
            Ok(_) => {}
        }
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => return (Self::default(), vec![format!("{name}: {e}; using defaults")]),
        };
        match String::from_utf8(bytes) {
            Ok(text) => Self::from_text(&text),
            Err(_) => (Self::default(), vec![format!("{name}: not valid UTF-8; using defaults")]),
        }
    }

    /// Save to `path` atomically: write a sibling temp file, flush it to disk, then rename over the target, so a
    /// crash or power loss leaves either the old file or the new one, never half of one. Creates parent
    /// directories. The temp file is removed on failure.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let name = path.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
        let tmp = dir.join(format!(".{}.{}.tmp", name.to_string_lossy(), std::process::id()));
        let write = || -> std::io::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(self.to_text().as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, path) // replaces an existing file on Unix and Windows
        };
        let r = write();
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        r
    }

    /// `load_from(default_path())`; defaults when there is no home/config directory at all.
    pub fn load() -> (Self, Vec<String>) {
        match default_path() {
            Some(p) => Self::load_from(&p),
            None => (Self::default(), Vec::new()),
        }
    }

    /// `save_to(default_path())`.
    pub fn save(&self) -> std::io::Result<()> {
        let p = default_path().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no config directory"))?;
        self.save_to(&p)
    }

    /// Mouse / touch look into `Aim`. `dx`,`dy` are device units (mouse) or px (aim pad), right/down positive.
    pub fn look(&self, aim: &mut Aim, dx: f32, dy: f32, touch: bool) {
        let rad = if touch { self.aim_pad_sens } else { self.mouse_sens };
        aim.look(dx, if self.invert_y { -dy } else { dy }, rad);
    }

    pub fn stick_radius_px(&self, screen_h: f32) -> f32 {
        self.stick_radius_frac * screen_h
    }

    /// Push stick settings into the controller (its `stick_radius` / `deadzone` fields are public).
    pub fn apply_to_touch(&self, tc: &mut TouchController, screen_w: f32, screen_h: f32) {
        tc.screen_w = screen_w;
        tc.stick_radius = self.stick_radius_px(screen_h);
        tc.deadzone = self.stick_deadzone;
        tc.left_handed = self.left_handed;
    }
}

/// Where the settings file lives: `%APPDATA%\\shinobi\\settings.cfg` on Windows, `~/Library/Application
/// Support/shinobi/settings.cfg` on macOS, else `$XDG_CONFIG_HOME` or `~/.config` + `/shinobi/settings.cfg`.
/// `None` when no usable directory variable is set.
pub fn default_path() -> Option<PathBuf> {
    let os = if cfg!(windows) {
        ConfigOs::Windows
    } else if cfg!(target_os = "macos") {
        ConfigOs::Mac
    } else {
        ConfigOs::Unix
    };
    config_path_from(os, &|k| std::env::var(k).ok())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigOs {
    Windows,
    Mac,
    Unix,
}

/// `default_path` with the platform and environment injected (testable). Empty and relative values are ignored:
/// a relative config dir would put the file wherever the process happened to start.
pub fn config_path_from(os: ConfigOs, env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let get = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.is_absolute());
    let base = match os {
        ConfigOs::Windows => get("APPDATA").or_else(|| get("USERPROFILE").map(|p| p.join("AppData").join("Roaming"))),
        ConfigOs::Mac => get("HOME").map(|h| h.join("Library").join("Application Support")),
        ConfigOs::Unix => get("XDG_CONFIG_HOME").or_else(|| get("HOME").map(|h| h.join(".config"))),
    }?;
    Some(base.join("shinobi").join("settings.cfg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_inside_their_own_ranges() {
        let d = Settings::default();
        assert_eq!(d, d.clamped());
    }

    #[test]
    fn text_round_trips_exactly() {
        let s = Settings {
            mouse_sens: 0.0031,
            aim_pad_sens: 0.0087,
            stick_radius_frac: 0.17,
            stick_deadzone: 0.25,
            fov_deg: 103.5,
            invert_y: true,
            left_handed: true,
            fx_intensity: 0.4,
            reduced_motion: true,
            crt_warp: 0.65,
            head_bob: true,
            hud_scale: 1.2,
            hints_seen: [3, 1, 0, 2],
        };
        let (back, warn) = Settings::from_text(&s.to_text());
        assert_eq!(back, s);
        assert!(warn.is_empty());
    }

    #[test]
    fn parse_is_forgiving_and_clamps() {
        let (s, w) = Settings::from_text(
            "# my config\n  fov = 500 # too wide\nmouse_sens=nan\ninvert_y=maybe\nbogus=1\nno equals here\nleft_handed=on\n\nstick_radius=0.001\n",
        );
        assert_eq!(s.fov_deg, FOV.1, "out-of-range clamps");
        assert_eq!(s.mouse_sens, MOUSE_SENS.2, "nan keeps default");
        assert!(!s.invert_y && s.left_handed);
        assert_eq!(s.stick_radius_frac, STICK_RADIUS.0);
        assert_eq!(w.len(), 4, "{w:?}"); // nan, maybe, bogus, no-equals
    }

    #[test]
    fn empty_and_garbage_input_yield_defaults() {
        assert_eq!(Settings::from_text("").0, Settings::default());
        assert_eq!(Settings::from_text("\u{0}\u{1}=\n===").0, Settings::default());
    }

    #[test]
    fn clamped_repairs_non_finite_and_extreme_values() {
        let s = Settings { mouse_sens: f32::INFINITY, fov_deg: -5.0, stick_deadzone: 9.0, ..Default::default() }.clamped();
        assert_eq!(s.mouse_sens, MOUSE_SENS.2);
        assert_eq!(s.fov_deg, FOV.0);
        assert_eq!(s.stick_deadzone, STICK_DEADZONE.1);
    }

    #[test]
    fn invert_y_flips_pitch_only_and_touch_uses_its_own_sensitivity() {
        let mut s = Settings::default();
        let (mut a, mut b) = (Aim::default(), Aim::default());
        s.look(&mut a, 0.0, 10.0, false);
        s.invert_y = true;
        s.look(&mut b, 0.0, 10.0, false);
        assert!(a.pitch < 0.0 && b.pitch > 0.0 && (a.pitch + b.pitch).abs() < 1e-7);
        assert_eq!(a.yaw, b.yaw);
        let (mut m, mut t) = (Aim::default(), Aim::default());
        let s = Settings { mouse_sens: 0.001, aim_pad_sens: 0.004, ..Default::default() };
        s.look(&mut m, 0.0, -10.0, false);
        s.look(&mut t, 0.0, -10.0, true);
        assert!((t.pitch / m.pitch - 4.0).abs() < 1e-4);
    }

    #[test]
    fn apply_to_touch_sets_radius_and_deadzone() {
        let s = Settings { stick_radius_frac: 0.2, stick_deadzone: 0.3, ..Default::default() };
        let mut tc = TouchController::new(100.0, 1.0);
        s.apply_to_touch(&mut tc, 1600.0, 800.0);
        assert_eq!((tc.screen_w, tc.stick_radius, tc.deadzone), (1600.0, 160.0, 0.3));
        assert!(!tc.left_handed);
        Settings { left_handed: true, ..s }.apply_to_touch(&mut tc, 1600.0, 800.0);
        assert!(tc.left_handed, "the setting flips the controller");
    }

    // ---- persistence ----

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("shinobi-settings-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn custom() -> Settings {
        Settings {
            mouse_sens: 0.0031,
            fov_deg: 103.5,
            invert_y: true,
            left_handed: true,
            fx_intensity: 0.4,
            reduced_motion: true,
            crt_warp: 0.65,
            head_bob: true,
            hud_scale: 1.2,
            ..Default::default()
        }
    }

    #[test]
    fn save_then_load_round_trips_and_creates_directories() {
        let d = temp_dir("roundtrip");
        let p = d.join("nested").join("deeper").join("settings.cfg");
        custom().save_to(&p).unwrap();
        let (back, warn) = Settings::load_from(&p);
        assert_eq!(back, custom());
        assert!(warn.is_empty(), "{warn:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_file_is_a_quiet_first_run() {
        let d = temp_dir("missing");
        let (s, warn) = Settings::load_from(&d.join("nope.cfg"));
        assert_eq!(s, Settings::default());
        assert!(warn.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn corrupt_files_fall_back_to_defaults_with_a_warning() {
        let d = temp_dir("corrupt");
        let bin = d.join("bin.cfg");
        std::fs::write(&bin, [0xff, 0xfe, 0x00, 0x80, b'=', 0xc3]).unwrap();
        let (s, w) = Settings::load_from(&bin);
        assert_eq!(s, Settings::default());
        assert!(w.len() == 1 && w[0].contains("UTF-8"), "{w:?}");

        let huge = d.join("huge.cfg");
        std::fs::write(&huge, vec![b'a'; 200_000]).unwrap();
        let (s, w) = Settings::load_from(&huge);
        assert_eq!(s, Settings::default());
        assert!(w.len() == 1 && w[0].contains("not a settings file"), "{w:?}");

        // a directory where the file should be is also just ignored
        let dir_as_file = d.join("dir.cfg");
        std::fs::create_dir_all(&dir_as_file).unwrap();
        let (s, w) = Settings::load_from(&dir_as_file);
        assert_eq!(s, Settings::default());
        assert_eq!(w.len(), 1, "{w:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_truncated_or_partly_garbage_file_keeps_what_it_can() {
        let d = temp_dir("partial");
        let p = d.join("settings.cfg");
        std::fs::write(&p, "fov=110\nmouse_sens=0.00").unwrap(); // cut off mid-value
        let (s, _) = Settings::load_from(&p);
        assert_eq!(s.fov_deg, 110.0);
        assert!(s.mouse_sens > 0.0 && s.mouse_sens <= MOUSE_SENS.1, "clamped, not zero or NaN: {}", s.mouse_sens);
        std::fs::write(&p, "fov=110\n\u{1}garbage\ninvert_y=true\n").unwrap();
        let (s, w) = Settings::load_from(&p);
        assert!(s.invert_y && s.fov_deg == 110.0 && w.len() == 1, "{w:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn save_replaces_atomically_and_leaves_no_temp_files() {
        let d = temp_dir("atomic");
        let p = d.join("settings.cfg");
        Settings::default().save_to(&p).unwrap();
        custom().save_to(&p).unwrap();
        assert_eq!(Settings::load_from(&p).0, custom(), "second save replaced the first");
        let names: Vec<String> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["settings.cfg".to_string()], "no temp file left behind: {names:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_save_reports_the_error_cleans_up_and_keeps_the_old_file() {
        let d = temp_dir("fail");
        let p = d.join("settings.cfg");
        custom().save_to(&p).unwrap();
        // target is a non-empty directory: the rename cannot replace it
        let blocked = d.join("blocked.cfg");
        std::fs::create_dir_all(blocked.join("child")).unwrap();
        assert!(Settings::default().save_to(&blocked).is_err());
        assert!(std::fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp")), "temp file removed");
        assert_eq!(Settings::load_from(&p).0, custom(), "unrelated file untouched");
        // a parent that is a file cannot be created
        assert!(Settings::default().save_to(&p.join("x").join("settings.cfg")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn default_path_follows_platform_conventions() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
        };
        let cfg = |p: &str| PathBuf::from(p).join("shinobi").join("settings.cfg");
        assert_eq!(config_path_from(ConfigOs::Unix, &env(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")])), Some(cfg("/xdg")));
        assert_eq!(config_path_from(ConfigOs::Unix, &env(&[("HOME", "/home/u")])), Some(cfg("/home/u/.config")));
        assert_eq!(config_path_from(ConfigOs::Unix, &env(&[("XDG_CONFIG_HOME", ""), ("HOME", "/home/u")])), Some(cfg("/home/u/.config")), "empty ignored");
        assert_eq!(config_path_from(ConfigOs::Unix, &env(&[("XDG_CONFIG_HOME", "relative/dir"), ("HOME", "/home/u")])), Some(cfg("/home/u/.config")), "relative ignored");
        assert_eq!(config_path_from(ConfigOs::Unix, &env(&[])), None);
        assert_eq!(config_path_from(ConfigOs::Mac, &env(&[("HOME", "/Users/u")])), Some(cfg("/Users/u/Library/Application Support")));
        assert_eq!(config_path_from(ConfigOs::Windows, &env(&[("APPDATA", "/ad"), ("USERPROFILE", "/up")])), Some(cfg("/ad")));
        assert_eq!(config_path_from(ConfigOs::Windows, &env(&[("USERPROFILE", "/up")])), Some(cfg("/up/AppData/Roaming")));
    }
}
