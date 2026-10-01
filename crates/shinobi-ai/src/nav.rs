//! Steering helpers. There is no navmesh: bots steer with whiskers (Rapier rays) over primitive level geometry,
//! (Placeable wall blocks, once the sim has them, are ordinary solids to these rays.)

use std::f32::consts::{PI, TAU};

use shinobi_sim::combat::GROUP_WORLD;
use shinobi_sim::rapier3d::prelude::*;
use shinobi_sim::tick::Server;

use crate::view::{PlayerView, WorldView, V3};

/// Horizontal part of a vector.
pub fn flat(v: V3) -> V3 {
    vector![v.x, 0.0, v.z]
}

pub fn unit_or_zero(v: V3) -> V3 {
    let n = v.norm();
    if n < 1e-5 { Vector::zeros() } else { v / n }
}

/// Wrap an angle to (-pi, pi].
pub fn wrap_pi(a: f32) -> f32 {
    let w = a.rem_euclid(TAU);
    if w > PI { w - TAU } else { w }
}

/// Forward vector for a yaw (0 faces -Z, positive turns toward -X). Same as `Aim::flat_forward`.
pub fn dir_from_yaw(yaw: f32) -> V3 {
    vector![-yaw.sin(), 0.0, -yaw.cos()]
}

pub fn right_of_yaw(yaw: f32) -> V3 {
    vector![yaw.cos(), 0.0, -yaw.sin()]
}

/// Yaw that faces `dir` (flat).
pub fn yaw_of(dir: V3) -> f32 {
    (-dir.x).atan2(-dir.z)
}

/// World direction -> view-frame stick (strafe, forward), each in -1..1, scaled by `mag`.
pub fn stick_for(dir: V3, mag: f32, yaw: f32) -> (f32, f32) {
    let d = unit_or_zero(flat(dir)) * mag.clamp(0.0, 1.0);
    (d.dot(&right_of_yaw(yaw)), d.dot(&dir_from_yaw(yaw)))
}

pub fn rotate_y(v: V3, a: f32) -> V3 {
    let (s, c) = a.sin_cos();
    vector![v.x * c + v.z * s, v.y, -v.x * s + v.z * c]
}

/// A flat-topped box a bot can hop onto (single + double jump). Derived from level colliders.
#[derive(Clone, Copy, Debug)]
pub struct Perch {
    /// Box centre x/z, y = top face.
    pub top_center: V3,
    pub half_xz: [f32; 2],
}

impl Perch {
    pub fn contains_xz(&self, p: V3, pad: f32) -> bool {
        (p.x - self.top_center.x).abs() <= self.half_xz[0] + pad && (p.z - self.top_center.z).abs() <= self.half_xz[1] + pad
    }

    /// Horizontal distance from `p` to the box outline (0 inside).
    pub fn edge_dist(&self, p: V3) -> f32 {
        let dx = ((p.x - self.top_center.x).abs() - self.half_xz[0]).max(0.0);
        let dz = ((p.z - self.top_center.z).abs() - self.half_xz[1]).max(0.0);
        (dx * dx + dz * dz).sqrt()
    }
}

/// Scan static level cuboids for hop-able tops. Deterministic: collider arena order, then sorted.
pub fn find_perches(server: &Server, min_top: f32, max_top: f32) -> Vec<Perch> {
    let mut out = Vec::new();
    for (_, c) in server.world.colliders.iter() {
        if c.is_sensor() || !c.collision_groups().memberships.intersects(GROUP_WORLD) {
            continue;
        }
        let Some(cu) = c.shape().as_cuboid() else { continue };
        let pos = c.position();
        if pos.rotation.angle().abs() > 0.05 {
            continue; // ramps and yawed boxes are not flat tops
        }
        let h = cu.half_extents;
        let top = pos.translation.y + h.y;
        if top < min_top || top > max_top || h.x < 0.6 || h.z < 0.6 || h.x > 60.0 || h.z > 60.0 {
            continue;
        }
        out.push(Perch { top_center: vector![pos.translation.x, top, pos.translation.z], half_xz: [h.x, h.z] });
    }
    out.sort_by(|a, b| {
        (a.top_center.x, a.top_center.z, a.top_center.y)
            .partial_cmp(&(b.top_center.x, b.top_center.z, b.top_center.y))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// Whisker steering over static geometry: if the way ahead is blocked, bend toward the freer side.
pub fn steer_static(world: &WorldView, me: &PlayerView, dir: V3) -> V3 {
    let d = unit_or_zero(flat(dir));
    if d == Vector::zeros() {
        return d;
    }
    let knee = vector![me.pos.x, me.pos.y - 0.3, me.pos.z];
    const LEN: f32 = 2.0;
    let ahead = world.ray(knee, d, LEN);
    if ahead.is_none() {
        return d;
    }
    let side = |a: f32| world.ray(knee, rotate_y(d, a), LEN).unwrap_or(LEN + 1.0);
    let (l, r) = (side(0.9), side(-0.9));
    let (l2, r2) = (side(1.6), side(-1.6));
    // prefer the side with more room; ties go left (deterministic)
    let (ls, rs) = (l + l2 * 0.5, r + r2 * 0.5);
    let a = if ls >= rs { 0.9 + 0.3 * (1.0 - ahead.unwrap() / LEN) } else { -0.9 - 0.3 * (1.0 - ahead.unwrap() / LEN) };
    rotate_y(d, a)
}

/// Which side (+1 = right of `heading`, -1 = left) has level geometry within wall-run reach, if any.
pub fn wall_side(world: &WorldView, me: &PlayerView, heading: V3) -> Option<f32> {
    let h = unit_or_zero(flat(heading));
    if h == Vector::zeros() {
        return None;
    }
    let right = vector![-h.z, 0.0, h.x].normalize();
    let reach = world.cfg.radius + world.cfg.wall_probe;
    let mut best: Option<(f32, f32)> = None;
    for s in [1.0f32, -1.0] {
        let hit = [-world.cfg.half_height, 0.0, world.cfg.half_height].iter().all(|dy| {
            let o = vector![me.pos.x, me.pos.y + dy, me.pos.z];
            world.ray(o, right * s, reach).is_some()
        });
        if hit {
            let d = world.ray(me.pos, right * s, reach).unwrap_or(reach);
            if best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, s));
            }
        }
    }
    best.map(|b| b.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stick_roundtrips_world_direction() {
        for yaw in [0.0f32, 0.7, 2.0, -1.3] {
            for ang in [0.0f32, 1.0, -2.0, 3.0] {
                let want = dir_from_yaw(ang);
                let (x, z) = stick_for(want, 1.0, yaw);
                // what the server does in PlayerIntent::world_move
                let (s, c) = yaw.sin_cos();
                let got = vector![z * -s + x * c, 0.0, z * -c + x * -s];
                assert!((got - want).norm() < 1e-4, "yaw {yaw} ang {ang}: {got:?} vs {want:?}");
            }
        }
    }

    #[test]
    fn yaw_of_inverts_dir_from_yaw() {
        for y in [0.0f32, 1.0, -2.5, 3.0] {
            assert!(wrap_pi(yaw_of(dir_from_yaw(y)) - y).abs() < 1e-4);
        }
    }
}
