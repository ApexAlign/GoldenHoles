//! Data-cube pickups + the grid-beam ultimate.
use rapier3d::parry::shape::Cylinder;
use rapier3d::prelude::*;

use super::{Aim, CombatEvent, CombatInput, DamageKind, GROUP_MELEE, TARGETS};
use crate::character::PhysicsWorld;

pub const SLOTS: usize = 3;
pub const PICKUP_RADIUS: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerupKind {
    GridBeam,
}

#[derive(Clone, Copy, Debug)]
pub struct Charge {
    pub kind: PowerupKind,
    /// Seconds before the charge decays unused.
    pub remaining: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct DataCube {
    pub id: u32,
    pub pos: Point<Real>,
    pub kind: PowerupKind,
    pub charge_secs: f32,
}

#[derive(Clone, Debug)]
pub struct GridBurst {
    pub origin: Point<Real>,
    pub radius: f32,
    pub speed: f32,
    pub max_radius: f32,
    pub damage: f32,
    hit: Vec<ColliderHandle>,
}

/// Grid-beam tunables (see `Balance`).
#[derive(Clone, Copy, Debug)]
pub struct UltimateCfg {
    pub speed: f32,
    pub max_radius: f32,
    pub damage: f32,
}

impl Default for UltimateCfg {
    fn default() -> Self {
        Self { speed: 60.0, max_radius: 120.0, damage: 9999.0 }
    }
}

#[derive(Default)]
pub struct PowerupState {
    pub ultimate: UltimateCfg,
    /// Pickups are plain proximity checks (no colliders): deterministic and step-independent.
    pub cubes: Vec<DataCube>,
    pub slots: [Option<Charge>; SLOTS],
    bursts: Vec<GridBurst>,
}

impl PowerupState {
    /// For the renderer: expanding ring radius + origin.
    pub fn bursts(&self) -> &[GridBurst] {
        &self.bursts
    }

    /// Put a charge in the first free slot. False when the inventory is full.
    pub fn grant(&mut self, kind: PowerupKind, charge_secs: f32) -> bool {
        match self.slots.iter_mut().find(|s| s.is_none()) {
            Some(s) => {
                *s = Some(Charge { kind, remaining: charge_secs });
                true
            }
            None => false,
        }
    }

    pub fn has_charge(&self) -> bool {
        self.slots.iter().any(|s| s.is_some())
    }

    pub fn free_slots(&self) -> usize {
        self.slots.iter().filter(|s| s.is_none()).count()
    }

    /// Match reset: no charges, no bursts in flight.
    pub fn clear(&mut self) {
        self.slots = [None; SLOTS];
        self.bursts.clear();
    }

    pub fn update(
        &mut self,
        dt: f32,
        input: &CombatInput,
        _aim: &Aim,
        world: &PhysicsWorld,
        body: RigidBodyHandle,
        events: &mut Vec<CombatEvent>,
    ) {
        let p = Point::from(*world.bodies[body].translation());

        // pickups
        let slots = &mut self.slots;
        self.cubes.retain(|c| {
            if (c.pos - p).norm() > PICKUP_RADIUS {
                return true;
            }
            match slots.iter_mut().find(|s| s.is_none()) {
                Some(s) => {
                    *s = Some(Charge { kind: c.kind, remaining: c.charge_secs });
                    events.push(CombatEvent::PickedUp { id: c.id, kind: c.kind });
                    false
                }
                None => true, // inventory full: leave it
            }
        });

        // decay
        for s in &mut self.slots {
            if let Some(c) = s {
                c.remaining -= dt;
                if c.remaining <= 0.0 {
                    *s = None;
                }
            }
        }

        // activate first charge
        if input.ultimate {
            if let Some(s) = self.slots.iter_mut().find(|s| s.is_some()) {
                if let Some(Charge { kind: PowerupKind::GridBeam, .. }) = s.take() {
                    self.bursts.push(GridBurst {
                        origin: p,
                        radius: 0.0,
                        speed: self.ultimate.speed,
                        max_radius: self.ultimate.max_radius,
                        damage: self.ultimate.damage,
                        hit: Vec::new(),
                    });
                    events.push(CombatEvent::UltimateStart { origin: p });
                }
            }
        }

        // expanding wavefront: every enemy inside the (vertically unbounded) disc takes damage once
        let filter = QueryFilter::default().groups(InteractionGroups::new(GROUP_MELEE, TARGETS));
        self.bursts.retain_mut(|b| {
            b.radius += b.speed * dt;
            if b.radius > 0.01 {
                let shape = Cylinder::new(200.0, b.radius);
                let pos = Isometry::from(b.origin.coords);
                let mut found = Vec::new();
                world.queries.intersections_with_shape(&world.bodies, &world.colliders, &pos, &shape, filter, |h| {
                    found.push(h);
                    true
                });
                for h in found {
                    if b.hit.contains(&h) {
                        continue;
                    }
                    b.hit.push(h);
                    let Some(col) = world.colliders.get(h) else { continue };
                    events.push(CombatEvent::Damage {
                        target: col.user_data,
                        collider: h,
                        amount: b.damage,
                        kind: DamageKind::GridBeam,
                        point: Point::from(*col.translation()),
                    });
                }
            }
            b.radius < b.max_radius
        });
    }
}
