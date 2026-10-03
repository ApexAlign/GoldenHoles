//! Procedural sound synthesis: every game sound is generated at startup from sine / saw / noise and envelopes, so the
//! game ships no audio asset files. Pure and deterministic (no bevy, no randomness source but a fixed LCG), so it is
//! unit-tested here and always compiled; only the playback backend (`audio.rs`, feature `audio`) needs a sound card.

#![cfg_attr(not(feature = "audio"), allow(dead_code))]

use std::f32::consts::TAU;

/// Sample rate (mono, 16-bit when encoded).
pub const SR: u32 = 22_050;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sound {
    /// Rising tone while the lock triangle converges (0.35 s = `combat.lock_secs`).
    LockTone,
    /// Sharp chirp at full lock.
    LockChirp,
    /// Falling fade when the lock breaks.
    LockBreak,
    Throw,
    Ricochet,
    /// Low thud: a body hit.
    HitBody,
    /// High tick: a heavy / precise hit.
    HitHard,
    Kill,
    Swing,
    Clash,
    Whiff,
    GrappleFire,
    GrappleAttach,
    GrappleRelease,
    Dash,
    Hurt,
    Heartbeat,
    Print,
    Derez,
    Boot,
    /// Loop: grapple reel hum.
    Reel,
    /// Loop: wall-run rush.
    WallRun,
}

impl Sound {
    pub const ALL: [Sound; 22] = [
        Sound::LockTone,
        Sound::LockChirp,
        Sound::LockBreak,
        Sound::Throw,
        Sound::Ricochet,
        Sound::HitBody,
        Sound::HitHard,
        Sound::Kill,
        Sound::Swing,
        Sound::Clash,
        Sound::Whiff,
        Sound::GrappleFire,
        Sound::GrappleAttach,
        Sound::GrappleRelease,
        Sound::Dash,
        Sound::Hurt,
        Sound::Heartbeat,
        Sound::Print,
        Sound::Derez,
        Sound::Boot,
        Sound::Reel,
        Sound::WallRun,
    ];

    pub fn looped(self) -> bool {
        matches!(self, Sound::Reel | Sound::WallRun)
    }
}

/// Deterministic white noise in -1..1.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ((self.0 >> 8) as f32 / (1u32 << 23) as f32) - 1.0
    }
}

fn len(secs: f32) -> usize {
    (secs * SR as f32) as usize
}

/// Linear attack, then power-law decay to zero at `dur`.
fn env(t: f32, dur: f32, attack: f32, pow: f32) -> f32 {
    if t < 0.0 || t >= dur {
        return 0.0;
    }
    let a = if attack > 0.0 { (t / attack).min(1.0) } else { 1.0 };
    a * (1.0 - t / dur).powf(pow)
}

/// Phase-accumulating oscillator over a frequency sweep f0 -> f1 (exponential), `shape`: 0 sine, 1 saw, 2 square.
fn sweep(secs: f32, f0: f32, f1: f32, shape: u8, amp: impl Fn(f32) -> f32) -> Vec<f32> {
    let n = len(secs);
    let mut phase = 0.0f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR as f32;
            let u = (t / secs).clamp(0.0, 1.0);
            let f = f0 * (f1 / f0).powf(u);
            phase = (phase + f / SR as f32).fract();
            let v = match shape {
                0 => (phase * TAU).sin(),
                1 => 2.0 * phase - 1.0,
                _ => if phase < 0.5 { 1.0 } else { -1.0 },
            };
            v * amp(t)
        })
        .collect()
}

fn noise_burst(secs: f32, seed: u32, amp: impl Fn(f32) -> f32) -> Vec<f32> {
    let mut nz = Noise(seed);
    (0..len(secs)).map(|i| nz.next() * amp(i as f32 / SR as f32)).collect()
}

/// One-pole low-pass (`k` 0..1: smaller = darker) for turning noise into a whoosh.
fn lowpass(x: &[f32], k: f32) -> Vec<f32> {
    let mut y = 0.0;
    x.iter().map(|v| { y += (v - y) * k; y }).collect()
}

fn mix(parts: &[(Vec<f32>, f32)]) -> Vec<f32> {
    let n = parts.iter().map(|(p, _)| p.len()).max().unwrap_or(0);
    (0..n).map(|i| parts.iter().map(|(p, g)| p.get(i).copied().unwrap_or(0.0) * g).sum()).collect()
}

/// Concatenate with each part starting at `offset` seconds.
fn place(parts: &[(f32, Vec<f32>)]) -> Vec<f32> {
    let end = parts.iter().map(|(o, p)| len(*o) + p.len()).max().unwrap_or(0);
    let mut out = vec![0.0; end];
    for (o, p) in parts {
        for (i, v) in p.iter().enumerate() {
            out[len(*o) + i] += v;
        }
    }
    out
}

/// Render one sound to mono f32 samples in -1..1.
pub fn render(s: Sound) -> Vec<f32> {
    let out = match s {
        Sound::LockTone => {
            let d = 0.35;
            mix(&[(sweep(d, 300.0, 1000.0, 0, |t| env(t, d, 0.02, 0.5)), 0.5), (sweep(d, 600.0, 2000.0, 0, |t| env(t, d, 0.05, 1.0)), 0.15)])
        }
        Sound::LockChirp => {
            let d = 0.14;
            mix(&[(sweep(d, 1400.0, 2400.0, 2, |t| env(t, d, 0.002, 1.5)), 0.22), (sweep(d, 2800.0, 4800.0, 0, |t| env(t, d, 0.002, 2.0)), 0.25)])
        }
        Sound::LockBreak => sweep(0.28, 900.0, 200.0, 0, |t| env(t, 0.28, 0.005, 1.0) * 0.5),
        Sound::Throw => {
            let d = 0.22;
            let w = lowpass(&noise_burst(d, 11, |t| env(t, d, 0.04, 1.2)), 0.25);
            mix(&[(w, 1.6), (sweep(d, 500.0, 1400.0, 0, |t| env(t, d, 0.02, 2.0)), 0.12)])
        }
        Sound::Ricochet => {
            let d = 0.3;
            mix(&[(sweep(d, 1800.0, 1750.0, 0, |t| env(t, d, 0.001, 3.0)), 0.4), (sweep(d, 3600.0, 3500.0, 0, |t| env(t, d, 0.001, 4.0)), 0.18)])
        }
        Sound::HitBody => {
            let d = 0.18;
            mix(&[(sweep(d, 160.0, 60.0, 0, |t| env(t, d, 0.002, 2.5)), 0.9), (noise_burst(0.03, 3, |t| env(t, 0.03, 0.0, 2.0)), 0.3)])
        }
        Sound::HitHard => {
            let d = 0.09;
            mix(&[(sweep(d, 2600.0, 2400.0, 0, |t| env(t, d, 0.001, 2.5)), 0.5), (noise_burst(0.02, 5, |t| env(t, 0.02, 0.0, 2.0)), 0.3)])
        }
        Sound::Kill => {
            let note = |f: f32| sweep(0.22, f, f, 0, |t| env(t, 0.22, 0.003, 1.6) * 0.4);
            place(&[(0.0, note(880.0)), (0.07, note(1175.0)), (0.14, note(1568.0))])
        }
        Sound::Swing => {
            let d = 0.26;
            let w = lowpass(&noise_burst(d, 21, |t| env(t, d, 0.09, 1.0) ), 0.35);
            w.iter().map(|v| v * 1.8).collect()
        }
        Sound::Clash => {
            let d = 0.34;
            mix(&[
                (noise_burst(0.05, 7, |t| env(t, 0.05, 0.0, 1.5)), 0.5),
                (sweep(d, 1320.0, 1310.0, 0, |t| env(t, d, 0.001, 2.5)), 0.28),
                (sweep(d, 2110.0, 2100.0, 0, |t| env(t, d, 0.001, 3.0)), 0.2),
                (sweep(d, 3330.0, 3320.0, 0, |t| env(t, d, 0.001, 3.5)), 0.12),
            ])
        }
        Sound::Whiff => {
            let d = 0.16;
            lowpass(&noise_burst(d, 31, |t| env(t, d, 0.05, 1.5)), 0.15).iter().map(|v| v * 1.2).collect()
        }
        Sound::GrappleFire => sweep(0.16, 400.0, 1800.0, 1, |t| env(t, 0.16, 0.005, 1.5) * 0.2),
        Sound::GrappleAttach => mix(&[
            (sweep(0.22, 110.0, 55.0, 0, |t| env(t, 0.22, 0.002, 2.0)), 0.9),
            (noise_burst(0.02, 9, |t| env(t, 0.02, 0.0, 1.0)), 0.5),
        ]),
        Sound::GrappleRelease => mix(&[(noise_burst(0.09, 13, |t| env(t, 0.09, 0.0, 2.5)), 0.5), (sweep(0.09, 1200.0, 300.0, 0, |t| env(t, 0.09, 0.0, 2.0)), 0.25)]),
        Sound::Dash => {
            let d = 0.22;
            lowpass(&noise_burst(d, 41, |t| env(t, d, 0.03, 1.4)), 0.4).iter().map(|v| v * 1.5).collect()
        }
        Sound::Hurt => mix(&[(sweep(0.24, 100.0, 45.0, 0, |t| env(t, 0.24, 0.002, 2.0)), 1.0), (noise_burst(0.05, 17, |t| env(t, 0.05, 0.0, 2.0)), 0.3)]),
        Sound::Heartbeat => {
            let thump = || sweep(0.12, 70.0, 45.0, 0, |t| env(t, 0.12, 0.004, 2.0));
            place(&[(0.0, thump()), (0.18, thump().iter().map(|v| v * 0.7).collect())]).iter().map(|v| v * 0.9).collect()
        }
        Sound::Print => {
            let blip = |f: f32| sweep(0.05, f, f, 2, |t| env(t, 0.05, 0.001, 1.0) * 0.12);
            place(&[(0.0, blip(660.0)), (0.05, blip(880.0)), (0.10, blip(1100.0)), (0.15, blip(1320.0))])
        }
        Sound::Derez => {
            let d = 0.42;
            let mut nz = Noise(77);
            let crackle: Vec<f32> = (0..len(d))
                .map(|i| {
                    let t = i as f32 / SR as f32;
                    let on = nz.next().abs() < 0.35 * (1.0 - t / d); // sparser as it fades
                    if on { nz.next() * env(t, d, 0.0, 1.0) } else { 0.0 }
                })
                .collect();
            mix(&[(crackle, 0.6), (sweep(d, 900.0, 120.0, 1, |t| env(t, d, 0.01, 1.5) * 0.12), 1.0)])
        }
        Sound::Boot => {
            let blip = |f: f32| sweep(0.07, f, f, 0, |t| env(t, 0.07, 0.003, 1.0) * 0.3);
            place(&[(0.0, blip(1046.0)), (0.1, blip(1568.0))])
        }
        // loops: integer-Hz partials so the 1 s buffer is seamless
        Sound::Reel => {
            let n = len(1.0);
            (0..n)
                .map(|i| {
                    let t = i as f32 / SR as f32;
                    ((TAU * 110.0 * t).sin() * 0.18 + (TAU * 220.0 * t).sin() * 0.1 + (TAU * 330.0 * t).sin() * 0.05) * (0.8 + 0.2 * (TAU * 6.0 * t).sin())
                })
                .collect()
        }
        Sound::WallRun => {
            // generate n + x samples and crossfade the extra tail into the start, so the loop point is continuous
            let (n, x) = (len(1.0), len(0.05));
            let mut nz = Noise(99);
            let raw: Vec<f32> = (0..n + x).map(|_| nz.next()).collect();
            let low = lowpass(&raw, 0.12);
            (0..n)
                .map(|i| {
                    let v = if i < x {
                        let k = i as f32 / x as f32;
                        low[i] * k + low[n + i] * (1.0 - k)
                    } else {
                        low[i]
                    };
                    v * 1.6
                })
                .collect()
        }
    };
    // hard limit; never clip the encoder
    out.into_iter().map(|v| v.clamp(-1.0, 1.0)).collect()
}

/// Encode mono f32 samples as a 16-bit PCM WAV (what `bevy_audio` decodes with its `wav` feature).
pub fn wav(samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + samples.len() * 2);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&SR.to_le_bytes());
    b.extend_from_slice(&(SR * 2).to_le_bytes()); // byte rate
    b.extend_from_slice(&2u16.to_le_bytes()); // block align
    b.extend_from_slice(&16u16.to_le_bytes()); // bits
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(v: &[f32]) -> f32 {
        v.iter().fold(0.0, |m, x| x.abs().max(m))
    }

    #[test]
    fn every_sound_renders_audible_bounded_and_finite() {
        for s in Sound::ALL {
            let v = render(s);
            assert!(v.len() > len(0.05), "{s:?} too short");
            assert!(v.len() < len(2.0), "{s:?} too long");
            assert!(v.iter().all(|x| x.is_finite() && x.abs() <= 1.0), "{s:?} out of range");
            assert!(peak(&v) > 0.05, "{s:?} is silent (peak {})", peak(&v));
        }
    }

    #[test]
    fn rendering_is_deterministic() {
        for s in [Sound::Throw, Sound::Derez, Sound::WallRun] {
            assert_eq!(render(s), render(s));
        }
    }

    #[test]
    fn one_shots_end_quietly_and_loops_are_one_second() {
        for s in Sound::ALL {
            let v = render(s);
            if s.looped() {
                assert_eq!(v.len(), SR as usize, "{s:?}");
            } else {
                let tail = &v[v.len() - 8..];
                assert!(peak(tail) < 0.05, "{s:?} ends with a click ({})", peak(tail));
            }
        }
    }

    #[test]
    fn loops_have_no_click_at_the_seam() {
        for s in [Sound::Reel, Sound::WallRun] {
            let v = render(s);
            let jump = (v[0] - v[v.len() - 1]).abs();
            assert!(jump < 0.08, "{s:?} seam jump {jump}");
        }
    }

    #[test]
    fn wav_header_and_length_are_valid() {
        let w = wav(&[0.0, 0.5, -0.5, 1.0]);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        assert_eq!(w.len(), 44 + 8);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 8);
        assert_eq!(i16::from_le_bytes(w[50..52].try_into().unwrap()), 32767);
    }

    #[test]
    fn lock_tone_rises() {
        // zero crossings per half: the second half oscillates faster than the first
        let v = render(Sound::LockTone);
        let zc = |s: &[f32]| s.windows(2).filter(|w| w[0] * w[1] < 0.0).count();
        let h = v.len() / 2;
        assert!(zc(&v[h..]) > zc(&v[..h]), "tone should rise in pitch");
    }
}
