use rapier3d::prelude::*;

use crate::groups;

/// `Collider::user_data` tag so enemy-contact handlers can recognise lethal light-walls.
pub const TRAIL_USER_DATA: u128 = 0x7241_11_u128;

#[derive(Clone, Copy, Debug)]
pub struct TrailSample {
    pub position: Point<Real>,
    pub time: f32,
    /// Collider for the wall segment running from the previous sample to this one.
    pub collider: Option<ColliderHandle>,
}

#[derive(Clone, Copy, Debug)]
pub struct TrailConfig {
    pub min_spacing: f32,
    pub lifetime: f32,
    pub wall_height: f32,
    pub wall_thickness: f32,
}

impl Default for TrailConfig {
    fn default() -> Self {
        Self { min_spacing: 0.4, lifetime: 4.0, wall_height: 1.6, wall_thickness: 0.12 }
    }
}

/// Fixed-capacity cyclic buffer of player position history (oldest -> newest).
/// Renderer reads `iter()`; each sample's segment owns a temporary static collider.
pub struct TrailBuffer<const N: usize> {
    samples: [Option<TrailSample>; N],
    head: usize, // next write slot
    len: usize,
    pub cfg: TrailConfig,
}

impl<const N: usize> TrailBuffer<N> {
    pub fn new(cfg: TrailConfig) -> Self {
        Self { samples: [None; N], head: 0, len: 0, cfg }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    fn tail(&self) -> usize {
        (self.head + N - self.len) % N
    }

    /// Chronological iterator (oldest first) for the render thread.
    pub fn iter(&self) -> impl Iterator<Item = &TrailSample> + '_ {
        let tail = self.tail();
        (0..self.len).filter_map(move |i| self.samples[(tail + i) % N].as_ref())
    }

    fn newest(&self) -> Option<&TrailSample> {
        if self.len == 0 { None } else { self.samples[(self.head + N - 1) % N].as_ref() }
    }

    fn drop_oldest(
        &mut self,
        colliders: &mut ColliderSet,
        islands: &mut IslandManager,
        bodies: &mut RigidBodySet,
    ) {
        if self.len == 0 {
            return;
        }
        let tail = self.tail();
        if let Some(s) = self.samples[tail].take() {
            if let Some(h) = s.collider {
                colliders.remove(h, islands, bodies, false);
            }
        }
        self.len -= 1;
    }

    /// Call once per frame after `CharacterSystem::update` (needs `&mut ColliderSet`,
    /// which the KCC query borrow can't share). Expires old walls, appends a sample
    /// when the player moved `min_spacing`, and spawns the wall collider.
    pub fn update(
        &mut self,
        now: f32,
        position: Point<Real>,
        colliders: &mut ColliderSet,
        islands: &mut IslandManager,
        bodies: &mut RigidBodySet,
    ) {
        while let Some(oldest) = self.samples[self.tail()].filter(|_| self.len > 0) {
            if now - oldest.time > self.cfg.lifetime {
                self.drop_oldest(colliders, islands, bodies);
            } else {
                break;
            }
        }

        let prev = self.newest().map(|s| s.position);
        if let Some(p) = prev {
            if (position - p).norm() < self.cfg.min_spacing {
                return;
            }
        }
        if self.len == N {
            self.drop_oldest(colliders, islands, bodies);
        }

        let collider = prev.and_then(|p| self.spawn_segment(p, position, colliders));
        self.samples[self.head] = Some(TrailSample { position, time: now, collider });
        self.head = (self.head + 1) % N;
        self.len += 1;
    }

    fn spawn_segment(
        &self,
        a: Point<Real>,
        b: Point<Real>,
        colliders: &mut ColliderSet,
    ) -> Option<ColliderHandle> {
        let d = b - a;
        let len = d.norm();
        if len < 1e-3 {
            return None;
        }
        let dir = d / len;
        let rot = if dir.cross(&Vector::y()).norm() > 1e-3 {
            UnitQuaternion::face_towards(&dir, &Vector::y())
        } else {
            UnitQuaternion::identity()
        };
        let mid = (a.coords + b.coords) * 0.5;
        let collider = ColliderBuilder::cuboid(
            self.cfg.wall_thickness * 0.5,
            self.cfg.wall_height * 0.5,
            len * 0.5,
        )
        .position(Isometry::from_parts(Translation::from(mid), rot))
        .collision_groups(InteractionGroups::new(groups::TRAIL, groups::ENEMY))
        .active_events(ActiveEvents::COLLISION_EVENTS)
        .user_data(TRAIL_USER_DATA)
        .build();
        Some(colliders.insert(collider))
    }
}
