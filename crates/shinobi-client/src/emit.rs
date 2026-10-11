//! Particle/emitter descriptors: shuriken trails, slash arcs, dash streaks, hit sparks, wall-run speed lines.
//! Driven by `CombatEvent` + character state; output is data only (no GPU). A backend uploads
//! `RibbonInstance`s to the instanced ribbon pass (`shaders/ribbon_fx.wgsl`).
//!
//! Deterministic: burst expansion hashes (seed, index), never `rand`, and stamps the synced server time.

use shinobi_sim::character::CharacterSystem;
use shinobi_sim::combat::{CombatEvent, DamageKind};

use crate::fx::{hash01, wrap_time};
use crate::hud::{Palette, Rgba};

/// One instanced ribbon segment. 16 floats = `struct Inst` in ribbon_fx.wgsl (64 B).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RibbonInstance {
    pub p0: [f32; 4],     // xyz, width at p0
    pub p1: [f32; 4],     // xyz, width at p1
    pub color: [f32; 4],  // rgb HDR, a = intensity
    pub params: [f32; 4], // spawn server time (wrapped), life s, spin rad/s, seed 0..1
}

impl RibbonInstance {
    pub const FLOATS: usize = 16;
    pub fn to_array(&self) -> [f32; 16] {
        let mut o = [0.0; 16];
        o[0..4].copy_from_slice(&self.p0);
        o[4..8].copy_from_slice(&self.p1);
        o[8..12].copy_from_slice(&self.color);
        o[12..16].copy_from_slice(&self.params);
        o
    }

    /// Single trail piece (shuriken/laser): caller passes last and current projectile position per frame.
    pub fn trail_segment(a: [f32; 3], b: [f32; 3], color: [f32; 4], width: f32, life: f32, server_time: f64) -> Self {
        Self {
            p0: [a[0], a[1], a[2], width * 0.3],
            p1: [b[0], b[1], b[2], width],
            color,
            params: [wrap_time(server_time), life, 0.0, 0.0],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmitterKind {
    /// Follows a projectile; backend feeds `RibbonInstance::trail_segment` each frame. No burst.
    ShurikenTrail,
    /// First-person katana arc is drawn by `viewmodel_draw`; this is the world-space echo for spectators.
    SlashArc,
    /// Speed streaks radiating from the view centre (view space) on dash/lunge.
    DashStreaks,
    HitSpark,
    /// Long thin lines streaming past while wall-running (view space).
    WallRunLines,
}

#[derive(Clone, Copy, Debug)]
pub struct Emitter {
    pub kind: EmitterKind,
    pub origin: [f32; 3],
    pub dir: [f32; 3], // unit
    pub count: u32,
    pub life: f32,
    pub speed: f32,
    pub spread: f32, // radians cone half-angle (HitSpark/streaks), 0 = parallel
    pub width: f32,
    pub color: [f32; 4], // rgb HDR, a = intensity
    pub view_space: bool,
    pub seed: u32,
}

fn hdr(c: Rgba, gain: f32) -> [f32; 4] {
    [c.0 * gain, c.1 * gain, c.2 * gain, 1.0]
}

fn norm3(v: [f32; 3], fallback: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l < 1e-5 { fallback } else { [v[0] / l, v[1] / l, v[2] / l] }
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// Two unit vectors orthogonal to `d`.
fn basis(d: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let up = if d[1].abs() < 0.95 { [0.0, 1.0, 0.0] } else { [1.0, 0.0, 0.0] };
    let u = norm3(cross(d, up), [1.0, 0.0, 0.0]);
    (u, cross(d, u))
}

impl Emitter {
    /// Deterministic burst expansion. `ShurikenTrail` returns nothing (per-frame segments instead).
    pub fn instances(&self, server_time: f64) -> Vec<RibbonInstance> {
        if self.kind == EmitterKind::ShurikenTrail {
            return Vec::new();
        }
        let t = wrap_time(server_time);
        let (u, v) = basis(self.dir);
        (0..self.count)
            .map(|i| {
                let h0 = hash01(self.seed.wrapping_mul(0x9e37_79b1).wrapping_add(i * 3));
                let h1 = hash01(self.seed.wrapping_mul(0x85eb_ca6b).wrapping_add(i * 3 + 1));
                let h2 = hash01(self.seed.wrapping_mul(0xc2b2_ae35).wrapping_add(i * 3 + 2));
                let ang = h0 * std::f32::consts::TAU;
                let cone = h1 * self.spread;
                let (s, c) = (cone.sin(), cone.cos());
                let side = [
                    (u[0] * ang.cos() + v[0] * ang.sin()) * s,
                    (u[1] * ang.cos() + v[1] * ang.sin()) * s,
                    (u[2] * ang.cos() + v[2] * ang.sin()) * s,
                ];
                let d = norm3([self.dir[0] * c + side[0], self.dir[1] * c + side[1], self.dir[2] * c + side[2]], self.dir);
                // streak kinds start spread around the origin; sparks start at the origin
                let (start, len) = match self.kind {
                    EmitterKind::DashStreaks | EmitterKind::WallRunLines => {
                        let r = 0.1 + 0.5 * h2;
                        let o = [
                            self.origin[0] + (u[0] * ang.cos() + v[0] * ang.sin()) * r,
                            self.origin[1] + (u[1] * ang.cos() + v[1] * ang.sin()) * r,
                            self.origin[2] + (u[2] * ang.cos() + v[2] * ang.sin()) * r,
                        ];
                        (o, self.speed * self.life * (0.5 + 0.5 * h2))
                    }
                    _ => (self.origin, self.speed * self.life * 0.25 * (0.4 + 0.6 * h2)),
                };
                RibbonInstance {
                    p0: [start[0], start[1], start[2], self.width],
                    p1: [start[0] + d[0] * len, start[1] + d[1] * len, start[2] + d[2] * len, self.width * 0.2],
                    color: self.color,
                    params: [t, self.life, 0.0, h2],
                }
            })
            .collect()
    }
}

/// Continuous movement inputs for the state-driven emitters.
#[derive(Clone, Copy, Debug, Default)]
pub struct MoveFx {
    pub vel: [f32; 3],
    pub dashing: bool,
    /// Wall normal (wall -> player) while wall-running.
    pub wall_normal: Option<[f32; 3]>,
}

impl MoveFx {
    pub fn from_character(c: &CharacterSystem, vel: [f32; 3]) -> Self {
        Self {
            vel,
            dashing: c.evading().is_some(),
            wall_normal: c.wall_run_normal().map(|n| [n.x, n.y, n.z]),
        }
    }
}

pub struct EmitterSystem {
    pub palette: Palette,
    /// Drained by the renderer each frame.
    pub out: Vec<Emitter>,
    was_dashing: bool,
    wall_accum: f32,
}

const WALL_LINES_PER_SEC: f32 = 30.0;

impl EmitterSystem {
    pub fn new(palette: Palette) -> Self {
        Self { palette, out: Vec::new(), was_dashing: false, wall_accum: 0.0 }
    }

    fn mk(&self, kind: EmitterKind, color: Rgba, gain: f32) -> Emitter {
        Emitter {
            kind,
            origin: [0.0; 3],
            dir: [0.0, 1.0, 0.0],
            count: 0,
            life: 0.3,
            speed: 6.0,
            spread: 0.6,
            width: 0.04,
            color: hdr(color, gain),
            view_space: false,
            seed: 0,
        }
    }

    pub fn on_event(&mut self, e: &CombatEvent, tick: u32) {
        let p = self.palette;
        match *e {
            CombatEvent::Damage { point, kind, amount, .. } => {
                let col = p.for_damage(kind);
                let mut em = self.mk(EmitterKind::HitSpark, col, 3.0);
                em.origin = [point.x, point.y, point.z];
                em.spread = std::f32::consts::PI * 0.5;
                em.count = match kind {
                    DamageKind::Slash => 18,
                    _ => 12,
                } + (amount / 25.0).min(10.0) as u32;
                em.seed = tick;
                self.out.push(em);
            }
            CombatEvent::Slash { dir, lunge, .. } => {
                let mut em = self.mk(EmitterKind::SlashArc, p.orange, 3.0);
                em.dir = norm3([dir.x, dir.y, dir.z], [0.0, 0.0, -1.0]);
                em.count = 10;
                em.life = 0.18;
                em.spread = 0.25;
                em.width = 0.08;
                em.view_space = true;
                em.seed = tick;
                self.out.push(em);
                if lunge {
                    self.push_dash(tick);
                }
            }
            CombatEvent::ShurikenThrown { pos, dir, .. } => {
                let mut em = self.mk(EmitterKind::ShurikenTrail, p.cyan, 4.0);
                em.origin = [pos.x, pos.y, pos.z];
                em.dir = norm3([dir.x, dir.y, dir.z], [0.0, 0.0, -1.0]);
                em.life = 0.25;
                em.width = 0.1;
                em.seed = tick;
                self.out.push(em);
            }
            CombatEvent::ShurikenBounce { pos, normal } => {
                let mut em = self.mk(EmitterKind::HitSpark, p.cyan, 3.5);
                em.origin = [pos.x, pos.y, pos.z];
                em.dir = norm3([normal.x, normal.y, normal.z], [0.0, 1.0, 0.0]);
                em.count = 8;
                em.spread = 0.9;
                em.seed = tick;
                self.out.push(em);
            }
            CombatEvent::SlashHit { .. } | CombatEvent::SlashMiss { .. } | CombatEvent::MarkHit { .. } => {} // Graphics: hit flash / whiff arc hook
            CombatEvent::PieceHit { .. } => {} // server-side damage to a piece; its de-rez arrives as NetEvent::BlockDerez
            CombatEvent::RicochetHit { .. } => {} // medal bookkeeping only; the Damage event right after drives the FX
            CombatEvent::ShurikenGone { pos } => {
                let mut em = self.mk(EmitterKind::HitSpark, p.cyan, 2.0);
                em.origin = [pos.x, pos.y, pos.z];
                em.count = 6;
                em.spread = std::f32::consts::PI * 0.5;
                em.seed = tick;
                self.out.push(em);
            }
            CombatEvent::UltimateStart { origin } => {
                let mut em = self.mk(EmitterKind::HitSpark, p.magenta, 4.0);
                em.origin = [origin.x, origin.y, origin.z];
                em.count = 48;
                em.spread = std::f32::consts::PI;
                em.speed = 14.0;
                em.life = 0.6;
                em.seed = tick;
                self.out.push(em);
            }
            // hook impact spark; the rope itself is drawn from `CharacterSystem.grapple` (Graphics owns the look)
            CombatEvent::GrappleAttach { anchor, .. } => {
                let mut em = self.mk(EmitterKind::HitSpark, p.cyan, 3.0);
                em.origin = [anchor.x, anchor.y, anchor.z];
                em.count = 10;
                em.spread = std::f32::consts::PI * 0.5;
                em.seed = tick;
                self.out.push(em);
            }
            // fizzle: a short dim puff a few metres out along the aim
            CombatEvent::GrappleMiss { origin, dir } => {
                let mut em = self.mk(EmitterKind::HitSpark, p.orange, 1.5);
                em.origin = [origin.x + dir.x * 3.0, origin.y + dir.y * 3.0, origin.z + dir.z * 3.0];
                em.count = 4;
                em.spread = std::f32::consts::PI * 0.5;
                em.life = 0.2;
                em.seed = tick;
                self.out.push(em);
            }
            // a barrel / canister landed (ours, predicted): a small grey-white puff off the surface
            CombatEvent::StarStuck { pos, normal, .. } => {
                let mut em = self.mk(EmitterKind::HitSpark, p.text, 2.0);
                em.origin = [pos.x, pos.y, pos.z];
                em.dir = norm3([normal.x, normal.y, normal.z], [0.0, 1.0, 0.0]);
                em.count = 8;
                em.spread = 0.7;
                em.seed = tick;
                self.out.push(em);
            }
            // flash burst (predicted for our own throws): white sparks in every direction; the volume FX is the world layer's
            CombatEvent::StarFlash { pos } => {
                let mut em = self.mk(EmitterKind::HitSpark, p.text, 6.0);
                em.origin = [pos.x, pos.y, pos.z];
                em.count = 32;
                em.spread = std::f32::consts::PI;
                em.speed = 12.0;
                em.life = 0.35;
                em.seed = tick;
                self.out.push(em);
            }
            // server-side outcomes: the world FX layer draws them from `NetEvent::StarFx`
            CombatEvent::StarBurst { .. } | CombatEvent::StarGone { .. } | CombatEvent::StarBlind { .. } => {}
            CombatEvent::Recoil { .. } | CombatEvent::PickedUp { .. } | CombatEvent::GrappleRelease { .. } | CombatEvent::PowerFx { .. } => {}
        }
    }

    fn push_dash(&mut self, tick: u32) {
        let mut em = self.mk(EmitterKind::DashStreaks, self.palette.cyan, 2.5);
        em.dir = [0.0, 0.0, -1.0];
        em.count = 24;
        em.life = 0.22;
        em.speed = 30.0;
        em.spread = 0.12;
        em.width = 0.03;
        em.origin = [0.0, 0.0, -1.0];
        em.view_space = true;
        em.seed = tick;
        self.out.push(em);
    }

    /// Call once per frame. Dash streaks fire on the dash rising edge; wall-run lines at a fixed rate.
    pub fn on_move(&mut self, dt: f32, m: &MoveFx, tick: u32) {
        if m.dashing && !self.was_dashing {
            self.push_dash(tick);
        }
        self.was_dashing = m.dashing;

        if let Some(n) = m.wall_normal {
            self.wall_accum += dt * WALL_LINES_PER_SEC;
            let whole = self.wall_accum.floor();
            self.wall_accum -= whole;
            if whole >= 1.0 {
                let back = norm3([-m.vel[0], -m.vel[1], -m.vel[2]], [0.0, 0.0, 1.0]);
                let mut em = self.mk(EmitterKind::WallRunLines, self.palette.cyan, 1.6);
                // start on the wall side of the view, stream backward past the camera
                em.origin = [-n[0] * 0.6, 0.0, -n[2] * 0.6];
                em.dir = back;
                em.count = whole as u32;
                em.life = 0.25;
                em.speed = 22.0;
                em.spread = 0.04;
                em.width = 0.02;
                em.view_space = true;
                em.seed = tick;
                self.out.push(em);
            }
        } else {
            self.wall_accum = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shinobi_sim::rapier3d::prelude::{Point, Vector};

    fn sys() -> EmitterSystem {
        EmitterSystem::new(Palette::TRON)
    }

    #[test]
    fn instance_is_64_bytes_of_floats() {
        assert_eq!(RibbonInstance::FLOATS * 4, 64);
    }

    #[test]
    fn slash_damage_spawns_orange_spark_burst() {
        let mut s = sys();
        s.on_event(
            &CombatEvent::Damage {
                target: 1,
                collider: shinobi_sim::rapier3d::prelude::ColliderHandle::invalid(),
                amount: 50.0,
                kind: DamageKind::Slash,
                point: Point::new(1.0, 2.0, 3.0),
            },
            10,
        );
        let e = s.out[0];
        assert_eq!(e.kind, EmitterKind::HitSpark);
        assert_eq!(e.origin, [1.0, 2.0, 3.0]);
        assert!(e.count >= 18 && e.color[0] > 1.0, "HDR orange");
        assert!(e.color[0] > e.color[2], "orange, not cyan");
    }

    #[test]
    fn lunge_slash_adds_dash_streaks_but_plain_slash_does_not() {
        let mut s = sys();
        let ev = |lunge| CombatEvent::Slash {
            flip: false,
            dir: Vector::new(1.0, 0.0, 0.0),
            lunge,
            hits: 0,
            origin: Point::origin(),
            reach: 5.0,
            half_width: 1.0,
            half_height: 1.0,
            damage: 120.0,
            kind: shinobi_sim::combat::SlashKind::Light,
        };
        s.on_event(&ev(false), 1);
        assert_eq!(s.out.len(), 1);
        s.out.clear();
        s.on_event(&ev(true), 2);
        let kinds: Vec<_> = s.out.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec![EmitterKind::SlashArc, EmitterKind::DashStreaks]);
        assert!(s.out.iter().all(|e| e.view_space));
    }

    #[test]
    fn dash_streaks_fire_once_per_dash_edge() {
        let mut s = sys();
        let dash = MoveFx { dashing: true, ..Default::default() };
        s.on_move(1.0 / 60.0, &dash, 1);
        s.on_move(1.0 / 60.0, &dash, 2);
        assert_eq!(s.out.len(), 1);
        s.on_move(1.0 / 60.0, &MoveFx::default(), 3);
        s.on_move(1.0 / 60.0, &dash, 4);
        assert_eq!(s.out.len(), 2);
    }

    #[test]
    fn wall_run_lines_are_rate_limited_and_stop_off_wall() {
        let mut s = sys();
        let wall = MoveFx { vel: [0.0, 0.0, -10.0], wall_normal: Some([1.0, 0.0, 0.0]), ..Default::default() };
        for i in 0..60 {
            s.on_move(1.0 / 60.0, &wall, i);
        }
        let total: u32 = s.out.iter().map(|e| e.count).sum();
        assert!((28..=32).contains(&total), "~30 lines in 1 s, got {total}");
        assert!(s.out[0].dir[2] > 0.9, "streams backward past the camera");
        let n = s.out.len();
        s.on_move(1.0 / 60.0, &MoveFx::default(), 99);
        assert_eq!(s.out.len(), n);
    }

    #[test]
    fn shuriken_thrown_makes_follow_trail_with_no_burst() {
        let mut s = sys();
        s.on_event(&CombatEvent::ShurikenThrown { pos: Point::new(0.0, 1.0, 0.0), dir: Vector::new(0.0, 0.0, -2.0), kind: shinobi_sim::combat::stars::StarKind::Standard }, 5);
        assert_eq!(s.out[0].kind, EmitterKind::ShurikenTrail);
        assert!(s.out[0].instances(0.0).is_empty());
        assert!((s.out[0].dir[2] + 1.0).abs() < 1e-5, "dir normalized");
        let seg = RibbonInstance::trail_segment([0.0; 3], [0.0, 0.0, -1.0], s.out[0].color, 0.1, 0.25, 3.0);
        assert!(seg.p1[3] > seg.p0[3], "tail thinner than head");
    }

    #[test]
    fn burst_expansion_deterministic_bounded_and_outward() {
        let mut s = sys();
        s.on_event(&CombatEvent::ShurikenBounce { pos: Point::new(0.0, 0.0, 0.0), normal: Vector::new(0.0, 1.0, 0.0) }, 77);
        let e = s.out[0];
        let a = e.instances(5.0);
        let b = e.instances(5.0);
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
        for r in &a {
            assert!(r.p1[1] >= r.p0[1] - 1e-4, "sparks leave the surface along the normal hemisphere");
            assert_eq!(r.params[1], e.life);
        }
        let e2 = Emitter { seed: 78, ..e };
        assert_ne!(a, e2.instances(5.0));
    }
}
