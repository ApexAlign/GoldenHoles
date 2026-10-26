//! Deterministic runtime for `zone`, `mover` and `hazard` level directives.
//!
//! Movers are an exact function of the server tick (`MotionPath` fixed point). Solid movers own a world collider that
//! is repositioned before the player loop; a player in the swept volume is displaced along the travel axis so a fast
//! mover cannot tunnel through them. Zones and hazards are axis-aligned trigger volumes evaluated after movement.

use std::collections::BTreeMap;

use shinobi_proto::quant::{dq_pos3, q_pos3, POS_SCALE};

use crate::character::{sync_body_colliders, Cfg, PhysicsWorld};
use crate::combat::GROUP_WORLD;
use crate::level::{HazardDef, LevelFeatures, MoveAxis, MoverDef, ZoneDef};
use crate::obstacle::MotionPath;
use crate::rapier3d::prelude::*;
use crate::tick::Player;

pub const LEVEL_HAZARD_ID_BASE: u16 = 0xE000;
/// Damage source id for every `piston` face strike (just below the level hazard range).
pub const PISTON_SOURCE_ID: u16 = LEVEL_HAZARD_ID_BASE - 1;
pub use crate::level::MAX_LEVEL_HAZARDS;

pub fn hazard_source_id(index: usize) -> u16 {
    LEVEL_HAZARD_ID_BASE + index as u16
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum FeatureEvent {
    ZoneEnter { tick: u32, zone: u16, player: u16 },
    ZoneExit { tick: u32, zone: u16, player: u16 },
    HazardHit { tick: u32, hazard: u16, player: u16, amount: f32 },
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HazardContact {
    pub victim: usize,
    pub amount: f32,
    pub source: u16,
}

/// A `piston` mover's leading face shoved a player this tick (once per player per stroke). Resolved by the tick:
/// a parry launches the player along `dir`, anything else is damage and a stagger.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PistonStrike {
    pub victim: usize,
    pub mover: u16,
    /// Unit push direction (the face normal along the mover axis).
    pub dir: [f32; 3],
}

impl MoverDef {
    pub const PISTON: &'static str = "piston";

    pub fn tuning_or(&self, key: &str, default: f32) -> f32 {
        self.tuning.get(key).copied().unwrap_or(default)
    }
}

struct MoverRuntime {
    index: u16,
    collider: ColliderHandle,
}

#[derive(Clone, Copy, Default)]
struct ZoneMemory {
    inside: bool,
    ready_tick: u32,
}

#[derive(Default)]
pub struct Features {
    defs: LevelFeatures,
    movers: Vec<MoverRuntime>,
    /// Static `tube` / `ring` segment colliders, in `ShapeDef::segments` order.
    shape_colliders: Vec<ColliderHandle>,
    zone_memory: BTreeMap<u16, Vec<ZoneMemory>>,
    hazard_hit: BTreeMap<(u16, u16), u32>,
    events: Vec<FeatureEvent>,
    contacts: Vec<HazardContact>,
    strikes: Vec<PistonStrike>,
    /// (mover, player) -> stroke index of the last strike, so a pinned player is struck once per stroke.
    strike_memory: BTreeMap<(u16, u16), u32>,
}

impl MoverDef {
    pub fn motion_path(&self) -> MotionPath {
        let a = q_pos3(self.center);
        let mut b = a;
        let d = (self.distance * POS_SCALE).round() as i32;
        let i = self.axis.index();
        b[i] = b[i].saturating_add(d);
        MotionPath::PingPong { a, b, period: self.period_ticks.min(u16::MAX as u32) as u16 }
    }

    pub fn phase_ticks(&self) -> u32 {
        self.tuning.get("phase_ticks").copied().map_or(0, |v| v.max(0.0) as u32)
    }

    pub fn pos_at(&self, tick: u32) -> [f32; 3] {
        let path = crate::obstacle::ObstacleDesc {
            id: 0,
            kind: 0,
            start_tick: 0,
            end_tick: 0,
            half: [0; 3],
            path: self.motion_path(),
        };
        dq_pos3(path.pos_at(tick.saturating_add(self.phase_ticks())))
    }
}

impl HazardDef {
    pub fn active_ticks(&self) -> u32 {
        self.tuning.get("active_ticks").copied().map_or(1, |v| (v.max(0.0) as u32).max(1))
    }

    pub fn phase_ticks(&self) -> u32 {
        self.tuning.get("phase_ticks").copied().map_or(0, |v| v.max(0.0) as u32) % self.period_ticks.max(1)
    }

    pub fn local_tick(&self, tick: u32) -> u32 {
        let cycle = self.period_ticks.max(1) as u64;
        let phase = self.phase_ticks() as u64 % cycle;
        ((tick as u64 % cycle + cycle - phase) % cycle) as u32
    }

    pub fn is_active(&self, tick: u32) -> bool {
        self.local_tick(tick) < self.active_ticks()
    }

    pub fn is_telegraph(&self, tick: u32) -> bool {
        let cycle = self.period_ticks.max(1);
        let local = self.local_tick(tick);
        !self.is_active(tick) && self.telegraph_ticks > 0 && local >= cycle.saturating_sub(self.telegraph_ticks)
    }

    /// Index of the mover this hazard rides (`mover=N`, a glitch trap on a piston face), if any.
    pub fn mover_index(&self) -> Option<usize> {
        self.tuning.get("mover").map(|v| v.max(0.0) as usize)
    }

    /// Centre at `tick`: the authored centre, carried by its mover's displacement when it rides one, and lowered
    /// linearly by `sweep_y` metres across each active window (the De-Frag data-purge plane).
    pub fn center_at(&self, tick: u32, movers: &[MoverDef]) -> [f32; 3] {
        let mut c = match self.mover_index().and_then(|i| movers.get(i)) {
            Some(m) => {
                let p = m.pos_at(tick);
                [0, 1, 2].map(|i| self.center[i] + p[i] - m.center[i])
            }
            None => self.center,
        };
        if let Some(sweep) = self.tuning.get("sweep_y").copied() {
            let local = self.local_tick(tick);
            let active = self.active_ticks();
            if local < active {
                c[1] -= sweep * local as f32 / active.max(1) as f32;
            }
        }
        c
    }

    /// `shelter=1`: a player with static geometry straight above them, up to where the hazard starts (its authored
    /// centre, the top of a `sweep_y` descent), is not hit.
    pub fn shelter(&self) -> bool {
        self.tuning.get("shelter").is_some_and(|v| *v > 0.0)
    }

    fn activation(&self, tick: u32) -> u32 {
        let cycle = self.period_ticks.max(1) as u64;
        let t = tick as u64 + cycle - (self.phase_ticks() as u64 % cycle);
        (t / cycle) as u32
    }
}

impl ZoneDef {
    pub const CORE_VENT: &'static str = "core_vent";

    pub fn cooldown_ticks(&self) -> u32 {
        self.tuning.get("cooldown_ticks").copied().map_or(0, |v| v.max(0.0) as u32)
    }

    pub fn fill_ticks(&self) -> u32 {
        self.tuning.get("fill_ticks").copied().map_or(90, |v| v.max(1.0) as u32)
    }

    pub fn reveal_ticks(&self) -> u32 {
        self.tuning.get("reveal_ticks").copied().map_or(300, |v| v.max(0.0) as u32)
    }
}

impl Features {
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    pub fn defs(&self) -> &LevelFeatures {
        &self.defs
    }

    pub fn events(&self) -> &[FeatureEvent] {
        &self.events
    }

    pub fn hazard_contacts(&self) -> &[HazardContact] {
        &self.contacts
    }

    pub fn piston_strikes(&self) -> &[PistonStrike] {
        &self.strikes
    }

    pub fn mover_position(&self, index: usize, tick: u32) -> Option<[f32; 3]> {
        self.defs.movers.get(index).map(|m| m.pos_at(tick))
    }

    pub fn forget_player(&mut self, id: u16) {
        self.zone_memory.remove(&id);
        self.hazard_hit.retain(|(_, p), _| *p != id);
        self.strike_memory.retain(|(_, p), _| *p != id);
    }

    pub fn load(&mut self, defs: LevelFeatures, world: &mut PhysicsWorld) {
        self.clear(world);
        self.movers = defs
            .movers
            .iter()
            .enumerate()
            .filter(|(_, m)| m.solid)
            .map(|(i, m)| {
                let p = m.pos_at(0);
                let collider = world.colliders.insert(
                    ColliderBuilder::cuboid(m.half[0], m.half[1], m.half[2])
                        .position(Isometry::translation(p[0], p[1], p[2]))
                        .collision_groups(InteractionGroups::new(GROUP_WORLD, Group::ALL))
                        .build(),
                );
                MoverRuntime { index: i as u16, collider }
            })
            .collect();
        for shape in &defs.shapes {
            for seg in shape.segments() {
                let c = ColliderBuilder::cuboid(seg.half[0], seg.half[1], seg.half[2])
                    .position(Isometry::new(vector![seg.center[0], seg.center[1], seg.center[2]], vector![0.0, seg.yaw, 0.0]))
                    .collision_groups(InteractionGroups::new(GROUP_WORLD, Group::ALL))
                    .build();
                self.shape_colliders.push(world.colliders.insert(c));
            }
        }
        self.defs = defs;
    }

    pub fn clear(&mut self, world: &mut PhysicsWorld) {
        for m in self.movers.drain(..) {
            world.colliders.remove(m.collider, &mut world.islands, &mut world.bodies, false);
        }
        for c in self.shape_colliders.drain(..) {
            world.colliders.remove(c, &mut world.islands, &mut world.bodies, false);
        }
        self.defs = LevelFeatures::default();
        self.zone_memory.clear();
        self.hazard_hit.clear();
        self.events.clear();
        self.contacts.clear();
        self.strikes.clear();
        self.strike_memory.clear();
    }

    /// Move solid colliders to their exact tick position and push any player caught in the swept volume.
    pub fn begin_tick(&mut self, world: &mut PhysicsWorld, players: &mut [Player], tick: u32, cfg: &Cfg) {
        self.events.clear();
        self.contacts.clear();
        self.strikes.clear();
        if self.movers.is_empty() {
            return;
        }
        let player_half = player_half(cfg);
        let mut struck = Vec::new();
        for m in &self.movers {
            let Some((def, prev, next)) = self.advance_mover(world, m, tick) else { continue };
            let piston = def.tag == MoverDef::PISTON;
            for (pi, p) in players.iter_mut().enumerate().filter(|(_, p)| p.vitals.alive()) {
                carry_rider(world, p.ch.body, prev, next, def.half, player_half);
                let a = def.axis.index();
                let d = next[a] - prev[a];
                if push_out_of_sweep(world, p.ch.body, prev, next, def.half, def.axis, player_half) && piston && d != 0.0 {
                    let stroke = tick.saturating_add(def.phase_ticks()) / def.period_ticks.max(1);
                    let mut dir = [0.0; 3];
                    dir[a] = d.signum();
                    struck.push((m.index, p.id, stroke, PistonStrike { victim: pi, mover: m.index, dir }));
                }
            }
        }
        for (mover, player, stroke, strike) in struck {
            if self.strike_memory.insert((mover, player), stroke) != Some(stroke) {
                self.strikes.push(strike);
            }
        }
    }

    /// Client-safe path: move the solid mover colliders to `tick` and push one body out of every swept face, with
    /// the same tick-exact math as the server. No events, contacts, zones or hazards: those stay server-only.
    pub fn begin_tick_single(&self, world: &mut PhysicsWorld, body: RigidBodyHandle, tick: u32, cfg: &Cfg) {
        if self.movers.is_empty() {
            return;
        }
        let player_half = player_half(cfg);
        for m in &self.movers {
            let Some((def, prev, next)) = self.advance_mover(world, m, tick) else { continue };
            carry_rider(world, body, prev, next, def.half, player_half);
            push_out_of_sweep(world, body, prev, next, def.half, def.axis, player_half);
        }
    }

    fn advance_mover<'a>(&'a self, world: &mut PhysicsWorld, m: &MoverRuntime, tick: u32) -> Option<(&'a MoverDef, [f32; 3], [f32; 3])> {
        let def = self.defs.movers.get(m.index as usize)?;
        let prev = def.pos_at(tick.saturating_sub(1));
        let next = def.pos_at(tick);
        if let Some(c) = world.colliders.get_mut(m.collider) {
            c.set_position(Isometry::translation(next[0], next[1], next[2]));
        }
        Some((def, prev, next))
    }

    /// Zone edges and hazard contacts for this tick. `damaging` is false while the match is frozen.
    pub fn end_tick(&mut self, world: &PhysicsWorld, players: &mut [Player], tick: u32, cfg: &Cfg, damaging: bool) {
        let player_half = [cfg.radius, cfg.half_height + cfg.radius, cfg.radius];

        for (zi, z) in self.defs.zones.iter().enumerate() {
            for p in players.iter_mut() {
                let inside = overlaps(z.center, z.half, p.pos(world), player_half);
                let mem = self.zone_memory.entry(p.id).or_insert_with(|| vec![ZoneMemory::default(); self.defs.zones.len()]);
                let Some(m) = mem.get_mut(zi) else { continue };
                let was = m.inside;
                if was && !inside {
                    m.inside = false;
                    m.ready_tick = tick.saturating_add(z.cooldown_ticks());
                    self.events.push(FeatureEvent::ZoneExit { tick, zone: zi as u16, player: p.id });
                } else if !was && inside && tick >= m.ready_tick {
                    m.inside = true;
                    self.events.push(FeatureEvent::ZoneEnter { tick, zone: zi as u16, player: p.id });
                }
                if damaging && inside && z.tag == ZoneDef::CORE_VENT && p.vitals.alive() {
                    if p.shield.vent_tick(tick, z.fill_ticks(), z.cooldown_ticks()) {
                        p.shield.vent_reveal_until = tick.saturating_add(z.reveal_ticks());
                    }
                }
            }
        }
        self.zone_memory.retain(|id, _| players.iter().any(|p| p.id == *id));

        if !damaging {
            return;
        }
        for (hi, h) in self.defs.hazards.iter().enumerate() {
            if !h.is_active(tick) {
                continue;
            }
            // `pulse_every`: within one active window the hazard re-arms every N ticks (a repeating pulse)
            let pulse = h.tuning.get("pulse_every").map_or(0, |v| v.max(0.0) as u32);
            let activation = if pulse > 0 { h.activation(tick).wrapping_mul(4096).wrapping_add(h.local_tick(tick) / pulse) } else { h.activation(tick) };
            let push = h.tuning.get("push").copied().unwrap_or(0.0);
            let centre = h.center_at(tick, &self.defs.movers);
            for (pi, p) in players.iter_mut().enumerate() {
                if !p.vitals.alive() || !overlaps(centre, h.half, p.pos(world), player_half) {
                    continue;
                }
                if h.shelter() && sheltered(world, p.ch.body, p.pos(world), h.center[1] - h.half[1]) {
                    continue;
                }
                let key = (hi as u16, p.id);
                if self.hazard_hit.get(&key) == Some(&activation) {
                    continue;
                }
                self.hazard_hit.insert(key, activation);
                self.events.push(FeatureEvent::HazardHit { tick, hazard: hi as u16, player: p.id, amount: h.damage });
                if push > 0.0 {
                    knock_outward(p, world, centre, push);
                }
                if h.damage > 0.0 {
                    self.contacts.push(HazardContact { victim: pi, amount: h.damage, source: hazard_source_id(hi) });
                }
            }
        }
    }
}

/// `push`: a horizontal shove away from the hazard centre (straight out along +X from the exact centre) plus a
/// small lift, so a grounded player leaves the floor. Y-up only: no gravity or camera change.
fn knock_outward(p: &mut Player, world: &PhysicsWorld, centre: [f32; 3], push: f32) {
    let pos = p.pos(world);
    let (dx, dz) = (pos[0] - centre[0], pos[2] - centre[2]);
    let len = libm::sqrtf(dx * dx + dz * dz);
    let (nx, nz) = if len > 1e-3 { (dx / len, dz / len) } else { (1.0, 0.0) };
    p.ch.vel = vector![nx * push, p.ch.vel.y.max(PUSH_LIFT), nz * push];
    if matches!(p.ch.state, crate::character::MoveState::Grounded) {
        p.ch.state = crate::character::MoveState::Airborne;
    }
}

/// Vertical speed (m/s) a `push` hazard gives at least, so the shove lifts a grounded player off the floor.
pub const PUSH_LIFT: f32 = 4.0;

/// Static geometry between the player's head and `ceiling_y`, straight up. Players and moving bodies don't count.
fn sheltered(world: &PhysicsWorld, body: RigidBodyHandle, pos: [f32; 3], ceiling_y: f32) -> bool {
    let from = point![pos[0], pos[1], pos[2]];
    let len = ceiling_y - pos[1];
    if len <= 0.0 {
        return false;
    }
    let ray = Ray::new(from, vector![0.0, 1.0, 0.0]);
    let pred = |_h: ColliderHandle, c: &Collider| c.parent().map_or(true, |b| world.bodies.get(b).map_or(true, |rb| rb.is_fixed()));
    let filter = QueryFilter::default().exclude_rigid_body(body).exclude_sensors().predicate(&pred);
    world.queries.cast_ray(&world.bodies, &world.colliders, &ray, len, true, filter).is_some()
}

fn overlaps(center: [f32; 3], half: [f32; 3], pos: [f32; 3], player_half: [f32; 3]) -> bool {
    (0..3).all(|i| (pos[i] - center[i]).abs() <= half[i] + player_half[i])
}

fn player_half(cfg: &Cfg) -> [f32; 3] {
    [cfg.radius, cfg.half_height + cfg.radius, cfg.radius]
}

/// Feet within this of a solid mover's top face (and over it) count as standing on it.
pub const CARRY_FOOT_TOLERANCE: f32 = 0.08;

/// A player standing on a solid mover's top face at `prev` moves with it to `next`, on every axis. Runs before the
/// sweep push so a rider on a rising platform is lifted, not shoved, and a rider on a sliding one stays on.
fn carry_rider(world: &mut PhysicsWorld, body: RigidBodyHandle, prev: [f32; 3], next: [f32; 3], half: [f32; 3], player_half: [f32; 3]) {
    let d = [next[0] - prev[0], next[1] - prev[1], next[2] - prev[2]];
    if d == [0.0; 3] {
        return;
    }
    let t = world.bodies[body].translation();
    let pos = [t.x, t.y, t.z];
    let feet = pos[1] - player_half[1];
    let top = prev[1] + half[1];
    if (feet - top).abs() > CARRY_FOOT_TOLERANCE {
        return;
    }
    if (pos[0] - prev[0]).abs() > half[0] + player_half[0] || (pos[2] - prev[2]).abs() > half[2] + player_half[2] {
        return;
    }
    world.bodies[body].set_position(Isometry::translation(pos[0] + d[0], pos[1] + d[1], pos[2] + d[2]), true);
    sync_body_colliders(world, body);
}

fn push_out_of_sweep(
    world: &mut PhysicsWorld,
    body: RigidBodyHandle,
    prev: [f32; 3],
    next: [f32; 3],
    half: [f32; 3],
    axis: MoveAxis,
    player_half: [f32; 3],
) -> bool {
    let t = world.bodies[body].translation();
    let pos = [t.x, t.y, t.z];
    let a = axis.index();
    let d = next[a] - prev[a];
    if d > 0.0 && pos[a] < prev[a] {
        return false;
    }
    if d < 0.0 && pos[a] > prev[a] {
        return false;
    }
    let (lo, hi) = if d >= 0.0 { (prev[a], next[a]) } else { (next[a], prev[a]) };
    for i in 0..3 {
        let hit = if i == a {
            pos[i] + player_half[i] >= lo - half[i] && pos[i] - player_half[i] <= hi + half[i]
        } else {
            (pos[i] - next[i]).abs() <= half[i] + player_half[i]
        };
        if !hit {
            return false;
        }
    }
    if axis != MoveAxis::Y && pos[1] - player_half[1] >= next[1] + half[1] - 0.08 {
        return false;
    }
    let target = if d > 0.0 {
        next[a] + half[a] + player_half[a] + 0.002
    } else if d < 0.0 {
        next[a] - half[a] - player_half[a] - 0.002
    } else if pos[a] >= next[a] {
        next[a] + half[a] + player_half[a] + 0.002
    } else {
        next[a] - half[a] - player_half[a] - 0.002
    };
    let mut to = pos;
    to[a] = target;
    world.bodies[body].set_position(Isometry::translation(to[0], to[1], to[2]), true);
    sync_body_colliders(world, body);
    true
}
