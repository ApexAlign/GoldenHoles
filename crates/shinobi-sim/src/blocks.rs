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
}

impl Default for BlockCfg {
    fn default() -> Self {
        Self { size: 1.0, max_per_owner: 64 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceError {
    /// Cell already holds a block.
    Occupied,
    /// A player body overlaps the cell (would entomb them).
    Blocked,
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
        let hit = |p: &[f32; 3]| (p[0] - c[0]).abs() < h + 0.5 && (p[1] - c[1]).abs() < h + 1.0 && (p[2] - c[2]).abs() < h + 0.5;
        if players.iter().any(hit) {
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
        let mut b = Blocks::new(BlockCfg { size: 1.0, max_per_owner: 3 });
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
