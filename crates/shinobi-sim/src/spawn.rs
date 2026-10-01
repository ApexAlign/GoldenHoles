//! Spawn selection: farthest from living enemies, preferring points no enemy can see.
//!
//! Deterministic: no randomness; candidates rotate by index with the pick counter. Used for new players (`Server::pick_spawn`) and
//! for every respawn. Line of sight is a ray against solid world geometry only (players, hitboxes and sensors
//! never block it). Light-walls are `GROUP_WORLD` solids, so a fresh trail can hide a spawn too; that is
//! deliberate and cheap.

use rapier3d::prelude::*;

use crate::character::PhysicsWorld;
use crate::combat::{solid_groups, GROUP_MELEE};

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// True if a solid blocks the straight line `from -> to`.
pub fn blocked(world: &PhysicsWorld, from: [f32; 3], to: [f32; 3]) -> bool {
    let d = dist(from, to);
    if d < 1e-3 {
        return false;
    }
    let dir = vector![to[0] - from[0], to[1] - from[1], to[2] - from[2]] / d;
    let ray = Ray::new(Point::new(from[0], from[1], from[2]), dir);
    let filter = QueryFilter::default().exclude_sensors().groups(InteractionGroups::new(GROUP_MELEE, solid_groups()));
    world.queries.cast_ray(&world.bodies, &world.colliders, &ray, d - 0.05, true, filter).is_some()
}

/// Index of the best spawn, or `None` if the list is empty. `enemies`: positions of living opponents.
/// Callers must have run `world.queries.update` since colliders last changed.
///
/// 1. no enemies: rotate through the list with `rotation` (successive joins spread out);
/// 2. prefer spawns that no enemy has line of sight to (if every spawn is seen, consider them all);
/// 3. among those, the ones whose distance to the nearest enemy is within `NEAR_BEST` (70%) of the largest;
///    take the one used longest ago (`rotation` is the packed pick history, see `push_history`), so repeated
///    respawns spread over the arena yet never choose a spawn that is clearly worse than the best.
pub fn choose(world: &PhysicsWorld, spawns: &[[f32; 3]], enemies: &[[f32; 3]], rotation: u32) -> Option<usize> {
    if spawns.is_empty() {
        return None;
    }
    if enemies.is_empty() {
        return least_recent((0..spawns.len()).collect(), rotation);
    }
    let hidden: Vec<usize> =
        (0..spawns.len()).filter(|&i| enemies.iter().all(|e| blocked(world, *e, spawns[i]))).collect();
    let pool: Vec<usize> = if hidden.is_empty() { (0..spawns.len()).collect() } else { hidden };

    let nearest = |i: usize| enemies.iter().map(|e| dist(*e, spawns[i])).fold(f32::INFINITY, f32::min);
    let best = pool.iter().map(|&i| nearest(i)).fold(0.0f32, f32::max);
    // every spawn nearly as good as the best is a fair candidate; rotating through them (instead of always taking
    // the single farthest) stops the same corner being used for every respawn when enemies camp
    let candidates: Vec<usize> = pool.iter().copied().filter(|&i| nearest(i) >= best * NEAR_BEST).collect();
    least_recent(candidates, rotation)
}

/// `history`: the last 8 picks packed 4 bits each (most recent in the low nibble, spawn index + 1, 0 = empty).
/// Callers record a pick with `push_history`.
pub fn push_history(history: u32, picked: usize) -> u32 {
    (history << 4) | (picked.min(14) as u32 + 1)
}

/// How many picks ago `i` was used (0 = last pick); 8 = not in the history.
fn age(history: u32, i: usize) -> u32 {
    let tag = i.min(14) as u32 + 1;
    (0..8).find(|n| (history >> (4 * n)) & 0xF == tag).unwrap_or(8)
}

/// The candidate used longest ago (never = oldest); ties -> lowest index.
fn least_recent(candidates: Vec<usize>, history: u32) -> Option<usize> {
    let mut best: Option<(u32, usize)> = None;
    for i in candidates {
        let a = age(history, i);
        if best.map_or(true, |(ba, _)| a > ba) {
            best = Some((a, i));
        }
    }
    best.map(|(_, i)| i)
}

/// A spawn within this fraction of the best distance is "good enough" to rotate through.
pub const NEAR_BEST: f32 = 0.7;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::character::Cfg;
    use crate::pvp::PvpEvent;
    use crate::tick::Server;

    const A: u16 = 1;
    const B: u16 = 2;
    const E: [f32; 3] = [0.0, 0.81, 0.0];
    const NEAR: [f32; 3] = [3.0, 0.81, 0.0];
    const FAR_VISIBLE: [f32; 3] = [30.0, 0.81, 0.0];
    const HIDDEN: [f32; 3] = [0.0, 0.81, -12.0];

    fn server(with_wall: bool) -> Server {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        if with_wall {
            s.add_static_box([0.0, 1.0, -6.0], [10.0, 3.0, 0.2]); // between E and HIDDEN
        }
        s.add_player(A, E);
        s.set_spawns(vec![NEAR, FAR_VISIBLE, HIDDEN]);
        s
    }

    #[test]
    fn a_hidden_spawn_beats_a_farther_visible_one() {
        let mut s = server(true);
        // HIDDEN (12 m) is nearer than FAR_VISIBLE (30 m) but nobody can see it
        assert_eq!(s.pick_spawn(Some(B)), HIDDEN);
        // control: remove the wall and the farthest point wins
        let mut s = server(false);
        assert_eq!(s.pick_spawn(Some(B)), FAR_VISIBLE);
    }

    #[test]
    fn equally_good_spawns_rotate_in_index_order_instead_of_repeating_one() {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        s.add_player(A, E);
        let ring = vec![[10.0, 0.81, 0.0], [-10.0, 0.81, 0.0], [0.0, 0.81, 10.0]]; // all 10 m away
        s.set_spawns(ring.clone());
        let picks: Vec<_> = (0..6).map(|_| s.pick_spawn(Some(B))).collect();
        assert_eq!(picks, vec![ring[0], ring[1], ring[2], ring[0], ring[1], ring[2]], "starts at the lowest index, then rotates");
        // control: a clearly worse spawn (3 m away) is never in the rotation
        let mut ring2 = ring.clone();
        ring2.push([3.0, 0.81, 0.0]);
        s.set_spawns(ring2.clone());
        let picks: Vec<_> = (0..8).map(|_| s.pick_spawn(Some(B))).collect();
        assert!(!picks.contains(&ring2[3]), "{picks:?}");
    }

    #[test]
    fn with_nobody_to_avoid_successive_joins_rotate_through_the_list() {
        let mut s = Server::new(Cfg::default());
        s.set_spawns(vec![NEAR, FAR_VISIBLE, HIDDEN]);
        let picks: Vec<_> = (0..4).map(|_| s.pick_spawn(None)).collect();
        assert_eq!(picks, vec![NEAR, FAR_VISIBLE, HIDDEN, NEAR]);
    }

    #[test]
    fn dead_players_and_the_joiner_do_not_count_as_enemies() {
        let mut s = server(true);
        // A (the only possible enemy) is picking for itself: nobody to avoid, so the rotation applies
        assert_eq!(s.pick_spawn(Some(A)), NEAR);
    }

    #[test]
    fn respawns_use_the_selector_instead_of_the_fixed_home() {
        let mut s = server(true);
        s.add_player(B, [0.0, 0.81, -3.0]); // home spawn: right in front of A
        // A slices B (reach 5 m) dead
        let t = s.tick;
        let it = shinobi_proto::intent::PlayerIntent {
            tick: t,
            buttons: shinobi_proto::intent::Buttons(shinobi_proto::intent::Buttons::SLICE),
            aim_pitch: shinobi_proto::intent::PITCH_LEVEL,
            ..Default::default()
        };
        let pkt = shinobi_proto::intent::ClientPacket { ack_snapshot_tick: t, intents: vec![it], ..Default::default() };
        s.ingest(A, &pkt.encode().unwrap());
        let mut ev = Vec::new();
        for _ in 0..260 {
            s.frame(Duration::from_nanos(16_666_667));
            ev.extend(s.pvp_events.iter().copied());
        }
        assert!(ev.iter().any(|e| matches!(e, PvpEvent::Kill { victim: B, .. })));
        assert!(ev.iter().any(|e| matches!(e, PvpEvent::Respawn { player: B, .. })));
        let b = s.players.iter().find(|p| p.id == B).unwrap().pos(&s.world);
        let want = [HIDDEN[0], HIDDEN[1], HIDDEN[2]];
        assert!(dist(b, want) < 0.5, "respawned at the hidden point, not home: {b:?}");
    }
}
