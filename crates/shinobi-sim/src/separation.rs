//! Soft player-vs-player push-out. The character controller lets capsules overlap; this keeps ninjas from
//! standing inside each other.
//!
//! Server-authoritative *correction*, not movement: it is NOT part of `movement::step_player`, so the client
//! predictor never runs it. The client's reconciler replays from the acked server state whenever the error
//! exceeds its 2 cm deadband and absorbs the jump in a decaying visual offset, so a push of a few cm per
//! tick is corrected smoothly. Overlap is mostly resolved each tick (both players share the penetration) but
//! capped, so a teleport-on-top never produces a visible pop.
//!
//! Properties: horizontal only; pairwise displacements are computed from the pre-push positions and summed,
//! so player order cannot matter; exactly coincident players split along a fixed axis chosen by id; a push
//! never carries a player into level geometry (world-only shape cast); velocities are untouched. Run it after
//! `check_step` so the anti-cheat never counts a push as player displacement.

use rapier3d::parry::shape::Capsule;
use rapier3d::parry::query::ShapeCastOptions;
use rapier3d::prelude::*;

use crate::character::{sync_body_colliders, Cfg, PhysicsWorld};
use crate::combat::{solid_groups, GROUP_MELEE};
use crate::tick::Player;

#[derive(Clone, Copy, Debug)]
pub struct SepCfg {
    /// Largest push applied to one player in one tick (metres).
    pub max_push_per_tick: f32,
    /// Overlap below this is left alone (stops micro-jitter around exactly-touching capsules).
    pub slop: f32,
}

impl Default for SepCfg {
    fn default() -> Self {
        Self { max_push_per_tick: 0.12, slop: 0.005 }
    }
}

pub fn separate(players: &[Player], world: &mut PhysicsWorld, ch: &Cfg, sep: &SepCfg) {
    let alive: Vec<usize> = (0..players.len()).filter(|&i| players[i].vitals.alive()).collect();
    let pos: Vec<Vector<Real>> = alive.iter().map(|&i| *world.bodies[players[i].ch.body].translation()).collect();
    let min = 2.0 * ch.radius;
    let reach_y = 2.0 * (ch.half_height + ch.radius);

    let mut disp = vec![Vector::<Real>::zeros(); alive.len()];
    let mut any = false;
    for a in 0..alive.len() {
        for b in a + 1..alive.len() {
            let d = pos[b] - pos[a];
            if d.y.abs() >= reach_y {
                continue; // one is above the other
            }
            let flat = vector![d.x, 0.0, d.z];
            let dist = flat.norm();
            let pen = min - dist;
            if pen <= sep.slop {
                continue;
            }
            // coincident: fixed axis, lower id toward -x
            let axis = if dist > 1e-4 {
                flat / dist
            } else if players[alive[a]].id <= players[alive[b]].id {
                Vector::x()
            } else {
                -Vector::x()
            };
            disp[a] -= axis * (pen * 0.5);
            disp[b] += axis * (pen * 0.5);
            any = true;
        }
    }
    if !any {
        return;
    }
    world.queries.update(&world.colliders);
    let capsule = Capsule::new_y(ch.half_height, ch.radius);
    for (k, &i) in alive.iter().enumerate() {
        let mut d = disp[k];
        let len = d.norm();
        if len < 1e-6 {
            continue;
        }
        if len > sep.max_push_per_tick {
            d *= sep.max_push_per_tick / len;
        }
        let (dir, len) = (d.normalize(), d.norm());
        let body = players[i].ch.body;
        let filter = QueryFilter::default()
            .exclude_rigid_body(body)
            .exclude_sensors()
            .groups(InteractionGroups::new(GROUP_MELEE, solid_groups()));
        // stop short of any level geometry in the way
        let travel = world
            .queries
            .cast_shape(
                &world.bodies,
                &world.colliders,
                &Isometry::from(pos[k] + Vector::y() * 0.05), // lifted: the floor under the feet is not an obstacle
                &dir,
                &capsule,
                ShapeCastOptions { max_time_of_impact: len, target_distance: 0.0, stop_at_penetration: true, compute_impact_geometry_on_penetration: false },
                filter,
            )
            .map_or(len, |(_, hit)| (hit.time_of_impact - 0.01).max(0.0));
        if travel <= 0.0 {
            continue;
        }
        let to = pos[k] + dir * travel;
        world.bodies[body].set_position(Isometry::translation(to.x, to.y, to.z), true);
        sync_body_colliders(world, body);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shinobi_proto::intent::{ClientPacket, PlayerIntent, PITCH_LEVEL};

    use super::*;
    use crate::tick::Server;

    const R2: f32 = 0.6; // 2 * capsule radius

    fn server(ps: &[(u16, [f32; 3])]) -> Server {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        for (id, at) in ps {
            s.add_player(*id, *at);
        }
        s
    }

    fn run(s: &mut Server, n: u32) {
        for _ in 0..n {
            s.frame(Duration::from_nanos(16_666_667));
        }
    }

    fn pos(s: &Server, id: u16) -> [f32; 3] {
        s.players.iter().find(|p| p.id == id).unwrap().pos(&s.world)
    }

    fn flat_dist(a: [f32; 3], b: [f32; 3]) -> f32 {
        ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    #[test]
    fn coincident_players_are_split_along_a_fixed_axis_in_a_few_ticks_and_stay_put() {
        let mut s = server(&[(2, [0.0, 0.81, 0.0]), (1, [0.0, 0.81, 0.0])]);
        run(&mut s, 1);
        let (p1, p2) = (pos(&s, 1), pos(&s, 2));
        assert!(p1[0] < p2[0], "lower id goes toward -x: {p1:?} {p2:?}");
        assert!((p1[0] - 0.0).abs() <= 0.12 + 1e-4 && (p2[0] - 0.0).abs() <= 0.12 + 1e-4, "capped per tick");
        run(&mut s, 6);
        assert!(flat_dist(pos(&s, 1), pos(&s, 2)) >= R2 - 0.01, "separated");
        // stable: no jitter once apart
        let (a, b) = (pos(&s, 1), pos(&s, 2));
        run(&mut s, 30);
        // (y wobbles ~1e-4 from the ground-stick; horizontal must not move at all)
        let (a2, b2) = (pos(&s, 1), pos(&s, 2));
        assert!(flat_dist(a, a2) < 1e-4 && flat_dist(b, b2) < 1e-4, "jitter: {a:?}->{a2:?} {b:?}->{b2:?}");
        // anti-cheat never saw it as movement
        assert!(s.players.iter().all(|p| p.guard.strikes == 0));
    }

    #[test]
    fn push_does_not_depend_on_player_order() {
        // the pass in isolation (full-sim order effects come from movement, where later players see earlier
        // ones' new positions by design)
        let at = |id: u16| [0.1 * id as f32, 0.81, 0.0];
        let mut a = server(&[(1, at(1)), (2, at(2)), (3, at(3))]);
        let mut b = server(&[(3, at(3)), (1, at(1)), (2, at(2))]);
        let (c, sep) = (Cfg::default(), SepCfg::default());
        for _ in 0..120 {
            separate(&a.players, &mut a.world, &c, &sep);
            separate(&b.players, &mut b.world, &c, &sep);
        }
        for id in 1..=3 {
            assert_eq!(pos(&a, id), pos(&b, id), "player {id}");
        }
        for (x, y) in [(1, 2), (1, 3), (2, 3)] {
            assert!(flat_dist(pos(&a, x), pos(&a, y)) >= R2 - 0.02, "{x}-{y} still overlap: {:?} {:?} {:?}", pos(&a,1), pos(&a,2), pos(&a,3));
        }
    }

    #[test]
    fn dead_players_and_players_at_different_heights_are_left_alone() {
        let mut s = server(&[(1, [0.0, 0.81, 0.0]), (2, [0.2, 0.81, 0.0])]);
        s.players[1].vitals.dead_until_tick = Some(10_000);
        let before = pos(&s, 1);
        run(&mut s, 5);
        assert!(flat_dist(pos(&s, 1), before) < 1e-4, "a corpse does not shove the living");
        // control: both alive -> they do move
        let mut s = server(&[(1, [0.0, 0.81, 0.0]), (2, [0.2, 0.81, 0.0])]);
        run(&mut s, 5);
        assert!(flat_dist(pos(&s, 1), before) > 0.05);
        // 1.7 m above is a different level
        let mut s = server(&[(1, [0.0, 0.81, 0.0]), (2, [0.0, 0.81 + 1.7, 0.0])]);
        s.players[1].ch.vel = Vector::zeros();
        let before = pos(&s, 1);
        s.world.bodies[s.players[1].ch.body].set_position(Isometry::translation(0.0, 2.51, 0.0), true);
        crate::character::sync_body_colliders(&mut s.world, s.players[1].ch.body);
        separate(&s.players, &mut s.world, &Cfg::default(), &SepCfg::default());
        assert!(flat_dist(pos(&s, 1), before) < 1e-6 && flat_dist(pos(&s, 2), [0.0, 0.0, 0.0]) < 1e-6);
    }

    #[test]
    fn a_push_never_carries_a_player_into_a_wall() {
        // wall face at x = 1.0; player 1 stands against it, player 2 overlaps from the left
        let mut s = server(&[(1, [0.65, 0.81, 0.0]), (2, [0.2, 0.81, 0.0])]);
        s.add_static_box([1.5, 1.0, 0.0], [0.5, 3.0, 5.0]);
        run(&mut s, 30);
        let p1 = pos(&s, 1);
        assert!(p1[0] + 0.3 <= 1.0 + 1e-3, "capsule stays out of the wall: x = {}", p1[0]);
        // control: without the wall player 1 is pushed right past x = 0.7
        let mut s = server(&[(1, [0.65, 0.81, 0.0]), (2, [0.2, 0.81, 0.0])]);
        run(&mut s, 30);
        assert!(pos(&s, 1)[0] > 0.7);
    }

    #[test]
    fn a_walking_player_cannot_stand_inside_a_stationary_one() {
        let mut s = server(&[(1, [0.0, 0.81, 0.0]), (2, [0.0, 0.81, 3.0])]);
        for _ in 0..90 {
            let t = s.tick;
            let it = PlayerIntent { tick: t, move_z: 127, aim_pitch: PITCH_LEVEL, ..Default::default() }; // 2 walks toward -Z
            s.ingest(2, &ClientPacket { ack_snapshot_tick: t, intents: vec![it], ..Default::default() }.encode().unwrap());
            s.frame(Duration::from_nanos(16_666_667));
            assert!(flat_dist(pos(&s, 1), pos(&s, 2)) >= R2 - 0.06 || s.tick < 3, "overlap at tick {}", s.tick);
        }
    }
}
