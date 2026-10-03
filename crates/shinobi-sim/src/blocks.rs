//! Player-built blocks: grid-snapped solid cubes in the authoritative world.
//!
//! A block is a fixed cuboid in `GROUP_WORLD`, so movement, shurikens and spawn line-of-sight treat it like
//! level geometry. Cells are integer grid coordinates (`cell * size` = centre), so server and clients derive
//! identical positions from the same `(cx, cy, cz)`. Each owner has a cap; placing past it evicts their oldest.
//! Callers must `world.queries.update` before raycasting after a change (same rule as every collider edit).

use std::collections::{HashMap, VecDeque};

use rapier3d::prelude::*;

use crate::character::PhysicsWorld;
use crate::combat::GROUP_WORLD;

pub type Cell = [i32; 3];

#[derive(Clone, Copy, Debug)]
pub struct BlockCfg {
    /// Edge length, metres.
    pub size: f32,
    /// Live blocks per owner; the oldest is evicted past this.
    pub max_per_owner: usize,
    /// Furthest point (m, along the aim) a block can be placed at.
    pub range: f32,
    /// Ticks between two placements by the same owner.
    pub cooldown_ticks: u32,
}

impl Default for BlockCfg {
    fn default() -> Self {
        Self { size: 1.0, max_per_owner: 64, range: 6.0, cooldown_ticks: 12 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceError {
    /// Cell already holds a block.
    Occupied,
    /// A player body overlaps the cell (would entomb them).
    Blocked,
    /// Owner placed too recently (`BlockCfg::cooldown_ticks`).
    Cooldown,
}

/// What `Server` reports after a build attempt, in tick order (`Server::block_events`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockEvent {
    Placed { tick: u32, owner: u16, cell: Cell },
    /// Removed: evicted by the owner's cap, or the owner left.
    Removed { tick: u32, cell: Cell },
    Refused { tick: u32, owner: u16, why: PlaceError },
}

struct Block {
    owner: u16,
    handle: ColliderHandle,
}

#[derive(Default)]
pub struct Blocks {
    pub cfg: BlockCfg,
    cells: HashMap<Cell, Block>,
    order: HashMap<u16, VecDeque<Cell>>,
    /// First tick each owner may place again.
    ready_at: HashMap<u16, u32>,
}

impl Blocks {
    pub fn new(cfg: BlockCfg) -> Self {
        Self { cfg, ..Default::default() }
    }

    /// Cell containing world point `p`.
    pub fn cell_of(&self, p: [f32; 3]) -> Cell {
        p.map(|v| (v / self.cfg.size).floor() as i32)
    }

    /// Centre of `cell`.
    pub fn center(&self, cell: Cell) -> [f32; 3] {
        cell.map(|c| (c as f32 + 0.5) * self.cfg.size)
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn owner_of(&self, cell: Cell) -> Option<u16> {
        self.cells.get(&cell).map(|b| b.owner)
    }

    /// All live blocks as `(owner, cell)`, sorted by cell (deterministic for replication).
    pub fn iter(&self) -> Vec<(u16, Cell)> {
        let mut v: Vec<_> = self.cells.iter().map(|(c, b)| (b.owner, *c)).collect();
        v.sort_by_key(|&(_, c)| c);
        v
    }

    /// True if `owner` may place again at `tick` (cooldown over).
    pub fn ready(&self, owner: u16, tick: u32) -> bool {
        !self.ready_at.get(&owner).is_some_and(|&t| (tick.wrapping_sub(t) as i32) < 0)
    }

    /// True if `cell` has no block.
    pub fn is_free(&self, cell: Cell) -> bool {
        !self.cells.contains_key(&cell)
    }

    /// Whether a block at `cell` would entomb a player standing at one of `players` (body centres).
    pub fn overlaps_player(&self, cell: Cell, players: &[[f32; 3]]) -> bool {
        let (c, h) = (self.center(cell), self.cfg.size * 0.5);
        players.iter().any(|p| (p[0] - c[0]).abs() < h + 0.5 && (p[1] - c[1]).abs() < h + 1.0 && (p[2] - c[2]).abs() < h + 0.5)
    }

    /// Cell a build press aimed from `origin` along unit `dir` would fill: against the first solid hit (the cell
    /// in front of the face that was struck), else mid-air at `range` (bridging). `skip` = the builder's body.
    pub fn target_cell(&self, world: &PhysicsWorld, origin: [f32; 3], dir: [f32; 3], skip: RigidBodyHandle) -> Cell {
        let o = Point::new(origin[0], origin[1], origin[2]);
        let d = vector![dir[0], dir[1], dir[2]];
        let filter = QueryFilter::default()
            .exclude_rigid_body(skip)
            .exclude_sensors()
            .groups(InteractionGroups::new(crate::combat::GROUP_MELEE, crate::combat::solid_groups()));
        let p = match world.queries.cast_ray_and_get_normal(&world.bodies, &world.colliders, &Ray::new(o, d), self.cfg.range, true, filter) {
            Some((_, hit)) => o + d * hit.time_of_impact + hit.normal * (self.cfg.size * 0.25),
            None => o + d * self.cfg.range,
        };
        self.cell_of([p.x, p.y, p.z])
    }

    /// One build press at `tick`: cooldown, then `place`. Records the outcome in `events`.
    #[allow(clippy::too_many_arguments)]
    pub fn try_build(
        &mut self,
        world: &mut PhysicsWorld,
        tick: u32,
        owner: u16,
        origin: [f32; 3],
        dir: [f32; 3],
        skip: RigidBodyHandle,
        players: &[[f32; 3]],
        events: &mut Vec<BlockEvent>,
    ) {
        if !self.ready(owner, tick) {
            events.push(BlockEvent::Refused { tick, owner, why: PlaceError::Cooldown });
            return;
        }
        let cell = self.target_cell(world, origin, dir, skip);
        match self.place(world, owner, cell, players) {
            Ok(evicted) => {
                self.ready_at.insert(owner, tick.wrapping_add(self.cfg.cooldown_ticks));
                events.push(BlockEvent::Placed { tick, owner, cell });
                if let Some(e) = evicted {
                    events.push(BlockEvent::Removed { tick, cell: e });
                }
            }
            Err(why) => events.push(BlockEvent::Refused { tick, owner, why }),
        }
    }

    /// Place a block. Refuses an occupied cell or one overlapping a player capsule (`players`: body centres).
    /// Returns the cell evicted by the owner's cap, if any.
    pub fn place(
        &mut self,
        world: &mut PhysicsWorld,
        owner: u16,
        cell: Cell,
        players: &[[f32; 3]],
    ) -> Result<Option<Cell>, PlaceError> {
        if self.cells.contains_key(&cell) {
            return Err(PlaceError::Occupied);
        }
        let c = self.center(cell);
        let h = self.cfg.size * 0.5;
        // generous capsule bound: half-width 0.5, half-height 1.0 around the body centre
        if self.overlaps_player(cell, players) {
            return Err(PlaceError::Blocked);
        }
        let handle = world.colliders.insert(
            ColliderBuilder::cuboid(h, h, h)
                .translation(vector![c[0], c[1], c[2]])
                .collision_groups(InteractionGroups::new(GROUP_WORLD, Group::ALL))
                .build(),
        );
        self.cells.insert(cell, Block { owner, handle });
        let q = self.order.entry(owner).or_default();
        q.push_back(cell);
        let evicted = if q.len() > self.cfg.max_per_owner { q.pop_front() } else { None };
        if let Some(e) = evicted {
            self.remove_collider(world, e);
        }
        Ok(evicted)
    }

    /// Remove the block in `cell`; true if there was one.
    pub fn remove(&mut self, world: &mut PhysicsWorld, cell: Cell) -> bool {
        let Some(b) = self.cells.get(&cell) else { return false };
        let owner = b.owner;
        if let Some(q) = self.order.get_mut(&owner) {
            q.retain(|c| *c != cell);
        }
        self.remove_collider(world, cell)
    }

    /// Remove every block of `owner` (they left the match).
    pub fn clear_owner(&mut self, world: &mut PhysicsWorld, owner: u16) -> usize {
        self.ready_at.remove(&owner);
        let cells: Vec<Cell> = self.order.remove(&owner).map(|q| q.into_iter().collect()).unwrap_or_default();
        cells.into_iter().filter(|c| self.remove_collider(world, *c)).count()
    }

    fn remove_collider(&mut self, world: &mut PhysicsWorld, cell: Cell) -> bool {
        let Some(b) = self.cells.remove(&cell) else { return false };
        world.colliders.remove(b.handle, &mut world.islands, &mut world.bodies, false);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Cfg;
    use crate::tick::Server;

    fn server() -> Server {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        s
    }

    #[test]
    fn cells_round_trip_and_snap() {
        let b = Blocks::default();
        assert_eq!(b.cell_of([0.2, 0.9, -0.1]), [0, 0, -1]);
        assert_eq!(b.center([0, 0, -1]), [0.5, 0.5, -0.5]);
    }

    #[test]
    fn placed_block_stops_a_ray_and_removal_clears_it() {
        let mut s = server();
        let mut b = Blocks::default();
        let from = [0.5, 0.5, -8.5];
        let to = [0.5, 0.5, 8.5];
        s.world.queries.update(&s.world.colliders);
        assert!(!crate::spawn::blocked(&s.world, from, to));
        assert_eq!(b.place(&mut s.world, 1, [0, 0, 0], &[]), Ok(None));
        s.world.queries.update(&s.world.colliders);
        assert!(crate::spawn::blocked(&s.world, from, to));
        assert!(b.remove(&mut s.world, [0, 0, 0]));
        s.world.queries.update(&s.world.colliders);
        assert!(!crate::spawn::blocked(&s.world, from, to));
        assert!(!b.remove(&mut s.world, [0, 0, 0]), "already gone");
    }

    #[test]
    fn occupied_and_entombing_placements_are_refused() {
        let mut s = server();
        let mut b = Blocks::default();
        assert_eq!(b.place(&mut s.world, 1, [3, 0, 3], &[]), Ok(None));
        assert_eq!(b.place(&mut s.world, 2, [3, 0, 3], &[]), Err(PlaceError::Occupied));
        assert_eq!(b.place(&mut s.world, 1, [5, 0, 5], &[[5.5, 0.81, 5.5]]), Err(PlaceError::Blocked));
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn cap_evicts_the_oldest_and_clear_owner_frees_all() {
        let mut s = server();
        let mut b = Blocks::new(BlockCfg { max_per_owner: 3, ..BlockCfg::default() });
        for x in 0..3 {
            assert_eq!(b.place(&mut s.world, 1, [x, 0, 0], &[]), Ok(None));
        }
        assert_eq!(b.place(&mut s.world, 1, [3, 0, 0], &[]), Ok(Some([0, 0, 0])));
        assert_eq!(b.len(), 3);
        assert_eq!(b.owner_of([0, 0, 0]), None);
        assert_eq!(b.place(&mut s.world, 2, [9, 0, 9], &[]), Ok(None));
        assert_eq!(b.clear_owner(&mut s.world, 1), 3);
        assert_eq!(b.iter(), vec![(2, [9, 0, 9])]);
    }
}

#[cfg(test)]
mod build_tests {
    use super::*;
    use crate::character::Cfg;
    use crate::tick::Server;
    use shinobi_proto::intent::{ClientPacket, PlayerIntent, PITCH_LEVEL};
    use std::time::Duration;

    const A: u16 = 1;

    fn press(s: &mut Server, build: bool, pitch: u16) {
        let t = s.tick;
        let it = PlayerIntent { tick: t, aim_pitch: pitch, build, ..Default::default() };
        let pkt = ClientPacket { ack_snapshot_tick: t, intents: vec![it], ..Default::default() };
        s.ingest(A, &pkt.encode().unwrap());
        s.frame(Duration::from_nanos(16_666_667));
    }

    fn server() -> Server {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        s.add_player(A, [0.5, 0.81, 0.5]);
        s
    }

    #[test]
    fn build_press_places_a_block_ahead_and_the_cooldown_refuses_a_second() {
        let mut s = server();
        press(&mut s, true, PITCH_LEVEL);
        let placed: Vec<_> = s.block_events.iter().filter(|e| matches!(e, BlockEvent::Placed { .. })).collect();
        assert_eq!(placed.len(), 1, "{:?}", s.block_events);
        assert_eq!(s.blocks.len(), 1);
        press(&mut s, true, PITCH_LEVEL);
        assert!(s.block_events.iter().any(|e| matches!(e, BlockEvent::Refused { why: PlaceError::Cooldown, .. })));
        assert_eq!(s.blocks.len(), 1);
    }

    #[test]
    fn identical_inputs_build_identical_blocks() {
        let run = || {
            let mut s = server();
            for t in 0..40 {
                press(&mut s, t % 14 == 0, PITCH_LEVEL);
            }
            s.blocks.iter()
        };
        let (a, b) = (run(), run());
        assert!(!a.is_empty());
        assert_eq!(a, b);
    }
}

#[cfg(test)]
mod leave_tests {
    use super::*;
    use crate::character::Cfg;
    use crate::tick::Server;
    use std::time::Duration;

    #[test]
    fn a_leavers_blocks_vanish_and_are_reported_on_the_next_frame() {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        s.add_player(1, [0.5, 0.81, 0.5]);
        s.add_player(2, [20.5, 0.81, 0.5]);
        s.blocks.place(&mut s.world, 1, [5, 0, 5], &[]).unwrap();
        s.blocks.place(&mut s.world, 2, [9, 0, 9], &[]).unwrap();
        assert!(s.remove_player(1));
        assert_eq!(s.blocks.iter(), vec![(2, [9, 0, 9])]);
        s.frame(Duration::from_nanos(16_666_667));
        assert!(s.block_events.iter().any(|e| matches!(e, BlockEvent::Removed { cell: [5, 0, 5], .. })), "{:?}", s.block_events);
    }
}
