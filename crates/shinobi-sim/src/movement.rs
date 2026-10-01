//! The one movement step. Server and client prediction both call this, so they cannot drift apart:
//! refresh the query pipeline, run `CharacterSystem::update`, then commit the kinematic target.

use crate::character::{sync_body_colliders, CharacterSystem, PhysicsWorld, PlayerInput};
use crate::rapier3d::parry::partitioning::QbvhDataGenerator;
use crate::rapier3d::prelude::*;

/// Broad-phase input for movement queries: every enabled NON-sensor collider, nothing else.
///
/// Why movement does not use `world.queries`: that pipeline also holds sensors (hit boxes, pickups) which the KCC filters out afterwards, but their presence still changes the tree layout, and the
/// KCC's tie-breaking between simultaneous contacts (floor + wall) follows the layout. A hit-box sensor sitting at
/// the server player's feet is enough to move a wall contact by 4e-5 m, which the discrete jump/landing logic then
/// amplifies to metres within a few hundred ticks. Server and predictor therefore both query a solid-only tree.
pub struct SolidAabb<'a> {
    pub colliders: &'a ColliderSet,
}

impl QbvhDataGenerator<ColliderHandle> for SolidAabb<'_> {
    fn size_hint(&self) -> usize {
        self.colliders.len()
    }

    fn for_each(&mut self, mut f: impl FnMut(ColliderHandle, Aabb)) {
        for (h, co) in self.colliders.iter_enabled() {
            if !co.is_sensor() {
                f(h, co.compute_aabb());
            }
        }
    }
}

/// Rebuild a movement pipeline from the solid colliders (see `SolidAabb`).
pub fn refresh_solid(q: &mut QueryPipeline, colliders: &ColliderSet) {
    q.update_with_generator(SolidAabb { colliders });
}

/// Advance one player by `dt`. `now` is sim time (currently unused by movement).
///
/// Builds its own solid-only pipeline: the right call for a lone player (the client predictor).
pub fn step_player(ch: &mut CharacterSystem, input: &PlayerInput, world: &mut PhysicsWorld, now: f32, dt: f32) {
    let mut solid = QueryPipeline::new();
    refresh_solid(&mut solid, &world.colliders);
    step_player_with(ch, input, world, now, dt, &mut solid);
}

/// Same step against a solid pipeline the caller refreshed for this tick. The server refreshes once per tick
/// and steps every player against it (a rebuild per player over thousands of colliders was the dominant step
/// cost). Other players' capsules are then as of the start of the tick for culling purposes; static geometry,
/// which is all the predictor sees, is unaffected.
pub fn step_player_with(
    ch: &mut CharacterSystem,
    input: &PlayerInput,
    world: &mut PhysicsWorld,
    now: f32,
    dt: f32,
    solid: &mut QueryPipeline,
) {
    // CharacterSystem::update reads `world.queries`; lend it the solid pipeline for the duration
    std::mem::swap(&mut world.queries, solid);
    ch.update(dt, input, world, now);
    std::mem::swap(&mut world.queries, solid);
    let next = *world.bodies[ch.body].next_position();
    world.bodies[ch.body].set_position(next, true);
    sync_body_colliders(world, ch.body); // parented capsule must follow, or other players query a stale hull
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rapier3d::prelude::*;

    const DT: f32 = 1.0 / 60.0;

    fn add_player(w: &mut PhysicsWorld, at: [f32; 3]) -> CharacterSystem {
        let cfg = crate::character::Cfg::default();
        let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![at[0], at[1], at[2]]).build());
        w.colliders.insert_with_parent(ColliderBuilder::capsule_y(cfg.half_height, cfg.radius).build(), body, &mut w.bodies);
        CharacterSystem::new(body, cfg)
    }

    fn down_ray_hit(w: &PhysicsWorld, x: f32, z: f32) -> Option<ColliderHandle> {
        let ray = Ray::new(point![x, 5.0, z], vector![0.0, -1.0, 0.0]);
        w.queries.cast_ray(&w.bodies, &w.colliders, &ray, 10.0, true, QueryFilter::default()).map(|(h, _)| h)
    }

    #[test]
    fn moved_players_capsule_is_where_queries_see_it() {
        let mut w = PhysicsWorld {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            queries: QueryPipeline::new(),
        };
        w.colliders.insert(ColliderBuilder::cuboid(100.0, 0.5, 100.0).translation(vector![0.0, -0.5, 0.0]).build());
        let mut a = add_player(&mut w, [0.0, 0.81, 5.0]);
        let _b = add_player(&mut w, [0.0, 0.81, -5.0]);
        let a_cap = w.bodies[a.body].colliders()[0];
        let run = PlayerInput { move_dir: vector![1.0, 0.0, 0.0], ..Default::default() };
        let mut now = 0.0;
        for _ in 0..75 {
            step_player(&mut a, &run, &mut w, now, DT);
            now += DT;
        }
        w.queries.update(&w.colliders);
        let x = w.bodies[a.body].translation().x;
        assert!(x > 10.0, "moved {x} m");
        assert_eq!(down_ray_hit(&w, x, 5.0), Some(a_cap), "ray at the new spot must hit the moved capsule");
        assert_ne!(down_ray_hit(&w, 0.0, 5.0), Some(a_cap), "and the old spot must be empty");
        // the collider's own position agrees with the body
        assert!((w.colliders[a_cap].translation().x - x).abs() < 1e-5);
    }

    #[test]
    fn restore_resyncs_the_capsule() {
        let mut w = PhysicsWorld {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            queries: QueryPipeline::new(),
        };
        let mut a = add_player(&mut w, [0.0, 0.81, 0.0]);
        let mut snap = a.snapshot(&w);
        snap.pos = vector![7.0, 3.0, -2.0];
        a.restore(&mut w, &snap);
        let cap = w.bodies[a.body].colliders()[0];
        assert!((w.colliders[cap].translation() - snap.pos).norm() < 1e-6);
    }
}
