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
    let all: Vec<usize> = (0..spawns.len()).collect();
    choose_among(world, spawns, &all, enemies, rotation)
}

/// Spawn points with an optional team each (`NO_TEAM` = usable by anyone), as the level declares them.
#[derive(Clone, Copy)]
pub struct SpawnSet<'a> {
    pub pos: &'a [[f32; 3]],
    /// Parallel to `pos`; shorter (or empty) = the rest are `NO_TEAM`.
    pub teams: &'a [u8],
}

impl SpawnSet<'_> {
    /// Indices a player of `team` may spawn at: its own team's points plus the shared ones. If its team has
    /// no point at all (a one-sided level), every point is allowed rather than none.
    pub fn allowed(&self, team: u8) -> Vec<usize> {
        let t = |i: usize| self.teams.get(i).copied().unwrap_or(crate::tick::NO_TEAM);
        let mine: Vec<usize> = (0..self.pos.len()).filter(|&i| t(i) == crate::tick::NO_TEAM || t(i) == team).collect();
        if team == crate::tick::NO_TEAM || mine.is_empty() { (0..self.pos.len()).collect() } else { mine }
    }
}

/// `choose` restricted to the `allowed` spawn indices (team spawns). `enemies` = living non-allies.
pub fn choose_among(world: &PhysicsWorld, spawns: &[[f32; 3]], allowed: &[usize], enemies: &[[f32; 3]], rotation: u32) -> Option<usize> {
    if allowed.is_empty() {
        return None;
    }
    if enemies.is_empty() {
        return least_recent(allowed.to_vec(), rotation);
    }
    let hidden: Vec<usize> =
        allowed.iter().copied().filter(|&i| enemies.iter().all(|e| blocked(world, *e, spawns[i]))).collect();
    let mut pool: Vec<usize> = if hidden.is_empty() { allowed.to_vec() } else { hidden };
    // a lone hidden spawn that was also the previous pick would repeat back to back (campers keep it the only
    // hidden one): let the far visible spawns share the rotation for that one pick
    if pool.len() == 1 && last_was_respawn(rotation) && age(rotation, pool[0]) == 0 && spawns.len() > 1 {
        let near = |i: usize| enemies.iter().map(|e| dist(*e, spawns[i])).fold(f32::INFINITY, f32::min);
        let far_best = (0..spawns.len()).map(near).fold(0.0f32, f32::max);
        pool = (0..spawns.len()).filter(|&i| near(i) >= far_best * NEAR_BEST).collect();
    }

    let nearest = |i: usize| enemies.iter().map(|e| dist(*e, spawns[i])).fold(f32::INFINITY, f32::min);
    // a spawn at least FAR_DIST from every enemy beats a closer one, so a join/respawn never lands mid-fight
    let far: Vec<usize> = pool.iter().copied().filter(|&i| nearest(i) >= FAR_DIST).collect();
    if !far.is_empty() {
        pool = far;
    }
    let best = pool.iter().map(|&i| nearest(i)).fold(0.0f32, f32::max);
    // every spawn nearly as good as the best is a fair candidate; rotating through them (instead of always taking
    // the single farthest) stops the same corner being used for every respawn when enemies camp
    let mut candidates: Vec<usize> = pool.iter().copied().filter(|&i| nearest(i) >= best * NEAR_BEST).collect();
    if candidates.len() < MIN_POOL {
        // camping enemies leave only one or two spawns inside the window, which would repeat back to back;
        // widen with the next-best spawns as long as they are still a safe distance from every enemy
        let mut rest: Vec<usize> = pool.iter().copied().filter(|i| !candidates.contains(i)).collect();
        rest.sort_by(|&a, &b| nearest(b).total_cmp(&nearest(a)));
        candidates.extend(rest.into_iter().filter(|&i| nearest(i) >= SAFE_DIST).take(MIN_POOL - candidates.len()));
    }
    least_recent(candidates, rotation)
}

/// `history`: the last 7 picks packed 4 bits each (most recent in the low nibble, spawn index + 1, 0 = empty),
/// bit 31 = the most recent pick was a respawn (not a join).
/// Callers record a pick with `push_history`.
pub fn push_history(history: u32, picked: usize, respawn: bool) -> u32 {
    let picks = ((history << 4) | (picked.min(14) as u32 + 1)) & PICKS_MASK;
    picks | (respawn as u32) << 31
}

const PICKS_MASK: u32 = 0x0FFF_FFFF;

fn last_was_respawn(history: u32) -> bool {
    history >> 31 != 0
}

/// How many picks ago `i` was used (0 = last pick); 7 = not in the history.
fn age(history: u32, i: usize) -> u32 {
    let tag = i.min(14) as u32 + 1;
    (0..7).find(|n| (history >> (4 * n)) & 0xF == tag).unwrap_or(7)
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
/// Rotation needs at least this many candidates to avoid back-to-back repeats.
pub const MIN_POOL: usize = 3;
/// Preferred minimum distance (m) from every enemy for a spawn, when the map has one.
const FAR_DIST: f32 = 60.0;
/// Spawns padding the pool below `MIN_POOL` must be at least this far (metres) from every enemy.
pub const SAFE_DIST: f32 = 15.0;

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

#[cfg(test)]
mod team_tests {
    use std::time::Duration;

    use crate::character::Cfg;
    use crate::tick::Server;
    use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, PITCH_LEVEL};

    fn slice_duel(team_a: u8, team_b: u8) -> f32 {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        s.add_player(1, [0.0, 0.81, 0.0]);
        s.add_player(2, [0.0, 0.81, -3.0]);
        s.players[0].team = team_a;
        s.players[1].team = team_b;
        let t = s.tick;
        let it = PlayerIntent { tick: t, buttons: Buttons(Buttons::SLICE), aim_pitch: PITCH_LEVEL, ..Default::default() };
        let pkt = ClientPacket { ack_snapshot_tick: t, intents: vec![it], ..Default::default() };
        s.ingest(1, &pkt.encode().unwrap());
        for _ in 0..20 {
            s.frame(Duration::from_nanos(16_666_667));
        }
        s.players[1].vitals.hp
    }

    #[test]
    fn team_spawns_are_used_only_by_their_team_and_allies_do_not_count_as_enemies() {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        let (c0, c1, o0, o1) = ([-30.0, 0.81, 0.0], [-30.0, 0.81, 8.0], [30.0, 0.81, 0.0], [30.0, 0.81, 8.0]);
        s.set_team_spawns(vec![c0, c1, o0, o1], vec![0, 0, 1, 1]);
        s.add_player(1, c0);
        s.players[0].team = 0;
        s.add_player(2, o0);
        s.players[1].team = 1;
        s.add_player(3, [0.0, 0.81, 0.0]);
        s.players[2].team = 1;
        for _ in 0..6 {
            assert!([o0, o1].contains(&s.pick_spawn(Some(3))), "orange respawns at orange points");
        }
        s.players[2].team = 0;
        for _ in 0..6 {
            assert!([c0, c1].contains(&s.pick_spawn(Some(3))), "cyan respawns at cyan points");
        }
        // a one-sided level (no point for team 1) still spawns team 1 somewhere
        s.set_team_spawns(vec![c0, c1], vec![0, 0]);
        s.players[2].team = 1;
        assert!([c0, c1].contains(&s.pick_spawn(Some(3))));
    }

    #[test]
    fn teammates_cannot_slice_each_other_but_enemies_and_no_team_can() {
        let full = slice_duel(0, 0);
        assert!(slice_duel(0, 1) < full, "enemies are hurt");
        assert!(slice_duel(crate::tick::NO_TEAM, crate::tick::NO_TEAM) < full, "no team = free-for-all");
        assert_eq!(full, slice_duel(1, 1), "teammates unhurt");
    }
}

#[cfg(test)]
mod kill_tag_tests {
    use std::time::Duration;

    use crate::character::Cfg;
    use crate::tick::Server;
    use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, PITCH_LEVEL};

    fn send(s: &mut Server, id: u16, buttons: u8) {
        let t = s.tick;
        let it = PlayerIntent { tick: t, buttons: Buttons(buttons), aim_pitch: PITCH_LEVEL, ..Default::default() };
        let pkt = ClientPacket { ack_snapshot_tick: t, intents: vec![it], ..Default::default() };
        s.ingest(id, &pkt.encode().unwrap());
    }

    /// A throws toward -Z with B standing behind A: only a disc that bounced off the wall in front can reach B.
    #[test]
    fn a_disc_that_bounced_first_tags_the_kill_as_a_ricochet_and_a_straight_one_does_not() {
        let run = |wall: bool| {
            let mut s = Server::new(Cfg::default());
            s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
            s.add_player(1, [0.0, 0.81, 0.0]);
            s.add_player(2, [0.0, 0.81, 3.0]); // behind A (A faces -Z)
            s.players[0].team = 0;
            s.players[1].team = 1;
            s.players[1].vitals.hp = 1.0; // any hit kills
            if wall {
                s.add_static_box([0.0, 1.0, -4.0], [5.0, 3.0, 0.2]); // A's shot bounces straight back through A to B
            }
            send(&mut s, 1, Buttons::FIRE);
            let mut tags = Vec::new();
            let mut killed = false;
            for _ in 0..60 {
                s.frame(Duration::from_nanos(16_666_667));
                tags.extend(s.kill_tags.iter().copied());
                killed |= s.pvp_events.iter().any(|e| matches!(e, crate::pvp::PvpEvent::Kill { victim: 2, .. }));
            }
            (killed, tags)
        };
        let (killed, tags) = run(true);
        assert!(killed, "the rebounding disc killed B");
        assert_eq!(tags.len(), 1);
        assert!(tags[0].ricochet && !tags[0].wall_run, "{tags:?}");
        let (killed, tags) = run(false);
        assert!(!killed && tags.is_empty(), "control: without the wall the disc never reaches B");
    }
}

#[cfg(test)]
mod lunge_probe {
    use std::time::Duration;

    use crate::character::Cfg;
    use crate::tick::Server;
    use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, PITCH_LEVEL};

    fn lunge(b_hp: f32, b_z: f32) -> (f32, f32) {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        s.add_player(1, [0.5, 0.81, 0.5]);
        s.add_player(2, [0.5, 0.81, b_z]);
        s.players[1].vitals.hp = b_hp;
        for _ in 0..30 {
            s.frame(Duration::from_nanos(16_666_667));
        }
        let t = s.tick;
        let it = PlayerIntent { tick: t, buttons: Buttons(Buttons::SLICE), aim_pitch: PITCH_LEVEL, ..Default::default() };
        let pkt = ClientPacket { ack_snapshot_tick: t, intents: vec![it], ..Default::default() };
        s.ingest(1, &pkt.encode().unwrap());
        for _ in 0..40 {
            s.frame(Duration::from_nanos(16_666_667));
        }
        (s.players[0].pos(&s.world)[2], s.players[1].pos(&s.world)[2])
    }

    #[test]
    fn probe() {
        println!("no one ahead:       A ends z = {:?}", lunge(100.0, 50.0));
        println!("B alive (hp 1000):  A, B end z = {:?}", lunge(1000.0, -3.5));
        println!("B dies to the cut:  A, B end z = {:?}", lunge(30.0, -3.5));
    }
}
