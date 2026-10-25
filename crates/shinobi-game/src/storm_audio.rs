//! Sound for lightning, from the events `storm_fx` drains (`StormFeed`). A rising positional whine
//! on each marked bolt (faster, higher, louder as the strike nears); on the strike a thunder crack + rolling rumble that
//! arrives later the farther away it is (dark and muffled when far), and ringing ears (the world dips) within 8 m.
//! Cues go out through `audio::ExtraCues` / `audio::ExtraMuffle`. The decisions are pure and tested here.

#![cfg_attr(not(feature = "audio"), allow(dead_code))]

use bevy::prelude::*;
use shinobi_client::netclient::EnvFx;

use crate::audio::{Cue, ExtraCues, ExtraMuffle};
use crate::net::ViewRes;
use crate::storm_fx::{whine_look, StormFeed, StormSet};
use crate::synth::Sound;

/// Game-scaled speed of sound (m/s): thunder trails the flash noticeably but never by more than `MAX_DELAY`.
pub const SOUND_SPEED: f32 = 150.0;
pub const MAX_DELAY: f32 = 3.0;
/// Strikes closer than this ring in your ears.
pub const EAR_RING_M: f32 = 8.0;
/// Beyond this the muffled thunder plays.
pub const FAR_M: f32 = 90.0;
const WHINE_RANGE: f32 = 70.0;

/// Seconds until the thunder of a strike `dist` metres away is heard.
pub fn thunder_delay(dist: f32) -> f32 {
    (dist.max(0.0) / SOUND_SPEED).min(MAX_DELAY)
}

/// `(sound, volume)` of the thunder at `dist`: loudest close, dark and quieter far away, always audible.
pub fn thunder_cue(dist: f32) -> (Sound, f32) {
    if dist > FAR_M {
        (Sound::ThunderFar, (0.9 - (dist - FAR_M) / 500.0).clamp(0.35, 0.9))
    } else {
        (Sound::Thunder, (1.0 - 0.45 * dist.max(0.0) / FAR_M).clamp(0.55, 1.0))
    }
}

/// The ear muffle (seconds, intensity) of a strike `dist` metres away; only inside `EAR_RING_M`.
pub fn muffle_for(dist: f32) -> Option<(f32, f32)> {
    if dist >= EAR_RING_M {
        return None;
    }
    let k = 1.0 - dist.max(0.0) / EAR_RING_M;
    Some((1.8 + 1.4 * k, 0.55 + 0.45 * k))
}

struct Mark {
    id: u8,
    pos: [f32; 3],
    left: f32,
    warn: f32,
    next: f32,
}

#[derive(Resource, Default)]
struct StormAudio {
    marks: Vec<Mark>,
    /// (seconds from now, sound, volume, position) still to play.
    due: Vec<(f32, Sound, f32, [f32; 3])>,
}

pub struct StormAudioPlugin;

impl Plugin for StormAudioPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StormAudio>().init_resource::<ExtraCues>().init_resource::<ExtraMuffle>().add_systems(Update, storm_sfx.after(StormSet));
    }
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f32>().sqrt()
}

fn storm_sfx(time: Res<Time>, view: Res<ViewRes>, feed: Res<StormFeed>, mut st: ResMut<StormAudio>, mut out: ResMut<ExtraCues>, mut muffle: ResMut<ExtraMuffle>) {
    let dt = time.delta_secs();
    let eye = view.0.eye;
    for ev in feed.events.iter().copied() {
        match ev {
            EnvFx::BoltMark { id, pos, warn_s } => {
                st.marks.retain(|m| m.id != id);
                st.marks.push(Mark { id, pos, left: warn_s.max(0.1), warn: warn_s.max(0.1), next: 0.0 });
            }
            EnvFx::BoltStrike { id, pos } => {
                st.marks.retain(|m| m.id != id);
                let d = dist(eye, pos);
                let (s, v) = thunder_cue(d);
                st.due.push((thunder_delay(d), s, v, pos));
                if muffle_for(d).is_some() {
                    st.due.push((0.12, Sound::EarRing, 0.5, eye));
                    muffle.0 = muffle_for(d);
                }
            }
        }
    }

    // marked bolts: the whine retriggers faster as the strike nears
    for m in st.marks.iter_mut() {
        m.left = (m.left - dt).max(0.0);
        m.next -= dt;
        if m.next <= 0.0 {
            let (gap, pitch, vol) = whine_look(1.0 - m.left / m.warn);
            m.next = gap;
            out.push(Cue::at(Sound::BoltWhine, m.pos, vol).pitched(pitch).ranged(WHINE_RANGE));
        }
    }

    // thunder on its way
    let due = std::mem::take(&mut st.due);
    for (left, s, v, p) in due {
        let left = left - dt;
        if left > 0.0 {
            st.due.push((left, s, v, p));
        } else {
            out.push(Cue::at(s, p, v).ranged(1000.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thunder_arrives_later_the_farther_it_is_but_is_capped() {
        assert_eq!(thunder_delay(0.0), 0.0);
        assert!(thunder_delay(30.0) < thunder_delay(120.0));
        assert!((thunder_delay(150.0) - 1.0).abs() < 1e-6);
        assert_eq!(thunder_delay(5000.0), MAX_DELAY);
        assert_eq!(thunder_delay(-3.0), 0.0);
    }

    #[test]
    fn thunder_is_loudest_near_and_dark_when_far() {
        assert_eq!(thunder_cue(2.0).0, Sound::Thunder);
        assert_eq!(thunder_cue(91.0).0, Sound::ThunderFar);
        assert!(thunder_cue(2.0).1 > thunder_cue(80.0).1);
        assert!(thunder_cue(2.0).1 >= 0.95);
        assert!(thunder_cue(2000.0).1 >= 0.35, "still audible");
        assert!(thunder_cue(2000.0).1 < thunder_cue(100.0).1);
    }

    #[test]
    fn ears_ring_only_on_a_very_close_strike() {
        assert!(muffle_for(8.0).is_none() && muffle_for(40.0).is_none());
        let (ns, ni) = muffle_for(1.0).unwrap();
        let (fs, fi) = muffle_for(7.5).unwrap();
        assert!(ns > fs && ni > fi);
        assert!(ni <= 1.0 && ns < 4.0);
    }
}
