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
    /// You took a hit: layered crack + sub thump + short ring.
    Hurt,
    /// You took a big hit (>= 30 % of max hp): longer, deeper, harsher.
    HurtHeavy,
    /// You took a headshot: harshest, with a ringing high tone.
    HurtHead,
    /// An enemy projectile zipped past you: whoosh with a doppler pitch fall.
    Whoosh,
    /// Loud metallic clang: a star / arrow hit your raised shield.
    Clang,
    /// Your guard broke: shattering metal + low boom.
    GuardBreak,
    Heartbeat,
    Print,
    Derez,
    Boot,
    /// Loop: grapple reel hum.
    Reel,
    /// Loop: wall-run rush.
    WallRun,
    /// Bright bell: a parried hit.
    Parry,
    /// Dull clang: the shield took a hit.
    Block,
    /// Buzz: star energy empty.
    Lockout,
    /// Soft ping: star energy is back.
    Refill,
    /// Rising whine while the heavy cut charges.
    HeavyCharge,
    HeavyRelease,
    /// Low boom: dive slash.
    AirSlam,
    Whirl,
    /// Slice + chime: a katana kill.
    Execute,
    /// Zap: the counter cut after a parry.
    CounterFlash,
    GrappleYank,
    /// Soft tick while reeling in.
    RopePulse,
    Step,
    Jump,
    DoubleJump,
    Land,
    LandHard,
    Slide,
    WallSlash,
    GuardLow,
    /// Kill streak note (played at a rising speed).
    Streak,
    /// A flow tier just stepped up (2 -> 4 -> 6 chained moves, `combat::flow`): a bright ascending sparkle,
    /// distinct from the kill streak note so the two "building toward something" cues never read as the same beat.
    FlowTierUp,
    /// Landed the kill on the marked BOUNTY (docs/bounty.md): the normal kill chime plus an extra triumphant layer.
    BountyKill,
    /// The local player just went live as the map-wide BOUNTY (docs/bounty.md): a rising, ominous stinger (the
    /// "you're being hunted now" beat; the HUD banner alone had no sound before this).
    BecomeBounty,
    /// A one-shot kill landed while still cloaked (`cloak.rs`, docs/melee.md "Cloak"): rare and high-skill, so it
    /// gets its own harsher, metallic sting instead of reusing the plain `Kill` chime.
    StealthKill,
    /// Falling whoosh into the void.
    VoidFall,
    DeathSting,
    Respawn,
    MatchStart,
    Victory,
    Defeat,
    UiClick,
    UiBack,
    /// Loop (ambient bus): wind bed, 3 s.
    Wind,
    /// Loop (ambient bus): city hum, 1 s.
    Hum,
    /// Destructible boxes: a short stone tick when one is cracked (played at a falling pitch as it weakens).
    CrackStone,
    /// Destructible boxes: a drier, higher wood snap (thin boxes: bamboo).
    CrackWood,
    /// A box breaks: noise burst + low thump + falling-debris rattle (~0.8 s).
    Shatter,
    /// `Shatter` heard from far away: dark and muffled.
    ShatterFar,
    /// A tall box / tower breaks: long rumble with thumps (~1.6 s).
    Collapse,
    /// `Collapse` heard from far away: dark and muffled.
    CollapseFar,
    /// A broken box re-forms: quiet rising neon shimmer (~0.5 s).
    Reform,
    /// Storm: low earth rumble (1.6 s), retriggered and swelling during a quake warning and the shaking.
    QuakeRumble,
    /// Storm: loose-debris rattle (0.4 s) during the shaking.
    QuakeRattle,
    /// Storm: the settle thud when a quake ends.
    QuakeThud,
    /// Storm: rising electrical whine of a marked bolt (0.5 s, retriggered faster and higher as it nears).
    BoltWhine,
    /// Storm: thunder crack + rolling rumble (3.2 s).
    Thunder,
    /// Storm: `Thunder` heard from far away: dark and muffled.
    ThunderFar,
    /// Storm: ringing in the ears after a very close strike.
    EarRing,
    /// Flash star bursts: a hard crack with a zap on top (~0.5 s).
    FlashBang,
    /// Dazed by a flash: a thin high whine that fades over ~3.3 s.
    Tinnitus,
    /// The grapple hooks an enemy: a heavy whip crack.
    WhipCrack,
}

impl Sound {
    pub const ALL: [Sound; 79] = [
        Sound::QuakeRumble,
        Sound::QuakeRattle,
        Sound::QuakeThud,
        Sound::BoltWhine,
        Sound::Thunder,
        Sound::ThunderFar,
        Sound::EarRing,
        Sound::FlashBang,
        Sound::Tinnitus,
        Sound::WhipCrack,
        Sound::HurtHeavy,
        Sound::HurtHead,
        Sound::Whoosh,
        Sound::Clang,
        Sound::GuardBreak,
        Sound::CrackStone,
        Sound::CrackWood,
        Sound::Shatter,
        Sound::ShatterFar,
        Sound::Collapse,
        Sound::CollapseFar,
        Sound::Reform,
        Sound::Parry,
        Sound::Block,
        Sound::Lockout,
        Sound::Refill,
        Sound::HeavyCharge,
        Sound::HeavyRelease,
        Sound::AirSlam,
        Sound::Whirl,
        Sound::Execute,
        Sound::CounterFlash,
        Sound::GrappleYank,
        Sound::RopePulse,
        Sound::Step,
        Sound::Jump,
        Sound::DoubleJump,
        Sound::Land,
        Sound::LandHard,
        Sound::Slide,
        Sound::WallSlash,
        Sound::GuardLow,
        Sound::Streak,
        Sound::FlowTierUp,
        Sound::BountyKill,
        Sound::BecomeBounty,
        Sound::StealthKill,
        Sound::VoidFall,
        Sound::DeathSting,
        Sound::Respawn,
        Sound::MatchStart,
        Sound::Victory,
        Sound::Defeat,
        Sound::UiClick,
        Sound::UiBack,
        Sound::Wind,
        Sound::Hum,
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
        matches!(self, Sound::Reel | Sound::WallRun | Sound::Wind | Sound::Hum)
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

/// Decaying sine note.
fn tone(f: f32, d: f32, pow: f32, g: f32) -> Vec<f32> {
    sweep(d, f, f, 0, |t| env(t, d, 0.003, pow) * g)
}

/// Several decaying sines at once.
fn chord(fs: &[f32], d: f32, pow: f32, g: f32) -> Vec<f32> {
    mix(&fs.iter().map(|f| (tone(*f, d, pow, 1.0), g)).collect::<Vec<_>>())
}

fn scaled(v: Vec<f32>, g: f32) -> Vec<f32> {
    v.into_iter().map(|x| x * g).collect()
}

/// Noise minus its low-passed self: the airy part (slices, ticks).
fn highpass(x: &[f32], k: f32) -> Vec<f32> {
    lowpass(x, k).iter().zip(x).map(|(l, v)| v - l).collect()
}

/// Falling-debris rattle: tiny decaying pings whose rate thins out over `secs` (`rate` = clicks per second at the start).
fn rattle(secs: f32, rate: f32, seed: u32, gain: f32) -> Vec<f32> {
    let mut nz = Noise(seed);
    let n = len(secs);
    let mut out = vec![0.0f32; n];
    for i in 0..n {
        let u = i as f32 / n as f32;
        let p = rate * (1.0 - u).powf(1.6) / SR as f32;
        if (nz.next() * 0.5 + 0.5) < p {
            let f = 500.0 + (nz.next() * 0.5 + 0.5) * 2600.0;
            let g = gain * (0.4 + 0.6 * (nz.next() * 0.5 + 0.5)) * (1.0 - u);
            let d = len(0.03);
            for j in 0..d.min(n - i) {
                let t = j as f32 / SR as f32;
                out[i + j] += (TAU * f * t).sin() * (1.0 - j as f32 / d as f32).powi(3) * g;
            }
        }
    }
    out
}

fn shatter() -> Vec<f32> {
    let d = 0.8;
    mix(&[
        (lowpass(&noise_burst(0.35, 211, |t| env(t, 0.35, 0.004, 1.6)), 0.55), 1.5),
        (highpass(&noise_burst(0.12, 213, |t| env(t, 0.12, 0.0, 1.5)), 0.3), 0.9),
        (sweep(0.4, 95.0, 32.0, 0, |t| env(t, 0.4, 0.003, 1.8)), 0.9),
        (rattle(d, 60.0, 215, 0.35), 1.0),
    ])
}

fn collapse() -> Vec<f32> {
    let d = 1.6;
    let rumble = lowpass(&noise_burst(d, 221, |t| env(t, d, 0.12, 0.8) * (0.7 + 0.3 * (TAU * 9.0 * t).sin())), 0.07);
    let thump = |f: f32, dur: f32, g: f32| sweep(dur, f, f * 0.4, 0, move |t| env(t, dur, 0.004, 1.8) * g);
    mix(&[
        (rumble, 5.0),
        (place(&[(0.0, thump(75.0, 0.6, 0.9)), (0.28, thump(62.0, 0.5, 0.6)), (0.6, thump(50.0, 0.5, 0.45))]), 1.0),
        (lowpass(&noise_burst(0.4, 223, |t| env(t, 0.4, 0.004, 1.5)), 0.5), 1.3),
        (rattle(d, 90.0, 225, 0.3), 1.0),
    ])
}

/// Scale so the loudest sample is `peak` (silence stays silent).
fn normalized(v: Vec<f32>, peak: f32) -> Vec<f32> {
    let m = v.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    if m < 1e-6 {
        return v;
    }
    scaled(v, peak / m)
}

/// Sharp crack, a deep boom, then a long rolling rumble that swells and fades (3.2 s).
fn thunder() -> Vec<f32> {
    let d = 3.2;
    let roll = lowpass(&noise_burst(d, 341, |t| env(t, d, 0.08, 1.1) * (0.6 + 0.4 * (TAU * 4.0 * t).sin() * (TAU * 1.3 * t).cos())), 0.06);
    normalized(
        mix(&[
            (highpass(&noise_burst(0.25, 343, |t| env(t, 0.25, 0.001, 2.2)), 0.35), 1.6),
            (lowpass(&noise_burst(0.6, 345, |t| env(t, 0.6, 0.002, 1.8)), 0.4), 1.4),
            (sweep(1.0, 85.0, 28.0, 0, |t| env(t, 1.0, 0.004, 1.5)), 1.1),
            (roll, 7.0),
            (rattle(2.0, 40.0, 347, 0.2), 1.0),
        ]),
        0.95,
    )
}

/// Muffled far-away copy: dark low-pass, a bit louder to make up for the lost level.
fn muffled(v: &[f32], gain: f32) -> Vec<f32> {
    scaled(lowpass(&lowpass(v, 0.09), 0.09), gain)
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
        Sound::Hurt => mix(&[
            // crack, sub thump (+ a 2nd harmonic so small speakers hear it), short ring
            (highpass(&noise_burst(0.05, 17, |t| env(t, 0.05, 0.0, 1.6)), 0.3), 1.6),
            (sweep(0.3, 110.0, 52.0, 0, |t| env(t, 0.3, 0.002, 1.8)), 1.1),
            (sweep(0.12, 220.0, 120.0, 0, |t| env(t, 0.12, 0.001, 2.0)), 0.5),
            (tone(2300.0, 0.18, 2.5, 0.28), 1.0),
        ]),
        Sound::HurtHeavy => mix(&[
            (highpass(&noise_burst(0.09, 18, |t| env(t, 0.09, 0.0, 1.4)), 0.25), 2.0),
            (lowpass(&noise_burst(0.2, 19, |t| env(t, 0.2, 0.0, 1.8)), 0.3), 1.2),
            (sweep(0.5, 95.0, 42.0, 0, |t| env(t, 0.5, 0.002, 1.5)), 1.4),
            (sweep(0.22, 200.0, 70.0, 2, |t| env(t, 0.22, 0.001, 2.0)), 0.2),
            (tone(1750.0, 0.4, 2.2, 0.3), 1.0),
            (tone(2600.0, 0.3, 2.5, 0.2), 1.0),
        ]),
        Sound::HurtHead => mix(&[
            (highpass(&noise_burst(0.07, 20, |t| env(t, 0.07, 0.0, 1.2)), 0.2), 2.4),
            (sweep(0.14, 3800.0, 1500.0, 2, |t| env(t, 0.14, 0.0, 1.6)), 0.3),
            (sweep(0.4, 100.0, 40.0, 0, |t| env(t, 0.4, 0.002, 1.5)), 1.4),
            (tone(3100.0, 0.7, 1.6, 0.3), 1.0),
            (tone(4300.0, 0.55, 1.8, 0.2), 1.0),
            (sweep(0.3, 190.0, 60.0, 2, |t| env(t, 0.3, 0.001, 2.0)), 0.18),
        ]),
        Sound::Whoosh => {
            // doppler: a bright band of noise that darkens as it passes, a tone sliding down ~ 1500 -> 520 Hz
            let d = 0.5;
            let mut nz = Noise(131);
            let (mut a, mut b) = (0.0f32, 0.0f32);
            let air: Vec<f32> = (0..len(d))
                .map(|i| {
                    let t = i as f32 / SR as f32;
                    let k = 0.55 - 0.45 * (t / d); // bright -> dark
                    let x = nz.next();
                    a += (x - a) * k;
                    b += (a - b) * k;
                    (a - b) * 6.0 * env(t, d, 0.12, 1.1)
                })
                .collect();
            mix(&[(air, 1.4), (sweep(d, 1500.0, 520.0, 0, |t| env(t, d, 0.1, 1.4)), 0.25)])
        }
        Sound::Clang => {
            let d = 0.6;
            mix(&[
                (highpass(&noise_burst(0.05, 133, |t| env(t, 0.05, 0.0, 1.2)), 0.3), 1.2),
                (sweep(d, 880.0, 870.0, 0, |t| env(t, d, 0.001, 2.2)), 0.4),
                (sweep(d, 1405.0, 1395.0, 0, |t| env(t, d, 0.001, 2.6)), 0.34),
                (sweep(d, 2310.0, 2300.0, 0, |t| env(t, d, 0.001, 3.0)), 0.3),
                (sweep(d, 3790.0, 3780.0, 0, |t| env(t, d, 0.001, 3.5)), 0.22),
                (sweep(0.12, 200.0, 90.0, 0, |t| env(t, 0.12, 0.001, 2.0)), 0.7),
            ])
        }
        Sound::GuardBreak => {
            let d = 0.8;
            mix(&[
                (highpass(&noise_burst(0.35, 137, |t| env(t, 0.35, 0.0, 1.3)), 0.2), 1.8),
                (sweep(d, 1100.0, 150.0, 1, |t| env(t, d, 0.002, 1.8) * 0.28), 1.0),
                (sweep(d, 1860.0, 1500.0, 0, |t| env(t, d, 0.001, 2.4)), 0.3),
                (sweep(d, 2790.0, 2200.0, 0, |t| env(t, d, 0.001, 2.8)), 0.22),
                (sweep(0.5, 90.0, 38.0, 0, |t| env(t, 0.5, 0.002, 1.6)), 1.3),
            ])
        }
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
        Sound::Parry => {
            let bell = |f: f32| chord(&[f, f * 1.5, f * 2.0], 0.5, 2.5, 0.2);
            place(&[(0.0, scaled(noise_burst(0.02, 61, |t| env(t, 0.02, 0.0, 1.0)), 0.5)), (0.0, bell(1568.0)), (0.08, bell(2093.0))])
        }
        Sound::Block => mix(&[
            (noise_burst(0.04, 63, |t| env(t, 0.04, 0.0, 1.5)), 0.5),
            (sweep(0.22, 520.0, 495.0, 0, |t| env(t, 0.22, 0.001, 3.0)), 0.45),
            (sweep(0.15, 780.0, 760.0, 0, |t| env(t, 0.15, 0.001, 3.0)), 0.2),
        ]),
        Sound::Lockout => {
            let d = 0.28;
            sweep(d, 110.0, 105.0, 2, |t| env(t, d, 0.004, 0.4) * if (t * 36.0).fract() < 0.5 { 0.32 } else { 0.1 })
        }
        Sound::Refill => place(&[(0.0, tone(1318.0, 0.16, 1.8, 0.3)), (0.07, tone(1760.0, 0.3, 2.0, 0.3))]),
        Sound::HeavyCharge => {
            let d = 0.7;
            mix(&[
                (sweep(d, 120.0, 720.0, 1, |t| { let u = t / d; u.powf(1.5) * (1.0 - ((u - 0.92) / 0.08).clamp(0.0, 1.0)) * 0.22 }), 1.0),
                (sweep(d, 240.0, 1440.0, 0, |t| { let u = t / d; u * u * (1.0 - ((u - 0.92) / 0.08).clamp(0.0, 1.0)) * 0.12 }), 1.0),
            ])
        }
        Sound::HeavyRelease => {
            let d = 0.38;
            mix(&[
                (lowpass(&noise_burst(d, 71, |t| env(t, d, 0.03, 1.2)), 0.3), 2.0),
                (sweep(0.3, 95.0, 38.0, 0, |t| env(t, 0.3, 0.002, 2.0)), 0.8),
                (noise_burst(0.025, 73, |t| env(t, 0.025, 0.0, 1.0)), 0.5),
            ])
        }
        Sound::AirSlam => mix(&[
            (sweep(0.5, 75.0, 28.0, 0, |t| env(t, 0.5, 0.002, 1.8)), 1.0),
            (lowpass(&noise_burst(0.35, 79, |t| env(t, 0.35, 0.0, 1.5)), 0.25), 1.4),
            (noise_burst(0.03, 81, |t| env(t, 0.03, 0.0, 1.0)), 0.5),
        ]),
        Sound::Whirl => {
            let d = 0.5;
            let w = lowpass(&noise_burst(d, 83, |t| env(t, d, 0.06, 0.8) * (0.55 + 0.45 * (TAU * 5.0 * t).sin())), 0.3);
            mix(&[(w, 2.0), (sweep(d, 300.0, 1100.0, 1, |t| env(t, d, 0.05, 1.2) * 0.1), 1.0)])
        }
        Sound::Execute => place(&[
            (0.0, scaled(highpass(&noise_burst(0.07, 85, |t| env(t, 0.07, 0.0, 1.5)), 0.2), 1.2)),
            (0.0, sweep(0.2, 120.0, 45.0, 0, |t| env(t, 0.2, 0.002, 2.0) * 0.8)),
            (0.07, chord(&[1760.0, 2349.0], 0.4, 2.0, 0.18)),
        ]),
        Sound::CounterFlash => mix(&[
            (sweep(0.14, 3200.0, 700.0, 2, |t| env(t, 0.14, 0.001, 2.0)), 0.2),
            (tone(2400.0, 0.2, 3.0, 1.0), 0.25),
            (noise_burst(0.03, 87, |t| env(t, 0.03, 0.0, 1.0)), 0.4),
        ]),
        Sound::GrappleYank => mix(&[
            (sweep(0.22, 700.0, 110.0, 1, |t| env(t, 0.22, 0.002, 2.0)), 0.22),
            (sweep(0.2, 160.0, 70.0, 0, |t| env(t, 0.2, 0.002, 2.0)), 0.6),
            (noise_burst(0.02, 89, |t| env(t, 0.02, 0.0, 1.0)), 0.4),
        ]),
        Sound::RopePulse => mix(&[(tone(140.0, 0.07, 2.0, 1.0), 0.45), (tone(280.0, 0.05, 2.0, 1.0), 0.15)]),
        Sound::Step => mix(&[(lowpass(&noise_burst(0.07, 91, |t| env(t, 0.07, 0.003, 2.0)), 0.3), 1.4), (sweep(0.06, 95.0, 55.0, 0, |t| env(t, 0.06, 0.002, 2.0)), 0.5)]),
        Sound::Jump => mix(&[(sweep(0.11, 180.0, 380.0, 0, |t| env(t, 0.11, 0.005, 1.5)), 0.35), (lowpass(&noise_burst(0.1, 93, |t| env(t, 0.1, 0.02, 1.5)), 0.3), 0.8)]),
        Sound::DoubleJump => mix(&[
            (sweep(0.15, 300.0, 760.0, 0, |t| env(t, 0.15, 0.005, 1.4)), 0.35),
            (tone(1500.0, 0.2, 2.5, 1.0), 0.14),
            (lowpass(&noise_burst(0.12, 95, |t| env(t, 0.12, 0.02, 1.5)), 0.4), 0.8),
        ]),
        Sound::Land => mix(&[(sweep(0.12, 110.0, 55.0, 0, |t| env(t, 0.12, 0.002, 2.0)), 0.7), (lowpass(&noise_burst(0.08, 97, |t| env(t, 0.08, 0.0, 2.0)), 0.3), 1.0)]),
        Sound::LandHard => mix(&[(sweep(0.24, 80.0, 34.0, 0, |t| env(t, 0.24, 0.002, 1.8)), 1.0), (lowpass(&noise_burst(0.14, 99, |t| env(t, 0.14, 0.0, 1.8)), 0.25), 1.5)]),
        Sound::Slide => {
            let d = 0.42;
            scaled(lowpass(&noise_burst(d, 101, |t| env(t, d, 0.1, 1.0)), 0.18), 2.0)
        }
        Sound::WallSlash => mix(&[
            (highpass(&noise_burst(0.18, 103, |t| env(t, 0.18, 0.01, 1.5)), 0.25), 1.1),
            (sweep(0.18, 2300.0, 900.0, 0, |t| env(t, 0.18, 0.005, 2.0)), 0.18),
        ]),
        Sound::GuardLow => {
            let b = || sweep(0.07, 1000.0, 1000.0, 2, |t| env(t, 0.07, 0.003, 0.8) * 0.2);
            place(&[(0.0, b()), (0.11, b())])
        }
        Sound::Streak => place(&[(0.0, tone(988.0, 0.12, 1.5, 0.3)), (0.06, tone(1319.0, 0.22, 2.0, 0.3))]),
        Sound::FlowTierUp => place(&[
            (0.0, tone(784.0, 0.1, 1.6, 0.25)),
            (0.05, tone(1047.0, 0.12, 1.6, 0.28)),
            (0.1, chord(&[1319.0, 1760.0], 0.3, 1.8, 0.22)),
        ]),
        Sound::BountyKill => place(&[
            (0.0, {
                let note = |f: f32| sweep(0.22, f, f, 0, |t| env(t, 0.22, 0.003, 1.6) * 0.4);
                place(&[(0.0, note(880.0)), (0.07, note(1175.0)), (0.14, note(1568.0))])
            }),
            (0.1, chord(&[1568.0, 1976.0, 2349.0], 0.4, 1.6, 0.22)),
            (0.0, scaled(noise_burst(0.03, 97, |t| env(t, 0.03, 0.0, 1.5)), 0.4)),
        ]),
        Sound::BecomeBounty => {
            let d = 0.5;
            mix(&[
                (sweep(d, 160.0, 420.0, 1, |t| env(t, d, 0.03, 0.9) * 0.3), 1.0),
                (sweep(d, 320.0, 840.0, 0, |t| env(t, d, 0.03, 0.9) * 0.14), 1.0),
                (highpass(&noise_burst(0.08, 103, |t| env(t, 0.08, 0.0, 1.5)), 0.2), 0.8),
            ])
        }
        Sound::StealthKill => place(&[
            (0.0, scaled(highpass(&noise_burst(0.05, 109, |t| env(t, 0.05, 0.0, 1.8)), 0.15), 1.5)),
            (0.0, sweep(0.3, 70.0, 30.0, 0, |t| env(t, 0.3, 0.001, 1.8) * 0.9)),
            (0.03, chord(&[1200.0, 1800.0, 2400.0], 0.35, 2.4, 0.2)),
        ]),
        Sound::VoidFall => {
            let d = 1.0;
            mix(&[
                (sweep(d, 900.0, 90.0, 0, |t| env(t, d, 0.12, 0.8) * 0.3), 1.0),
                (lowpass(&noise_burst(d, 105, |t| env(t, d, 0.1, 0.8)), 0.12), 2.0),
            ])
        }
        Sound::DeathSting => {
            let n = |f: f32| mix(&[(tone(f, 0.4, 1.8, 0.3), 1.0), (sweep(0.4, f, f, 1, |t| env(t, 0.4, 0.005, 2.5) * 0.08), 1.0)]);
            place(&[(0.0, n(392.0)), (0.2, n(311.0)), (0.4, n(262.0)), (0.0, sweep(0.6, 90.0, 40.0, 0, |t| env(t, 0.6, 0.003, 1.6) * 0.8))])
        }
        Sound::Respawn => {
            let n = |f: f32| mix(&[(tone(f, 0.3, 2.0, 0.3), 1.0), (tone(f * 2.0, 0.2, 3.0, 0.1), 1.0)]);
            place(&[(0.0, n(523.0)), (0.07, n(659.0)), (0.14, n(784.0)), (0.21, n(1047.0))])
        }
        Sound::MatchStart => place(&[
            (0.0, chord(&[330.0, 495.0], 0.3, 1.5, 0.25)),
            (0.2, chord(&[440.0, 660.0], 0.3, 1.5, 0.25)),
            (0.4, chord(&[660.0, 990.0, 1320.0], 0.7, 1.8, 0.2)),
        ]),
        Sound::Victory => place(&[
            (0.0, tone(523.0, 0.3, 1.5, 0.3)),
            (0.12, tone(659.0, 0.3, 1.5, 0.3)),
            (0.24, tone(784.0, 0.3, 1.5, 0.3)),
            (0.36, chord(&[1047.0, 1319.0, 1568.0], 0.95, 1.8, 0.2)),
        ]),
        Sound::Defeat => place(&[
            (0.0, tone(392.0, 0.4, 1.5, 0.3)),
            (0.25, tone(349.0, 0.4, 1.5, 0.3)),
            (0.5, tone(311.0, 0.4, 1.5, 0.3)),
            (0.75, chord(&[262.0, 247.0], 0.45, 1.8, 0.25)),
        ]),
        Sound::UiClick => mix(&[(sweep(0.035, 1800.0, 1800.0, 2, |t| env(t, 0.035, 0.001, 1.5)), 0.15), (tone(900.0, 0.07, 2.0, 1.0), 0.2)]),
        Sound::UiBack => sweep(0.06, 1200.0, 700.0, 0, |t| env(t, 0.06, 0.002, 1.5) * 0.3),
        Sound::CrackStone => mix(&[
            (highpass(&noise_burst(0.05, 231, |t| env(t, 0.05, 0.0, 2.0)), 0.25), 0.9),
            (sweep(0.09, 1500.0, 1100.0, 0, |t| env(t, 0.09, 0.001, 2.5)), 0.28),
            (sweep(0.07, 140.0, 80.0, 0, |t| env(t, 0.07, 0.001, 2.0)), 0.6),
        ]),
        Sound::CrackWood => mix(&[
            (highpass(&noise_burst(0.04, 233, |t| env(t, 0.04, 0.0, 1.5)), 0.15), 1.1),
            (sweep(0.1, 950.0, 600.0, 2, |t| env(t, 0.1, 0.001, 3.0)), 0.16),
            (sweep(0.06, 220.0, 150.0, 0, |t| env(t, 0.06, 0.001, 2.0)), 0.4),
        ]),
        Sound::Shatter => shatter(),
        Sound::ShatterFar => muffled(&shatter(), 2.2),
        Sound::Collapse => collapse(),
        Sound::CollapseFar => muffled(&collapse(), 2.0),
        Sound::Reform => {
            let d = 0.5;
            mix(&[
                (sweep(d, 420.0, 1700.0, 0, |t| env(t, d, 0.12, 0.7) * 0.2), 1.0),
                (chord(&[880.0, 1320.0, 1760.0], d, 1.4, 0.08), 1.0),
                (scaled(highpass(&noise_burst(d, 235, |t| env(t, d, 0.2, 1.2) * (0.5 + 0.5 * (TAU * 18.0 * t).sin())), 0.2), 0.5), 1.0),
            ])
        }
        Sound::QuakeRumble => {
            let d = 1.6;
            let r = lowpass(&noise_burst(d, 301, |t| env(t, d, 0.35, 0.6) * (0.65 + 0.35 * (TAU * 7.0 * t).sin())), 0.05);
            normalized(mix(&[(r, 6.0), (sweep(d, 46.0, 36.0, 0, |t| env(t, d, 0.3, 0.8)), 0.5), (rattle(d, 30.0, 303, 0.15), 1.0)]), 0.8)
        }
        Sound::QuakeRattle => {
            let d = 0.4;
            let chop = |t: f32| 0.55 + 0.45 * (TAU * 38.0 * t).sin();
            normalized(
                mix(&[
                    (highpass(&noise_burst(d, 311, |t| env(t, d, 0.01, 1.3) * chop(t)), 0.2), 1.0),
                    (rattle(d, 140.0, 313, 0.5), 1.0),
                    (sweep(d, 120.0, 70.0, 0, |t| env(t, d, 0.01, 1.8)), 0.3),
                ]),
                0.7,
            )
        }
        Sound::QuakeThud => normalized(
            mix(&[
                (sweep(0.7, 72.0, 28.0, 0, |t| env(t, 0.7, 0.004, 1.7)), 1.0),
                (lowpass(&noise_burst(0.5, 321, |t| env(t, 0.5, 0.004, 1.6)), 0.12), 4.0),
                (rattle(0.7, 50.0, 323, 0.2), 1.0),
            ]),
            0.9,
        ),
        Sound::BoltWhine => {
            let d = 0.5;
            let trem = |t: f32| 0.7 + 0.3 * (TAU * 55.0 * t).sin();
            normalized(
                mix(&[
                    (sweep(d, 520.0, 2600.0, 1, |t| env(t, d, 0.35, 0.5) * trem(t)), 0.35),
                    (sweep(d, 1040.0, 5200.0, 0, |t| env(t, d, 0.35, 0.5)), 0.25),
                    (highpass(&noise_burst(d, 331, |t| env(t, d, 0.4, 0.6) * trem(t)), 0.3), 0.25),
                ]),
                0.7,
            )
        }
        Sound::Thunder => thunder(),
        Sound::ThunderFar => normalized(muffled(&thunder(), 2.4), 0.8),
        Sound::EarRing => {
            let d = 1.2;
            normalized(mix(&[(tone(4200.0, d, 1.4, 1.0), 1.0), (tone(3100.0, d, 1.8, 1.0), 0.5)]), 0.35)
        }
        Sound::FlashBang => mix(&[
            (highpass(&noise_burst(0.06, 341, |t| env(t, 0.06, 0.0, 1.2)), 0.2), 2.0),
            (sweep(0.25, 5000.0, 900.0, 2, |t| env(t, 0.25, 0.001, 2.0)), 0.2),
            (tone(3000.0, 0.4, 2.5, 0.3), 1.0),
            (sweep(0.3, 120.0, 50.0, 0, |t| env(t, 0.3, 0.002, 2.0)), 0.9),
        ]),
        Sound::Tinnitus => {
            let d = 3.3;
            let t_of = |i: usize| i as f32 / SR as f32;
            (0..len(d))
                .map(|i| {
                    let t = t_of(i);
                    let a = env(t, d, 0.03, 0.9) * (0.9 + 0.1 * (TAU * 7.0 * t).sin());
                    ((TAU * 4300.0 * t).sin() * 0.2 + (TAU * 6100.0 * t).sin() * 0.07) * a
                })
                .collect()
        }
        Sound::WhipCrack => mix(&[
            (highpass(&noise_burst(0.04, 351, |t| env(t, 0.04, 0.0, 1.0)), 0.2), 2.6),
            (sweep(0.12, 2800.0, 300.0, 1, |t| env(t, 0.12, 0.001, 2.0)), 0.25),
            (sweep(0.25, 110.0, 45.0, 0, |t| env(t, 0.25, 0.002, 2.0)), 0.9),
            (lowpass(&noise_burst(0.08, 353, |t| env(t, 0.08, 0.0, 1.8)), 0.3), 1.0),
        ]),
        // loops: integer-Hz partials so the 1 s buffer is seamless
        Sound::Hum => {
            let n = len(1.0);
            (0..n)
                .map(|i| {
                    let t = i as f32 / SR as f32;
                    ((TAU * 55.0 * t).sin() * 0.22 + (TAU * 110.0 * t).sin() * 0.1 + (TAU * 165.0 * t + 1.0).sin() * 0.04) * (0.85 + 0.15 * (TAU * t).sin())
                })
                .collect()
        }
        Sound::Wind => {
            let (n, x) = (len(3.0), len(0.1));
            let mut nz = Noise(131);
            let raw: Vec<f32> = (0..n + x).map(|_| nz.next()).collect();
            let low = lowpass(&raw, 0.05);
            (0..n)
                .map(|i| {
                    let v = if i < x {
                        let k = i as f32 / x as f32;
                        low[i] * k + low[n + i] * (1.0 - k)
                    } else {
                        low[i]
                    };
                    let t = i as f32 / SR as f32;
                    v * 2.4 * (0.8 + 0.2 * (TAU * t / 3.0).sin())
                })
                .collect()
        }
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
            assert!(v.len() < len(3.5), "{s:?} too long");
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
    fn one_shots_end_quietly_and_loops_are_whole_seconds() {
        for s in Sound::ALL {
            let v = render(s);
            if s.looped() {
                assert_eq!(v.len() % SR as usize, 0, "{s:?} loops are whole seconds");
            } else {
                let tail = &v[v.len() - 8..];
                assert!(peak(tail) < 0.05, "{s:?} ends with a click ({})", peak(tail));
            }
        }
    }

    #[test]
    fn loops_have_no_click_at_the_seam() {
        for s in Sound::ALL.into_iter().filter(|s| s.looped()) {
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
    fn envelope_attacks_monotonically_then_decays_to_zero() {
        let (d, a) = (0.5, 0.1);
        let mut last = -1.0;
        for i in 0..10 {
            let v = env(i as f32 * 0.01, d, a, 1.0);
            assert!(v >= last, "attack rises");
            last = v;
        }
        assert!((env(a, d, a, 1.0) - 0.8).abs() < 1e-5, "peak just after the attack: (1 - 0.1/0.5)");
        let mut last = 2.0;
        for i in 10..50 {
            let v = env(i as f32 * 0.01, d, a, 2.0);
            assert!(v <= last, "decay falls");
            last = v;
        }
        assert_eq!(env(d, d, a, 1.0), 0.0);
        assert_eq!(env(-0.1, d, a, 1.0), 0.0);
    }

    #[test]
    fn one_shots_are_balanced_not_clipping_and_not_buried() {
        for s in Sound::ALL.into_iter().filter(|s| !s.looped()) {
            let p = peak(&render(s));
            assert!(p <= 1.0 && p > 0.1, "{s:?} peak {p}");
        }
        for s in [Sound::Wind, Sound::Hum, Sound::Reel, Sound::WallRun] {
            let p = peak(&render(s));
            assert!(p < 0.95, "{s:?} loop is too hot ({p})");
        }
    }

    #[test]
    fn stingers_differ_and_wind_loop_is_longer_than_hum() {
        assert_ne!(render(Sound::Victory), render(Sound::Defeat));
        assert_ne!(render(Sound::Parry), render(Sound::Block));
        assert!(render(Sound::Wind).len() > render(Sound::Hum).len());
    }

    #[test]
    fn destructible_sounds_have_their_lengths_and_far_is_darker() {
        let secs = |s| render(s).len() as f32 / SR as f32;
        assert!((0.7..0.95).contains(&secs(Sound::Shatter)));
        assert!((1.5..1.8).contains(&secs(Sound::Collapse)));
        assert!((0.4..0.65).contains(&secs(Sound::Reform)));
        assert!(secs(Sound::CrackStone) < 0.15 && secs(Sound::CrackWood) < 0.15);
        // fewer zero crossings = darker
        let zc = |v: &[f32]| v.windows(2).filter(|w| w[0] * w[1] < 0.0).count();
        assert!(zc(&render(Sound::ShatterFar)) < zc(&render(Sound::Shatter)) / 2);
        assert!(zc(&render(Sound::CollapseFar)) < zc(&render(Sound::Collapse)) / 2);
    }

    #[test]
    fn storm_sounds_have_their_lengths_and_far_thunder_is_darker() {
        let secs = |s| render(s).len() as f32 / SR as f32;
        assert!((3.1..3.3).contains(&secs(Sound::Thunder)));
        assert!((1.5..1.7).contains(&secs(Sound::QuakeRumble)));
        assert!(secs(Sound::BoltWhine) < 0.6 && secs(Sound::QuakeRattle) < 0.5);
        let zc = |v: &[f32]| v.windows(2).filter(|w| w[0] * w[1] < 0.0).count();
        assert!(zc(&render(Sound::ThunderFar)) < zc(&render(Sound::Thunder)) / 2);
        // the whine rises
        let v = render(Sound::BoltWhine);
        let h = v.len() / 2;
        assert!(zc(&v[h..]) > zc(&v[..h]));
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
