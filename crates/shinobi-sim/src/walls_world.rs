//! Tron walls as solid collision in the authoritative world.
//!
//! A wall (`WallSpawn`) becomes a fixed, non-sensor cuboid (a compound part, see `CHUNK_TICKS`) in `GROUP_WORLD`,
//! so the character controller and projectile raycasts treat it like level geometry. Lifetime is pure integer tick math
//! (`spawn_tick + life_ticks`), identical to what clients derive, so no despawn message exists.
//!
//! `grace_ticks` delays activation so the owner is not entombed by the segment that ends at their own feet.
//! Enough grace to run clear of the newest segment (~15 ticks = 2.25 m at run speed) keeps movement smooth;
//! doubling back onto your own trail afterwards still blocks you, which is the point of the genre.

use std::collections::HashMap;

use rapier3d::prelude::*;

use crate::character::PhysicsWorld;
use crate::combat::GROUP_WORLD;
use crate::walls::{OwnerId, WallId, WallSpawn};

/// Ticks a new wall stays non-solid so its owner can run clear of the segment at their own feet.
pub const WALL_GRACE_TICKS: u32 = 15;

/// Walls of one owner spawned within the same `CHUNK_TICKS` window share ONE compound collider.
/// The query BVH is rebuilt every player step, so its cost scales with collider count, not shape count:
/// a 4 s trail is ~90 cuboids but only `life_ticks / CHUNK_TICKS` (10) colliders. Expiry stays exact: the chunk
/// is rebuilt whenever its set of armed parts changes, so a part never outlives (or precedes) its own timer.
pub const CHUNK_TICKS: u32 = 24;

type ChunkKey = (OwnerId, u32);

struct Entry {
    spawn: WallSpawn,
    killed_at: Option<u32>,
    armed: bool,
}

#[derive(Default)]
struct Chunk {
    handle: Option<ColliderHandle>,
    /// Ids currently baked into `handle`'s compound shape (sorted).
    parts: Vec<WallId>,
}

pub struct WallsWorld {
    pub life_ticks: u32,
    pub grace_ticks: u32,
    pub height: f32,
    pub thick: f32,
    entries: HashMap<WallId, Entry>,
    chunks: HashMap<ChunkKey, Chunk>,
}

fn chunk_key(s: &WallSpawn) -> ChunkKey {
    (s.owner, s.spawn_tick / CHUNK_TICKS)
}

impl WallsWorld {
    pub fn new(life_ticks: u32, grace_ticks: u32, height: f32, thick: f32) -> Self {
        Self { life_ticks, grace_ticks, height, thick, entries: HashMap::new(), chunks: HashMap::new() }
    }

    /// Register a wall; it becomes solid on the first `sync` at or after `spawn_tick + grace_ticks`.
    pub fn add(&mut self, spawn: WallSpawn) {
        self.entries.entry(spawn.id).or_insert(Entry { spawn, killed_at: None, armed: false });
    }

    /// Early removal (cap overflow, owner death). Effective from `tick`.
    pub fn kill(&mut self, id: WallId, tick: u32) {
        if let Some(e) = self.entries.get_mut(&id) {
            e.killed_at = Some(e.killed_at.map_or(tick, |k| k.min(tick)));
        }
    }

    fn wanted(&self, e: &Entry, tick: u32) -> bool {
        tick >= e.spawn.spawn_tick.saturating_add(self.grace_ticks)
            && tick < e.spawn.expire_tick(self.life_ticks)
            && e.killed_at.map_or(true, |k| tick < k)
    }

    /// Same cuboid the single-collider version used (`WallGeometry::collider`), as a compound part.
    fn part(&self, spawn: &WallSpawn) -> (Isometry<Real>, SharedShape) {
        let g = spawn.geometry();
        (
            Isometry::new(Vector::new(g.mid[0], g.mid[1] + self.height * 0.5, g.mid[2]), Vector::new(0.0, g.yaw, 0.0)),
            SharedShape::cuboid(g.half_len + 0.02, self.height * 0.5, self.thick * 0.5),
        )
    }

    /// Reconcile colliders with `tick`. Call once per server frame (after the step that reached `tick`).
    pub fn sync(&mut self, tick: u32, w: &mut PhysicsWorld) {
        // 1. which walls should be solid now, grouped by chunk (sorted ids => deterministic compound order)
        let mut wanted: HashMap<ChunkKey, Vec<WallId>> = HashMap::new();
        let flags: Vec<(WallId, bool)> = self.entries.iter().map(|(id, e)| (*id, self.wanted(e, tick))).collect();
        for (id, want) in flags {
            let e = self.entries.get_mut(&id).unwrap();
            e.armed = want;
            if want {
                wanted.entry(chunk_key(&e.spawn)).or_default().push(id);
            }
        }
        for ids in wanted.values_mut() {
            ids.sort_unstable();
        }

        // 2. rebuild every chunk whose armed set changed
        let mut keys: Vec<ChunkKey> = wanted.keys().copied().chain(self.chunks.keys().copied()).collect();
        keys.sort_unstable();
        keys.dedup();
        for key in keys {
            let want = wanted.remove(&key).unwrap_or_default();
            let chunk = self.chunks.entry(key).or_default();
            if chunk.parts == want {
                continue;
            }
            if let Some(h) = chunk.handle.take() {
                w.colliders.remove(h, &mut w.islands, &mut w.bodies, false);
            }
            if !want.is_empty() {
                let parts: Vec<_> = want.iter().map(|id| self.part(&self.entries[id].spawn)).collect();
                let collider = ColliderBuilder::new(SharedShape::compound(parts))
                    .collision_groups(InteractionGroups::new(GROUP_WORLD, Group::ALL))
                    .build();
                self.chunks.get_mut(&key).unwrap().handle = Some(w.colliders.insert(collider));
            }
            self.chunks.get_mut(&key).unwrap().parts = want;
        }
        self.chunks.retain(|_, c| !c.parts.is_empty());

        // 3. forget walls that are over (they are no longer armed, so nothing references them)
        let life = self.life_ticks;
        self.entries.retain(|_, e| {
            let over = tick >= e.spawn.expire_tick(life) || e.killed_at.map_or(false, |k| tick >= k);
            !(over && !e.armed)
        });
    }

    /// Map change: drop every wall and its collider.
    pub fn clear(&mut self, w: &mut PhysicsWorld) {
        for (_, c) in self.chunks.drain() {
            if let Some(h) = c.handle {
                w.colliders.remove(h, &mut w.islands, &mut w.bodies, false);
            }
        }
        self.entries.clear();
    }

    /// Walls that are solid right now (past grace, not expired or killed as of the last `sync`).
    /// Read API for hazards: `spawn.owner`, `spawn.spawn_tick`, `spawn.geometry()`, plus `height` / `thick`.
    pub fn armed(&self) -> impl Iterator<Item = &WallSpawn> + '_ {
        self.entries.values().filter(|e| e.armed).map(|e| &e.spawn)
    }

    /// Armed wall segments.
    pub fn active(&self) -> usize {
        self.entries.values().filter(|e| e.armed).count()
    }

    /// Physics colliders backing them (the number that drives query cost).
    pub fn colliders(&self) -> usize {
        self.chunks.values().filter(|c| c.handle.is_some()).count()
    }

    pub fn tracked(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::{CharacterSystem, Cfg, PlayerInput};
    use crate::combat::GROUP_PLAYER;
    use shinobi_proto::quant::q_pos3 as q;

    const DT: f32 = 1.0 / 60.0;

    fn world_with_player() -> (PhysicsWorld, CharacterSystem) {
        let cfg = Cfg::default();
        let mut w = PhysicsWorld {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            queries: QueryPipeline::new(),
        };
        w.colliders.insert(
            ColliderBuilder::cuboid(100.0, 0.5, 100.0)
                .translation(vector![0.0, -0.5, 0.0])
                .collision_groups(InteractionGroups::new(GROUP_WORLD, Group::ALL))
                .build(),
        );
        let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![0.0, 0.81, 0.0]).build());
        w.colliders.insert_with_parent(
            ColliderBuilder::capsule_y(cfg.half_height, cfg.radius)
                .collision_groups(InteractionGroups::new(GROUP_PLAYER, Group::ALL))
                .build(),
            body,
            &mut w.bodies,
        );
        (w, CharacterSystem::new(body, cfg))
    }

    fn step(w: &mut PhysicsWorld, ch: &mut CharacterSystem, now: f32) {
        w.queries.update(&w.colliders);
        let input = PlayerInput { move_dir: vector![1.0, 0.0, 0.0], ..Default::default() };
        ch.update(DT, &input, w, now);
        let next = *w.bodies[ch.body].next_position();
        w.bodies[ch.body].set_position(next, true);
    }

    #[test]
    fn wall_blocks_player_then_vanishes_at_expiry() {
        const LIFE: u32 = 90;
        let (mut w, mut ch) = world_with_player();
        let mut walls = WallsWorld::new(LIFE, 0, 1.6, 0.15);
        // wall across the +x run path at x = 3, spawned at tick 0
        walls.add(WallSpawn { id: 1, owner: 9, spawn_tick: 0, a: q([3.0, 0.0, -3.0]), b: q([3.0, 0.0, 3.0]) });

        for t in 0..LIFE {
            walls.sync(t, &mut w);
            step(&mut w, &mut ch, t as f32 * DT);
            let x = w.bodies[ch.body].translation().x;
            assert!(x < 3.0, "tick {t}: player tunnelled through wall (x={x})");
        }
        assert_eq!(walls.active(), 1);
        let held = w.bodies[ch.body].translation().x;
        assert!(held > 2.0, "player should have run up to the wall, x={held}");

        // expiry tick: collider removed, player runs through
        walls.sync(LIFE, &mut w);
        assert_eq!(walls.active(), 0);
        assert_eq!(walls.tracked(), 0);
        for t in LIFE..LIFE + 30 {
            step(&mut w, &mut ch, t as f32 * DT);
        }
        assert!(w.bodies[ch.body].translation().x > 3.5);
    }

    #[test]
    fn grace_delays_activation_and_kill_removes_early() {
        let (mut w, _ch) = world_with_player();
        let mut walls = WallsWorld::new(240, 15, 1.6, 0.15);
        walls.add(WallSpawn { id: 7, owner: 1, spawn_tick: 100, a: q([0.0; 3]), b: q([2.0, 0.0, 0.0]) });
        walls.sync(114, &mut w);
        assert_eq!(walls.active(), 0);
        walls.sync(115, &mut w);
        assert_eq!(walls.active(), 1);
        walls.kill(7, 130);
        walls.sync(129, &mut w);
        assert_eq!(walls.active(), 1);
        walls.sync(130, &mut w);
        assert_eq!(walls.active(), 0);
        assert_eq!(walls.tracked(), 0);
    }

    #[test]
    fn chunking_cuts_collider_count_and_expiry_stays_exact_per_wall() {
        const LIFE: u32 = 240;
        let (mut w, _ch) = world_with_player();
        let mut walls = WallsWorld::new(LIFE, 0, 1.6, 0.15);
        // one owner running +x: a segment every 3 ticks for 120 ticks = 40 segments
        let mut ids = Vec::new();
        for k in 0..40u32 {
            let id = k + 1;
            let x = k as f32 * 0.4;
            walls.add(WallSpawn { id, owner: 4, spawn_tick: k * 3, a: q([x, 0.0, 0.0]), b: q([x + 0.4, 0.0, 0.0]) });
            ids.push(id);
        }
        walls.sync(121, &mut w);
        assert_eq!(walls.active(), 40);
        assert!(walls.colliders() <= 40 / 6, "chunked: {} colliders for 40 walls", walls.colliders());

        // wall k=0 expires at tick 240; wall k=1 (spawn 3) at 243. At tick 240 exactly one is gone.
        walls.sync(239, &mut w);
        assert_eq!(walls.active(), 40);
        walls.sync(240, &mut w);
        assert_eq!(walls.active(), 39, "only the first wall's timer fired, though it shares a chunk");
        walls.sync(243, &mut w);
        assert_eq!(walls.active(), 38);
        walls.sync(10_000, &mut w);
        assert_eq!((walls.active(), walls.colliders(), walls.tracked()), (0, 0, 0));
        // every collider handed out was returned: only the floor + player capsule remain
        assert_eq!(w.colliders.len(), 2);
    }

    #[test]
    fn compound_wall_still_blocks_a_player_and_kill_removes_one_part() {
        let (mut w, mut ch) = world_with_player();
        let mut walls = WallsWorld::new(600, 0, 1.6, 0.15);
        // two adjacent segments across the run path, same chunk; killing one opens a gap only there
        walls.add(WallSpawn { id: 1, owner: 9, spawn_tick: 0, a: q([3.0, 0.0, -3.0]), b: q([3.0, 0.0, 0.0]) });
        walls.add(WallSpawn { id: 2, owner: 9, spawn_tick: 1, a: q([3.0, 0.0, 0.0]), b: q([3.0, 0.0, 3.0]) });
        for t in 0..90u32 {
            walls.sync(t, &mut w);
            step(&mut w, &mut ch, t as f32 * DT);
            assert!(w.bodies[ch.body].translation().x < 3.0, "tick {t}: compound wall leaked");
        }
        assert_eq!(walls.colliders(), 1);
        walls.kill(2, 90); // the player runs along z=0 -> wall 1 covers z<=0 edge, wall 2 covers z>=0
        walls.sync(90, &mut w);
        assert_eq!((walls.active(), walls.colliders()), (1, 1));
    }
}
