//! Pure logic for "you are being hit / shot at" audio (no bevy, no sound card, unit-tested): damage -> cue mapping,
//! the low-health heartbeat rate, the short "ear muffle" duck after a hit, and the near-miss projectile test.
//! `audio::Cuer` calls these; `audio::backend` applies the muffle gain. See docs/AUDIO.md.

#![cfg_attr(not(feature = "audio"), allow(dead_code))]

use crate::synth::Sound;

/// A hit this share of max hp or more (and any headshot) uses the harsher variant.
pub const BIG_HIT_FRAC: f32 = 0.30;
/// Health fraction under which the heartbeat warning runs.
pub const LOW_HP: f32 = 0.30;
/// Heartbeat period at `LOW_HP` and at (almost) zero health, seconds.
pub const BEAT_SLOW: f32 = 1.05;
pub const BEAT_FAST: f32 = 0.42;
/// An enemy projectile passing this close to the eye whooshes.
pub const NEAR_MISS_M: f32 = 2.5;
/// Longest a projectile can have moved between frames and still be matched to itself.
const MATCH_M: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HurtKind {
    Light,
    Heavy,
    Head,
}

/// 0..=1 punch of a hit: 0 for a scratch, 1 at half of max hp or more.
pub fn intensity(amount: u8, max_hp: f32) -> f32 {
    if max_hp <= 0.0 {
        return 0.0;
    }
    (amount as f32 / (max_hp * 0.5)).clamp(0.0, 1.0)
}

pub fn classify(amount: u8, head: bool, max_hp: f32) -> HurtKind {
    if head {
        HurtKind::Head
    } else if max_hp > 0.0 && amount as f32 >= max_hp * BIG_HIT_FRAC - 1e-3 {
        HurtKind::Heavy
    } else {
        HurtKind::Light
    }
}

/// The "I got hit" cue for the worst hit of a frame: (sound, volume, playback speed). Bigger = louder and lower.
pub fn hurt_cue(amount: u8, head: bool, max_hp: f32) -> (Sound, f32, f32) {
    let i = intensity(amount, max_hp);
    let s = match classify(amount, head, max_hp) {
        HurtKind::Light => Sound::Hurt,
        HurtKind::Heavy => Sound::HurtHeavy,
        HurtKind::Head => Sound::HurtHead,
    };
    (s, 0.75 + 0.25 * i, 1.12 - 0.22 * i)
}

/// How long everything else is muffled after a hit, seconds (bigger hit = longer).
pub fn muffle_secs(amount: u8, max_hp: f32) -> f32 {
    0.25 + 0.25 * intensity(amount, max_hp)
}

/// Heartbeat period for a health fraction, `None` when healthy or dead. Falls (beats faster) as health falls.
pub fn heartbeat_period(hp: f32) -> Option<f32> {
    if !(hp > 0.0) || hp >= LOW_HP {
        return None;
    }
    Some(BEAT_FAST + (BEAT_SLOW - BEAT_FAST) * (hp / LOW_HP).clamp(0.0, 1.0))
}

/// Heartbeat loudness: louder as health falls.
pub fn heartbeat_vol(hp: f32) -> f32 {
    1.0 - 0.3 * (hp / LOW_HP).clamp(0.0, 1.0)
}

/// Brief duck of everything that is not a hit cue ("ear muffle"): instant dip, hold, linear return.
#[derive(Default, Clone, Copy, Debug)]
pub struct Muffle {
    left: f32,
    total: f32,
    depth: f32,
}

/// How far the world drops at a scratch / at the biggest hit (share removed).
const DEPTH_MIN: f32 = 0.45;
const DEPTH_MAX: f32 = 0.8;
/// Share of the duration held flat before the return.
const HOLD: f32 = 0.4;

impl Muffle {
    /// A hit: `secs` long, intensity `i` 0..=1. A stronger hit replaces a weaker one in progress.
    pub fn hit(&mut self, secs: f32, i: f32) {
        let depth = DEPTH_MIN + (DEPTH_MAX - DEPTH_MIN) * i.clamp(0.0, 1.0);
        if secs > 0.0 && (self.left <= 0.0 || depth >= self.depth || secs > self.left) {
            self.left = secs;
            self.total = secs;
            self.depth = depth;
        }
    }

    /// Gain for everything else this frame (1 = untouched).
    pub fn update(&mut self, dt: f32) -> f32 {
        if self.left <= 0.0 {
            return 1.0;
        }
        let u = 1.0 - self.left / self.total.max(1e-3); // 0 at the hit, 1 at the end
        let k = if u < HOLD { 1.0 } else { 1.0 - (u - HOLD) / (1.0 - HOLD) };
        self.left = (self.left - dt.max(0.0)).max(0.0);
        1.0 - self.depth * k.clamp(0.0, 1.0)
    }

    pub fn active(&self) -> bool {
        self.left > 0.0
    }
}

/// Is this a cue that cuts through the muffle (the hit / shield / near-miss family)?
pub fn is_alert(s: Sound) -> bool {
    matches!(
        s,
        Sound::Hurt
            | Sound::HurtHeavy
            | Sound::HurtHead
            | Sound::Heartbeat
            | Sound::Whoosh
            | Sound::Clang
            | Sound::GuardBreak
            | Sound::Parry
            | Sound::Block
            | Sound::DeathSting
            | Sound::Kill
    )
}

fn d2(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    d[0] * d[0] + d[1] * d[1] + d[2] * d[2]
}

/// Closest point of segment `a`..`b` to `p`, and its distance.
pub fn segment_closest(a: [f32; 3], b: [f32; 3], p: [f32; 3]) -> ([f32; 3], f32) {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
    let t = if len2 <= 1e-9 { 0.0 } else { (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1] + (p[2] - a[2]) * ab[2]) / len2).clamp(0.0, 1.0) };
    let q = [a[0] + ab[0] * t, a[1] + ab[1] * t, a[2] + ab[2] * t];
    (q, d2(q, p).sqrt())
}

/// A projectile moving `prev` -> `cur` just came within `r` of `eye` (it was outside before). Returns the closest point.
pub fn crossed_near(prev: [f32; 3], cur: [f32; 3], eye: [f32; 3], r: f32) -> Option<[f32; 3]> {
    if d2(prev, eye) < r * r {
        return None;
    }
    let (q, d) = segment_closest(prev, cur, eye);
    (d < r).then_some(q)
}

/// Near misses this frame: each current hostile projectile `(pos, owner)` is matched to the nearest previous one of the same
/// owner; those that just entered `NEAR_MISS_M` of `eye` give a position to play the whoosh at.
pub fn near_misses(prev: &[([f32; 3], u16)], cur: &[([f32; 3], u16)], eye: [f32; 3]) -> Vec<[f32; 3]> {
    cur.iter()
        .filter_map(|(c, o)| {
            let (p, dist2) = prev.iter().filter(|(_, po)| po == o).map(|(p, _)| (*p, d2(*p, *c))).min_by(|a, b| a.1.total_cmp(&b.1))?;
            if dist2 > MATCH_M * MATCH_M {
                return None;
            }
            crossed_near(p, *c, eye, NEAR_MISS_M)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_maps_to_intensity_and_variant() {
        assert_eq!(intensity(0, 100.0), 0.0);
        assert!((intensity(25, 100.0) - 0.5).abs() < 1e-6);
        assert_eq!(intensity(200, 100.0), 1.0);
        assert_eq!(intensity(20, 0.0), 0.0);
        assert_eq!(classify(20, false, 100.0), HurtKind::Light);
        assert_eq!(classify(29, false, 100.0), HurtKind::Light);
        assert_eq!(classify(30, false, 100.0), HurtKind::Heavy, "30 % of max hp is a big hit");
        assert_eq!(classify(5, true, 100.0), HurtKind::Head, "any headshot is harsh");
        assert_eq!(classify(90, true, 100.0), HurtKind::Head);
        let (ls, lv, lp) = hurt_cue(10, false, 100.0);
        let (hs, hv, hp) = hurt_cue(60, false, 100.0);
        assert_eq!((ls, hs), (Sound::Hurt, Sound::HurtHeavy));
        assert!(hv > lv && hp < lp, "bigger hit: louder and lower");
        assert!(lv >= 0.75 && hv <= 1.0);
    }

    #[test]
    fn muffle_is_longer_for_bigger_hits() {
        assert!(muffle_secs(60, 100.0) > muffle_secs(5, 100.0));
        assert!((0.25..=0.5).contains(&muffle_secs(5, 100.0)) && muffle_secs(100, 100.0) <= 0.5 + 1e-6);
    }

    #[test]
    fn heartbeat_speeds_up_as_health_falls_and_stops_when_healthy_or_dead() {
        assert_eq!(heartbeat_period(0.9), None);
        assert_eq!(heartbeat_period(LOW_HP), None);
        assert_eq!(heartbeat_period(0.0), None, "dead: silent");
        let a = heartbeat_period(0.28).unwrap();
        let b = heartbeat_period(0.15).unwrap();
        let c = heartbeat_period(0.02).unwrap();
        assert!(a > b && b > c, "{a} {b} {c}");
        assert!(a <= BEAT_SLOW && c >= BEAT_FAST);
        assert!(heartbeat_vol(0.02) > heartbeat_vol(0.28));
    }

    #[test]
    fn muffle_dips_at_once_holds_then_recovers_on_time() {
        let mut m = Muffle::default();
        assert_eq!(m.update(0.016), 1.0, "idle: untouched");
        m.hit(0.25, 1.0);
        let g0 = m.update(0.016);
        assert!(g0 < 0.3, "instant deep dip, got {g0}");
        let mut t = 0.016;
        let mut gain_mid = 0.0;
        while t < 0.1 {
            gain_mid = m.update(0.016);
            t += 0.016;
        }
        assert!(gain_mid < 0.3, "held through the first 40 %");
        while t < 0.2 {
            m.update(0.016);
            t += 0.016;
        }
        let g_late = m.update(0.016);
        assert!(g_late > g0, "recovering");
        for _ in 0..10 {
            m.update(0.016);
        }
        assert_eq!(m.update(0.016), 1.0, "back to normal after ~0.25 s");
        assert!(!m.active());
        // a light hit dips less
        let mut l = Muffle::default();
        l.hit(0.25, 0.0);
        assert!(l.update(0.0) > g0);
    }

    #[test]
    fn muffle_stronger_hit_replaces_weaker_but_not_vice_versa() {
        let mut m = Muffle::default();
        m.hit(0.25, 0.0);
        m.update(0.1);
        m.hit(0.5, 1.0);
        assert!(m.update(0.0) < 0.3);
        let mut n = Muffle::default();
        n.hit(0.5, 1.0);
        n.update(0.05);
        n.hit(0.25, 0.0);
        assert!(n.update(0.0) < 0.3, "a scratch does not soften a big dip");
    }

    #[test]
    fn near_miss_distance_test() {
        let eye = [0.0, 1.6, 0.0];
        // flies past 1 m to the side
        let q = crossed_near([-10.0, 1.6, 1.0], [10.0, 1.6, 1.0], eye, NEAR_MISS_M).expect("passes within 2.5 m");
        assert!((q[0]).abs() < 1e-4 && (q[2] - 1.0).abs() < 1e-4, "closest point abeam of the eye: {q:?}");
        // 5 m off: no
        assert_eq!(crossed_near([-10.0, 1.6, 5.0], [10.0, 1.6, 5.0], eye, NEAR_MISS_M), None);
        // already inside last frame: not again
        assert_eq!(crossed_near([-1.0, 1.6, 1.0], [1.0, 1.6, 1.0], eye, NEAR_MISS_M), None);
        // a short step that ends 2.0 m away from outside: yes
        assert!(crossed_near([4.0, 1.6, 0.0], [2.0, 1.6, 0.0], eye, NEAR_MISS_M).is_some());
        // short step that stays outside: no
        assert_eq!(crossed_near([6.0, 1.6, 0.0], [4.0, 1.6, 0.0], eye, NEAR_MISS_M), None);
    }

    #[test]
    fn near_misses_match_by_owner_and_fire_once() {
        let eye = [0.0, 1.6, 0.0];
        let a = vec![([6.0, 1.6, 0.5], 9u16)];
        let b = vec![([2.0, 1.6, 0.5], 9u16)];
        assert_eq!(near_misses(&a, &b, eye).len(), 1);
        assert!(near_misses(&b, &b, eye).is_empty(), "already close: once only");
        let other_owner = vec![([2.0, 1.6, 0.5], 4u16)];
        assert!(near_misses(&a, &other_owner, eye).is_empty(), "a different owner's projectile is not the same one");
        assert!(near_misses(&[], &b, eye).is_empty(), "newly spawned: no whoosh");
        let far = vec![([40.0, 1.6, 0.5], 9u16)];
        assert!(near_misses(&far, &b, eye).is_empty(), "teleported: not the same projectile");
    }

    #[test]
    fn alerts_cut_through_the_muffle_footsteps_do_not() {
        assert!(is_alert(Sound::HurtHead) && is_alert(Sound::Clang) && is_alert(Sound::Whoosh));
        assert!(!is_alert(Sound::Step) && !is_alert(Sound::Wind));
    }
}
