//! Read-only world snapshot a brain may look at. Fair-play rule: only what a client could know (replicated
//! poses incl. yaw, level geometry via rays). No hitbox or cooldown peeking at other players.

use shinobi_proto::quant::{dq_pitch, dq_yaw};
use shinobi_sim::character::{Cfg, MoveState};
use shinobi_sim::combat::GROUP_WORLD;
use shinobi_sim::rapier3d::prelude::*;
use shinobi_sim::tick::Server;

/// Eye above the capsule centre (capsule centre = feet + 0.8).
pub const EYE_OFFSET: f32 = 0.6;

pub type V3 = Vector<Real>;

#[derive(Clone, Copy, Debug)]
pub struct PlayerView {
    pub id: u16,
    /// Capsule centre.
    pub pos: V3,
    pub vel: V3,
    pub yaw: f32,
    pub pitch: f32,
    pub alive: bool,
    pub hp_frac: f32,
    pub spawn_protected: bool,
    pub state: MoveState,
    pub dash_ready: bool,
    /// Rope attached / wrist grapple ready to fire (the katana turns a hooked swing into a hook-strike).
    pub hooked: bool,
    pub hook_ready: bool,
    /// The rope let go within the hook-strike window (hook -> air-slash).
    pub hook_recent: bool,
    pub double_used_hint: bool,
    pub is_bot: bool,
    /// `Player.team` (replicated to every client, so fair to read).
    pub team: u8,
    /// Stealth cloak up (`cloak.rs`): every client sees this as the shimmer outline, so fair to read.
    pub cloaked: bool,
    /// Loud despite the cloak: sprinting, dashing/rolling or the grapple attached (`shinobi_sim::cloak::loud`).
    pub cloak_loud: bool,
}

impl PlayerView {
    pub fn eye(&self) -> V3 {
        self.pos + vector![0.0, EYE_OFFSET, 0.0]
    }

    pub fn grounded(&self) -> bool {
        self.state == MoveState::Grounded
    }

    pub fn flat_speed(&self) -> f32 {
        (self.vel.x * self.vel.x + self.vel.z * self.vel.z).sqrt()
    }
}

pub struct WorldView<'a> {
    pub tick: u32,
    pub server: &'a Server,
    pub cfg: Cfg,
    /// Sorted by id.
    pub players: Vec<PlayerView>,
}

impl<'a> WorldView<'a> {
    pub fn build(server: &'a Server, is_bot: impl Fn(u16) -> bool) -> Self {
        let cfg = *server.cfg();
        let mut players: Vec<PlayerView> = server
            .players
            .iter()
            .map(|p| {
                let t = server.world.bodies[p.ch.body].translation();
                PlayerView {
                    id: p.id,
                    pos: vector![t.x, t.y, t.z],
                    vel: p.ch.vel,
                    yaw: dq_yaw(p.yaw),
                    pitch: dq_pitch(p.pitch),
                    alive: p.vitals.alive(),
                    hp_frac: p.vitals.hp_frac(),
                    spawn_protected: p.vitals.spawn_protected(server.tick),
                    state: p.ch.state,
                    dash_ready: p.ch.dash_ready(),
                    hooked: p.ch.grapple.is_some(),
                    hook_ready: p.ch.grapple.is_none() && p.ch.grapple_cooldown() <= 0.0,
                    hook_recent: p.ch.grapple.is_none() && p.ch.hook_age <= cfg.hookstrike_window.max(server.balance.combat.katana.hook_air_window),
                    double_used_hint: p.ch.double_jumped,
                    is_bot: is_bot(p.id),
                    team: p.team,
                    cloaked: p.cloak.active,
                    cloak_loud: shinobi_sim::cloak::loud(
                        (p.ch.vel.x * p.ch.vel.x + p.ch.vel.z * p.ch.vel.z).sqrt(),
                        cfg.run_speed,
                        p.ch.evading().is_some(),
                        p.ch.grapple.is_some(),
                    ),
                }
            })
            .collect();
        players.sort_by_key(|p| p.id);
        Self { tick: server.tick, server, cfg, players }
    }

    pub fn player(&self, id: u16) -> Option<&PlayerView> {
        self.players.binary_search_by_key(&id, |p| p.id).ok().map(|i| &self.players[i])
    }

    /// Level solids only (no players, no sensors). Same set the movement controller collides with.
    pub fn solid_filter() -> QueryFilter<'static> {
        QueryFilter::default().exclude_sensors().groups(InteractionGroups::new(Group::ALL, GROUP_WORLD))
    }

    /// Distance to the first solid along `dir` (unit) from `origin`, or `None` within `max`.
    pub fn ray(&self, origin: V3, dir: V3, max: f32) -> Option<f32> {
        if !(origin.iter().all(|v| v.is_finite()) && dir.iter().all(|v| v.is_finite()) && dir.norm_squared() > 1e-12) {
            return None;
        }
        let ray = Ray::new(Point::from(origin), dir);
        self.server
            .world
            .queries
            .cast_ray(&self.server.world.bodies, &self.server.world.colliders, &ray, max, true, Self::solid_filter())
            .map(|(_, toi)| toi)
    }

    /// Unobstructed line between two points (level geometry and solid walls block).
    pub fn los(&self, a: V3, b: V3) -> bool {
        let d = b - a;
        let len = d.norm();
        if len < 1e-3 {
            return true;
        }
        self.ray(a, d / len, len - 0.2).is_none()
    }
}

/// First-person camera frustum. Vertical FOV matches `CameraCfg::base_fov_deg` (90).
#[derive(Clone, Copy, Debug)]
pub struct Frustum {
    pub fov_v: f32,
    pub aspect: f32,
}

impl Default for Frustum {
    fn default() -> Self {
        Self { fov_v: 90f32.to_radians(), aspect: 16.0 / 9.0 }
    }
}

/// Where a point sits in camera space, as angles.
#[derive(Clone, Copy, Debug)]
pub struct ViewAngles {
    /// + = to the camera's right.
    pub h: f32,
    /// + = above the crosshair.
    pub v: f32,
    /// Depth along the view axis (<= 0 = behind the camera).
    pub depth: f32,
}

impl Frustum {
    pub fn half_h(&self) -> f32 {
        ((self.fov_v * 0.5).tan() * self.aspect).atan()
    }

    pub fn half_v(&self) -> f32 {
        self.fov_v * 0.5
    }

    pub fn angles(&self, eye: V3, yaw: f32, pitch: f32, p: V3) -> ViewAngles {
        let (sy, cy) = yaw.sin_cos();
        let (sp, cp) = pitch.sin_cos();
        let f = vector![-sy * cp, sp, -cy * cp];
        let r = vector![cy, 0.0, -sy];
        let u = r.cross(&f);
        let d = p - eye;
        let (x, y, z) = (d.dot(&r), d.dot(&u), d.dot(&f));
        ViewAngles { h: x.atan2(z.max(1e-4)), v: y.atan2(z.max(1e-4)), depth: z }
    }

    /// `margin` (radians) widens the frustum: a bot reacting at margin > 0 breaks line of sight *before* it is seen.
    pub fn contains(&self, eye: V3, yaw: f32, pitch: f32, p: V3, margin: f32) -> bool {
        let a = self.angles(eye, yaw, pitch, p);
        a.depth > 0.05 && a.h.abs() <= self.half_h() + margin && a.v.abs() <= self.half_v() + margin
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frustum_basics() {
        let f = Frustum::default();
        let eye = vector![0.0, 1.0, 0.0];
        // yaw 0 faces -Z
        assert!(f.contains(eye, 0.0, 0.0, vector![0.0, 1.0, -10.0], 0.0));
        assert!(!f.contains(eye, 0.0, 0.0, vector![0.0, 1.0, 10.0], 0.0), "behind");
        // 70 deg right of the axis: outside the ~60.6 deg half-width
        let a = 70f32.to_radians();
        assert!(!f.contains(eye, 0.0, 0.0, vector![a.sin() * 10.0, 1.0, -a.cos() * 10.0], 0.0));
        assert!(f.contains(eye, 0.0, 0.0, vector![a.sin() * 10.0, 1.0, -a.cos() * 10.0], 12f32.to_radians()));
        // turning the camera right (yaw negative = toward +X) brings it into view
        assert!(f.contains(eye, -a, 0.0, vector![a.sin() * 10.0, 1.0, -a.cos() * 10.0], 0.0));
        // right of axis is positive h
        assert!(f.angles(eye, 0.0, 0.0, vector![5.0, 1.0, -10.0]).h > 0.0);
        // looking straight up hides things at eye level
        assert!(!f.contains(eye, 0.0, 80f32.to_radians(), vector![0.0, 1.0, -10.0], 0.0));
    }
}
