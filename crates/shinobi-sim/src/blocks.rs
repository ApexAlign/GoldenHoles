//! Player-built pieces ("print the grid", see DESIGN.md): grid-snapped solid slabs in the authoritative world.
//!
//! Two kinds: `Floor` (a flat slab on the bottom of its cell: floors and, later, ramps) and `Wall` (an upright
//! slab through the cell centre, facing along X or Z). A piece is a fixed cuboid in `GROUP_WORLD`, so movement,
//! shurikens and spawn line-of-sight treat it like level geometry. Cells are integer grid coordinates
//! (`cell * size`), so server and clients derive identical geometry from the same `(cell, piece, scale)`.
//! One piece per cell. Each owner has a cap; placing past it evicts their oldest.
//!
//! Hooks (inert until tuned, all in `BlockCfg` / Balance `blocks.*`): per-piece health, age (older = more
//! durable, `age_bonus_per_sec`), size scaling by the owner's kills (`scale_per_kill`, `scale_cap`), and the
//! `DeRez` event when a piece is destroyed (`Blocks::damage`). Callers must `world.queries.update` before
//! raycasting after a change (same rule as every collider edit).

use std::collections::{HashMap, VecDeque};

use rapier3d::prelude::*;

use crate::character::PhysicsWorld;
use crate::combat::GROUP_WORLD;

pub type Cell = [i32; 3];

/// Bit set in the `user_data` of every piece collider; the low 96 bits pack the cell. Weapons read it to know
/// that what they struck is a destructible piece (player/NPC hitboxes carry small ids, never this bit).
pub const PIECE_TAG: u128 = 1 << 127;

pub fn piece_user_data(cell: Cell) -> u128 {
    PIECE_TAG | (cell[0] as u32 as u128) | (cell[1] as u32 as u128) << 32 | (cell[2] as u32 as u128) << 64
}

/// The cell of a piece collider from its `user_data`, `None` for anything else.
pub fn piece_cell(user_data: u128) -> Option<Cell> {
    (user_data & PIECE_TAG != 0).then(|| [user_data as u32 as i32, (user_data >> 32) as u32 as i32, (user_data >> 64) as u32 as i32])
}

/// Ticks per second for age maths (`shinobi_proto::TICK_HZ`).
const HZ: f32 = 60.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PieceKind {
    /// Flat slab on the bottom of the cell (`N` key).
    Floor,
    /// Upright slab through the cell centre (`M` key).
    Wall,
    /// Tilted slab rising across the cell (`N` again: tap N twice quickly to cycle floor <-> ramp).
    Ramp,
}

impl PieceKind {
    /// Wire value.
    pub fn id(self) -> u8 {
        self as u8
    }

    pub fn from_id(id: u8) -> Self {
        match id {
            1 => PieceKind::Wall,
            2 => PieceKind::Ramp,
            _ => PieceKind::Floor,
        }
    }
}

/// What a cell holds: the kind plus its grid-snapped orientation `axis`: for walls the axis the slab's normal
/// points along (0 = X, 1 = Z); for ramps the direction it rises toward (0 = +X, 1 = +Z, 2 = -X, 3 = -Z); floors
/// always 0. Picked from the builder's aim, so it is deterministic from the intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Piece {
    pub kind: PieceKind,
    pub axis: u8,
}

impl Piece {
    pub const FLOOR: Piece = Piece { kind: PieceKind::Floor, axis: 0 };
    /// Wall whose normal points along X (blocks movement along X).
    pub const WALL_X: Piece = Piece { kind: PieceKind::Wall, axis: 0 };
    /// Wall whose normal points along Z.
    pub const WALL_Z: Piece = Piece { kind: PieceKind::Wall, axis: 1 };

    /// A wall facing across the aim direction `dir`: its normal runs along the dominant horizontal axis.
    pub fn wall_facing(dir: [f32; 3]) -> Piece {
        Piece { kind: PieceKind::Wall, axis: u8::from(dir[2].abs() >= dir[0].abs()) }
    }

    /// A ramp rising away from the builder: toward the dominant horizontal axis of the aim `dir`.
    pub fn ramp_facing(dir: [f32; 3]) -> Piece {
        let axis = if dir[0].abs() >= dir[2].abs() {
            if dir[0] >= 0.0 { 0 } else { 2 }
        } else if dir[2] >= 0.0 {
            1
        } else {
            3
        };
        Piece { kind: PieceKind::Ramp, axis }
    }

    /// The piece a press of `kind` places when aiming along `dir`.
    pub fn for_press(kind: PieceKind, dir: [f32; 3]) -> Piece {
        match kind {
            PieceKind::Floor => Piece::FLOOR,
            PieceKind::Wall => Piece::wall_facing(dir),
            PieceKind::Ramp => Piece::ramp_facing(dir),
        }
    }

    /// Half extents of the slab (before rotation) for a cell of edge `size`, slab `thick`ness, grown by `scale`.
    pub fn half_extents(self, size: f32, thick: f32, scale: f32) -> [f32; 3] {
        let (h, t) = (size * 0.5 * scale, thick * 0.5);
        match (self.kind, self.axis) {
            (PieceKind::Floor, _) => [h, t, h],
            (PieceKind::Wall, 0) => [t, h, h],
            (PieceKind::Wall, _) => [h, h, t],
            // a ramp's slab is as long as the slope: one cell run at RAMP_DEG
            (PieceKind::Ramp, a) if a % 2 == 0 => [h / RAMP_DEG.to_radians().cos(), t, h],
            (PieceKind::Ramp, _) => [h, t, h / RAMP_DEG.to_radians().cos()],
        }
    }

    /// Centre of the slab for `cell`: floors sit on the cell's bottom face, walls are centred, ramps run from
    /// the cell's bottom edge on the low side up to `size * tan(RAMP_DEG)` on the high side.
    pub fn center(self, cell: Cell, size: f32, thick: f32) -> [f32; 3] {
        let c = cell.map(|c| (c as f32 + 0.5) * size);
        let bottom = cell[1] as f32 * size;
        match self.kind {
            PieceKind::Floor => [c[0], bottom + thick * 0.5, c[2]],
            PieceKind::Wall => c,
            PieceKind::Ramp => {
                let (rise, cos) = (size * RAMP_DEG.to_radians().tan(), RAMP_DEG.to_radians().cos());
                [c[0], bottom + rise * 0.5 + thick * 0.5 / cos, c[2]]
            }
        }
    }

    /// Orientation as a scaled-axis rotation vector (axis * angle, radians); zero for floors and walls.
    pub fn rotation(self) -> [f32; 3] {
        let a = RAMP_DEG.to_radians();
        match (self.kind, self.axis) {
            (PieceKind::Ramp, 0) => [0.0, 0.0, a],  // rising toward +X: tilt about Z
            (PieceKind::Ramp, 1) => [-a, 0.0, 0.0], // rising toward +Z: tilt about X
            (PieceKind::Ramp, 2) => [0.0, 0.0, -a],
            (PieceKind::Ramp, _) => [a, 0.0, 0.0],
            _ => [0.0; 3],
        }
    }
}

/// Slope of a ramp, degrees. Under the character's 45 degree slope limit so it is walkable.
pub const RAMP_DEG: f32 = 40.0;

/// Where and how a piece sits: centre, half extents (before rotation) and the scaled-axis rotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slab {
    pub center: [f32; 3],
    pub half: [f32; 3],
    pub rot: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockCfg {
    /// Cell edge length, metres.
    pub size: f32,
    /// Slab thickness of floors and walls, metres.
    pub thickness: f32,
    /// Live pieces per owner; the oldest is evicted past this.
    pub max_per_owner: usize,
    /// Furthest point (m, along the aim) a piece can be placed at.
    pub range: f32,
    /// Ticks between two placements by the same owner.
    pub cooldown_ticks: u32,
    /// HOOK: damage a piece absorbs before it de-rezzes. Placeholder until balance decides.
    pub hp: f32,
    /// HOOK: older = more durable. Incoming damage is divided by `1 + age_bonus_per_sec * age_seconds`.
    /// 0 = no bonus (inert).
    pub age_bonus_per_sec: f32,
    /// HOOK: piece size grows by this fraction per kill of its owner. 0 = no scaling (inert).
    pub scale_per_kill: f32,
    /// HOOK: largest scale factor scaling may reach; 0 = uncapped. Open design question, no number chosen.
    pub scale_cap: f32,
}

impl Default for BlockCfg {
    fn default() -> Self {
        Self {
            size: 1.0,
            thickness: 0.2,
            max_per_owner: 64,
            range: 6.0,
            cooldown_ticks: 12,
            hp: 100.0,
            age_bonus_per_sec: 0.02,
            scale_per_kill: 0.1,
            scale_cap: 2.0,
        }
    }
}

/// The build rules a client needs to predict placement exactly like the server (sent once on join as
/// `NetEvent::BlockRules`). Fixed point, so server and client derive identical floats: the server runs its own
/// config through `BlockCfg::quantized` when a balance is loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockRules {
    pub thickness_mm: u16,
    pub range_dm: u16,
    pub cooldown_ticks: u16,
    pub scale_per_kill_milli: u16,
    pub scale_cap_centi: u16,
    pub max_per_owner: u16,
}

/// Scale is sent as 1/16ths (16 = 1.0): no float on the wire, identical on server and client.
pub const SCALE_ONE: u8 = 16;

impl BlockCfg {
    pub fn rules(&self) -> BlockRules {
        let q = |v: f32, k: f32| (v * k).round().clamp(0.0, u16::MAX as f32) as u16;
        BlockRules {
            thickness_mm: q(self.thickness, 1000.0),
            range_dm: q(self.range, 10.0),
            cooldown_ticks: self.cooldown_ticks.min(u16::MAX as u32) as u16,
            scale_per_kill_milli: q(self.scale_per_kill, 1000.0),
            scale_cap_centi: q(self.scale_cap, 100.0),
            max_per_owner: self.max_per_owner.min(u16::MAX as usize) as u16,
        }
    }

    /// `base` with the client-relevant fields replaced by `r` (hp and age stay server-only).
    pub fn with_rules(mut self, r: &BlockRules) -> BlockCfg {
        self.thickness = r.thickness_mm as f32 / 1000.0;
        self.range = r.range_dm as f32 / 10.0;
        self.cooldown_ticks = r.cooldown_ticks as u32;
        self.scale_per_kill = r.scale_per_kill_milli as f32 / 1000.0;
        self.scale_cap = r.scale_cap_centi as f32 / 100.0;
        self.max_per_owner = r.max_per_owner as usize;
        self
    }

    /// This config as a client reconstructs it from `rules()`: identical floats on both sides.
    pub fn quantized(self) -> BlockCfg {
        self.with_rules(&self.rules())
    }

    /// Scale (1/16ths) for an owner with `kills` kills.
    pub fn scale_q(&self, kills: u32) -> u8 {
        let mut s = 1.0 + self.scale_per_kill * kills as f32;
        if self.scale_cap > 0.0 {
            s = s.min(self.scale_cap);
        }
        ((s * SCALE_ONE as f32).round() as i64).clamp(SCALE_ONE as i64, 255) as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceError {
    /// Cell already holds a piece.
    Occupied,
    /// A player body overlaps the cell (would entomb them).
    Blocked,
    /// Owner placed too recently (`BlockCfg::cooldown_ticks`).
    Cooldown,
}

/// What `Server` reports after a build attempt, in tick order (`Server::block_events`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockEvent {
    Placed { tick: u32, owner: u16, cell: Cell, piece: Piece, scale_q: u8 },
    /// Removed: evicted by the owner's cap, or the owner left.
    Removed { tick: u32, cell: Cell },
    /// Destroyed by damage: dissolves into pixels (Graphics).
    DeRez { tick: u32, cell: Cell, owner: u16, piece: Piece },
    /// Hurt but standing: `hp_q` = remaining health in 1/255ths of its limit (255 = untouched), for cracking visuals.
    Damaged { tick: u32, cell: Cell, hp_q: u8 },
    Refused { tick: u32, owner: u16, why: PlaceError },
}

/// One live piece, for replication and drawing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockInfo {
    pub owner: u16,
    pub cell: Cell,
    pub piece: Piece,
    pub scale_q: u8,
}

struct Block {
    owner: u16,
    handle: ColliderHandle,
    piece: Piece,
    scale_q: u8,
    placed_tick: u32,
    /// Damage absorbed so far (after the age discount).
    hurt: f32,
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

    /// Centre of `cell` (the cell box, not the slab: see `Piece::center`).
    pub fn center(&self, cell: Cell) -> [f32; 3] {
        cell.map(|c| (c as f32 + 0.5) * self.cfg.size)
    }

    /// Slab centre and half extents of `piece` in `cell` at `scale_q`, for drawing a ghost or replicated piece.
    pub fn geometry(&self, cell: Cell, piece: Piece, scale_q: u8) -> ([f32; 3], [f32; 3]) {
        let s = self.slab(cell, piece, scale_q);
        (s.center, s.half)
    }

    /// Full placement of a piece (centre, half extents, rotation): what to draw and what the collider is.
    pub fn slab(&self, cell: Cell, piece: Piece, scale_q: u8) -> Slab {
        let scale = scale_q as f32 / SCALE_ONE as f32;
        Slab {
            center: piece.center(cell, self.cfg.size, self.cfg.thickness),
            half: piece.half_extents(self.cfg.size, self.cfg.thickness, scale),
            rot: piece.rotation(),
        }
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

    /// All live pieces as `(owner, cell)`, sorted by cell (deterministic for replication).
    pub fn iter(&self) -> Vec<(u16, Cell)> {
        let mut v: Vec<_> = self.cells.iter().map(|(c, b)| (b.owner, *c)).collect();
        v.sort_by_key(|&(_, c)| c);
        v
    }

    /// All live pieces with their kind and scale, sorted by cell.
    pub fn pieces(&self) -> Vec<BlockInfo> {
        let mut v: Vec<_> = self.cells.iter().map(|(c, b)| BlockInfo { owner: b.owner, cell: *c, piece: b.piece, scale_q: b.scale_q }).collect();
        v.sort_by_key(|b| b.cell);
        v
    }

    /// True if `owner` may place again at `tick` (cooldown over).
    pub fn ready(&self, owner: u16, tick: u32) -> bool {
        !self.ready_at.get(&owner).is_some_and(|&t| (tick.wrapping_sub(t) as i32) < 0)
    }

    /// True if `cell` holds nothing.
    pub fn is_free(&self, cell: Cell) -> bool {
        !self.cells.contains_key(&cell)
    }

    /// Whether a piece in `cell` would entomb a player standing at one of `players` (body centres). Uses the
    /// whole cell box, so it is conservative for thin slabs.
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

    /// One build press at `tick`: cooldown, then `place`. `kills` = the owner's kills (scale hook). Records the
    /// outcome in `events`.
    #[allow(clippy::too_many_arguments)]
    pub fn try_build(
        &mut self,
        world: &mut PhysicsWorld,
        tick: u32,
        owner: u16,
        kind: PieceKind,
        kills: u32,
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
        let piece = Piece::for_press(kind, dir);
        let scale_q = self.cfg.scale_q(kills);
        match self.place(world, owner, cell, piece, scale_q, tick, players) {
            Ok(evicted) => {
                self.ready_at.insert(owner, tick.wrapping_add(self.cfg.cooldown_ticks));
                events.push(BlockEvent::Placed { tick, owner, cell, piece, scale_q });
                if let Some(e) = evicted {
                    events.push(BlockEvent::Removed { tick, cell: e });
                }
            }
            Err(why) => events.push(BlockEvent::Refused { tick, owner, why }),
        }
    }

    /// Place a piece. Refuses an occupied cell or one overlapping a player capsule (`players`: body centres).
    /// Returns the cell evicted by the owner's cap, if any.
    #[allow(clippy::too_many_arguments)]
    pub fn place(
        &mut self,
        world: &mut PhysicsWorld,
        owner: u16,
        cell: Cell,
        piece: Piece,
        scale_q: u8,
        tick: u32,
        players: &[[f32; 3]],
    ) -> Result<Option<Cell>, PlaceError> {
        if self.cells.contains_key(&cell) {
            return Err(PlaceError::Occupied);
        }
        if self.overlaps_player(cell, players) {
            return Err(PlaceError::Blocked);
        }
        let Slab { center: c, half: h, rot } = self.slab(cell, piece, scale_q);
        let handle = world.colliders.insert(
            ColliderBuilder::cuboid(h[0], h[1], h[2])
                .translation(vector![c[0], c[1], c[2]])
                .rotation(vector![rot[0], rot[1], rot[2]])
                .collision_groups(InteractionGroups::new(GROUP_WORLD, Group::ALL))
                .user_data(piece_user_data(cell))
                .build(),
        );
        self.cells.insert(cell, Block { owner, handle, piece, scale_q, placed_tick: tick, hurt: 0.0 });
        let q = self.order.entry(owner).or_default();
        q.push_back(cell);
        let evicted = if q.len() > self.cfg.max_per_owner { q.pop_front() } else { None };
        if let Some(e) = evicted {
            self.remove_collider(world, e);
        }
        Ok(evicted)
    }

    /// Remaining health of the piece in `cell` in 1/255ths of its limit (255 = untouched, 1 = nearly gone); 255 when
    /// there is no piece. Same quantisation as `BlockEvent::Damaged`: for join catch-up of already-hurt pieces.
    pub fn hp_q(&self, cell: Cell) -> u8 {
        let Some(b) = self.cells.get(&cell) else { return 255 };
        let limit = (self.cfg.hp * (b.scale_q as f32 / SCALE_ONE as f32)).max(1e-3);
        if b.hurt <= 0.0 {
            255
        } else {
            ((1.0 - b.hurt / limit) * 255.0).round().clamp(1.0, 255.0) as u8
        }
    }

    /// HOOK: `amount` damage to the piece in `cell` at `tick`. Older pieces take less (`age_bonus_per_sec`);
    /// past `hp` (times its scale) the piece de-rezzes: removed, and `DeRez` pushed. True if destroyed.
    /// Nothing calls this yet: weapons stay inert against pieces until balance decides the numbers.
    pub fn damage(&mut self, world: &mut PhysicsWorld, cell: Cell, amount: f32, tick: u32, events: &mut Vec<BlockEvent>) -> bool {
        let Some(b) = self.cells.get_mut(&cell) else { return false };
        let age = tick.wrapping_sub(b.placed_tick) as f32 / HZ;
        b.hurt += amount / (1.0 + self.cfg.age_bonus_per_sec * age);
        let limit = self.cfg.hp * (b.scale_q as f32 / SCALE_ONE as f32);
        if b.hurt < limit {
            let hp_q = ((1.0 - b.hurt / limit.max(1e-3)) * 255.0).round().clamp(1.0, 255.0) as u8;
            events.push(BlockEvent::Damaged { tick, cell, hp_q });
            return false;
        }
        let (owner, piece) = (b.owner, b.piece);
        if let Some(q) = self.order.get_mut(&owner) {
            q.retain(|c| *c != cell);
        }
        self.remove_collider(world, cell);
        events.push(BlockEvent::DeRez { tick, cell, owner, piece });
        true
    }

    /// Remove the piece in `cell`; true if there was one.
    pub fn remove(&mut self, world: &mut PhysicsWorld, cell: Cell) -> bool {
        let Some(b) = self.cells.get(&cell) else { return false };
        let owner = b.owner;
        if let Some(q) = self.order.get_mut(&owner) {
            q.retain(|c| *c != cell);
        }
        self.remove_collider(world, cell)
    }

    /// Remove every piece of `owner` (they left the match).
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

/// Test helper: a wall piece facing Z at normal scale, placed at tick 0.
#[cfg(test)]
pub(crate) fn put(b: &mut Blocks, world: &mut PhysicsWorld, owner: u16, cell: Cell, players: &[[f32; 3]]) -> Result<Option<Cell>, PlaceError> {
    b.place(world, owner, cell, Piece { kind: PieceKind::Wall, axis: 1 }, SCALE_ONE, 0, players)
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
        assert_eq!(put(&mut b, &mut s.world, 1, [0, 0, 0], &[]), Ok(None));
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
        assert_eq!(put(&mut b, &mut s.world, 1, [3, 0, 3], &[]), Ok(None));
        assert_eq!(put(&mut b, &mut s.world, 2, [3, 0, 3], &[]), Err(PlaceError::Occupied));
        assert_eq!(put(&mut b, &mut s.world, 1, [5, 0, 5], &[[5.5, 0.81, 5.5]]), Err(PlaceError::Blocked));
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn cap_evicts_the_oldest_and_clear_owner_frees_all() {
        let mut s = server();
        let mut b = Blocks::new(BlockCfg { max_per_owner: 3, ..BlockCfg::default() });
        for x in 0..3 {
            assert_eq!(put(&mut b, &mut s.world, 1, [x, 0, 0], &[]), Ok(None));
        }
        assert_eq!(put(&mut b, &mut s.world, 1, [3, 0, 0], &[]), Ok(Some([0, 0, 0])));
        assert_eq!(b.len(), 3);
        assert_eq!(b.owner_of([0, 0, 0]), None);
        assert_eq!(put(&mut b, &mut s.world, 2, [9, 0, 9], &[]), Ok(None));
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
        put(&mut s.blocks, &mut s.world, 1, [5, 0, 5], &[]).unwrap();
        put(&mut s.blocks, &mut s.world, 2, [9, 0, 9], &[]).unwrap();
        assert!(s.remove_player(1));
        assert_eq!(s.blocks.iter(), vec![(2, [9, 0, 9])]);
        s.frame(Duration::from_nanos(16_666_667));
        assert!(s.block_events.iter().any(|e| matches!(e, BlockEvent::Removed { cell: [5, 0, 5], .. })), "{:?}", s.block_events);
    }
}

#[cfg(test)]
mod e2e_tests {
    use super::*;
    use crate::character::Cfg;
    use crate::combat::CombatEvent;
    use crate::tick::Server;
    use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, PITCH_LEVEL};
    use std::time::Duration;

    fn press(s: &mut Server, buttons: u8, build: bool, wall: bool) {
        let t = s.tick;
        let it = PlayerIntent { tick: t, buttons: Buttons(buttons), aim_pitch: PITCH_LEVEL, build, build_wall: wall, ..Default::default() };
        let pkt = ClientPacket { ack_snapshot_tick: t, intents: vec![it], ..Default::default() };
        s.ingest(1, &pkt.encode().unwrap());
        s.frame(Duration::from_nanos(16_666_667));
    }

    /// Build intent -> piece in the world (events say what and where) -> a disc thrown at it ricochets instead
    /// of flying on. `Placed` is exactly what the replicator broadcasts as `NetEvent::BlockPlaced`.
    #[test]
    fn a_printed_wall_is_solid_to_discs_and_reported_with_its_kind() {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        s.add_player(1, [0.5, 0.81, 0.5]);
        press(&mut s, 0, true, true);
        let placed = s.block_events.iter().find_map(|e| match e {
            BlockEvent::Placed { cell, piece, scale_q, .. } => Some((*cell, *piece, *scale_q)),
            _ => None,
        });
        let (cell, piece, scale_q) = placed.expect("server accepted the build press");
        assert_eq!((piece, scale_q), (Piece::WALL_Z, SCALE_ONE), "aiming along -Z faces the wall across Z");
        assert!(cell[2] < 0, "placed ahead of the builder: {cell:?}");
        // throw at it
        let mut bounced = false;
        press(&mut s, Buttons::FIRE, false, false);
        for _ in 0..40 {
            s.frame(Duration::from_nanos(16_666_667));
            bounced |= s.events.iter().any(|(_, e)| matches!(e, CombatEvent::ShurikenBounce { .. }));
        }
        assert!(bounced, "the disc bounced off the printed wall");
    }

    #[test]
    fn floors_sit_on_the_cell_floor_and_scale_follows_kills_only_when_tuned() {
        let mut b = Blocks::default();
        let (c, h) = b.geometry([2, 3, 4], Piece::FLOOR, SCALE_ONE);
        assert_eq!(c, [2.5, 3.0 + 0.1, 4.5]);
        assert_eq!(h, [0.5, 0.1, 0.5]);
        b.cfg.scale_per_kill = 0.0;
        assert_eq!(b.cfg.scale_q(40), SCALE_ONE, "off when scale_per_kill is 0");
        b.cfg.scale_per_kill = 0.05;
        b.cfg.scale_cap = 0.0;
        assert_eq!(b.cfg.scale_q(4), 19); // 1.2 * 16 = 19.2
        b.cfg.scale_cap = 1.5;
        assert_eq!(b.cfg.scale_q(1000), 24);
    }

    #[test]
    fn damage_de_rezzes_a_piece_and_age_makes_it_tougher() {
        let mut s = Server::new(Cfg::default());
        let mut b = Blocks::new(BlockCfg { hp: 100.0, age_bonus_per_sec: 1.0, ..BlockCfg::default() });
        let mut ev = Vec::new();
        b.place(&mut s.world, 1, [0, 0, 0], Piece::FLOOR, SCALE_ONE, 0, &[]).unwrap();
        b.place(&mut s.world, 1, [5, 0, 0], Piece::FLOOR, SCALE_ONE, 600, &[]).unwrap(); // placed 10 s later
        // at tick 600 the first piece is 10 s old (x11 tougher); the second is brand new
        assert!(!b.damage(&mut s.world, [0, 0, 0], 100.0, 600, &mut ev), "old piece shrugs off 100");
        assert!(b.damage(&mut s.world, [5, 0, 0], 100.0, 600, &mut ev), "fresh piece dies to 100");
        assert!(matches!(ev[..], [BlockEvent::Damaged { cell: [0, 0, 0], .. }, BlockEvent::DeRez { cell: [5, 0, 0], owner: 1, .. }]), "{ev:?}");
        // the old piece reports how hurt it is: 100 dmg / (1 + 1.0 * 10 s) = 9.09 of 100 -> ~91% left
        let BlockEvent::Damaged { hp_q, .. } = ev[0] else { unreachable!() };
        assert!((hp_q as i32 - 232).abs() <= 1, "hp_q {hp_q}");
        assert!(b.is_free([5, 0, 0]) && !b.is_free([0, 0, 0]));
    }
}

#[cfg(test)]
mod weapon_damage_tests {
    use super::*;
    use crate::character::Cfg;
    use crate::tick::Server;
    use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, PITCH_LEVEL};
    use std::time::Duration;

    #[test]
    fn piece_user_data_round_trips_negative_cells_and_never_looks_like_a_player() {
        for cell in [[0, 0, 0], [-1, 5, -300], [i32::MIN, i32::MAX, 7]] {
            assert_eq!(piece_cell(piece_user_data(cell)), Some(cell));
        }
        assert_eq!(piece_cell(7), None, "a player id is not a piece");
    }

    /// Shurikens thrown at a printed wall wear it down; the owner's own pieces are not immune; at 0 hp it de-rezzes.
    #[test]
    fn discs_and_the_katana_damage_pieces_until_they_de_rez() {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        s.add_player(1, [0.5, 0.81, 0.5]);
        let mut b = s.balance.clone();
        b.blocks.hp = 30.0;
        b.combat.shuriken.piece_damage = 12.0;
        s.set_balance(b).unwrap();
        // wall straight ahead (owner is player 1 itself)
        s.blocks.place(&mut s.world, 1, [0, 1, -4], Piece::WALL_Z, SCALE_ONE, 0, &[]).unwrap();
        s.world.queries.update(&s.world.colliders);
        let mut derezzed = false;
        let mut throws = 0;
        for t in 0..400 {
            // keep throwing (cooldown permitting); every disc dies on the wall or ricochets back into it
            let it = PlayerIntent { tick: s.tick, buttons: Buttons(if t % 14 == 0 { Buttons::FIRE } else { 0 }), aim_pitch: PITCH_LEVEL, ..Default::default() };
            throws += (t % 14 == 0) as u32;
            let pkt = ClientPacket { ack_snapshot_tick: s.tick, intents: vec![it], ..Default::default() };
            s.ingest(1, &pkt.encode().unwrap());
            s.frame(Duration::from_nanos(16_666_667));
            derezzed |= s.block_events.iter().any(|e| matches!(e, BlockEvent::DeRez { cell: [0, 1, -4], owner: 1, .. }));
            if derezzed {
                break;
            }
        }
        assert!(derezzed, "30 hp / 12 per bounce: three hits de-rez the wall ({throws} throws)");
        assert!(s.blocks.is_free([0, 1, -4]), "the destroyed piece is gone from the world");
    }
}

#[cfg(test)]
mod rules_tests {
    use super::*;

    #[test]
    fn a_client_rebuilds_the_servers_quantized_rules_exactly_and_scales_identically() {
        let tuned = BlockCfg { thickness: 0.237, range: 6.35, cooldown_ticks: 17, scale_per_kill: 0.0537, scale_cap: 1.9, max_per_owner: 40, ..BlockCfg::default() };
        let server = tuned.quantized();
        let client = BlockCfg::default().with_rules(&server.rules());
        assert_eq!((client.thickness, client.range, client.cooldown_ticks, client.max_per_owner), (server.thickness, server.range, server.cooldown_ticks, server.max_per_owner));
        assert_eq!((client.scale_per_kill, client.scale_cap), (server.scale_per_kill, server.scale_cap));
        for kills in 0..300 {
            assert_eq!(client.scale_q(kills), server.scale_q(kills), "scale at {kills} kills");
        }
        assert_eq!(server.scale_q(1000), (1.9f32 * 16.0).round() as u8, "capped by scale_cap");
        assert_eq!(server.quantized(), server, "quantizing is idempotent");
    }
}

#[cfg(test)]
mod ramp_tests {
    use super::*;
    use crate::character::Cfg;
    use crate::tick::Server;
    use std::time::Duration;

    #[test]
    fn ramps_rise_toward_the_aim_axis_and_snap_to_the_grid() {
        assert_eq!(Piece::ramp_facing([0.2, 0.0, -1.0]).axis, 3, "facing -Z rises toward -Z");
        assert_eq!(Piece::ramp_facing([1.0, 0.3, 0.2]).axis, 0);
        assert_eq!(Piece::ramp_facing([-1.0, 0.0, 0.9]).axis, 2);
        assert_eq!(Piece::ramp_facing([0.1, 0.0, 1.0]).axis, 1);
        assert_eq!(PieceKind::from_id(PieceKind::Ramp.id()), PieceKind::Ramp);
        let b = Blocks::default();
        let slab = b.slab([2, 0, 2], Piece { kind: PieceKind::Ramp, axis: 0 }, SCALE_ONE);
        let rise = RAMP_DEG.to_radians().tan();
        assert!((slab.center[1] - (rise * 0.5 + 0.1 / RAMP_DEG.to_radians().cos())).abs() < 1e-5, "{slab:?}");
        assert_eq!((slab.center[0], slab.center[2]), (2.5, 2.5));
        assert!(slab.rot[2] > 0.0 && slab.rot[0] == 0.0, "rising toward +X tilts about Z: {slab:?}");
    }

    /// A character can walk up a printed ramp (40 degrees < the 45 degree limit) and ends up higher than a floor.
    #[test]
    fn a_player_walks_up_a_printed_ramp() {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        s.add_player(1, [0.5, 0.81, 4.5]);
        // two ramps in a row rising toward -Z (axis 3): cells z = 2 and z = 1, then a floor on top at z = 0, y = 1
        s.blocks.place(&mut s.world, 1, [0, 0, 2], Piece { kind: PieceKind::Ramp, axis: 3 }, SCALE_ONE, 0, &[]).unwrap();
        s.world.queries.update(&s.world.colliders);
        let it = shinobi_proto::intent::PlayerIntent { move_z: 127, ..Default::default() };
        for _ in 0..90 {
            let t = s.tick;
            let pkt = shinobi_proto::intent::ClientPacket { ack_snapshot_tick: t, intents: vec![shinobi_proto::intent::PlayerIntent { tick: t, ..it }], ..Default::default() };
            s.ingest(1, &pkt.encode().unwrap());
            s.frame(Duration::from_nanos(16_666_667));
        }
        let p = s.players[0].pos(&s.world);
        assert!(p[2] < 1.5, "walked across the ramp cell and beyond: {p:?}");
        // while on the ramp the capsule centre was higher than standing height: sample mid-climb in a fresh run
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        s.add_player(1, [0.5, 0.81, 4.0]);
        s.blocks.place(&mut s.world, 1, [0, 0, 2], Piece { kind: PieceKind::Ramp, axis: 3 }, SCALE_ONE, 0, &[]).unwrap();
        s.world.queries.update(&s.world.colliders);
        let mut max_y = 0.0f32;
        for _ in 0..90 {
            let t = s.tick;
            let pkt = shinobi_proto::intent::ClientPacket { ack_snapshot_tick: t, intents: vec![shinobi_proto::intent::PlayerIntent { tick: t, ..it }], ..Default::default() };
            s.ingest(1, &pkt.encode().unwrap());
            s.frame(Duration::from_nanos(16_666_667));
            max_y = max_y.max(s.players[0].pos(&s.world)[1]);
        }
        assert!(max_y > 1.2, "the capsule climbed the ramp (peak centre height {max_y})");
    }
}
