//! Server-owned pickups: fixed points, respawn timers, capsule-overlap collection.
//!
//! Pickups live here (one store for the whole match), not in a player's `CombatSystem`, so anyone can take
//! them. Charges land in the collector's `combat.powerups` slots. Everything is a function of the tick
//! counter, so it is deterministic and replayable. Resolved once per tick after movement, never inside a
//! per-player update, so player order cannot decide who wins a contested cube: nearest capsule first, then
//! the lower player id.

use rapier3d::parry::query::intersection_test;
use rapier3d::parry::shape::{Capsule, Cuboid};
use rapier3d::prelude::*;

use crate::character::{Cfg, PhysicsWorld};
use crate::combat::powerup::PowerupKind;
use crate::tick::Player;

/// Half extent of the cube's pickup volume (the visible data-cube is smaller).
pub const PICKUP_HALF: f32 = 0.4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PickupEvent {
    /// A cube appeared (match start, or its respawn timer elapsed).
    Spawned { tick: u32, id: u32, pos: [f32; 3], kind: PowerupKind },
    Taken { tick: u32, id: u32, player: u16, kind: PowerupKind },
}

#[derive(Clone, Copy, Debug)]
pub struct PickupPoint {
    pub id: u32,
    pub pos: [f32; 3],
    pub kind: PowerupKind,
    pub respawn_ticks: u32,
    /// Seconds the collected charge lasts in the inventory.
    pub charge_secs: f32,
}

#[derive(Clone, Copy, Debug)]
struct Slot {
    point: PickupPoint,
    active: bool,
    /// Tick the cube reappears (only meaningful while !active).
    ready_at: u32,
    announced: bool,
    /// Dropped loot: removed when taken or expired, cleared by `reset`, never respawns.
    one_shot: bool,
    expires_at: u32,
}

#[derive(Default)]
pub struct Pickups {
    slots: Vec<Slot>,
}

/// `a` has been reached by `now` (wrap-safe).
fn reached(now: u32, a: u32) -> bool {
    now.wrapping_sub(a) < u32::MAX / 2
}

impl Pickups {
    pub fn add_point(&mut self, point: PickupPoint) {
        self.slots.push(Slot { point, active: true, ready_at: 0, announced: false, one_shot: false, expires_at: 0 });
    }

    /// One-shot loot (enemy drops). `id` must not collide with fixed points (drops use 1_000_000+).
    /// Announced with `Spawned` on the next update; vanishes when taken or at `expires_at`.
    pub fn drop_loot(&mut self, point: PickupPoint, expires_at: u32) {
        self.slots.push(Slot { point, active: true, ready_at: 0, announced: false, one_shot: true, expires_at });
    }

    /// Active cubes right now: (id, pos, kind). For HUD/FX or a snapshot.
    pub fn active(&self) -> impl Iterator<Item = (u32, [f32; 3], PowerupKind)> + '_ {
        self.slots.iter().filter(|s| s.active).map(|s| (s.point.id, s.point.pos, s.point.kind))
    }

    /// Ticks until a taken cube is back, `None` if it is up (HUD respawn pips).
    pub fn ticks_until_back(&self, id: u32, now: u32) -> Option<u32> {
        let s = self.slots.iter().find(|s| s.point.id == id && !s.active)?;
        Some(if reached(now, s.ready_at) { 0 } else { s.ready_at.wrapping_sub(now) })
    }

    /// Match reset: every cube is up again, re-announced on the next `update`.
    pub fn reset(&mut self) {
        self.slots.retain(|s| !s.one_shot);
        for s in &mut self.slots {
            s.active = true;
            s.announced = false;
        }
    }

    pub fn update(&mut self, tick: u32, world: &PhysicsWorld, players: &mut [Player], ch: &Cfg, out: &mut Vec<PickupEvent>) {
        let capsule = Capsule::new_y(ch.half_height, ch.radius);
        self.slots.retain(|s| !(s.one_shot && reached(tick, s.expires_at)));
        for s in &mut self.slots {
            if !s.active && reached(tick, s.ready_at) {
                s.active = true;
                s.announced = false;
            }
            if !s.active {
                continue;
            }
            if !s.announced {
                s.announced = true;
                out.push(PickupEvent::Spawned { tick, id: s.point.id, pos: s.point.pos, kind: s.point.kind });
            }
            let cube = Cuboid::new(vector![PICKUP_HALF, PICKUP_HALF, PICKUP_HALF]);
            let at = Isometry::translation(s.point.pos[0], s.point.pos[1], s.point.pos[2]);

            // nearest capsule centre wins, then lowest id; only players who can actually hold a charge
            let mut best: Option<(f32, u16, usize)> = None;
            for (i, p) in players.iter().enumerate() {
                if !p.vitals.alive() || p.combat.powerups.free_slots() == 0 {
                    continue;
                }
                let body = *world.bodies[p.ch.body].position();
                if !intersection_test(&at, &cube, &body, &capsule).unwrap_or(false) {
                    continue;
                }
                let d = (body.translation.vector - vector![s.point.pos[0], s.point.pos[1], s.point.pos[2]]).norm();
                if best.map_or(true, |(bd, bid, _)| (d, p.id) < (bd, bid)) {
                    best = Some((d, p.id, i));
                }
            }
            if let Some((_, id, i)) = best {
                if players[i].combat.powerups.grant(s.point.kind, s.point.charge_secs) {
                    s.active = false;
                    s.ready_at = if s.one_shot { s.expires_at } else { tick.wrapping_add(s.point.respawn_ticks) };
                    out.push(PickupEvent::Taken { tick, id: s.point.id, player: id, kind: s.point.kind });
                }
            }
        }
        self.slots.retain(|s| !(s.one_shot && !s.active)); // taken loot is gone for good
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::tick::Server;

    const A: u16 = 1;
    const B: u16 = 2;
    const CUBE: [f32; 3] = [5.0, 0.81, 0.0];

    fn sim(players: &[(u16, f32)], respawn_secs: f32) -> Server {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        for (id, dx) in players {
            s.add_player(*id, [CUBE[0] + dx, 0.81, CUBE[2]]);
        }
        s.add_pickup(1, CUBE, PowerupKind::GridBeam, respawn_secs);
        s
    }

    fn run(s: &mut Server, n: u32) -> Vec<PickupEvent> {
        let mut ev = Vec::new();
        for _ in 0..n {
            s.frame(Duration::from_nanos(16_666_667));
            ev.extend(s.pickup_events.iter().copied());
        }
        ev
    }

    fn charges(s: &Server, id: u16) -> usize {
        let p = s.players.iter().find(|p| p.id == id).unwrap();
        3 - p.combat.powerups.free_slots()
    }

    fn taken_by(ev: &[PickupEvent]) -> Vec<u16> {
        ev.iter().filter_map(|e| if let PickupEvent::Taken { player, .. } = e { Some(*player) } else { None }).collect()
    }

    #[test]
    fn cube_is_announced_then_collected_and_comes_back_on_its_timer() {
        let mut s = sim(&[(A, 0.0)], 1.0); // 60 ticks
        let ev = run(&mut s, 125);
        assert!(matches!(ev[0], PickupEvent::Spawned { tick: 0, id: 1, .. }));
        let takes: Vec<u32> = ev.iter().filter_map(|e| if let PickupEvent::Taken { tick, .. } = e { Some(*tick) } else { None }).collect();
        assert_eq!(takes, vec![0, 60, 120], "taken at once, then exactly one timer later each time");
        assert_eq!(charges(&s, A), 3);
        assert_eq!(ev.iter().filter(|e| matches!(e, PickupEvent::Spawned { .. })).count(), 3);
    }

    #[test]
    fn contested_cube_goes_to_the_nearer_capsule_then_the_lower_id() {
        // B is nearer (|dx| 0.1 vs 0.5)
        let mut s = sim(&[(A, 0.5), (B, -0.1)], 100.0);
        assert_eq!(taken_by(&run(&mut s, 2)), vec![B]);
        // control: swap who is nearer
        let mut s = sim(&[(A, -0.1), (B, 0.5)], 100.0);
        assert_eq!(taken_by(&run(&mut s, 2)), vec![A]);
        // exact tie: lower id
        let mut s = sim(&[(B, 0.3), (A, -0.3)], 100.0);
        assert_eq!(taken_by(&run(&mut s, 2)), vec![A]);
    }

    #[test]
    fn pickup_needs_capsule_overlap_not_just_being_near() {
        // cube half 0.4 + capsule radius 0.3 = 0.7: 0.9 m away is close, but not touching
        let mut s = sim(&[(A, 0.9)], 100.0);
        assert!(taken_by(&run(&mut s, 5)).is_empty());
        assert_eq!(charges(&s, A), 0);
        // control: 0.5 m is touching
        let mut s = sim(&[(A, 0.5)], 100.0);
        assert_eq!(taken_by(&run(&mut s, 5)), vec![A]);
    }

    #[test]
    fn full_inventory_leaves_the_cube_up_for_someone_else() {
        let mut s = sim(&[(A, 0.0)], 100.0);
        for _ in 0..3 {
            assert!(s.players[0].combat.powerups.grant(PowerupKind::GridBeam, 20.0));
        }
        assert!(taken_by(&run(&mut s, 10)).is_empty(), "no room, no pickup");
        assert_eq!(s.pickups.active().count(), 1, "cube stays up");
        // control: a free slot lets A take it
        s.players[0].combat.powerups.slots[0] = None;
        assert_eq!(taken_by(&run(&mut s, 2)), vec![A]);
    }
}
