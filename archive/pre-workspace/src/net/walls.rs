//! Tron light-wall replication. A wall is fully described by integers: quantised endpoints + spawn tick.
//! Expiry is `spawn_tick + life_ticks` (integer), so no despawn message is needed; early removal
//! (trail cap overflow, owner death) is an explicit `Kill` event on the reliable stream.
//! The server builds its collider from the *dequantised* endpoints, identical to what clients render.

use std::collections::{HashMap, VecDeque};

use rapier3d::prelude::*;

use super::quant::{dq_pos3, q_pos3};

pub type WallId = u32;
pub type OwnerId = u8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WallSpawn {
    pub id: WallId,
    pub owner: OwnerId,
    pub spawn_tick: u32,
    /// 1/512 m fixed point
    pub a: [i32; 3],
    pub b: [i32; 3],
}

impl WallSpawn {
    pub fn expire_tick(&self, life_ticks: u32) -> u32 {
        self.spawn_tick.saturating_add(life_ticks)
    }

    pub fn geometry(&self) -> WallGeometry {
        WallGeometry::from_endpoints(dq_pos3(self.a), dq_pos3(self.b))
    }
}

/// Derived from quantised endpoints only; same formula as `character::update_trail`.
#[derive(Clone, Copy, Debug)]
pub struct WallGeometry {
    pub mid: [f32; 3],
    pub half_len: f32,
    pub yaw: f32,
}

impl WallGeometry {
    pub fn from_endpoints(a: [f32; 3], b: [f32; 3]) -> Self {
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        Self {
            mid: [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5, (a[2] + b[2]) * 0.5],
            half_len: (d[0] * d[0] + d[2] * d[2]).sqrt() * 0.5,
            yaw: (-d[2]).atan2(d[0]),
        }
    }

    /// Sensor cuboid matching the local-player trail collider (`character.rs`).
    pub fn collider(&self, height: f32, thick: f32) -> Collider {
        ColliderBuilder::cuboid(self.half_len + 0.02, height * 0.5, thick * 0.5)
            .position(Isometry::new(
                Vector::new(self.mid[0], self.mid[1] + height * 0.5, self.mid[2]),
                Vector::new(0.0, self.yaw, 0.0),
            ))
            .sensor(true)
            .build()
    }
}

/// Output of feeding one position sample to an `OwnerTrail`.
#[derive(Default, Debug)]
pub struct TrailStep {
    pub spawned: Option<WallSpawn>,
    /// Walls that ended this tick (time expiry). Server removes their colliders; no network message.
    pub expired: Vec<WallId>,
    /// Walls killed early (cap). These need a `Kill` event.
    pub killed: Vec<WallId>,
}

/// Server-side trail of one player. Mirrors `CharacterSystem::update_trail` but on integer ticks.
pub struct OwnerTrail {
    pub owner: OwnerId,
    pub spacing: f32,
    pub life_ticks: u32,
    pub cap: usize,
    last: Option<[i32; 3]>,
    live: VecDeque<(WallId, u32)>, // id, expire tick
}

impl OwnerTrail {
    pub fn new(owner: OwnerId, spacing: f32, life_ticks: u32, cap: usize) -> Self {
        Self { owner, spacing, life_ticks, cap, last: None, live: VecDeque::new() }
    }

    pub fn sample(&mut self, tick: u32, feet: [f32; 3], next_id: &mut WallId) -> TrailStep {
        let mut out = TrailStep::default();
        while let Some(&(id, exp)) = self.live.front() {
            if tick >= exp {
                out.expired.push(id);
                self.live.pop_front();
            } else {
                break;
            }
        }
        let cur = q_pos3(feet);
        let Some(prev) = self.last else {
            self.last = Some(cur);
            return out;
        };
        let (dx, dz) = (dq_pos3(cur)[0] - dq_pos3(prev)[0], dq_pos3(cur)[2] - dq_pos3(prev)[2]);
        if (dx * dx + dz * dz).sqrt() < self.spacing {
            return out;
        }
        if self.live.len() >= self.cap {
            if let Some((id, _)) = self.live.pop_front() {
                out.killed.push(id);
            }
        }
        let w = WallSpawn { id: *next_id, owner: self.owner, spawn_tick: tick, a: prev, b: cur };
        *next_id += 1;
        self.live.push_back((w.id, w.expire_tick(self.life_ticks)));
        self.last = Some(cur);
        out.spawned = Some(w);
        out
    }

    /// Owner died / respawned: kill everything, restart the chain.
    pub fn clear(&mut self) -> Vec<WallId> {
        self.last = None;
        self.live.drain(..).map(|(id, _)| id).collect()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Wall {
    pub spawn: WallSpawn,
    pub killed_at: Option<u32>,
}

impl Wall {
    pub fn is_live(&self, tick: u32, life_ticks: u32) -> bool {
        tick >= self.spawn.spawn_tick
            && tick < self.spawn.expire_tick(life_ticks)
            && self.killed_at.map_or(true, |k| tick < k)
    }
}

/// Client-side (and server-side, for hit queries) wall table.
#[derive(Default)]
pub struct WallStore {
    walls: HashMap<WallId, Wall>,
}

impl WallStore {
    pub fn apply_spawn(&mut self, s: WallSpawn) {
        self.walls.entry(s.id).or_insert(Wall { spawn: s, killed_at: None });
    }

    pub fn apply_kill(&mut self, id: WallId, tick: u32) {
        if let Some(w) = self.walls.get_mut(&id) {
            w.killed_at = Some(w.killed_at.map_or(tick, |k| k.min(tick)));
        }
    }

    /// Walls visible at `tick` (pass the interpolated render tick floor for drawing,
    /// the server tick for collision).
    pub fn live_at(&self, tick: u32, life_ticks: u32) -> impl Iterator<Item = &Wall> + '_ {
        self.walls.values().filter(move |w| w.is_live(tick, life_ticks))
    }

    pub fn prune(&mut self, tick: u32, life_ticks: u32, grace_ticks: u32) {
        self.walls.retain(|_, w| {
            tick < w.spawn.expire_tick(life_ticks).saturating_add(grace_ticks)
                && w.killed_at.map_or(true, |k| tick < k.saturating_add(grace_ticks))
        });
    }

    pub fn len(&self) -> usize {
        self.walls.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIFE: u32 = 240;

    #[test]
    fn server_and_client_agree_on_liveness() {
        let mut trail = OwnerTrail::new(1, 0.4, LIFE, 128);
        let mut id = 1;
        let mut server = WallStore::default();
        let mut client = WallStore::default();
        let mut spawned = vec![];
        for tick in 0..600u32 {
            let step = trail.sample(tick, [tick as f32 * 0.15, 0.0, 0.0], &mut id);
            if let Some(s) = step.spawned {
                server.apply_spawn(s);
                spawned.push(s);
                // wire roundtrip is lossless by construction (integers); client sees it a few ticks late
                client.apply_spawn(s);
            }
        }
        assert!(spawned.len() > 100);
        for tick in [0u32, 50, 239, 240, 300, 500, 599] {
            let mut s: Vec<_> = server.live_at(tick, LIFE).map(|w| w.spawn.id).collect();
            let mut c: Vec<_> = client.live_at(tick, LIFE).map(|w| w.spawn.id).collect();
            s.sort();
            c.sort();
            assert_eq!(s, c);
        }
        let first = spawned[0];
        assert!(server.live_at(first.spawn_tick + LIFE - 1, LIFE).any(|w| w.spawn.id == first.id));
        assert!(!server.live_at(first.spawn_tick + LIFE, LIFE).any(|w| w.spawn.id == first.id));
    }

    #[test]
    fn cap_overflow_kills_oldest() {
        let mut trail = OwnerTrail::new(2, 0.4, LIFE, 3);
        let mut id = 1;
        let mut kills = 0;
        for t in 0..10u32 {
            let step = trail.sample(t, [t as f32, 0.0, 0.0], &mut id);
            kills += step.killed.len();
        }
        assert_eq!(kills, 6); // 9 walls spawned, 3 live
    }

    #[test]
    fn geometry_uses_quantised_endpoints() {
        let w = WallSpawn { id: 1, owner: 0, spawn_tick: 0, a: q_pos3([0.0, 0.0, 0.0]), b: q_pos3([2.0, 0.0, 0.0]) };
        let g = w.geometry();
        assert!((g.half_len - 1.0).abs() < 1e-3);
        let _ = g.collider(1.6, 0.15);
    }
}
