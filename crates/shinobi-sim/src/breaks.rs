//! Destructible level geometry: level boxes with `hp > 0` (`dbox` lines of a level file) wear down under weapons, break
//! (the collider is removed on the server AND on every client's predicted world, by the same code), and re-form a while
//! later. Decks, ramps and stairs are `box` lines and never break (bots and the arena tests depend on them).
//!
//! * Weapons already report hits on player-built pieces as `CombatEvent::PieceHit { cell }`; a destructible level box carries
//!   the piece tag with the reserved cell `[WORLD_CELL, idx, 0]`, so shurikens and katana swings reach it unchanged
//!   (`tick.rs` routes the cell here). Tag blasts call `LevelBreaks::blast`.
//! * SUPPORT: a free-standing box (`rot_z == 0`) that rests only on other destructible boxes collapses `COLLAPSE_TICKS`
//!   after the last of them breaks, so a tower cut through at the foot brings its upper segments down in a chain.
//! * RE-FORM: after `rez_ticks` a broken box comes back, unless a player is standing in it or one of its supports is
//!   still broken (it waits a second and tries again).
//! * Wire: `NetEvent::WorldBreak / WorldRez / WorldCrack`, catch-up for late joiners (docs/destructible.md).

use crate::blocks::{piece_user_data, Cell};
use crate::level::{LevelBox, LevelDesc};
use crate::rapier3d::prelude::*;
use crate::character::PhysicsWorld;
use std::collections::{BTreeSet, HashMap};

/// First component of the cell a destructible level box reports in `PieceHit` (player cells never get near it).
pub const WORLD_CELL: i32 = i32::MIN;

pub fn world_cell(idx: u16) -> Cell {
    [WORLD_CELL, idx as i32, 0]
}

/// `Some(idx)` when `cell` names a destructible level box.
pub fn world_idx(cell: Cell) -> Option<u16> {
    (cell[0] == WORLD_CELL && (0..=u16::MAX as i32).contains(&cell[1])).then(|| cell[1] as u16)
}

pub fn world_user_data(idx: u16) -> u128 {
    piece_user_data(world_cell(idx))
}

/// Ticks between the last support breaking and the box above it coming down (a visible chain, not one bang).
pub const COLLAPSE_TICKS: u32 = 18;

#[derive(Clone, Copy, Debug)]
pub struct BreakCfg {
    /// Ticks a broken box stays gone (40 s at 60 Hz); 0 = it never comes back.
    pub rez_ticks: u32,
    /// Tag blast damage on a box = the tag's player damage at that distance times this.
    pub blast_mult: f32,
    /// Retry delay when a re-form is blocked (a player inside, a support still missing).
    pub retry_ticks: u32,
    /// Every hit is divided by this (global toughness).
    pub hp_mult: f32,
    /// Weapon chip damage (stars, katana, arrows) is multiplied by this.
    pub chip_mult: f32,
    /// Ticks between the last support breaking and the box above falling.
    pub collapse_ticks: u32,
}

impl Default for BreakCfg {
    fn default() -> Self {
        Self { rez_ticks: 2400, blast_mult: 5.0, retry_ticks: 60, hp_mult: 1.0, chip_mult: 1.0, collapse_ticks: COLLAPSE_TICKS }
    }
}

impl BreakCfg {
    pub fn from_balance(b: &crate::balance::DestructBalance) -> Self {
        Self {
            rez_ticks: (b.rez_s * 60.0).round() as u32,
            blast_mult: b.blast_mult,
            retry_ticks: b.retry_ticks.max(1),
            hp_mult: b.hp_mult.max(1e-3),
            chip_mult: b.chip_mult,
            collapse_ticks: b.collapse_ticks,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakEvent {
    /// The box is gone. `cascade`: it fell because its supports broke.
    Break { tick: u32, idx: u16, cascade: bool },
    /// The box is back.
    Rez { tick: u32, idx: u16 },
    /// Hurt but standing; `hp_q` = remaining health in 1/255ths (255 untouched), for cracks.
    Damaged { tick: u32, idx: u16, hp_q: u8 },
}

struct Entry {
    idx: u16,
    b: LevelBox,
    hurt: f32,
    handle: Option<ColliderHandle>,
    /// Entries (not box indices) this box rests on, all destructible.
    supports: Vec<usize>,
    /// Rests on at least one permanent box (never collapses).
    static_support: bool,
    collapse_at: Option<u32>,
    rez_at: u32,
}

#[derive(Default)]
pub struct LevelBreaks {
    pub cfg: BreakCfg,
    entries: Vec<Entry>,
    by_idx: HashMap<u16, usize>,
}

fn overlap_xz(a: &LevelBox, b: &LevelBox) -> bool {
    (a.center[0] - b.center[0]).abs() < a.half[0] + b.half[0] - 0.05 && (a.center[2] - b.center[2]).abs() < a.half[2] + b.half[2] - 0.05
}

/// Distance from `p` to the nearest point of the (rolled) box.
fn dist_to_box(b: &LevelBox, p: [f32; 3]) -> f32 {
    let (s, c) = b.rot_z.sin_cos();
    let (dx, dy, dz) = (p[0] - b.center[0], p[1] - b.center[1], p[2] - b.center[2]);
    let (u, v) = (dx * c + dy * s, -dx * s + dy * c);
    let (ex, ey, ez) = ((u.abs() - b.half[0]).max(0.0), (v.abs() - b.half[1]).max(0.0), (dz.abs() - b.half[2]).max(0.0));
    (ex * ex + ey * ey + ez * ez).sqrt()
}

impl LevelBreaks {
    /// `handles[i]` = the collider `LevelDesc::insert_into` made for box `i`.
    pub fn build(level: &LevelDesc, handles: &[ColliderHandle]) -> Self {
        let mut me = Self::default();
        for (i, b) in level.boxes.iter().enumerate() {
            if b.hp > 0.0 && i <= u16::MAX as usize {
                me.by_idx.insert(i as u16, me.entries.len());
                me.entries.push(Entry { idx: i as u16, b: *b, hurt: 0.0, handle: handles.get(i).copied(), supports: Vec::new(), static_support: false, collapse_at: None, rez_at: 0 });
            }
        }
        // what each destructible box rests on: boxes whose top face is this box's bottom face under it
        for e in 0..me.entries.len() {
            let eb = me.entries[e].b;
            if eb.rot_z != 0.0 {
                continue;
            }
            let bottom = eb.center[1] - eb.half[1];
            let mut supports = Vec::new();
            let mut stat = false;
            for (j, o) in level.boxes.iter().enumerate() {
                if j as u16 == me.entries[e].idx || o.rot_z != 0.0 {
                    continue;
                }
                if (o.center[1] + o.half[1] - bottom).abs() < 0.2 && overlap_xz(&eb, o) {
                    match me.by_idx.get(&(j as u16)) {
                        Some(&k) => supports.push(k),
                        None => stat = true,
                    }
                }
            }
            me.entries[e].supports = supports;
            me.entries[e].static_support = stat;
        }
        me
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Hit points of box `idx` (0 for a permanent box).
    pub fn max_hp(&self, idx: u16) -> f32 {
        self.by_idx.get(&idx).map_or(0.0, |&e| self.entries[e].b.hp)
    }

    pub fn is_broken(&self, idx: u16) -> bool {
        self.by_idx.get(&idx).map_or(false, |&e| self.entries[e].handle.is_none())
    }

    /// Indices of the broken boxes, ascending (join catch-up).
    pub fn broken(&self) -> Vec<u16> {
        let mut v: Vec<u16> = self.entries.iter().filter(|e| e.handle.is_none()).map(|e| e.idx).collect();
        v.sort_unstable();
        v
    }

    /// Remaining health in 1/255ths (255 untouched), like `Blocks::hp_q`.
    pub fn hp_q(&self, idx: u16) -> u8 {
        let Some(&e) = self.by_idx.get(&idx) else { return 255 };
        let e = &self.entries[e];
        if e.hurt <= 0.0 || e.handle.is_none() {
            255
        } else {
            ((1.0 - e.hurt / e.b.hp.max(1e-3)) * 255.0).round().clamp(1.0, 255.0) as u8
        }
    }

    /// Damaged boxes still standing (idx, hp_q), ascending (join catch-up for cracks).
    pub fn cracked(&self) -> Vec<(u16, u8)> {
        let mut v: Vec<(u16, u8)> = self.entries.iter().filter(|e| e.handle.is_some() && e.hurt > 0.0).map(|e| (e.idx, self.hp_q(e.idx))).collect();
        v.sort_unstable();
        v
    }

    fn remove(&mut self, world: &mut PhysicsWorld, e: usize) -> bool {
        let Some(h) = self.entries[e].handle.take() else { return false };
        world.colliders.remove(h, &mut world.islands, &mut world.bodies, false);
        true
    }

    fn restore(&mut self, world: &mut PhysicsWorld, e: usize) -> bool {
        if self.entries[e].handle.is_some() {
            return false;
        }
        let b = self.entries[e].b;
        let idx = self.entries[e].idx;
        self.entries[e].handle = Some(crate::level::insert_level_box(world, &b, Some(idx)));
        self.entries[e].hurt = 0.0;
        self.entries[e].collapse_at = None;
        true
    }

    /// Server: `amount` damage to box `idx`. True when the box broke (the caller refreshes the query pipeline).
    pub fn damage(&mut self, world: &mut PhysicsWorld, idx: u16, amount: f32, tick: u32, ev: &mut Vec<BreakEvent>) -> bool {
        let Some(&e) = self.by_idx.get(&idx) else { return false };
        if self.entries[e].handle.is_none() || amount <= 0.0 {
            return false;
        }
        self.entries[e].hurt += amount / self.cfg.hp_mult;
        let hp = self.entries[e].b.hp;
        if self.entries[e].hurt < hp {
            let hp_q = ((1.0 - self.entries[e].hurt / hp) * 255.0).round().clamp(1.0, 255.0) as u8;
            ev.push(BreakEvent::Damaged { tick, idx, hp_q });
            return false;
        }
        self.break_entry(world, e, tick, false, ev);
        true
    }

    fn break_entry(&mut self, world: &mut PhysicsWorld, e: usize, tick: u32, cascade: bool, ev: &mut Vec<BreakEvent>) {
        if self.remove(world, e) {
            self.entries[e].rez_at = if self.cfg.rez_ticks == 0 { u32::MAX } else { tick.saturating_add(self.cfg.rez_ticks) };
            self.entries[e].collapse_at = None;
            ev.push(BreakEvent::Break { tick, idx: self.entries[e].idx, cascade });
        }
    }

    /// Server: a radial blast. `dmg(d)` = damage at distance `d` (m) from the centre to the box's nearest point, 0 beyond
    /// `radius`; boxes behind other geometry still take it (blasts shake the whole structure). True if anything broke.
    pub fn blast(&mut self, world: &mut PhysicsWorld, centre: [f32; 3], radius: f32, dmg: impl Fn(f32) -> f32, tick: u32, ev: &mut Vec<BreakEvent>) -> bool {
        let mut broke = false;
        for e in 0..self.entries.len() {
            if self.entries[e].handle.is_none() {
                continue;
            }
            let d = dist_to_box(&self.entries[e].b, centre);
            if d > radius {
                continue;
            }
            let idx = self.entries[e].idx;
            broke |= self.damage(world, idx, dmg(d) * self.cfg.blast_mult, tick, ev);
        }
        broke
    }

    /// Server, once per tick: unsupported boxes fall, broken boxes re-form. `players` = centres of the live players.
    /// True when a collider changed.
    pub fn step(&mut self, world: &mut PhysicsWorld, tick: u32, players: &[[f32; 3]], ev: &mut Vec<BreakEvent>) -> bool {
        let mut changed = false;
        for e in 0..self.entries.len() {
            if self.entries[e].handle.is_some() {
                let en = &self.entries[e];
                if !en.static_support && !en.supports.is_empty() && en.supports.iter().all(|&s| self.entries[s].handle.is_none()) {
                    let at = *self.entries[e].collapse_at.get_or_insert(tick + self.cfg.collapse_ticks);
                    if tick >= at {
                        self.break_entry(world, e, tick, true, ev);
                        changed = true;
                    }
                } else {
                    self.entries[e].collapse_at = None;
                }
            } else if tick >= self.entries[e].rez_at {
                let blocked = self.entries[e].supports.iter().any(|&s| self.entries[s].handle.is_none() && !self.entries[e].static_support)
                    || players.iter().any(|p| {
                        let b = &self.entries[e].b;
                        dist_to_box(b, *p) < 1.6
                    });
                if blocked {
                    self.entries[e].rez_at = tick + self.cfg.retry_ticks;
                } else if self.restore(world, e) {
                    ev.push(BreakEvent::Rez { tick, idx: self.entries[e].idx });
                    changed = true;
                }
            }
        }
        changed
    }

    /// Quake: every standing box takes `frac` of its hit points this tick (jittered per box; boxes held up by at most one
    /// other box, or by nothing permanent, take 2.5x), SILENTLY (no crack events: `flush_cracks` reports them once the
    /// shaking stops). True when something broke.
    pub fn quake(&mut self, world: &mut PhysicsWorld, tick: u32, frac: f32, ev: &mut Vec<BreakEvent>) -> bool {
        let mut broke = false;
        for e in 0..self.entries.len() {
            if self.entries[e].handle.is_none() {
                continue;
            }
            let en = &self.entries[e];
            let weak = !en.static_support && en.supports.len() <= 1;
            let h = (en.idx as u32).wrapping_mul(2654435761).wrapping_add((tick / 10).wrapping_mul(40503));
            let jitter = 0.5 + ((h >> 8) & 1023) as f32 / 1023.0;
            let dmg = en.b.hp * frac * jitter * if weak { 2.5 } else { 1.0 } / self.cfg.hp_mult;
            self.entries[e].hurt += dmg;
            if self.entries[e].hurt >= self.entries[e].b.hp {
                self.break_entry(world, e, tick, false, ev);
                broke = true;
            }
        }
        broke
    }

    /// Report the most damaged standing boxes (at most `cap`) as `Damaged` events (after a quake).
    pub fn flush_cracks(&self, tick: u32, cap: usize, ev: &mut Vec<BreakEvent>) {
        let mut v: Vec<(f32, u16, u8)> = self
            .entries
            .iter()
            .filter(|e| e.handle.is_some() && e.hurt > 0.0)
            .map(|e| (e.hurt / e.b.hp, e.idx, self.hp_q(e.idx)))
            .collect();
        v.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, idx, hp_q) in v.into_iter().take(cap) {
            ev.push(BreakEvent::Damaged { tick, idx, hp_q });
        }
    }

    /// Client: make the world match the server's broken set (idempotent). True when a collider changed.
    pub fn set_broken(&mut self, world: &mut PhysicsWorld, broken: &BTreeSet<u16>) -> bool {
        let mut changed = false;
        for e in 0..self.entries.len() {
            let want = broken.contains(&self.entries[e].idx);
            let gone = self.entries[e].handle.is_none();
            if want && !gone {
                changed |= self.remove(world, e);
            } else if !want && gone {
                changed |= self.restore(world, e);
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Cfg;
    use crate::tick::Server;

    fn level() -> LevelDesc {
        let mut tower_low = LevelBox::new([0.0, 3.0, 0.0], [2.0, 3.0, 2.0]);
        tower_low.hp = 100.0;
        let mut tower_high = LevelBox::new([0.0, 9.0, 0.0], [2.0, 3.0, 2.0]);
        tower_high.hp = 100.0;
        let mut pillar = LevelBox::new([20.0, 2.0, 0.0], [0.5, 2.0, 0.5]);
        pillar.hp = 50.0;
        LevelDesc { boxes: vec![LevelBox::new([0.0, -0.5, 0.0], [60.0, 0.5, 60.0]), tower_low, tower_high, pillar], spawns: vec![[10.0, 0.81, 10.0]] }
    }

    fn server() -> Server {
        let mut s = Server::new(Cfg::default());
        level().apply_to_server(&mut s);
        s
    }

    #[test]
    fn cells_round_trip_and_never_look_like_a_player_block() {
        for idx in [0u16, 1, 77, u16::MAX] {
            assert_eq!(world_idx(world_cell(idx)), Some(idx));
            assert_eq!(crate::blocks::piece_cell(world_user_data(idx)), Some(world_cell(idx)));
        }
        assert_eq!(world_idx([0, 5, 0]), None);
    }

    #[test]
    fn only_dbox_levels_have_breakables_and_supports_are_found() {
        let s = server();
        assert_eq!(s.breaks.len(), 3);
        assert_eq!(s.breaks.max_hp(3), 50.0);
        assert_eq!(s.breaks.max_hp(0), 0.0, "the floor is permanent");
        // the upper segment rests on the lower one only; the lower on the floor
        let up = s.breaks.entries[s.breaks.by_idx[&2]].supports.clone();
        assert_eq!(up, vec![s.breaks.by_idx[&1]]);
        assert!(s.breaks.entries[s.breaks.by_idx[&1]].static_support);
    }

    #[test]
    fn damage_cracks_then_breaks_and_the_ray_goes_through() {
        let mut s = server();
        let mut ev = Vec::new();
        s.world.queries.update(&s.world.colliders);
        let (a, b) = ([20.0, 1.0, -5.0], [20.0, 1.0, 5.0]);
        assert!(crate::spawn::blocked(&s.world, a, b));
        assert!(!s.breaks.damage(&mut s.world, 3, 20.0, 10, &mut ev));
        assert_eq!(ev, vec![BreakEvent::Damaged { tick: 10, idx: 3, hp_q: 153 }]);
        assert_eq!(s.breaks.hp_q(3), 153);
        assert!(s.breaks.damage(&mut s.world, 3, 40.0, 11, &mut ev));
        assert_eq!(ev.last(), Some(&BreakEvent::Break { tick: 11, idx: 3, cascade: false }));
        s.world.queries.update(&s.world.colliders);
        assert!(!crate::spawn::blocked(&s.world, a, b), "the pillar is gone");
        assert!(s.breaks.is_broken(3));
        assert_eq!(s.breaks.broken(), vec![3]);
        assert!(!s.breaks.damage(&mut s.world, 3, 40.0, 12, &mut ev), "already broken");
    }

    #[test]
    fn a_tower_cut_at_the_foot_brings_the_top_down_and_both_come_back() {
        let mut s = server();
        let mut ev = Vec::new();
        assert!(s.breaks.damage(&mut s.world, 1, 500.0, 100, &mut ev));
        assert!(!s.breaks.step(&mut s.world, 101, &[], &mut ev), "the top hangs for a moment");
        assert!(!s.breaks.is_broken(2));
        assert!(s.breaks.step(&mut s.world, 100 + COLLAPSE_TICKS + 1, &[], &mut ev));
        assert!(s.breaks.is_broken(2), "unsupported top fell");
        assert!(ev.contains(&BreakEvent::Break { tick: 100 + COLLAPSE_TICKS + 1, idx: 2, cascade: true }));
        // re-form after rez_ticks: someone standing in the tower holds it back, then it returns
        let t = 100 + s.breaks.cfg.rez_ticks;
        assert!(!s.breaks.step(&mut s.world, t + 5, &[[0.5, 1.0, 0.5]], &mut ev), "player inside: wait");
        assert!(s.breaks.is_broken(1));
        assert!(s.breaks.step(&mut s.world, t + 70, &[], &mut ev));
        assert!(!s.breaks.is_broken(1), "foot is back");
        assert!(ev.iter().any(|e| matches!(e, BreakEvent::Rez { idx: 1, .. })));
        s.breaks.step(&mut s.world, t + 71, &[], &mut ev);
        assert!(!s.breaks.is_broken(2), "top returns once its support stands");
        assert_eq!(s.breaks.hp_q(1), 255, "a new box is untouched");
    }

    #[test]
    fn blast_hurts_by_distance_to_the_box() {
        let mut s = server();
        let mut ev = Vec::new();
        // 2 m from the pillar face (x 19.5): 10 hp at 2 m, x5 mult = 50 >= its 50 hp: it breaks; the tower 17 m away is out of range
        let dmg = |d: f32| (30.0 - 10.0 * d).max(0.0);
        assert!(s.breaks.blast(&mut s.world, [17.5, 2.0, 0.0], 5.0, dmg, 7, &mut ev));
        assert!(s.breaks.is_broken(3) && !s.breaks.is_broken(1));
        // 4 m away: 0 damage beyond what the falloff gives; a second pillar is not there, the tower (2 m face at x 2) takes 0
        assert!(!s.breaks.blast(&mut s.world, [6.0, 3.0, 0.0], 5.0, dmg, 8, &mut ev));
        assert_eq!(s.breaks.hp_q(1), 255 - ((30.0f32 - 10.0 * 4.0).max(0.0) * 5.0 / 100.0 * 255.0).round() as u8);
    }

    #[test]
    fn client_world_follows_the_broken_set_exactly() {
        let s = server();
        let lv = level();
        let mut w = PhysicsWorld { bodies: RigidBodySet::new(), colliders: ColliderSet::new(), islands: IslandManager::new(), queries: QueryPipeline::new() };
        let handles = lv.insert_into(&mut w);
        let mut cb = LevelBreaks::build(&lv, &handles);
        let n = w.colliders.len();
        let set: BTreeSet<u16> = [3u16, 2].into_iter().collect();
        assert!(cb.set_broken(&mut w, &set));
        assert_eq!(w.colliders.len(), n - 2);
        assert!(!cb.set_broken(&mut w, &set), "idempotent");
        assert!(cb.set_broken(&mut w, &BTreeSet::new()));
        assert_eq!(w.colliders.len(), n);
        let _ = s;
    }
}
