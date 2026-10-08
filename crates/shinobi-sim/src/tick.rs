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
    /// `(seq, server tick)` of the newest client packet received (echoed so the client can measure its round trip).
    pub last_pkt: Option<(u16, u32)>,
    /// The client's own ping / correction report and the server tick it arrived on (telemetry only).
    pub client_report: Option<(shinobi_proto::intent::ClientReport, u32)>,
    /// Server tick of the newest intent whose aim was applied (a hitching client may skip ticks: the aim slew
    /// allowance grows with the gap).
    pub last_aim_tick: u32,
    /// Wall clash in progress: (opponent id, last tick of the strike window), and the tick before which no new clash starts.
    pub clash: Option<(u16, u32)>,
    pub clash_ready_tick: u32,
    /// Block / parry shield, guard energy, stagger and strike-window state (`block.rs`).
    pub blk: crate::block::BlockState,
    /// Katana moves: counter window, shield-lowered tick, air-cut slam (`katana_moves.rs`).
    pub kmoves: crate::katana_moves::KatanaMoveState,
    /// Melee heighteners: last attacker (ring-out), knock in progress (wall-splat), kill streak (`melee.rs`).
    pub melee: crate::melee::MeleeState,
    /// White-out from a flash star lasts until this tick: no lock-on forms and the lock breaks (`star_pass.rs`).
    pub blind_until: u32,
    /// The power weapon in hand: yumi or fuda tags (`power.rs`).
    pub power: crate::power::PowerState,
}

/// A team id that matches nobody (free-for-all pick for a joiner that has no `Player` yet, tests): never allied.
pub const NO_TEAM: u8 = u8::MAX;

/// Same real team (never true for `NO_TEAM`). Teammates never hurt each other.
pub fn allied(a: u8, b: u8) -> bool {
    a != NO_TEAM && a == b
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

/// Extra facts about a kill that `PvpEvent::Kill` does not carry (HUD medals): see `Server::kill_tags`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KillTag {
    pub tick: u32,
    pub killer: u16,
    pub victim: u16,
    /// The killing disc had bounced off geometry first.
    pub ricochet: bool,
    /// The killer was wall-running when the shot landed.
    pub wall_run: bool,
}

pub struct Server {
    /// Next tick to simulate.
    pub tick: u32,
    /// Why strikes were handed out (player, tick, check and the values), for the server's log. The host drains it.
    pub strike_log: Vec<String>,

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
    /// Team of each spawn point (parallel to `spawns`; `NO_TEAM` = shared). See `set_team_spawns`.
    pub spawn_teams: Vec<u8>,
    spawn_counter: u32,
    /// One entry per `PvpEvent::Kill` of the most recent `frame` that has a medal-worthy fact (ricochet / wall-run).
    pub kill_tags: Vec<KillTag>,
    /// Player-built blocks (see `blocks.rs`).
    pub blocks: crate::blocks::Blocks,
    /// Placed / Removed / Refused of the most recent `frame` (cleared each frame).
    pub block_events: Vec<crate::blocks::BlockEvent>,
    /// `Removed` events of blocks dropped between frames (a player left); delivered with the next frame's events.
    pending_block_events: Vec<crate::blocks::BlockEvent>,
    /// Spawned / Taken of the most recent `frame` (cleared each frame).
    pub pickup_events: Vec<PickupEvent>,
    /// Arrows in flight and fuda tags (flying, stuck, counting down), see `power_pass.rs`.
    pub arrows: Vec<crate::power_pass::Arrow>,
    pub tags: Vec<crate::power_pass::Tag>,
    /// Owners that pressed detonate this tick, and the counter that names arrows / tags / floor drops.
    pub(crate) power_launch: Vec<crate::power_pass::Launch>,
    pub(crate) power_uid: u16,
    pub(crate) drop_seq: u32,
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
            spawn_teams: Vec::new(),
            spawn_counter: 0,
            kill_tags: Vec::new(),
            strike_log: Vec::new(),
            blocks: crate::blocks::Blocks::default(),
            block_events: Vec::new(),
            pending_block_events: Vec::new(),
            pickup_events: Vec::new(),
            arrows: Vec::new(),
            tags: Vec::new(),
            power_launch: Vec::new(),
            power_uid: 0,
            drop_seq: 0,
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
        let mut vitals = pvp::new_vitals(&mut self.world, &cfg, id, spawn, self.balance.player.max_hp);
        vitals.invuln_until_tick = self.tick + self.balance.player.first_spawn_invuln_ticks;
        // free-for-all (the default): no teams, so nobody is an ally; team mode auto-balances joiners
        let team = if self.match_state.team_mode { self.balanced_team() } else { NO_TEAM };
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
            last_pkt: None,
            client_report: None,
            last_aim_tick: 0,
            clash: None,
            clash_ready_tick: 0,
            blk: Default::default(),
            kmoves: Default::default(),
            melee: Default::default(),
            blind_until: 0,
            power: Default::default(),
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
        self.blocks.cfg = b.blocks.cfg().quantized(); // clients rebuild the same floats from `BlockRules`
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
        self.spawn_teams.clear();
    }

    /// Spawn points with a team each (`NO_TEAM` = either side), e.g. from level `spawn x y z [team]` lines.
    pub fn set_team_spawns(&mut self, spawns: Vec<[f32; 3]>, teams: Vec<u8>) {
        self.spawns = spawns;
        self.spawn_teams = teams;
    }

    fn spawn_set(&self) -> crate::spawn::SpawnSet<'_> {
        crate::spawn::SpawnSet { pos: &self.spawns, teams: &self.spawn_teams }
    }

    /// Where a player should appear (join or respawn): farthest from living enemies, unseen if possible.
    /// `for_player`: the id picking (excluded from "enemies"); `None` for a joiner with no `Player` yet.
    /// With no enemies, successive calls rotate through the list. Empty list: level origin at standing height.
    /// Refreshes the query structure itself, so it is safe to call between frames.
    pub fn pick_spawn(&mut self, for_player: Option<u16>) -> [f32; 3] {
        self.world.queries.update(&self.world.colliders);
        let team = for_player.and_then(|id| self.players.iter().find(|p| p.id == id)).map_or(NO_TEAM, |p| p.team);
        let enemies: Vec<[f32; 3]> = self
            .players
            .iter()
            .filter(|p| p.vitals.alive() && Some(p.id) != for_player && !allied(team, p.team))
            .map(|p| p.pos(&self.world))
            .collect();
        let allowed = self.spawn_set().allowed(team);
        let k = crate::spawn::choose_among(&self.world, &self.spawns, &allowed, &enemies, self.spawn_counter);
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
            weapon: crate::power::PowerKind::None,
            ammo: 0,
        });
    }

    /// Fixed power-weapon pedestal (the yumi or a fuda tag, `power.rs`): up at tick 0, `respawn_secs` after each pickup.
    pub fn add_weapon_pickup(&mut self, id: u32, pos: [f32; 3], weapon: crate::power::PowerKind, respawn_secs: f32) {
        self.pickups.add_point(crate::pickups::PickupPoint {
            id,
            pos,
            kind: crate::combat::powerup::PowerupKind::GridBeam,
            respawn_ticks: (respawn_secs * TICK_HZ as f32).round() as u32,
            charge_secs: 0.0,
            weapon,
            ammo: 0,
        });
    }

    /// Remove a player and everything it owns in the physics world: rigid body, solid capsule, hitbox sensor
    /// and trail-segment sensors (no ghost capsule is left behind). Light-walls already spawned stay: they
    /// belong to the wall store (`hazards.walls` / the replicator) and expire on their own timer, like a
    /// disconnect in Tron leaving the trail behind. Returns false if the id is unknown.
    pub fn remove_player(&mut self, id: u16) -> bool {
        let Some(i) = self.players.iter().position(|p| p.id == id) else { return false };
        let mut p = self.players.remove(i);
        // their blocks go with them (each reported as Removed on the next frame)
        let tick = self.tick;
        let gone: Vec<_> = self.blocks.iter().into_iter().filter(|(o, _)| *o == id).map(|(_, c)| c).collect();
        self.blocks.clear_owner(&mut self.world, id);
        self.pending_block_events.extend(gone.into_iter().map(|cell| crate::blocks::BlockEvent::Removed { tick, cell }));
        // a leaver's frags stay on their team's score
        let team = crate::team::valid(p.team) as usize;
        self.match_state.team_banked[team] = self.match_state.team_banked[team].saturating_add(p.vitals.kills);
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
                // newest by sequence number (wrapping), so a reordered older packet does not move the echo backwards
                if p.last_pkt.map_or(true, |(s, _)| (1..0x8000).contains(&pkt.seq.wrapping_sub(s))) {
                    p.last_pkt = Some((pkt.seq, tick));
                }
                if let Some(r) = pkt.report {
                    p.client_report = Some((r, tick));
                }
                let rejected = p.queue.push_packet(&pkt, tick);
                if rejected > 0 {
                    self.strike_log.push(format!("player {id} tick {tick}: future-tick intents x{rejected} (newest {}, ceiling {})", pkt.intents.first().map_or(0, |i| i.tick), tick + shinobi_proto::intent::STRIKE_FUTURE_TICKS));
                }
                p.guard.strike_n(rejected, &ac)
            }
            Err(e) => {
                self.strike_log.push(format!("player {id} tick {tick}: malformed datagram ({} B): {e:?}", bytes.len()));
                p.guard.strike_n(1, &ac)
            }
        }
    }

    /// Advance by wall-clock `elapsed` (time since the previous call). Runs 0..=5 fixed steps.
    pub fn frame(&mut self, elapsed: Duration) -> FrameReport {
        self.events.clear();
        self.pvp_events.clear();
        self.pickup_events.clear();
        self.block_events.clear();
        self.block_events.append(&mut self.pending_block_events);
        self.kill_tags.clear();
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
            crate::spawn::SpawnSet { pos: &self.spawns, teams: &self.spawn_teams },
            &mut self.spawn_counter,
            &mut self.pickups,
            self.balance.player.invuln_ticks,
            &mut self.match_events,
        );
        let frozen = self.match_state.frozen();
        pvp::tick_respawns(&mut self.players, &mut self.world, crate::spawn::SpawnSet { pos: &self.spawns, teams: &self.spawn_teams }, &mut self.spawn_counter, tick, self.balance.player.invuln_ticks, &mut self.pvp_events);
        let mut hits = Vec::new();
        // (attacker, victim) pairs hit by a bounced disc this tick, and attackers that were wall-running
        let mut ricochets: Vec<(u16, u16)> = Vec::new();
        let mut wall_runners: Vec<u16> = Vec::new();
        let mut clash_wins: Vec<(u16, u16)> = Vec::new();
        let mut yanks: Vec<(usize, Vector<Real>, Vector<Real>)> = Vec::new();
        // air-cut landings this tick: (centre, slammer, slammed): the shock-wave goes off after every player has moved
        let mut shocks: Vec<([f32; 3], u16, u16)> = Vec::new();
        // one refresh for the whole player loop, after match/respawn/pickup changes to the collider set
        self.world.queries.update(&self.world.colliders);
        crate::movement::refresh_solid(&mut self.solid_queries, &self.world.colliders);

        let bcfg = self.balance.guard.cfg();
        let mcfg = self.balance.melee.cfg();
        // star variants: who stands in smoke (hidden from every lock-on) and where this tick's combat events begin
        let in_smoke = self.in_smoke();
        let hidden_boxes: Vec<(usize, ColliderHandle)> = self.players.iter().enumerate().filter(|(j, _)| in_smoke[*j]).map(|(j, q)| (j, q.vitals.hitbox)).collect();
        let ev_start = self.events.len();
        for i in 0..self.players.len() {
            let intent = self.players[i].queue.take_for_tick(tick);
            let before = self.players[i].pos(&self.world);
            // the shield: raised only by a living player in a live match who is not staggered (see `block.rs`)
            let can_act = !frozen && self.players[i].vitals.alive();
            let was_up = self.players[i].blk.held;
            self.players[i].blk.update(intent.as_ref().map_or(false, |it| it.block), tick, &bcfg, can_act);
            if was_up && !self.players[i].blk.held {
                self.players[i].kmoves.lowered_shield(tick);
            }

            // aim: clamp claimed slew, then derive everything from the server copy
            if let Some(it) = &intent {
                let p = &mut self.players[i];
                // the slew ceiling is per tick: a client that hitched and skipped ticks may legitimately arrive with the
                // whole flick of the gap in one intent, so the allowance scales with the ticks it covers
                let gap = tick.saturating_sub(p.last_aim_tick).max(1);
                let mut turn_cfg = ac;
                turn_cfg.max_turn = ac.max_turn * gap as f32;
                let (prev, claimed) = (dq_yaw(p.yaw), dq_yaw(it.aim_yaw));
                let (yaw, v) = p.guard.limit_yaw(prev, claimed, &turn_cfg);
                p.last_aim_tick = tick;
                if yaw != claimed {
                    let turn = (claimed - prev + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                    self.strike_log.push(format!("player {} tick {tick}: aim slew {:.1} deg over {gap} tick(s), allowed {:.1}", p.id, turn.to_degrees(), turn_cfg.max_turn.to_degrees()));
                }
                // blocking locks the view: only the aim of the tick the shield went up counts, the rest is ignored
                if !(p.blk.held && !p.blk.just_raised(tick)) {
                    p.yaw = q_yaw(yaw);
                    p.pitch = it.aim_pitch;
                }
                if v == Verdict::Kick {
                    kicked.push(p.id);
                    continue;
                }
            }
            // dead players neither move nor fight (their intents are still consumed, so nothing queues up)
            if frozen || !self.players[i].vitals.alive() {
                continue;
            }
            let mut it = intent.unwrap_or_default();
            // the bow slows the walk only while it is in the hands (the client sets the same bit only then)
            it.draw = it.draw && self.players[i].power.kind == crate::power::PowerKind::Yumi;
            let (aim_yaw, aim_pitch) = (self.players[i].yaw, self.players[i].pitch);
            let mut input = to_player_input_scaled(&PlayerIntent { aim_yaw, ..it }, self.balance.combat.power.yumi.move_mult);
            // shots and slashes start at the EYE (`CharacterSystem::eye_above`, the height the client camera uses), so the
            // crosshair and the projectile agree; hitboxes stay centred on the capsule
            let origin = vector![before[0], before[1] + self.players[i].ch.eye_above(), before[2]];
            let aim = Aim { origin, yaw: dq_yaw(aim_yaw), pitch: dq_pitch(aim_pitch) };

            // combat runs against the world as this attacker saw it (lag compensation)
            // (skipped when nothing can query: the rebuild is the expensive part with many players)
            // a raised shield or a stagger (parried) means no throwing, cutting or dashing
            let shut = self.players[i].blk.held || self.players[i].blk.staggered(tick);
            let slice_in = it.buttons.has(Buttons::SLICE) && !shut;
            // power weapons (docs/powerups.md): the bow and the tags take the right button over while they are in hand
            let mut fire_in = it.buttons.has(Buttons::FIRE) && !shut;
            {
                let pcfg = self.balance.combat.power;
                let (armed, live) = (self.armed_tags(self.players[i].id), self.live_tags(self.players[i].id));
                let pid = self.players[i].id;
                let p = &mut self.players[i];
                let mut pev = Vec::new();
                p.power.update(dt, &crate::power::PowerInput { draw: it.draw && !shut, fire: fire_in, live_tags: live, armed_tags: armed, can_act: !shut }, &pcfg, &mut pev);
                if p.power.blocks_star() {
                    fire_in = false;
                }
                let dir = aim.forward();
                for ev in pev {
                    self.power_launch.push(crate::power_pass::Launch { owner: pid, ev, eye: [origin.x, origin.y, origin.z], dir: [dir.x, dir.y, dir.z] });
                }
            }
            if self.players[i].blk.staggered(tick) {
                input.dash_pressed = false;
            }
            let acting = fire_in || slice_in || self.players[i].combat.busy();
            if acting {
                pvp::rewind_hitboxes(&mut self.world, &self.players, i, tick, &self.cfg);
            }
            let mut ev = Vec::new();
            {
                let team = self.players[i].team;
                // teammates AND the owner's own hitbox: never assisted toward, locked onto, hit or bounced off
                let allies: Vec<ColliderHandle> =
                    self.players.iter().enumerate().filter(|(j, q)| *j == i || allied(team, q.team)).map(|(_, q)| q.vitals.hitbox).collect();
                // katana moves: the parried attacker's hitbox (counter cut), the shield just lowered (whirl cut)
                let counter = self.players[i]
                    .kmoves
                    .counter_at(tick)
                    .and_then(|(foe, parried)| self.players.iter().find(|q| q.id == foe && q.vitals.alive()).map(|q| (q.vitals.hitbox, parried)));
                let just_unblocked = self.players[i].kmoves.just_unblocked(tick);
                let p = &mut self.players[i];
                p.combat.allies = allies;
                p.combat.hidden = hidden_boxes.iter().filter(|(j, _)| *j != i).map(|(_, h)| *h).collect();
                p.combat.suppress_lock = tick < p.blind_until;
                p.combat.set_moves(crate::combat::MoveCtx { counter, just_unblocked });
                p.combat.update(
                    dt,
                    now,
                    &CombatInput {
                        fire: fire_in,
                        slice: slice_in,
                        ultimate: ultimate_pressed(&it) && !shut,
                        mark: it.mark && fire_in,
                        star: crate::combat::stars::StarKind::from_u8(it.star),
                    },
                    &aim,
                    &self.world,
                    &p.ch,
                    &mut input,
                    &mut ev,
                );
                p.ch.set_slice_cooldown(p.combat.slice_cooldown_left()); // travels in OwnState (lunge prediction)
            }
            // a swing leaves the swordsman open for a moment (the strike window the lock triangle shows)
            if ev.iter().any(|e| matches!(e, CombatEvent::Slash { .. })) {
                self.players[i].blk.open_for(tick, bcfg.swing_open_ticks);
            }
            // katana moves: an air cut slams what it hits (settled in `apply_hits`), a counter cut spends the window
            for e in &ev {
                if let CombatEvent::Slash { kind, .. } = *e {
                    match kind {
                        crate::combat::SlashKind::Air => self.players[i].kmoves.air_cut_tick = Some(tick),
                        crate::combat::SlashKind::Counter => self.players[i].kmoves.counter = None,
                        _ => {}
                    }
                }
            }
            let mut synthesized = Vec::new();
            let head = pvp::HeadCfg { min_height: self.balance.combat.head_min_height, mult: self.balance.combat.headshot_mult };
            let first_hit = hits.len();
            hits.extend(pvp::player_hits(&self.world, &self.players, i, &ev, tick, &self.cfg, head, &mut synthesized));
            // wall clash: the first katana hit inside the window wins the exchange: bigger hit, loser thrown off the wall
            if let Some((foe, until)) = self.players[i].clash {
                if tick <= until {
                    let me = self.players[i].id;
                    for h in hits[first_hit..].iter_mut() {
                        if h.cause == crate::pvp::Cause::Weapon(crate::combat::DamageKind::Slash) && self.players.get(h.victim).map_or(false, |v| v.id == foe) {
                            h.amount *= self.cfg.clash_mult;
                            clash_wins.push((me, foe));
                        }
                    }
                }
            }
            // weapons hurt player-built pieces too (anyone's, the owner's included); at zero hp a piece de-rezzes
            let mut pieces_changed = false;
            for e in &ev {
                if let CombatEvent::PieceHit { cell, amount, .. } = *e {
                    pieces_changed |= self.blocks.damage(&mut self.world, cell, amount, tick, &mut self.block_events);
                }
            }
            if pieces_changed {
                self.world.queries.update(&self.world.colliders);
            }
            ev.extend(synthesized);
            let id = self.players[i].id;
            // a MARK star struck a player: mark them for the thrower's next katana hit
            if mcfg.mark_ticks > 0 {
                for e in &ev {
                    if let CombatEvent::MarkHit { collider, .. } = *e {
                        let team = self.players[i].team;
                        if let Some(v) = self.players.iter().position(|q| q.vitals.hitbox == collider && q.id != id && !allied(team, q.team)) {
                            self.players[v].melee.marked_by = id;
                            self.players[v].melee.marked_until = tick + mcfg.mark_ticks;
                        }
                    }
                }
            }
            let mut ric = false;
            for e in &ev {
                match *e {
                    CombatEvent::RicochetHit { .. } => ric = true,
                    CombatEvent::Damage { collider, .. } if ric => {
                        ric = false;
                        if let Some(v) = self.players.iter().find(|q| q.vitals.hitbox == collider) {
                            ricochets.push((id, v.id));
                        }
                    }
                    _ => {}
                }
            }
            if matches!(self.players[i].ch.state, crate::character::MoveState::WallRun { .. }) {
                wall_runners.push(id);
            }
            self.events.extend(ev.into_iter().map(|e| (id, e)));

            if it.build {
                // one-tick edge: place a block at the aimed spot (server-side range / cooldown / overlap checks)
                let bodies: Vec<[f32; 3]> = self.players.iter().filter(|q| q.vitals.alive()).map(|q| q.pos(&self.world)).collect();
                let f = aim.forward();
                let skip = self.players[i].ch.body;
                let kind = if it.build_wall {
                    crate::blocks::PieceKind::Wall
                } else if it.build_ramp {
                    crate::blocks::PieceKind::Ramp
                } else {
                    crate::blocks::PieceKind::Floor
                };
                let kills = self.players[i].vitals.kills as u32;
                self.blocks.try_build(&mut self.world, tick, id, kind, kills, [origin.x, origin.y, origin.z], [f.x, f.y, f.z], skip, &bodies, &mut self.block_events);
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
                crate::character::GrappleEvent::Yank { origin, anchor } => {
                    let c = p.pos(&self.world);
                    self.events.push((id, CombatEvent::GrappleRelease { pos: Point::new(c[0], c[1], c[2]) }));
                    yanks.push((i, origin, anchor));
                }
                crate::character::GrappleEvent::Miss { origin, dir } => {
                    self.events.push((id, CombatEvent::GrappleMiss { origin: Point::from(origin), dir }));
                }
                crate::character::GrappleEvent::None => {}
            }

            // wall-splat: a launched player stopped dead in the air by a wall takes bonus damage and is stunned
            {
                // measured, not the stored velocity: a wall stops the body but the controller keeps its velocity
                let now_pos = p.pos(&self.world);
                let moved = ((now_pos[0] - before[0]).powi(2) + (now_pos[2] - before[2]).powi(2)).sqrt() / dt;
                let airborne = !matches!(p.ch.state, crate::character::MoveState::Grounded);
                if let Some(by) = mcfg.splat(&mut p.melee, moved, airborne, tick) {
                    p.blk.stagger_until = tick + mcfg.splat_stagger_ticks;
                    p.blk.open_for(tick, mcfg.splat_stagger_ticks);
                    hits.push(pvp::PvpHit { attacker: by, victim: i, amount: mcfg.splat_damage, cause: pvp::Cause::Hazard(pvp::SPLAT), head: false });
                }
            }
            // air-cut slam: when the slammed player lands, a shock-wave goes off (applied once everyone has moved)
            {
                let grounded = matches!(p.ch.state, crate::character::MoveState::Grounded);
                if let Some(by) = p.kmoves.slam_landed(grounded, tick) {
                    shocks.push((p.pos(&self.world), by, p.id));
                }
            }
            // delta ceiling: did the body move further than any legal mover can?
            let after = p.pos(&self.world);
            // rope and strike momentum: a far hook reels at up to `grapple_far_speed` and the release keeps it; allow what the
            // (server-side, authoritative) velocity says the body legitimately did this tick, never past the reel ceiling
            let v = p.ch.vel.norm();
            let allowance = if v > self.cfg.grapple_max_speed { (v.min(self.cfg.grapple_far_speed + 2.0) * dt * 1.25 - ac.max_step).max(0.0) } else { 0.0 };
            match p.guard.check_step(before, after, allowance, &ac) {
                Verdict::Ok => {}
                Verdict::SnapBack { to } => {
                    self.strike_log.push(format!("player {} tick {tick}: moved {:.2} m in one tick (ceiling {:.2}), vel {:.1} m/s, state {:?}", p.id, ((after[0] - before[0]).powi(2) + (after[1] - before[1]).powi(2) + (after[2] - before[2]).powi(2)).sqrt(), ac.max_step, p.ch.vel.norm(), p.ch.state));
                    let to = vector![to[0], to[1], to[2]];
                    self.world.bodies[p.ch.body].set_position(Isometry::translation(to.x, to.y, to.z), true);
                    crate::character::sync_body_colliders(&mut self.world, p.ch.body);
                    p.ch.vel = Vector::zeros();
                    p.ch.clear_grapple();
                }
                Verdict::Kick => kicked.push(p.id),
            }
        }

        self.resolve_yanks(tick, &yanks);
        // air-cut shock-waves: bystanders near a landing are thrown back (and credit the slammer if they go over the edge)
        for (centre, by, victim) in shocks {
            let by_team = self.players.iter().find(|q| q.id == by).map(|q| q.team);
            for q in self.players.iter_mut() {
                if q.id == victim || q.id == by || !q.vitals.alive() || by_team.map_or(false, |bt| allied(bt, q.team)) {
                    continue;
                }
                let pos = q.pos(&self.world);
                if let Some(v) = crate::katana_moves::shock_push(centre, pos, mcfg.shock_radius, mcfg.shock_knock) {
                    q.ch.vel = Vector::new(v[0], v[1], v[2]);
                    if matches!(q.ch.state, crate::character::MoveState::Grounded) {
                        q.ch.state = crate::character::MoveState::Airborne;
                    }
                    q.melee.knock_until = tick + mcfg.splat_ticks;
                    q.melee.knock_speed = mcfg.shock_knock;
                    q.melee.knock_by = by;
                    q.melee.last_hit = Some((by, tick));
                }
            }
        }

        self.resolve_clashes(tick, &clash_wins);
        self.star_pass(tick, ev_start, &mut hits);
        if frozen {
            self.arrows.clear();
            self.tags.clear();
            self.power_launch.clear();
        } else {
            self.power_pass(tick, ev_start, &mut hits);
        }

        // soft push-out: server-authoritative correction, after anti-cheat so it never counts as movement
        crate::separation::separate(&self.players, &mut self.world, &self.cfg, &self.separation);

        // environment: obstacles and light-walls (positions after this tick's movement)
        self.world.queries.update(&self.world.colliders);
        if !frozen {
            hits.extend(pvp::void_hits(&self.players, &self.world, self.kill_plane_y, tick, &mcfg));
            hits.extend(self.hazards.hits(&self.world, &self.players, tick, &self.cfg, &mut self.pvp_events));
        }

        // every attacker has run: settle damage once, then record the tick
        let first_new = self.pvp_events.len();
        pvp::apply_hits(&mut self.players, &mut self.world, &hits, tick, self.balance.player.respawn_ticks, &bcfg, &mcfg, &mut self.pvp_events);
        for e in &self.pvp_events[first_new..] {
            if let PvpEvent::Kill { tick, killer, victim, kind, .. } = *e {
                if kind == crate::combat::DamageKind::Slash {
                    // the katana's SlashHit was emitted before the damage resolved: tell it the target died
                    for (id, ev) in self.events.iter_mut() {
                        if let CombatEvent::SlashHit { target, killed, .. } = ev {
                            if *id == killer && *target == victim as u128 {
                                *killed = true;
                            }
                        }
                    }
                }
                let ricochet = kind == crate::combat::DamageKind::Shuriken && ricochets.contains(&(killer, victim));
                let wall_run = wall_runners.contains(&killer);
                if ricochet || wall_run {
                    self.kill_tags.push(KillTag { tick, killer, victim, ricochet, wall_run });
                }
            }
        }
        // a dead player's power weapon lies where they fell
        for k in 0..self.players.len() {
            if !self.players[k].vitals.alive() && self.players[k].power.held() {
                self.drop_power_weapon(k, tick);
            }
        }
        self.match_state.end_tick(tick, &self.players, &mut self.match_events);
        if !frozen {
            self.pickups.update(tick, &self.world, &mut self.players, &self.cfg, &self.balance.combat.power, &mut self.pickup_events);
        }
        if !frozen {
            let pb = &self.balance.player;
            pvp::regen(&mut self.players, tick, (pb.regen_delay_s * crate::match_state::TICK_HZ as f32) as u32, pb.regen_per_s / crate::match_state::TICK_HZ as f32);
        }
        pvp::finish_tick(&mut self.players, &mut self.world, tick);
        self.tick = tick.wrapping_add(1);
        self.now += dt;
    }
}

impl Server {
    /// Hook yank (docs/traversal_melee.md): the living, non-allied player at the rope's anchor is pulled toward the
    /// yanker (speed scales with the gap, capped at `yank_speed`) and staggered. Server only: the pull reaches the victim
    /// as an ordinary reconciliation, the yanker's own rope cut is predicted.
    fn resolve_yanks(&mut self, tick: u32, yanks: &[(usize, Vector<Real>, Vector<Real>)]) {
        let cfg = self.cfg;
        for &(i, origin, anchor) in yanks {
            let team = self.players[i].team;
            let mut best: Option<(f32, usize)> = None;
            for (j, q) in self.players.iter().enumerate() {
                if j == i || !q.vitals.alive() || allied(team, q.team) {
                    continue;
                }
                let c = q.pos(&self.world);
                let d = (vector![c[0], c[1], c[2]] - anchor).norm();
                if d <= 2.5 && best.map_or(true, |b| d < b.0) {
                    best = Some((d, j));
                }
            }
            let Some((_, j)) = best else { continue };
            let c = self.players[j].pos(&self.world);
            let me = self.players[i].pos(&self.world);
            let to = vector![me[0] - c[0], me[1] - c[1], me[2] - c[2]];
            let gap = to.norm();
            let _ = origin;
            let speed = (gap * cfg.yank_gain).clamp(cfg.yank_speed * 0.3, cfg.yank_speed);
            self.players[j].ch.yank_pull(to, speed, cfg.yank_hop);
            let ticks = (cfg.yank_stagger * TICK_HZ_F) as u32;
            self.players[j].blk.stagger_until = tick + ticks;
            self.players[j].blk.open_for(tick, ticks);
        }
    }

    /// Wall clash (docs/traversal_melee.md): two living players wall-running the same face within `clash_range` open a
    /// strike window; a katana hit inside it was boosted in `step` (`clash_wins`) and throws the loser off the wall; a
    /// window that ends with no hit knocks both apart. Server only: the knock arrives as an ordinary reconciliation.
    fn resolve_clashes(&mut self, tick: u32, wins: &[(u16, u16)]) {
        let cfg = self.cfg;
        let window = (cfg.clash_window * TICK_HZ_F) as u32;
        for &(winner, loser) in wins {
            let Some(n) = self.players.iter().find(|p| p.id == loser).and_then(|p| p.ch.wall_normal()) else { continue };
            if let Some(l) = self.players.iter_mut().find(|p| p.id == loser) {
                l.ch.knock_off(n, cfg.clash_knock);
                l.clash = None;
                l.clash_ready_tick = tick + (cfg.clash_cd * TICK_HZ_F) as u32;
            }
            if let Some(w) = self.players.iter_mut().find(|p| p.id == winner) {
                w.clash = None;
                w.clash_ready_tick = tick + (cfg.clash_cd * TICK_HZ_F) as u32;
            }
        }
        // windows that ran out: a draw throws both off the face
        for i in 0..self.players.len() {
            if let Some((foe, until)) = self.players[i].clash {
                if tick > until {
                    if let Some(n) = self.players[i].ch.wall_normal() {
                        self.players[i].ch.knock_off(n, cfg.clash_knock * 0.7);
                    }
                    self.players[i].clash = None;
                    self.players[i].clash_ready_tick = tick + (cfg.clash_cd * TICK_HZ_F) as u32;
                    let _ = foe;
                }
            }
        }
        // new clashes
        for i in 0..self.players.len() {
            for j in (i + 1)..self.players.len() {
                let (a, b) = (&self.players[i], &self.players[j]);
                if a.clash.is_some() || b.clash.is_some() || tick < a.clash_ready_tick || tick < b.clash_ready_tick {
                    continue;
                }
                if !a.vitals.alive() || !b.vitals.alive() || allied(a.team, b.team) {
                    continue;
                }
                let (Some(na), Some(nb)) = (a.ch.wall_normal(), b.ch.wall_normal()) else { continue };
                if na.dot(&nb) < 0.9 {
                    continue; // not the same face
                }
                let (pa, pb) = (a.pos(&self.world), b.pos(&self.world));
                let d = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
                if d > cfg.clash_range {
                    continue;
                }
                let (ia, ib) = (a.id, b.id);
                self.players[i].clash = Some((ib, tick + window));
                self.players[j].clash = Some((ia, tick + window));
            }
        }
    }
}

const TICK_HZ_F: f32 = crate::match_state::TICK_HZ as f32;

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
    to_player_input_scaled(it, crate::power::YumiCfg::default().move_mult)
}

/// As `to_player_input`, with the walk multiplier of a drawn bow (`combat.yumi.move_mult`) applied while `it.draw` is set.
pub fn to_player_input_scaled(it: &PlayerIntent, draw_mult: f32) -> PlayerInput {
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
        strike: false, // set by the katana (server) / predict_slice (client) when a swing launches
        walk_slow: if it.draw { 1.0 - draw_mult } else { 0.0 },
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
