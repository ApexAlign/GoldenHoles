//! Contact damage from the environment: moving obstacles and Tron light-walls.
//!
//! Inputs are deterministic integer state the server already owns (`ObstacleDesc::pos_at(tick)`,
//! `WallSpawn { owner, spawn_tick }`), so the result is identical on any machine that replays the ticks.
//! Output feeds `pvp::apply_hits`, so hazards obey the same rules as weapons: dead / spawn-protected /
//! rolling players are immune, and a victim dies once per tick.
//!
//! Wiring: `Server.hazards` is a mirror. Whoever replicates spawns (the `Replicator`) must also call
//! `hazards.walls.apply_spawn / apply_kill` and `hazards.obstacles.apply_spawn / apply_despawn`, and set
//! `hazards.cfg` (`life_ticks`, `grace_ticks`, `wall_height`, `wall_thick`) to match its `WallsWorld`.

use std::collections::HashMap;

use rapier3d::parry::query::intersection_test;
use rapier3d::parry::shape::{Capsule, Cuboid};
use rapier3d::prelude::*;
use shinobi_proto::quant::dq_pos3;

use crate::character::{Cfg, PhysicsWorld, GROUP_ENEMY, GROUP_TRAIL};
use crate::obstacle::ObstacleStore;
use crate::pvp::{Cause, HazardSource, PvpEvent, PvpHit, NO_ATTACKER};
use crate::tick::Player;
use crate::walls::{WallGeometry, WallStore};

#[derive(Clone, Copy, Debug)]
pub struct HazardCfg {
    /// Damage per contact. 100 = lethal at full health.
    pub obstacle_damage: f32,
    pub wall_damage: f32,
    /// Ticks before the same victim can be hurt again by the same source (a wave grazing for several
    /// ticks must not stack into an instant kill unless that is intended).
    pub rearm_ticks: u32,
    /// Must match the replicator's `WallsWorld`.
    pub life_ticks: u32,
    /// Owner is exempt from a wall for this long after it spawns (matches `WallsWorld::grace_ticks`).
    pub grace_ticks: u32,
    /// The owner is never hurt by their own trail (walls still block them). Off by default; the demo turns it on.
    pub owner_immune: bool,
    /// Light-wall contact damage. OFF by default: the game is a shooter, Tron is a visual style (trail walls are
    /// being removed). Kept so the old behaviour can be switched back on, and for its tests.
    pub walls_enabled: bool,
    /// Moving-obstacle contact damage. OFF by default (no level places obstacles in the shooter demo).
    pub obstacles_enabled: bool,
    pub wall_height: f32,
    pub wall_thick: f32,
    /// Walls are solid, so the character stops *touching* them; this slack makes touching count as contact.
    pub contact_margin: f32,
}

impl Default for HazardCfg {
    fn default() -> Self {
        Self {
            obstacle_damage: 100.0,
            wall_damage: 100.0,
            rearm_ticks: 30,
            life_ticks: 240,
            grace_ticks: 15,
            owner_immune: false,
            walls_enabled: false,
            obstacles_enabled: false,
            wall_height: 1.6,
            wall_thick: 0.15,
            contact_margin: 0.08,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Victim {
    Player(u16),
    Enemy(u128),
}

#[derive(Default)]
pub struct Hazards {
    pub cfg: HazardCfg,
    pub obstacles: ObstacleStore,
    pub walls: WallStore,
    rearm: HashMap<(Victim, HazardSource), u32>,
    /// ENEMY-group hits of the last `hits()` call: (collider `user_data`, damage, source). The enemy store
    /// applies them (`enemies.rs`).
    pub enemy_hits: Vec<(u128, f32, HazardSource)>,
}

impl Hazards {
    /// Every hazard hit for `tick`, sorted by (victim, source) so the result never depends on map order.
    /// Hits on ENEMY-group colliders are reported through `events` (there is no enemy health in the sim yet).
    pub fn hits(
        &mut self,
        world: &PhysicsWorld,
        players: &[Player],
        tick: u32,
        ch: &Cfg,
        events: &mut Vec<PvpEvent>,
    ) -> Vec<PvpHit> {
        let c = self.cfg;
        self.enemy_hits.clear();
        self.rearm.retain(|_, t| tick < t.saturating_add(c.rearm_ticks));
        if !c.walls_enabled && !c.obstacles_enabled {
            return Vec::new(); // nothing to evaluate: the shooter demo has no environmental damage
        }

        let capsule = Capsule::new_y(ch.half_height, ch.radius);
        let bodies: Vec<(usize, Isometry<Real>)> = players
            .iter()
            .enumerate()
            .filter(|(_, p)| p.vitals.alive())
            .map(|(i, p)| (i, *world.bodies[p.ch.body].position()))
            .collect();

        let mut out: Vec<(usize, HazardSource, PvpHit)> = Vec::new();

        // ---- moving obstacles: exact fixed-point position at this tick ----
        let mut obstacles: Vec<_> = if c.obstacles_enabled { self.obstacles.active_at(tick).copied().collect() } else { Vec::new() };
        obstacles.sort_by_key(|o| o.id);
        for o in obstacles {
            let p = dq_pos3(o.pos_at(tick));
            let half = vector![o.half[0] as f32, o.half[1] as f32, o.half[2] as f32] / 64.0;
            let (shape, pos) = (Cuboid::new(half), Isometry::translation(p[0], p[1], p[2]));
            let src = HazardSource::Obstacle(o.id);
            for &(i, ref bpos) in &bodies {
                if touching(&pos, &shape, bpos, &capsule) {
                    out.push((i, src, PvpHit { attacker: NO_ATTACKER, victim: i, amount: c.obstacle_damage, cause: Cause::Hazard(src) }));
                }
            }
        }

        // ---- light-walls: other players always; the owner only once the grace window has passed ----
        let mut live: Vec<_> = if c.walls_enabled { self.walls.live_at(tick, c.life_ticks).map(|w| w.spawn).collect() } else { Vec::new() };
        live.sort_by_key(|w| w.id);
        for w in live {
            let g = WallGeometry::from_endpoints(dq_pos3(w.a), dq_pos3(w.b));
            let col = g.collider(c.wall_height, c.wall_thick);
            let m = c.contact_margin;
            let shape = match col.shape().as_cuboid() {
                Some(cu) => Cuboid::new(cu.half_extents + vector![m, m, m]),
                None => continue,
            };
            let pos = *col.position();
            let src = HazardSource::Wall { id: w.id, owner: w.owner };
            let in_grace = tick < w.spawn_tick.saturating_add(c.grace_ticks);
            for &(i, ref bpos) in &bodies {
                let is_owner = players[i].id as u8 == w.owner;
                if (is_owner && (in_grace || c.owner_immune)) || !touching(&pos, &shape, bpos, &capsule) {
                    continue;
                }
                let attacker = players.iter().find(|p| p.id as u8 == w.owner).map_or(NO_ATTACKER, |p| p.id);
                out.push((i, src, PvpHit { attacker, victim: i, amount: c.wall_damage, cause: Cause::Hazard(src) }));
            }
            // non-player targets
            let filter = QueryFilter::default().groups(InteractionGroups::new(GROUP_TRAIL, GROUP_ENEMY));
            let mut enemies = Vec::new();
            world.queries.intersections_with_shape(&world.bodies, &world.colliders, &pos, &shape, filter, |h| {
                enemies.push(h);
                true
            });
            enemies.sort_by_key(|h| h.into_raw_parts());
            for h in enemies {
                let Some(id) = world.colliders.get(h).map(|x| x.user_data) else { continue };
                if self.armed(Victim::Enemy(id), src, tick) {
                    events.push(PvpEvent::EnemyHazardHit { tick, enemy: id, amount: c.wall_damage, source: src });
                    self.enemy_hits.push((id, c.wall_damage, src));
                }
            }
        }

        out.sort_by_key(|(i, src, _)| (*i, hash_key(*src)));
        out.into_iter()
            .filter(|(i, src, _)| self.armed(Victim::Player(players[*i].id), *src, tick))
            .map(|(_, _, h)| h)
            .collect()
    }

    /// Cooldown gate: true (and re-arms the timer) when this victim may be hurt by this source again.
    fn armed(&mut self, v: Victim, s: HazardSource, tick: u32) -> bool {
        match self.rearm.get(&(v, s)) {
            Some(&t) if tick < t.saturating_add(self.cfg.rearm_ticks) => false,
            _ => {
                self.rearm.insert((v, s), tick);
                true
            }
        }
    }
}

fn hash_key(s: HazardSource) -> (u8, u32) {
    match s {
        HazardSource::Obstacle(id) => (0, id as u32),
        HazardSource::Wall { id, .. } => (1, id),
    }
}

fn touching(pos1: &Isometry<Real>, s1: &dyn Shape, pos2: &Isometry<Real>, s2: &dyn Shape) -> bool {
    intersection_test(pos1, s1, pos2, s2).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shinobi_proto::intent::{ClientPacket, PlayerIntent};
    use shinobi_proto::quant::q_pos3 as q;

    use super::*;
    use crate::character::GROUP_ENEMY;
    use crate::obstacle::{MotionPath, ObstacleDesc};
    use crate::pvp::{PvpEvent, MAX_HP};
    use crate::tick::Server;
    use crate::walls::WallSpawn;

    const A: u16 = 1;
    const B: u16 = 2;
    const LEVEL: u16 = 2048;

    struct Sim {
        s: Server,
        ev: Vec<PvpEvent>,
    }

    impl Sim {
        fn new(players: &[(u16, [f32; 3])]) -> Self {
            let mut s = Server::new(Cfg::default());
            s.hazards.cfg.walls_enabled = true; // hazards are off by default (shooter); these tests exercise them
            s.hazards.cfg.obstacles_enabled = true;
            s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
            for (id, at) in players {
                s.add_player(*id, *at);
            }
            Self { s, ev: Vec::new() }
        }

        fn tick(&mut self) {
            let t = self.s.tick;
            for id in [A, B] {
                let it = PlayerIntent { tick: t, aim_pitch: LEVEL, ..Default::default() };
                let pkt = ClientPacket { ack_snapshot_tick: t, intents: vec![it], ..Default::default() };
                self.s.ingest(id, &pkt.encode().unwrap());
            }
            self.s.frame(Duration::from_nanos(16_666_667));
            self.ev.extend(self.s.pvp_events.iter().copied());
        }

        fn run(&mut self, n: u32) {
            for _ in 0..n {
                self.tick();
            }
        }

        fn p(&self, id: u16) -> &Player {
            self.s.players.iter().find(|p| p.id == id).unwrap()
        }
    }

    fn wall(id: u32, owner: u8, spawn_tick: u32, a: [f32; 3], b: [f32; 3]) -> WallSpawn {
        WallSpawn { id, owner, spawn_tick, a: q(a), b: q(b) }
    }

    fn sweeper(id: u16, from: [f32; 3], to: [f32; 3], period: u16) -> ObstacleDesc {
        ObstacleDesc {
            id,
            kind: 0,
            start_tick: 0,
            end_tick: 0,
            half: [64, 64 * 2, 64], // 1 x 2 x 1 m half extents
            path: MotionPath::PingPong { a: q(from), b: q(to), period },
        }
    }

    #[test]
    fn a_sweeping_obstacle_kills_a_player_in_its_path_and_scores_nobody() {
        let mut sim = Sim::new(&[(A, [0.0, 0.81, 0.0]), (B, [0.0, 0.81, 20.0])]); // B far from the wave
        sim.s.hazards.obstacles.apply_spawn(sweeper(7, [8.0, 1.0, 0.0], [-8.0, 1.0, 0.0], 60));
        sim.run(40);
        let v = &sim.p(A).vitals;
        assert!(!v.alive(), "wave passed through A");
        assert_eq!((v.deaths, sim.p(A).vitals.kills), (1, 0));
        assert!(sim.ev.iter().any(|e| matches!(e, PvpEvent::HazardKill { victim: A, source: HazardSource::Obstacle(7), killer: None, .. })));
        assert_eq!(sim.p(B).vitals.hp, MAX_HP, "untouched player is fine");
        assert_eq!(sim.p(B).vitals.kills, 0);
    }

    #[test]
    fn heavy_contact_rearms_instead_of_stacking_every_tick() {
        let mut sim = Sim::new(&[(A, [0.0, 0.81, 0.0])]);
        sim.s.hazards.cfg.obstacle_damage = 20.0;
        sim.s.hazards.cfg.rearm_ticks = 30;
        // parked on top of A forever
        sim.s.hazards.obstacles.apply_spawn(sweeper(1, [0.0, 1.0, 0.0], [0.0, 1.0, 0.0], 60));
        sim.run(65);
        let hits = sim.ev.iter().filter(|e| matches!(e, PvpEvent::HazardHit { victim: A, .. })).count();
        assert_eq!(hits, 3, "ticks 0, 30, 60 only");
        assert_eq!(sim.p(A).vitals.hp, MAX_HP - 60.0);
    }

    #[test]
    fn light_wall_kills_a_non_owner_credits_the_owner_and_spares_the_owner_during_grace() {
        let mut sim = Sim::new(&[(A, [0.0, 0.81, 0.0]), (B, [0.0, 0.81, 6.0])]);
        // A's wall runs through A (grace) and through B; feet on the floor
        sim.s.hazards.walls.apply_spawn(wall(1, A as u8, 0, [-3.0, 0.0, 0.0], [3.0, 0.0, 0.0]));
        sim.s.hazards.walls.apply_spawn(wall(2, A as u8, 0, [-3.0, 0.0, 6.0], [3.0, 0.0, 6.0]));
        sim.run(10);
        assert_eq!(sim.p(A).vitals.hp, MAX_HP, "owner is exempt while the wall is fresh");
        assert!(!sim.p(B).vitals.alive(), "non-owner is hurt immediately");
        assert_eq!(sim.p(A).vitals.kills, 1, "owner gets the kill");
        assert!(sim.ev.iter().any(|e| matches!(e, PvpEvent::HazardKill { victim: B, killer: Some(A), source: HazardSource::Wall { owner: 1, .. }, .. })));
        // after the grace window the owner can hit their own trail
        sim.run(10);
        assert!(!sim.p(A).vitals.alive(), "grace is over: own wall is lethal");
        assert_eq!(sim.p(A).vitals.kills, 1, "no points for self-kills");
    }

    /// A is standing inside her own wall for `ticks`; returns (hits on A, A's hp).
    fn own_wall(owner_immune: bool, ticks: u32) -> (usize, f32) {
        let mut sim = Sim::new(&[(A, [0.0, 0.81, 0.0])]);
        sim.s.hazards.cfg.owner_immune = owner_immune;
        sim.s.hazards.cfg.wall_damage = 20.0;
        sim.s.hazards.walls.apply_spawn(wall(1, A as u8, 0, [-3.0, 0.0, 0.0], [3.0, 0.0, 0.0]));
        sim.run(ticks);
        let hits = sim.ev.iter().filter(|e| matches!(e, PvpEvent::HazardHit { victim: A, .. })).count();
        (hits, sim.p(A).vitals.hp)
    }

    #[test]
    fn owner_immunity_makes_your_own_trail_harmless_forever() {
        // control: default rules hurt the owner once the 15-tick grace is over (30-tick rearm: ticks 15, 45, 75)
        let (hits, hp) = own_wall(false, 100);
        assert!(hits >= 3 && hp < 100.0, "control: own trail hurts after grace ({hits} hits, hp {hp})");
        // demo rule: never, however long you stand in it
        let (hits, hp) = own_wall(true, 300);
        assert_eq!((hits, hp), (0, 100.0));
    }

    #[test]
    fn one_pass_through_a_wall_is_one_hit_and_three_touches_kill() {
        // B stands in A's wall: with the demo numbers (34 damage, 60-tick rearm) that is a hit per second
        let mut sim = Sim::new(&[(A, [10.0, 0.81, 10.0]), (B, [0.0, 0.81, 0.0])]);
        sim.s.hazards.cfg.wall_damage = 34.0;
        sim.s.hazards.cfg.rearm_ticks = 60;
        sim.s.hazards.cfg.owner_immune = true;
        sim.s.hazards.walls.apply_spawn(wall(1, A as u8, 0, [-3.0, 0.0, 0.0], [3.0, 0.0, 0.0]));
        sim.run(59);
        let hits = |sim: &Sim| sim.ev.iter().filter(|e| matches!(e, PvpEvent::HazardHit { victim: B, .. })).count();
        assert_eq!(hits(&sim), 1, "a whole second of contact is still one hit");
        assert_eq!(sim.p(B).vitals.hp, MAX_HP - 34.0);
        sim.run(62); // ticks 59..=120: second hit at tick 60, third at tick 120
        assert_eq!(hits(&sim), 3);
        assert!(!sim.p(B).vitals.alive(), "34 x 3 = 102 kills");
        assert_eq!(sim.p(A).vitals.kills, 1, "the wall's owner gets the kill");
    }

    fn enemy_hits(life_ticks: u32, ticks: u32) -> usize {
        let mut sim = Sim::new(&[(A, [10.0, 0.81, 10.0])]);
        sim.s.world.colliders.insert(
            ColliderBuilder::cuboid(0.4, 0.9, 0.4)
                .translation(vector![0.0, 0.9, 0.0])
                .collision_groups(InteractionGroups::new(GROUP_ENEMY, Group::ALL))
                .user_data(99)
                .build(),
        );
        sim.s.hazards.cfg.life_ticks = life_ticks;
        sim.s.hazards.walls.apply_spawn(wall(1, A as u8, 0, [-2.0, 0.0, 0.0], [2.0, 0.0, 0.0]));
        sim.run(ticks);
        sim.ev.iter().filter(|e| matches!(e, PvpEvent::EnemyHazardHit { enemy: 99, .. })).count()
    }

    #[test]
    fn walls_hurt_enemy_group_targets_until_they_expire() {
        // wall lives 20 ticks: one contact at tick 0, the 30-tick rearm never lets a second one in
        assert_eq!(enemy_hits(20, 100), 1);
        // control: a wall that outlives the rearm hits again at ticks 30, 60, 90
        assert_eq!(enemy_hits(1000, 100), 4);
    }
}
