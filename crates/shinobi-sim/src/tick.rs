//! 60 Hz fixed-step accumulator + the authoritative server core.
//!
//! Wall-clock time only enters through `Server::frame(elapsed)`; every Rapier call happens inside `Server::step`,
//! which runs exactly `TICK_DT` of sim per call regardless of render/poll rate. The clock is injected so tests
//! (and replays) can drive it deterministically.

use std::time::Duration;

use rapier3d::prelude::*;
use shinobi_proto::intent::{Buttons, ClientPacket, IntentQueue, PlayerIntent};
use shinobi_proto::quant::{dq_pitch, dq_yaw, q_yaw};
use shinobi_proto::{TICK_DT, TICK_HZ};

use crate::anticheat::{AntiCheatCfg, Guard, Verdict};
use crate::character::{Cfg, CharacterSystem, PhysicsWorld, PlayerInput};
use crate::combat::{Aim, CombatEvent, CombatInput, CombatSystem, GROUP_PLAYER};
use crate::lagcomp::PoseHistory;
use crate::balance::Balance;
use crate::hazards::Hazards;
use crate::match_state::{MatchEvent, MatchState};
use crate::pickups::{PickupEvent, Pickups};
use crate::pvp::{self, PvpEvent, Vitals};

const NANOS: u128 = 1_000_000_000;

/// Hardware-rate-independent step counter. Integer math: `acc` is in nanoseconds * hz, one step costs 1e9,
/// so 60 Hz never drifts (1/60 s is not representable in integer nanoseconds).
#[derive(Clone, Debug)]
pub struct Accumulator {
    hz: u128,
    acc: u128,
    max_steps: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Advance {
    /// Fixed steps to run now.
    pub steps: u32,
    /// Steps of backlog thrown away by the spiral-of-death clamp (stall, debugger, thermal throttle).
    pub discarded: u32,
}

impl Accumulator {
    pub fn new(hz: u32, max_steps_per_frame: u32) -> Self {
        Self { hz: hz as u128, acc: 0, max_steps: max_steps_per_frame.max(1) }
    }

    pub fn advance(&mut self, frame: Duration) -> Advance {
        self.acc += frame.as_nanos() * self.hz;
        let due = (self.acc / NANOS) as u32;
        if due <= self.max_steps {
            self.acc -= due as u128 * NANOS;
            return Advance { steps: due, discarded: 0 };
        }
        self.acc %= NANOS; // keep only the sub-step remainder: no catch-up burst after a stall
        Advance { steps: self.max_steps, discarded: due - self.max_steps }
    }

    /// Fraction of the next step already elapsed (render interpolation; unused headless).
    pub fn alpha(&self) -> f32 {
        (self.acc % NANOS) as f32 / NANOS as f32
    }
}

pub struct Player {
    pub id: u16,
    pub ch: CharacterSystem,
    pub combat: CombatSystem,
    pub queue: IntentQueue,
    pub guard: Guard,
    pub history: PoseHistory,
    /// Server-side aim (after slew clamp). This, not the client's claim, feeds combat.
    pub yaw: u16,
    pub pitch: u16,
    /// hp / kills / deaths / respawn timers (see `pvp.rs`).
    pub vitals: Vitals,
    /// `team::TEAM_CYAN` / `TEAM_ORANGE`. Set by `add_player` (auto-balanced); rules (friendly fire, spawns) read it.
    pub team: u8,
}

impl Player {
    pub fn pos(&self, w: &PhysicsWorld) -> [f32; 3] {
        let t = w.bodies[self.ch.body].translation();
        [t.x, t.y, t.z]
    }
}

#[derive(Default, Debug)]
pub struct FrameReport {
    pub ticks: u32,
    pub discarded: u32,
    pub kicked: Vec<u16>,
}

pub struct Server {
    /// Next tick to simulate.
    pub tick: u32,
    pub world: PhysicsWorld,
    pub players: Vec<Player>,
    /// Combat events of the most recent `frame` (cleared at the start of each frame).
    pub events: Vec<(u16, CombatEvent)>,
    /// Hit / Kill / Respawn of the most recent `frame`, in tick order.
    pub pvp_events: Vec<PvpEvent>,
    /// Obstacle / light-wall mirror used for contact damage; the replicator keeps it in sync (see `hazards.rs`).
    pub hazards: Hazards,
    /// Shared power-up cubes (fixed points, respawn timers).
    pub pickups: Pickups,
    /// Every combat / enemy / match tunable (see `balance.rs`). Change with `set_balance`.
    pub balance: Balance,
    /// Players below this y die and respawn (`pvp::void_hits`).
    pub kill_plane_y: f32,
    pub separation: crate::separation::SepCfg,
    /// Warmup / Live / Intermission flow (see `match_state.rs`).
    pub match_state: MatchState,
    /// Countdown / MatchStart / MatchEnd / NextMap of the most recent `frame`.
    pub match_events: Vec<MatchEvent>,
    /// Spawn points (body-centre positions). Empty = every player respawns at its own `vitals.spawn`.
    pub spawns: Vec<[f32; 3]>,
    spawn_counter: u32,
    /// Player-built blocks (see `blocks.rs`).
    pub blocks: crate::blocks::Blocks,
    /// Placed / Removed / Refused of the most recent `frame` (cleared each frame).
    pub block_events: Vec<crate::blocks::BlockEvent>,
    /// Spawned / Taken of the most recent `frame` (cleared each frame).
    pub pickup_events: Vec<PickupEvent>,
    /// Movement-only broad phase (solid colliders), rebuilt once per tick before the player loop.
    solid_queries: QueryPipeline,
    cfg: Cfg,
    ac: AntiCheatCfg,
    acc: Accumulator,
    now: f32,
}

impl Server {
    /// Every in-flight shuriken as `(owner id, disc)`, players in `players` order.
    pub fn projectiles(&self) -> Vec<(u16, crate::combat::shuriken::Disc)> {
        self.players.iter().flat_map(|p| p.combat.loadout.discs().into_iter().map(move |d| (p.id, d))).collect()
    }

    pub fn new(cfg: Cfg) -> Self {
        Self {
            tick: 0,
            world: PhysicsWorld {
                bodies: RigidBodySet::new(),
                colliders: ColliderSet::new(),
                islands: IslandManager::new(),
                queries: QueryPipeline::new(),
            },
            players: Vec::new(),
            events: Vec::new(),
            pvp_events: Vec::new(),
            hazards: Hazards::default(),
            pickups: Pickups::default(),
            balance: Balance::default(),
            kill_plane_y: pvp::KILL_PLANE_Y,
            separation: Default::default(),
            match_state: MatchState::default(),
            match_events: Vec::new(),
            spawns: Vec::new(),
            spawn_counter: 0,
            blocks: crate::blocks::Blocks::default(),
            block_events: Vec::new(),
            pickup_events: Vec::new(),
            solid_queries: QueryPipeline::new(),
            ac: AntiCheatCfg::standard(&cfg),
            cfg,
            acc: Accumulator::new(TICK_HZ, 5),
            now: 0.0,
        }
    }

    pub fn cfg(&self) -> &Cfg {
        &self.cfg
    }

    /// Static level geometry: primitive cuboids only (never render meshes).
    pub fn add_static_box(&mut self, center: [f32; 3], half: [f32; 3]) {
        self.world.colliders.insert(
            ColliderBuilder::cuboid(half[0], half[1], half[2])
                .translation(vector![center[0], center[1], center[2]])
                .collision_groups(InteractionGroups::new(crate::combat::GROUP_WORLD, Group::ALL))
                .build(),
        );
    }

    pub fn add_player(&mut self, id: u16, spawn: [f32; 3]) {
        let cfg = self.cfg;
        let body = self.world.bodies.insert(
            RigidBodyBuilder::kinematic_position_based().translation(vector![spawn[0], spawn[1], spawn[2]]).build(),
        );
        // capsule hull: the only player collider the server knows about
        self.world.colliders.insert_with_parent(
            ColliderBuilder::capsule_y(cfg.half_height, cfg.radius)
                .collision_groups(InteractionGroups::new(GROUP_PLAYER, Group::ALL))
                .build(),
            body,
            &mut self.world.bodies,
        );
        let vitals = pvp::new_vitals(&mut self.world, &cfg, id, spawn, self.balance.player.max_hp);
        let team = self.balanced_team();
        self.players.push(Player {
            id,
            ch: CharacterSystem::new(body, cfg),
            combat: {
                let mut c = CombatSystem::ninja(&cfg);
                c.apply_balance(&self.balance, &cfg);
                c
            },
            queue: IntentQueue::default(),
            guard: Guard::default(),
            history: PoseHistory::default(),
            yaw: 0,
            pitch: 0,
            vitals,
            team,
        });
    }

    /// Load tunables: updates existing players (max hp, weapons, ultimate) and the server-side systems; players
    /// added later pick the balance up at creation. Rejects an invalid balance and changes nothing.
    pub fn set_balance(&mut self, b: Balance) -> Result<(), Vec<String>> {
        let errs = b.validate();
        if !errs.is_empty() {
            return Err(errs);
        }
        for p in &mut self.players {
            p.vitals.max_hp = b.player.max_hp;
            p.vitals.hp = p.vitals.hp.min(b.player.max_hp);
            p.combat.apply_balance(&b, &self.cfg);
        }
        self.hazards.cfg.obstacle_damage = b.hazards.obstacle_damage;
        self.hazards.cfg.wall_damage = b.hazards.wall_damage;
        self.hazards.cfg.rearm_ticks = b.hazards.rearm_ticks;
        self.hazards.cfg.owner_immune = b.hazards.owner_immune;
        self.hazards.cfg.walls_enabled = b.hazards.walls_enabled;
        self.hazards.cfg.obstacles_enabled = b.hazards.obstacles_enabled;
        self.match_state.cfg = b.match_cfg();
        self.balance = b;
        Ok(())
    }

    pub fn set_spawns(&mut self, spawns: Vec<[f32; 3]>) {
        self.spawns = spawns;
    }

    /// Where a player should appear (join or respawn): farthest from living enemies, unseen if possible.
    /// `for_player`: the id picking (excluded from "enemies"); `None` for a joiner with no `Player` yet.
    /// With no enemies, successive calls rotate through the list. Empty list: level origin at standing height.
    /// Refreshes the query structure itself, so it is safe to call between frames.
    pub fn pick_spawn(&mut self, for_player: Option<u16>) -> [f32; 3] {
        self.world.queries.update(&self.world.colliders);
        let enemies: Vec<[f32; 3]> = self
            .players
            .iter()
            .filter(|p| p.vitals.alive() && Some(p.id) != for_player)
            .map(|p| p.pos(&self.world))
            .collect();
        let k = crate::spawn::choose(&self.world, &self.spawns, &enemies, self.spawn_counter);
        if let Some(k) = k {
            self.spawn_counter = crate::spawn::push_history(self.spawn_counter, k, for_player.is_some());
        }
        k.map_or([0.0, self.cfg.half_height + self.cfg.radius, 0.0], |k| self.spawns[k])
    }

    /// Fixed power-up cube: first spawns at tick 0, `respawn_secs` after each pickup.
    pub fn add_pickup(&mut self, id: u32, pos: [f32; 3], kind: crate::combat::powerup::PowerupKind, respawn_secs: f32) {
        self.pickups.add_point(crate::pickups::PickupPoint {
            id,
            pos,
            kind,
            respawn_ticks: (respawn_secs * TICK_HZ as f32).round() as u32,
            charge_secs: 20.0,
        });
    }

    /// Remove a player and everything it owns in the physics world: rigid body, solid capsule, hitbox sensor
    /// and trail-segment sensors (no ghost capsule is left behind). Light-walls already spawned stay: they
    /// belong to the wall store (`hazards.walls` / the replicator) and expire on their own timer, like a
    /// disconnect in Tron leaving the trail behind. Returns false if the id is unknown.
    pub fn remove_player(&mut self, id: u16) -> bool {
        let Some(i) = self.players.iter().position(|p| p.id == id) else { return false };
        let mut p = self.players.remove(i);
        let w = &mut self.world;
        w.colliders.remove(p.vitals.hitbox, &mut w.islands, &mut w.bodies, false);
        w.bodies.remove(
            p.ch.body,
            &mut w.islands,
            &mut w.colliders,
            &mut ImpulseJointSet::new(),
            &mut MultibodyJointSet::new(),
            true, // the capsule is attached to the body
        );
        true
    }

    /// Raw datagram in. Malformed packets are dropped (and cost a strike); nothing here trusts a position.
    pub fn ingest(&mut self, id: u16, bytes: &[u8]) -> Verdict {
        let tick = self.tick;
        let ac = self.ac;
        let Some(p) = self.players.iter_mut().find(|p| p.id == id) else { return Verdict::Ok };
        match ClientPacket::decode(bytes) {
            Ok(pkt) => {
                let rejected = p.queue.push_packet(&pkt, tick);
                p.guard.strike_n(rejected, &ac)
            }
            Err(_) => p.guard.strike_n(1, &ac),
        }
    }

    /// Advance by wall-clock `elapsed` (time since the previous call). Runs 0..=5 fixed steps.
    pub fn frame(&mut self, elapsed: Duration) -> FrameReport {
        self.events.clear();
        self.pvp_events.clear();
        self.pickup_events.clear();
        self.block_events.clear();
        self.match_events.clear();
        let adv = self.acc.advance(elapsed);
        let mut report = FrameReport { ticks: adv.steps, discarded: adv.discarded, kicked: Vec::new() };
        for _ in 0..adv.steps {
            self.step(&mut report.kicked);
        }
        report.kicked.sort_unstable();
        report.kicked.dedup();
        for id in report.kicked.clone() {
            self.remove_player(id);
        }
        report
    }

    /// One 1/60 s tick. The only place Rapier state is mutated.
    fn step(&mut self, kicked: &mut Vec<u16>) {
        let (tick, dt, now, ac) = (self.tick, TICK_DT, self.now, self.ac);
        self.world.queries.update(&self.world.colliders);
        self.match_state.begin_tick(
            tick,
            &mut self.players,
            &mut self.world,
            &self.spawns,
            &mut self.spawn_counter,
            &mut self.pickups,
            self.balance.player.invuln_ticks,
            &mut self.match_events,
        );
        let frozen = self.match_state.frozen();
        pvp::tick_respawns(&mut self.players, &mut self.world, &self.spawns, &mut self.spawn_counter, tick, self.balance.player.invuln_ticks, &mut self.pvp_events);
        let mut hits = Vec::new();
        // one refresh for the whole player loop, after match/respawn/pickup changes to the collider set
        self.world.queries.update(&self.world.colliders);
        crate::movement::refresh_solid(&mut self.solid_queries, &self.world.colliders);

        for i in 0..self.players.len() {
            let intent = self.players[i].queue.take_for_tick(tick);
            let before = self.players[i].pos(&self.world);

            // aim: clamp claimed slew, then derive everything from the server copy
            if let Some(it) = &intent {
                let p = &mut self.players[i];
                let (yaw, v) = p.guard.limit_yaw(dq_yaw(p.yaw), dq_yaw(it.aim_yaw), &ac);
                p.yaw = q_yaw(yaw);
                p.pitch = it.aim_pitch;
                if v == Verdict::Kick {
                    kicked.push(p.id);
                    continue;
                }
            }
            // dead players neither move nor fight (their intents are still consumed, so nothing queues up)
            if frozen || !self.players[i].vitals.alive() {
                continue;
            }
            let it = intent.unwrap_or_default();
            let (aim_yaw, aim_pitch) = (self.players[i].yaw, self.players[i].pitch);
            let mut input = to_player_input(&PlayerIntent { aim_yaw, ..it });
            // shots and slashes start at the EYE (`CharacterSystem::eye_above`, the height the client camera uses), so the
            // crosshair and the projectile agree; hitboxes stay centred on the capsule
            let origin = vector![before[0], before[1] + self.players[i].ch.eye_above(), before[2]];
            let aim = Aim { origin, yaw: dq_yaw(aim_yaw), pitch: dq_pitch(aim_pitch) };

            // combat runs against the world as this attacker saw it (lag compensation)
            // (skipped when nothing can query: the rebuild is the expensive part with many players)
            let acting = it.buttons.has(Buttons::FIRE) || it.buttons.has(Buttons::SLICE) || self.players[i].combat.busy();
            if acting {
                pvp::rewind_hitboxes(&mut self.world, &self.players, i, tick, &self.cfg);
            }
            let mut ev = Vec::new();
            {
                let p = &mut self.players[i];
                p.combat.update(
                    dt,
                    now,
                    &CombatInput {
                        fire: it.buttons.has(Buttons::FIRE),
                        slice: it.buttons.has(Buttons::SLICE),
                        ultimate: ultimate_pressed(&it),
                    },
                    &aim,
                    &self.world,
                    &p.ch,
                    &mut input,
                    &mut ev,
                );
                p.ch.set_slice_cooldown(p.combat.slice_cooldown_left()); // travels in OwnState (lunge prediction)
            }
            let mut synthesized = Vec::new();
            let head = pvp::HeadCfg { min_height: self.balance.combat.head_min_height, mult: self.balance.combat.headshot_mult };
            hits.extend(pvp::player_hits(&self.world, &self.players, i, &ev, tick, &self.cfg, head, &mut synthesized));
            ev.extend(synthesized);
            let id = self.players[i].id;
            self.events.extend(ev.into_iter().map(|e| (id, e)));

            if it.build {
                // one-tick edge: place a block at the aimed spot (server-side range / cooldown / overlap checks)
                let bodies: Vec<[f32; 3]> = self.players.iter().filter(|q| q.vitals.alive()).map(|q| q.pos(&self.world)).collect();
                let f = aim.forward();
                let skip = self.players[i].ch.body;
                self.blocks.try_build(&mut self.world, tick, id, [origin.x, origin.y, origin.z], [f.x, f.y, f.z], skip, &bodies, &mut self.block_events);
                self.world.queries.update(&self.world.colliders);
            }

            let p = &mut self.players[i];
            crate::movement::step_player_with(&mut p.ch, &input, &mut self.world, now, dt, &mut self.solid_queries);
            match p.ch.grapple_event {
                crate::character::GrappleEvent::Attached { origin, anchor } => {
                    self.events.push((id, CombatEvent::GrappleAttach { origin: Point::from(origin), anchor: Point::from(anchor) }));
                }
                crate::character::GrappleEvent::Released => {
                    let c = p.pos(&self.world);
                    self.events.push((id, CombatEvent::GrappleRelease { pos: Point::new(c[0], c[1], c[2]) }));
                }
                crate::character::GrappleEvent::None => {}
            }

            // delta ceiling: did the body move further than any legal mover can?
            let after = p.pos(&self.world);
            match p.guard.check_step(before, after, 0.0, &ac) {
                Verdict::Ok => {}
                Verdict::SnapBack { to } => {
                    let to = vector![to[0], to[1], to[2]];
                    self.world.bodies[p.ch.body].set_position(Isometry::translation(to.x, to.y, to.z), true);
                    crate::character::sync_body_colliders(&mut self.world, p.ch.body);
                    p.ch.vel = Vector::zeros();
                }
                Verdict::Kick => kicked.push(p.id),
            }
        }

        // soft push-out: server-authoritative correction, after anti-cheat so it never counts as movement
        crate::separation::separate(&self.players, &mut self.world, &self.cfg, &self.separation);

        // environment: obstacles and light-walls (positions after this tick's movement)
        self.world.queries.update(&self.world.colliders);
        if !frozen {
            hits.extend(pvp::void_hits(&self.players, &self.world, self.kill_plane_y));
            hits.extend(self.hazards.hits(&self.world, &self.players, tick, &self.cfg, &mut self.pvp_events));
        }

        // every attacker has run: settle damage once, then record the tick
        pvp::apply_hits(&mut self.players, &mut self.world, &hits, tick, self.balance.player.respawn_ticks, &mut self.pvp_events);
        self.match_state.end_tick(tick, &self.players, &mut self.match_events);
        if !frozen {
            self.pickups.update(tick, &self.world, &mut self.players, &self.cfg, &mut self.pickup_events);
        }
        pvp::finish_tick(&mut self.players, &mut self.world, tick);
        self.tick = tick.wrapping_add(1);
        self.now += dt;
    }
}

/// Ultimate edge from the wire (`PlayerIntent.ultimate`; cleared on lost-packet repeats, so it fires once).
pub fn ultimate_pressed(it: &PlayerIntent) -> bool {
    it.ultimate
}

/// View direction from the quantised aim (libm, so client prediction equals the server).
pub fn view_dir(aim_yaw: u16, aim_pitch: u16) -> Vector<Real> {
    let (yaw, pitch) = (dq_yaw(aim_yaw), dq_pitch(aim_pitch));
    let (sy, cy) = (libm::sinf(yaw), libm::cosf(yaw));
    let (sp, cp) = (libm::sinf(pitch), libm::cosf(pitch));
    Vector::new(-sy * cp, sp, -cy * cp)
}

/// Wire intent -> sim input. View-frame stick is rotated by the *server's* yaw here.
pub fn to_player_input(it: &PlayerIntent) -> PlayerInput {
    let (x, z) = it.world_move();
    PlayerInput {
        move_dir: Vector::new(x, 0.0, z),
        jump_pressed: it.buttons.has(Buttons::JUMP_PRESSED),
        jump_held: it.buttons.has(Buttons::JUMP_HELD),
        dash_pressed: it.buttons.has(Buttons::DASH),
        roll_pressed: it.buttons.has(Buttons::ROLL),
        wall_cling: it.buttons.has(Buttons::WALL_CLING),
        slide: it.buttons.has(Buttons::SLIDE),
        grapple_aim: if it.zip { Some(view_dir(it.aim_yaw, it.aim_pitch)) } else { None },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(hz_frames: u32, secs: u32) -> u32 {
        let mut a = Accumulator::new(60, 5);
        let f = Duration::from_nanos(1_000_000_000 / hz_frames as u64);
        (0..hz_frames * secs).map(|_| a.advance(f).steps).sum()
    }

    #[test]
    fn step_rate_is_independent_of_frame_rate() {
        for fps in [20, 30, 60, 144, 240] {
            let steps = run(fps, 10);
            assert!((steps as i64 - 600).abs() <= 1, "{fps} fps -> {steps} steps in 10 s");
        }
    }

    #[test]
    fn stall_is_clamped_not_replayed() {
        let mut a = Accumulator::new(60, 5);
        let adv = a.advance(Duration::from_secs(5));
        assert_eq!(adv, Advance { steps: 5, discarded: 295 });
        assert_eq!(a.advance(Duration::ZERO).steps, 0, "no catch-up burst afterwards");
    }

    fn flat_server() -> Server {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        s.add_player(1, [0.0, 0.81, 0.0]);
        s
    }

    fn pkt(tick: u32, it: PlayerIntent) -> Vec<u8> {
        ClientPacket { intents: vec![PlayerIntent { tick, ..it }], ..Default::default() }.encode().unwrap()
    }

    /// A runs +x for 60 ticks, stops; B then walks -Z straight through A's resting spot. Returns (B's final z relative to A's z, colliders/bodies before add, after A's run, at the end).
    fn walk_through(remove_a: bool) -> (f32, (usize, usize), (usize, usize), (usize, usize)) {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        let base = (s.world.bodies.len(), s.world.colliders.len());
        s.add_player(1, [0.0, 0.81, 0.0]);
        let tick = |s: &mut Server, id: u16, it: PlayerIntent| {
            let t = s.tick;
            s.ingest(id, &pkt(t, PlayerIntent { aim_pitch: shinobi_proto::intent::PITCH_LEVEL, ..it }));
        };
        for _ in 0..60 {
            tick(&mut s, 1, PlayerIntent { move_z: 0, move_x: 127, ..Default::default() });
            s.frame(Duration::from_nanos(16_666_667));
        }
        let ax = s.players[0].pos(&s.world);
        let mid = (s.world.bodies.len(), s.world.colliders.len());
        if remove_a {
            assert!(s.remove_player(1));
            assert!(!s.remove_player(1), "second removal is a no-op");
        }
        let after = (s.world.bodies.len(), s.world.colliders.len());
        s.add_player(2, [ax[0], 0.81, ax[2] + 4.0]);
        for _ in 0..90 {
            tick(&mut s, 2, PlayerIntent { move_z: 127, ..Default::default() }); // yaw 0 = walk toward -Z
            s.frame(Duration::from_nanos(16_666_667));
        }
        let b = s.players.iter().find(|p| p.id == 2).unwrap().pos(&s.world);
        (b[2] - ax[2], base, mid, (after.0, after.1))
    }

    #[test]
    fn removed_player_leaves_no_bodies_colliders_or_ghost_capsule() {
        let (z_rel, base, mid, after) = walk_through(true);
        assert!(mid.0 > base.0 && mid.1 > base.1, "A's body and colliders are in the world: {base:?} -> {mid:?}");
        assert_eq!(after, base, "bodies/colliders return to the bare level");
        assert!(z_rel < -2.0, "B walked through A's old spot (z rel {z_rel})");
        // control: with A left standing, its capsule is still in the world (counts stay up)
        let (_, _, _, kept) = walk_through(false);
        assert!(kept.0 > base.0 && kept.1 > base.1);
    }

    #[test]
    fn server_ticks_and_moves_from_intents_only() {
        let mut s = flat_server();
        let fwd = PlayerIntent { move_z: 127, ..Default::default() };
        for t in 0..60u32 {
            s.ingest(1, &pkt(t, fwd));
            assert_eq!(s.frame(Duration::from_nanos(16_666_667)).ticks, 1);
        }
        let z = s.players[0].pos(&s.world)[2];
        assert!(z < -5.0, "1 s of forward stick at 9 m/s moved z to {z}");
        assert_eq!(s.tick, 60);
    }

    #[test]
    fn future_tick_flood_is_rejected_and_kicks() {
        let mut s = flat_server();
        let mut last = Verdict::Ok;
        for i in 0..20 {
            last = s.ingest(1, &pkt(1_000_000 + i, PlayerIntent::default()));
        }
        assert_eq!(last, Verdict::Kick);
    }

    #[test]
    fn displacement_over_ceiling_snaps_back_and_strikes() {
        let mut s = flat_server();
        s.ac.max_step = 0.001; // absurdly tight ceiling: any real movement is a breach
        s.ingest(1, &pkt(0, PlayerIntent { move_z: 127, ..Default::default() }));
        s.frame(Duration::from_nanos(16_666_667));
        let p = &s.players[0];
        assert_eq!(p.guard.strikes, 1);
        assert!(p.pos(&s.world)[2].abs() < 1e-3, "snapped back to spawn, at {:?}", p.pos(&s.world));
        assert_eq!(p.ch.vel, Vector::zeros());
    }
}
