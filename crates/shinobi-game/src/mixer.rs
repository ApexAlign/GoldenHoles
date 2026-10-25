//! Pure audio mixing logic (no bevy, no sound card, unit-tested): volume buses and their on-disk form, the
//! per-sound rate limiter / loudness budget that keeps spam from clipping, music ducking, crossfade curves,
//! distance falloff, wind-by-height, footstep cadence, and the music playlist (folder scan, shuffle, loop).

#![cfg_attr(not(feature = "audio"), allow(dead_code))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::synth::Sound;

/// Volume buses. Master and mute sit on top of all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bus {
    Sfx,
    Music,
    Ambient,
}

/// Master / sfx / music / ambient volumes (linear 0..=1), mute and the music on/off switch.
#[derive(bevy::prelude::Resource, Clone, Copy, Debug, PartialEq)]
pub struct AudioSettings {
    /// Master, 0..=1 linear (`/vol`).
    pub volume: f32,
    pub muted: bool,
    pub sfx: f32,
    pub music: f32,
    pub ambient: f32,
    pub music_on: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self { volume: 0.7, muted: false, sfx: 1.0, music: 0.5, ambient: 0.6, music_on: true }
    }
}

impl AudioSettings {
    /// Master gain actually applied (0 when muted).
    pub fn gain(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            self.volume.clamp(0.0, 1.0)
        }
    }

    /// Gain of one bus alone (master is applied globally, so it is not in here). Music is 0 when switched off.
    pub fn bus(&self, b: Bus) -> f32 {
        match b {
            Bus::Sfx => self.sfx,
            Bus::Music if !self.music_on => 0.0,
            Bus::Music => self.music,
            Bus::Ambient => self.ambient,
        }
        .clamp(0.0, 1.0)
    }

    pub fn set_bus(&mut self, b: Bus, v: f32) {
        let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { return };
        match b {
            Bus::Sfx => self.sfx = v,
            Bus::Music => self.music = v,
            Bus::Ambient => self.ambient = v,
        }
    }

    /// `key=value` lines (the file next to `settings.cfg`).
    pub fn to_text(&self) -> String {
        format!(
            "volume={:.3}\nmuted={}\nsfx={:.3}\nmusic={:.3}\nambient={:.3}\nmusic_on={}\n",
            self.volume, self.muted, self.sfx, self.music, self.ambient, self.music_on
        )
    }

    /// Lenient parse: unknown keys and bad values keep the default.
    pub fn from_text(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let num = v.parse::<f32>().ok().filter(|n| n.is_finite()).map(|n| n.clamp(0.0, 1.0));
            let flag = v.parse::<bool>().ok();
            match k {
                "volume" => s.volume = num.unwrap_or(s.volume),
                "sfx" => s.sfx = num.unwrap_or(s.sfx),
                "music" => s.music = num.unwrap_or(s.music),
                "ambient" => s.ambient = num.unwrap_or(s.ambient),
                "muted" => s.muted = flag.unwrap_or(s.muted),
                "music_on" => s.music_on = flag.unwrap_or(s.music_on),
                _ => {}
            }
        }
        s
    }
}

/// Where the audio prefs live: `audio.cfg` beside `settings.cfg`.
pub fn prefs_path() -> Option<PathBuf> {
    shinobi_client::settings::default_path().map(|p| p.with_file_name("audio.cfg"))
}

pub fn load_prefs() -> AudioSettings {
    prefs_path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| AudioSettings::from_text(&t)).unwrap_or_default()
}

pub fn save_prefs(s: &AudioSettings) {
    let Some(p) = prefs_path() else { return };
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Err(e) = std::fs::write(&p, s.to_text()) {
        eprintln!("audio: could not save {}: {e}", p.display());
    }
}

/// Which bus a sound plays on.
pub fn bus_of(s: Sound) -> Bus {
    match s {
        Sound::Wind | Sound::Hum => Bus::Ambient,
        _ => Bus::Sfx,
    }
}

// ---------------------------------------------------------------- rate limiting

/// Shortest time between two plays of the same sound (seconds).
pub fn min_gap(s: Sound) -> f32 {
    match s {
        Sound::HitBody | Sound::HitHard => 0.04,
        // being hit: one clear cue per ~0.1 s, never a buzz (the cuer also plays only the worst hit of a frame)
        Sound::Hurt | Sound::HurtHeavy | Sound::HurtHead => 0.1,
        Sound::Whoosh => 0.12,
        Sound::Clang => 0.09,
        Sound::GuardBreak => 0.5,
        Sound::Heartbeat => 0.25,
        Sound::Clash | Sound::Block | Sound::Parry => 0.06,
        Sound::Print | Sound::Derez => 0.05,
        Sound::Ricochet => 0.08,
        Sound::Step => 0.12,
        Sound::RopePulse => 0.2,
        Sound::Lockout => 0.3,
        Sound::CrackStone | Sound::CrackWood => 0.05,
        Sound::Shatter | Sound::ShatterFar | Sound::Collapse | Sound::CollapseFar => 0.04,
        Sound::Reform => 0.06,
        Sound::BoltWhine => 0.08,
        Sound::Thunder | Sound::ThunderFar => 0.15,
        Sound::GuardLow => 0.2,
        Sound::UiClick | Sound::UiBack => 0.04,
        Sound::Jump | Sound::DoubleJump | Sound::Land | Sound::LandHard => 0.08,
        _ => 0.02,
    }
}

/// Sounds that must never be dropped by the loudness budget (they may still be turned down a little).
pub fn priority(s: Sound) -> bool {
    matches!(
        s,
        Sound::Kill
            | Sound::Hurt
            | Sound::HurtHeavy
            | Sound::HurtHead
            | Sound::Whoosh
            | Sound::Clang
            | Sound::GuardBreak
            | Sound::Heartbeat
            | Sound::FlashBang | Sound::WhipCrack
            | Sound::DeathSting | Sound::Respawn | Sound::MatchStart | Sound::Victory | Sound::Defeat | Sound::Thunder | Sound::ThunderFar | Sound::Parry | Sound::Streak | Sound::Execute
            | Sound::FlowTierUp | Sound::BountyKill | Sound::BecomeBounty | Sound::StealthKill
    )
}

/// Loudness budget: how much "volume" of cues may start per second before new ones are turned down.
const LOAD_CAP: f32 = 3.0;
const LOAD_DECAY: f32 = 8.0;
/// Below this share of the budget a non-priority cue is dropped instead of played quietly.
const DROP_BELOW: f32 = 0.4;
/// Lowest volume scale an alert cue (`hit_audio::is_alert`) is turned down to.
const ALERT_FLOOR: f32 = 0.75;

/// Per-sound minimum gap + a soft loudness budget over all sounds, so a flurry of hits never stacks into clipping.
#[derive(Default)]
pub struct Limiter {
    now: f32,
    last: HashMap<Sound, f32>,
    load: f32,
}

impl Limiter {
    pub fn tick(&mut self, dt: f32) {
        self.now += dt.max(0.0);
        self.load = (self.load - dt.max(0.0) * LOAD_DECAY).max(0.0);
        if self.last.len() > 256 {
            let now = self.now;
            self.last.retain(|_, t| now - *t < 5.0);
        }
    }

    /// `Some(volume scale)` to play a cue of loudness `vol` now, `None` to drop it.
    pub fn admit(&mut self, s: Sound, vol: f32) -> Option<f32> {
        if let Some(t) = self.last.get(&s) {
            if self.now - t < min_gap(s) {
                return None;
            }
        }
        let vol = vol.max(1e-3);
        // headroom left in the budget, as a share of this cue's loudness
        let scale = ((LOAD_CAP - self.load) / vol).clamp(0.0, 1.0);
        if scale < DROP_BELOW && !priority(s) {
            return None;
        }
        // hit / shield / near-miss alerts are never turned down below 3/4: they must cut through a flurry
        let floor = if crate::hit_audio::is_alert(s) && priority(s) { ALERT_FLOOR } else { DROP_BELOW };
        let scale = scale.max(floor);
        self.last.insert(s, self.now);
        self.load += vol * scale;
        Some(scale)
    }
}

// ---------------------------------------------------------------- ducking, crossfade, falloff

/// Music duck: big events push the music down fast, it comes back over ~1.2 s.
pub struct Ducker {
    depth: f32,
    smooth: f32,
}

impl Default for Ducker {
    fn default() -> Self {
        Self { depth: 0.0, smooth: 1.0 }
    }
}

pub const DUCK_RELEASE_SECS: f32 = 1.2;
/// Music level at full duck.
pub const DUCK_FLOOR: f32 = 0.35;

impl Ducker {
    /// `amount` 0..=1 (1 = full duck).
    pub fn hit(&mut self, amount: f32) {
        self.depth = self.depth.max(amount.clamp(0.0, 1.0));
    }

    /// Music gain multiplier this frame (1 = no duck).
    pub fn update(&mut self, dt: f32) -> f32 {
        self.depth = (self.depth - dt / DUCK_RELEASE_SECS).max(0.0);
        let target = 1.0 - (1.0 - DUCK_FLOOR) * self.depth;
        self.smooth += (target - self.smooth) * (dt * 18.0).min(1.0);
        self.smooth
    }
}

/// Equal-power crossfade gains `(outgoing, incoming)` at progress `t / dur`.
pub fn crossfade(t: f32, dur: f32) -> (f32, f32) {
    let u = if dur <= 0.0 { 1.0 } else { (t / dur).clamp(0.0, 1.0) };
    ((u * std::f32::consts::FRAC_PI_2).cos(), (u * std::f32::consts::FRAC_PI_2).sin())
}

/// Hearing range of a remote sound, metres.
pub const HEAR_RANGE: f32 = 70.0;

/// Loudness of a remote sound `dist` metres away: close to 1 near you, gone at `HEAR_RANGE`.
pub fn falloff(dist: f32) -> f32 {
    falloff_range(dist, HEAR_RANGE)
}

/// `falloff` for a sound that carries `range` metres (the "loud inside" radius is a tenth of it): big crashes reach further.
pub fn falloff_range(dist: f32, range: f32) -> f32 {
    if !dist.is_finite() || !(range > 0.0) || dist >= range {
        return 0.0;
    }
    let near = 1.0 / (1.0 + (dist.max(0.0) / (range / 10.0)).powi(2));
    let edge = 1.0 - (dist / range).powi(2);
    (near * edge).clamp(0.0, 1.0)
}

/// Wind bed level by height above the ground: a whisper at street level, full roar on the rooftops.
pub fn wind_gain(height: f32) -> f32 {
    let u = ((height - 4.0) / 90.0).clamp(0.0, 1.0);
    0.06 + 0.94 * u * u * (3.0 - 2.0 * u)
}

/// Seconds between footsteps at horizontal speed `speed` (m/s); `None` when too slow to step.
pub fn step_interval(speed: f32) -> Option<f32> {
    if !speed.is_finite() || speed < 1.5 {
        return None;
    }
    Some((2.2 / speed).clamp(0.2, 0.55))
}

// ---------------------------------------------------------------- music

/// File extensions the music slot picks up.
pub const MUSIC_EXTS: [&str; 4] = ["ogg", "mp3", "wav", "flac"];

/// Folders to look in, best first: `SHINOBI_MUSIC_DIR`, `music/` next to the binary, `music/` in the working
/// directory (the repo root when run from there or from `crates/shinobi-game`).
pub fn music_dirs(env: Option<&str>, exe_dir: Option<&Path>, cwd: Option<&Path>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if let Some(e) = env.filter(|e| !e.trim().is_empty()) {
        v.push(PathBuf::from(e.trim()));
    }
    if let Some(d) = exe_dir {
        v.push(d.join("music"));
        // target/release/shinobi-game -> repo/music
        for up in d.ancestors().skip(1).take(4) {
            if up.join("Cargo.toml").exists() || up.join("music").is_dir() {
                v.push(up.join("music"));
            }
        }
    }
    if let Some(c) = cwd {
        v.push(c.join("music"));
        v.push(c.join("..").join("..").join("music"));
    }
    let mut seen = Vec::new();
    v.retain(|p| {
        let k = p.components().collect::<PathBuf>();
        let new = !seen.contains(&k);
        seen.push(k);
        new
    });
    v
}

/// Every music file in the first folder that has any (sorted, so the list is stable); empty when none.
pub fn scan_music(dirs: &[PathBuf]) -> Vec<PathBuf> {
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()).map_or(false, |x| MUSIC_EXTS.contains(&x.to_ascii_lowercase().as_str())))
            .collect();
        files.sort();
        if !files.is_empty() {
            return files;
        }
    }
    Vec::new()
}

/// Shuffled endless playlist: every track once per pass, a new order each pass, never the same track twice in a row.
pub struct Playlist {
    tracks: Vec<PathBuf>,
    order: Vec<usize>,
    pos: usize,
    rng: u32,
    last: Option<usize>,
}

impl Playlist {
    pub fn new(tracks: Vec<PathBuf>, seed: u32) -> Self {
        Self { tracks, order: Vec::new(), pos: 0, rng: seed | 1, last: None }
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    fn rand(&mut self) -> u32 {
        self.rng = self.rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.rng >> 8
    }

    fn reshuffle(&mut self) {
        self.order = (0..self.tracks.len()).collect();
        for i in (1..self.order.len()).rev() {
            let j = self.rand() as usize % (i + 1);
            self.order.swap(i, j);
        }
        if self.order.len() > 1 && Some(self.order[0]) == self.last {
            self.order.swap(0, 1);
        }
        self.pos = 0;
    }

    pub fn next(&mut self) -> Option<&Path> {
        if self.tracks.is_empty() {
            return None;
        }
        if self.pos >= self.order.len() {
            self.reshuffle();
        }
        let i = self.order[self.pos];
        self.pos += 1;
        self.last = Some(i);
        Some(&self.tracks[i])
    }

    /// Drop a track that failed to decode so it is not picked again.
    pub fn remove(&mut self, p: &Path) {
        self.tracks.retain(|t| t != p);
        self.order.clear();
        self.pos = 0;
    }
}

/// When to start the next track so it crossfades in: `xfade` seconds before the end (`None` duration = unknown,
/// wait for the sink to run dry).
pub fn start_next_at(duration: Option<f32>, xfade: f32) -> Option<f32> {
    duration.map(|d| (d - xfade).max(d * 0.5))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buses_clamp_mute_and_music_switch() {
        let mut s = AudioSettings::default();
        s.set_bus(Bus::Music, 3.0);
        assert_eq!(s.bus(Bus::Music), 1.0);
        s.set_bus(Bus::Sfx, f32::NAN);
        assert_eq!(s.bus(Bus::Sfx), 1.0, "NaN ignored");
        s.music_on = false;
        assert_eq!(s.bus(Bus::Music), 0.0);
        assert_eq!(AudioSettings { muted: true, ..Default::default() }.gain(), 0.0);
        assert_eq!(AudioSettings { volume: 3.0, ..Default::default() }.gain(), 1.0);
        assert_eq!(AudioSettings::default().gain(), 0.7);
    }

    #[test]
    fn prefs_round_trip_and_survive_junk() {
        let s = AudioSettings { volume: 0.4, muted: true, sfx: 0.8, music: 0.2, ambient: 0.1, music_on: false };
        assert_eq!(AudioSettings::from_text(&s.to_text()), AudioSettings { volume: 0.4, muted: true, sfx: 0.8, music: 0.2, ambient: 0.1, music_on: false });
        let j = AudioSettings::from_text("volume=loud\nnonsense\nmuted=maybe\nsfx=9\nfoo=1\n");
        assert_eq!(j.volume, AudioSettings::default().volume);
        assert_eq!(j.muted, false);
        assert_eq!(j.sfx, 1.0);
    }

    #[test]
    fn same_sound_is_rate_limited_but_other_sounds_pass() {
        let mut l = Limiter::default();
        assert!(l.admit(Sound::Step, 0.5).is_some());
        assert!(l.admit(Sound::Step, 0.5).is_none(), "same frame, same sound");
        assert!(l.admit(Sound::HitBody, 0.5).is_some());
        l.tick(0.05);
        assert!(l.admit(Sound::Step, 0.5).is_none(), "gap not over");
        l.tick(0.1);
        assert!(l.admit(Sound::Step, 0.5).is_some());
    }

    #[test]
    fn spam_never_exceeds_the_loudness_budget() {
        let mut l = Limiter::default();
        let sounds = [Sound::Throw, Sound::Swing, Sound::Dash, Sound::Jump, Sound::GrappleFire, Sound::Whiff, Sound::Slide, Sound::Whirl];
        let mut total = 0.0;
        for s in sounds {
            if let Some(k) = l.admit(s, 1.0) {
                total += k;
            }
        }
        assert!(total <= LOAD_CAP + 1e-3, "summed gain {total} over budget");
        // far fewer than 8 loud cues got through at full level
        assert!(sounds.iter().filter(|s| l.last.contains_key(*s)).count() < 8);
    }

    #[test]
    fn alerts_outrank_footsteps_in_a_flurry() {
        let mut l = Limiter::default();
        for s in [Sound::HitBody, Sound::Print, Sound::Clash, Sound::Ricochet, Sound::Lockout, Sound::Refill, Sound::Whirl, Sound::AirSlam] {
            l.admit(s, 1.0);
        }
        assert!(l.admit(Sound::Step, 0.5).is_none(), "a footstep is dropped in a flurry");
        let s = l.admit(Sound::HurtHeavy, 1.0).expect("a hit alert is never dropped");
        assert!(s >= 0.75, "and stays loud ({s})");
        assert!(priority(Sound::Whoosh) && priority(Sound::Clang) && priority(Sound::GuardBreak));
        assert!(min_gap(Sound::Hurt) >= 0.09 && min_gap(Sound::Whoosh) >= 0.1 && min_gap(Sound::GuardBreak) >= 0.4);
    }

    #[test]
    fn priority_sounds_are_turned_down_never_dropped() {
        let mut l = Limiter::default();
        for s in [Sound::Throw, Sound::Swing, Sound::Dash, Sound::Whirl] {
            l.admit(s, 1.0);
        }
        let k = l.admit(Sound::Kill, 1.0).expect("kill always plays");
        assert!(k >= DROP_BELOW && k < 1.0);
    }

    #[test]
    fn budget_recovers_with_time() {
        let mut l = Limiter::default();
        for s in [Sound::Throw, Sound::Swing, Sound::Dash, Sound::Whirl] {
            l.admit(s, 1.0);
        }
        l.tick(1.0);
        assert_eq!(l.admit(Sound::Whiff, 1.0), Some(1.0));
    }

    #[test]
    fn duck_drops_fast_and_recovers() {
        let mut d = Ducker::default();
        assert_eq!(d.update(0.016), 1.0);
        d.hit(1.0);
        let mut g = 1.0;
        for _ in 0..10 {
            g = d.update(0.016);
        }
        assert!(g < 0.6, "ducked ({g})");
        for _ in 0..200 {
            g = d.update(0.016);
        }
        assert!(g > 0.99, "recovered ({g})");
    }

    #[test]
    fn crossfade_is_equal_power() {
        for i in 0..=10 {
            let (o, n) = crossfade(i as f32, 10.0);
            assert!((o * o + n * n - 1.0).abs() < 1e-5);
        }
        assert_eq!(crossfade(0.0, 4.0).1, 0.0);
        assert!((crossfade(4.0, 4.0).0).abs() < 1e-6);
        assert_eq!(crossfade(1.0, 0.0), (crossfade(5.0, 0.0).0, 1.0));
    }

    #[test]
    fn falloff_is_monotone_and_zero_out_of_range() {
        assert!((falloff(0.0) - 1.0).abs() < 1e-6);
        let mut last = 2.0;
        for d in (0..70).step_by(5) {
            let f = falloff(d as f32);
            assert!(f <= last);
            last = f;
        }
        assert_eq!(falloff(HEAR_RANGE), 0.0);
        assert_eq!(falloff(f32::NAN), 0.0);
        assert!(falloff(5.0) > falloff(30.0) * 3.0, "a near fight is much louder than a far one");
    }

    #[test]
    fn falloff_range_matches_default_and_reaches_further() {
        for d in [0.0, 3.0, 20.0, 69.0, 80.0] {
            assert_eq!(falloff(d), falloff_range(d, HEAR_RANGE));
        }
        assert!(falloff_range(100.0, 200.0) > 0.0);
        assert_eq!(falloff_range(10.0, 0.0), 0.0);
    }

    #[test]
    fn wind_scales_with_height() {
        assert!(wind_gain(0.0) < 0.1);
        assert!(wind_gain(40.0) > wind_gain(10.0));
        assert!(wind_gain(200.0) > 0.99);
        assert_eq!(wind_gain(94.0), wind_gain(500.0));
    }

    #[test]
    fn footsteps_quicken_with_speed_and_stop_when_slow() {
        assert_eq!(step_interval(0.5), None);
        let walk = step_interval(4.0).unwrap();
        let run = step_interval(9.0).unwrap();
        assert!(run < walk);
        assert!(step_interval(100.0).unwrap() >= 0.2);
    }

    #[test]
    fn playlist_plays_each_track_per_pass_and_never_repeats_back_to_back() {
        let tracks: Vec<PathBuf> = (0..5).map(|i| PathBuf::from(format!("t{i}.ogg"))).collect();
        let mut p = Playlist::new(tracks.clone(), 42);
        let mut last: Option<PathBuf> = None;
        for _ in 0..4 {
            let mut pass: Vec<PathBuf> = Vec::new();
            for _ in 0..5 {
                let t = p.next().unwrap().to_path_buf();
                assert_ne!(Some(&t), last.as_ref(), "no immediate repeat");
                last = Some(t.clone());
                pass.push(t);
            }
            pass.sort();
            assert_eq!(pass, tracks, "every track once per pass");
        }
        p.remove(Path::new("t0.ogg"));
        assert_eq!(p.len(), 4);
        for _ in 0..8 {
            assert_ne!(p.next().unwrap(), Path::new("t0.ogg"));
        }
        assert!(Playlist::new(vec![], 1).next().is_none(), "an empty folder is fine");
    }

    #[test]
    fn single_track_loops() {
        let mut p = Playlist::new(vec![PathBuf::from("a.wav")], 7);
        for _ in 0..3 {
            assert_eq!(p.next().unwrap(), Path::new("a.wav"));
        }
    }

    #[test]
    fn music_dirs_put_the_env_var_first_and_dedupe() {
        let d = music_dirs(Some("/tmp/my music"), Some(Path::new("/opt/game")), Some(Path::new("/opt/game")));
        assert_eq!(d[0], PathBuf::from("/tmp/my music"));
        assert_eq!(d.iter().filter(|p| **p == PathBuf::from("/opt/game/music")).count(), 1);
        assert!(music_dirs(Some("  "), None, None).is_empty());
    }

    #[test]
    fn scan_picks_audio_files_only_and_tolerates_missing_or_empty_dirs() {
        let dir = std::env::temp_dir().join(format!("shinobi-music-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(scan_music(&[dir.clone(), dir.join("nope")]).is_empty(), "empty folder");
        for n in ["b.OGG", "a.mp3", "c.wav", "notes.txt", "cover.png"] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        let found = scan_music(&[PathBuf::from("/definitely/not/here"), dir.clone()]);
        let names: Vec<_> = found.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["a.mp3", "b.OGG", "c.wav"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn next_track_starts_before_the_end_for_a_crossfade() {
        assert_eq!(start_next_at(Some(180.0), 4.0), Some(176.0));
        assert_eq!(start_next_at(Some(6.0), 4.0), Some(3.0), "short tracks still play half");
        assert_eq!(start_next_at(None, 4.0), None);
    }

    #[test]
    fn ambient_sounds_are_on_their_own_bus() {
        assert_eq!(bus_of(Sound::Wind), Bus::Ambient);
        assert_eq!(bus_of(Sound::Hum), Bus::Ambient);
        assert_eq!(bus_of(Sound::Kill), Bus::Sfx);
    }
}
