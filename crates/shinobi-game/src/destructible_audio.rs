//! Sound for destructible level boxes: crack ticks, shatter / collapse, re-form shimmer. Diffs `NetClient::world_broken()`
//! itself (it never drains `take_world_fx`, the renderer owns that queue) and polls `world_hp_q` for the few boxes
//! nearest the camera. Cues go to the audio backend through `audio::ExtraCues`. The decision logic is pure and tested here.

#![cfg_attr(not(feature = "audio"), allow(dead_code))]

use std::collections::{BTreeSet, HashMap};

use bevy::prelude::*;
use shinobi_sim::level::LevelBox;

use crate::audio::{Cue, ExtraCues};
use crate::net::{LevelRes, Net, ViewRes};
use crate::synth::Sound;

/// Most destructible voices audible at once; more are skipped.
pub const MAX_VOICES: usize = 6;
/// Breaks farther than this play the muffled copy.
pub const FAR_M: f32 = 90.0;
/// Gap between the breaks of one cascade.
pub const STAGGER: f32 = 0.05;
/// Longest stagger (a cascade of ~10 stays within half a second).
pub const MAX_STAGGER: f32 = 0.5;
/// After joining (or on the first frame) diffs are absorbed silently this long: catch-up breaks arrive late.
pub const SETTLE: f32 = 0.75;
/// Cracks are polled for this many boxes nearest the camera, closer than `CRACK_RADIUS`.
pub const NEAREST: usize = 8;
pub const CRACK_RADIUS: f32 = 80.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Small,
    Big,
}

/// Tall boxes (towers, canyon walls) or tough ones collapse; the rest shatter.
pub fn size_variant(half: [f32; 3], hp: f32) -> Size {
    if half[1] >= 4.0 || hp >= 250.0 {
        Size::Big
    } else {
        Size::Small
    }
}

/// Thin boxes (bamboo) snap, thick ones tick like stone.
pub fn is_wood(half: [f32; 3]) -> bool {
    half[0].min(half[2]) < 0.6
}

/// Playback speed of a crack: higher when fresh, falling as `hp_q` (255 untouched .. 1 nearly gone) falls.
pub fn crack_speed(hp_q: u8) -> f32 {
    0.65 + 0.55 * (hp_q as f32 / 255.0)
}

/// Which break sound, vol and range for a box at `dist` metres.
pub fn break_cue(size: Size, dist: f32) -> (Sound, f32, f32) {
    match (size, dist > FAR_M) {
        (Size::Small, false) => (Sound::Shatter, 1.0, 160.0),
        (Size::Big, false) => (Sound::Collapse, 1.0, 220.0),
        (Size::Small, true) => (Sound::ShatterFar, 0.8, 300.0),
        (Size::Big, true) => (Sound::CollapseFar, 0.8, 300.0),
    }
}

/// Seconds a sound keeps a voice busy.
pub fn voice_secs(s: Sound) -> f32 {
    match s {
        Sound::Collapse | Sound::CollapseFar => 1.6,
        Sound::Shatter | Sound::ShatterFar => 0.8,
        Sound::Reform => 0.5,
        _ => 0.1,
    }
}

/// Audible-voice cap: tracks when each live voice ends.
#[derive(Default)]
pub struct Voices {
    ends: Vec<f32>,
}

impl Voices {
    /// `Some(gain)` (a little ducking as it gets crowded) and the voice is taken; `None` = skip it.
    pub fn admit(&mut self, now: f32, secs: f32) -> Option<f32> {
        self.ends.retain(|e| *e > now);
        if self.ends.len() >= MAX_VOICES {
            return None;
        }
        let gain = (1.0 - 0.1 * self.ends.len() as f32).max(0.5);
        self.ends.push(now + secs);
        Some(gain)
    }
}

/// Diffs the broken set; silent while settling (first frame, joining) so catch-up breaks make no sound.
pub struct Tracker {
    prev: BTreeSet<u16>,
    settle: f32,
    was_joined: bool,
}

impl Default for Tracker {
    fn default() -> Self {
        Self { prev: BTreeSet::new(), settle: SETTLE, was_joined: false }
    }
}

impl Tracker {
    /// `(newly broken, newly re-formed)` since the last call.
    pub fn diff(&mut self, now: &BTreeSet<u16>, joined: bool, dt: f32) -> (Vec<u16>, Vec<u16>) {
        if joined && !self.was_joined {
            self.settle = SETTLE;
        }
        self.was_joined = joined;
        if !joined || self.settle > 0.0 {
            if joined {
                self.settle -= dt.max(0.0);
            }
            self.prev = now.clone();
            return (Vec::new(), Vec::new());
        }
        let broke = now.difference(&self.prev).copied().collect();
        let rez = self.prev.difference(now).copied().collect();
        self.prev = now.clone();
        (broke, rez)
    }

    pub fn settling(&self) -> bool {
        !self.was_joined || self.settle > 0.0
    }
}

/// Delay of the `k`th break of a same-frame cascade of `n`.
pub fn stagger(k: usize, n: usize) -> f32 {
    if n <= 1 {
        0.0
    } else {
        (k as f32 * STAGGER).min(MAX_STAGGER)
    }
}

/// The `n` points closest to `eye` within `radius`, nearest first.
pub fn nearest(points: &[(u16, [f32; 3])], eye: [f32; 3], n: usize, radius: f32) -> Vec<u16> {
    let d2 = |p: &[f32; 3]| (0..3).map(|i| (p[i] - eye[i]).powi(2)).sum::<f32>();
    let mut v: Vec<(f32, u16)> = points.iter().map(|(i, p)| (d2(p), *i)).filter(|(d, _)| *d <= radius * radius).collect();
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v.truncate(n);
    v.into_iter().map(|(_, i)| i).collect()
}

/// Compare the polled `(idx, hp_q)` with the last seen values: returns the boxes whose hp dropped. A box seen for the
/// first time (or after leaving the polled set) is only recorded. `quiet` records without reporting.
pub fn crack_updates(last: &mut HashMap<u16, u8>, polled: &[(u16, u8)], quiet: bool) -> Vec<(u16, u8)> {
    last.retain(|i, _| polled.iter().any(|(p, _)| p == i));
    let mut out = Vec::new();
    for &(i, q) in polled {
        if let Some(prev) = last.insert(i, q) {
            if q < prev && !quiet {
                out.push((i, q));
            }
        }
    }
    out
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Break,
    Rez,
    Crack(u8),
}

#[derive(Resource, Default)]
struct DestructAudio {
    clock: f32,
    tracker: Tracker,
    voices: Voices,
    pending: Vec<(f32, u16, Kind)>,
    hp: HashMap<u16, u8>,
    /// `(idx, centre)` of every destructible box, built from the level on first use.
    dest: Vec<(u16, [f32; 3])>,
    built: bool,
}

pub struct DestructibleAudioPlugin;

impl Plugin for DestructibleAudioPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DestructAudio>().init_resource::<ExtraCues>().add_systems(Update, destructible_sfx);
    }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f32>().sqrt()
}

fn destructible_sfx(time: Res<Time>, net: Res<Net>, view: Res<ViewRes>, level: Res<LevelRes>, mut st: ResMut<DestructAudio>, mut out: ResMut<ExtraCues>) {
    let dt = time.delta_secs();
    st.clock += dt;
    if !st.built {
        st.built = true;
        st.dest = level.0.boxes.iter().enumerate().filter(|(_, b)| b.hp > 0.0).map(|(i, b)| (i as u16, b.center)).collect();
    }
    if st.dest.is_empty() {
        return;
    }
    let eye = view.0.eye;
    let joined = net.0.own_id().is_some();
    let nc = &net.0.game.net;
    let boxes: &[LevelBox] = &level.0.boxes;

    // breaks / re-forms by diffing the broken set (bottom boxes first, so a tower comes down from the foot)
    let (mut broke, rez) = st.tracker.diff(nc.world_broken(), joined, dt);
    broke.retain(|i| boxes.get(*i as usize).is_some_and(|b| b.hp > 0.0));
    broke.sort_by(|a, b| boxes[*a as usize].center[1].total_cmp(&boxes[*b as usize].center[1]));
    let (now, n) = (st.clock, broke.len());
    for (k, i) in broke.into_iter().enumerate() {
        st.pending.push((now + stagger(k, n), i, Kind::Break));
    }
    for i in rez {
        if boxes.get(i as usize).is_some_and(|b| b.hp > 0.0) {
            st.pending.push((now, i, Kind::Rez));
        }
    }

    // cracks: only the nearest few intact boxes are polled
    let quiet = st.tracker.settling();
    let near = nearest(&st.dest, eye, NEAREST, CRACK_RADIUS);
    let polled: Vec<(u16, u8)> = near.into_iter().filter(|i| !nc.world_broken().contains(i)).map(|i| (i, nc.world_hp_q(i))).collect();
    for (i, q) in crack_updates(&mut st.hp, &polled, quiet) {
        st.pending.push((now, i, Kind::Crack(q)));
    }

    // play what is due
    let pending = std::mem::take(&mut st.pending);
    for (at, idx, kind) in pending {
        if at > st.clock {
            st.pending.push((at, idx, kind));
            continue;
        }
        let Some(b) = boxes.get(idx as usize) else { continue };
        let d = dist(eye, b.center);
        let (sound, vol, range, speed) = match kind {
            Kind::Break => {
                let (s, v, r) = break_cue(size_variant(b.half, b.hp), d);
                (s, v, r, 1.0)
            }
            Kind::Rez if d < 60.0 => (Sound::Reform, 0.35, 60.0, 1.0),
            Kind::Rez => continue,
            Kind::Crack(q) => (if is_wood(b.half) { Sound::CrackWood } else { Sound::CrackStone }, 0.7, 50.0, crack_speed(q)),
        };
        // out of earshot: no voice taken
        if vol * crate::mixer::falloff_range(d, range) < 0.02 {
            continue;
        }
        let clock = st.clock;
        let Some(gain) = st.voices.admit(clock, voice_secs(sound)) else { continue };
        out.push(Cue::at(sound, b.center, vol * gain).pitched(speed).ranged(range));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &[u16]) -> BTreeSet<u16> {
        v.iter().copied().collect()
    }

    #[test]
    fn tall_or_tough_boxes_collapse_the_rest_shatter() {
        assert_eq!(size_variant([1.0, 4.0, 1.0], 50.0), Size::Big);
        assert_eq!(size_variant([1.0, 3.9, 1.0], 249.0), Size::Small);
        assert_eq!(size_variant([1.0, 1.0, 1.0], 250.0), Size::Big);
        assert!(is_wood([0.3, 5.0, 0.3]) && !is_wood([2.0, 2.0, 2.0]));
    }

    #[test]
    fn crack_pitch_falls_with_hp_and_stays_in_range() {
        let mut last = f32::MAX;
        for q in (1..=255u16).rev().step_by(17) {
            let s = crack_speed(q as u8);
            assert!(s <= last && (0.6..1.3).contains(&s));
            last = s;
        }
        assert!(crack_speed(1) < crack_speed(255) - 0.5);
    }

    #[test]
    fn far_breaks_play_the_muffled_quiet_copy() {
        assert_eq!(break_cue(Size::Small, 30.0).0, Sound::Shatter);
        assert_eq!(break_cue(Size::Small, 91.0).0, Sound::ShatterFar);
        assert_eq!(break_cue(Size::Big, 10.0).0, Sound::Collapse);
        assert_eq!(break_cue(Size::Big, 200.0).0, Sound::CollapseFar);
        assert!(break_cue(Size::Big, 200.0).1 < break_cue(Size::Big, 10.0).1);
    }

    #[test]
    fn voice_cap_skips_beyond_six_and_frees_up_later() {
        let mut v = Voices::default();
        let gains: Vec<_> = (0..8).map(|_| v.admit(0.0, 0.8)).collect();
        assert_eq!(gains.iter().filter(|g| g.is_some()).count(), MAX_VOICES);
        assert!(gains[0].unwrap() > gains[5].unwrap(), "crowded = ducked");
        assert!(v.admit(0.5, 0.8).is_none());
        assert!(v.admit(0.9, 0.8).is_some(), "earlier voices ended");
    }

    #[test]
    fn quiet_join_catch_up_breaks_play_nothing() {
        let mut t = Tracker::default();
        // not joined yet, then joined with 3 boxes already broken, still settling
        assert_eq!(t.diff(&set(&[]), false, 0.016), (vec![], vec![]));
        assert_eq!(t.diff(&set(&[1, 2, 3]), true, 0.016), (vec![], vec![]));
        assert!(t.settling());
        assert_eq!(t.diff(&set(&[1, 2, 3, 4]), true, 0.3), (vec![], vec![]), "late catch-up batch");
        t.diff(&set(&[1, 2, 3, 4]), true, 0.6);
        assert!(!t.settling());
        // now a real break and a real re-form
        assert_eq!(t.diff(&set(&[2, 3, 4, 9]), true, 0.016), (vec![9], vec![1]));
        assert_eq!(t.diff(&set(&[2, 3, 4, 9]), true, 0.016), (vec![], vec![]));
        // reconnect re-primes
        t.diff(&set(&[]), false, 0.016);
        assert_eq!(t.diff(&set(&[5]), true, 0.016), (vec![], vec![]));
    }

    #[test]
    fn cascades_stagger_by_50ms_and_stay_bounded() {
        assert_eq!(stagger(0, 1), 0.0);
        assert_eq!(stagger(0, 10), 0.0);
        assert!((stagger(3, 10) - 0.15).abs() < 1e-6);
        assert_eq!(stagger(40, 50), MAX_STAGGER);
    }

    #[test]
    fn nearest_picks_the_closest_inside_the_radius() {
        let pts = [(1u16, [10.0, 0.0, 0.0]), (2, [1.0, 0.0, 0.0]), (3, [5.0, 0.0, 0.0]), (4, [500.0, 0.0, 0.0])];
        assert_eq!(nearest(&pts, [0.0; 3], 2, 80.0), vec![2, 3]);
        assert_eq!(nearest(&pts, [0.0; 3], 8, 80.0), vec![2, 3, 1]);
    }

    #[test]
    fn cracks_fire_only_when_hp_drops_on_a_box_already_seen() {
        let mut last = HashMap::new();
        assert!(crack_updates(&mut last, &[(1, 255), (2, 255)], false).is_empty(), "first sight records only");
        assert_eq!(crack_updates(&mut last, &[(1, 200), (2, 255)], false), vec![(1, 200)]);
        assert!(crack_updates(&mut last, &[(1, 200), (2, 255)], false).is_empty(), "no change, no sound");
        assert!(crack_updates(&mut last, &[(1, 255), (2, 255)], false).is_empty(), "re-form raises hp");
        assert!(crack_updates(&mut last, &[(1, 100), (2, 255)], true).is_empty(), "quiet records");
        // leaves the polled set (stale), comes back lower: recorded, not sounded
        crack_updates(&mut last, &[(2, 255)], false);
        assert!(crack_updates(&mut last, &[(1, 10), (2, 255)], false).is_empty());
    }
}
