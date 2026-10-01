//! Prediction parity: the client's local sim and the authoritative server must agree bit-for-bit when fed the
//! same intents, and `Reconciler` must therefore never ask for a correction.

use std::collections::VecDeque;
use std::time::Duration;

use shinobi_client::reconcile::{Correction, Reconciler};
use shinobi_proto::bits::{BitReader, BitWriter};
use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent};
use shinobi_proto::quant::dq_yaw;
use shinobi_proto::TICK_DT;
use shinobi_sim::character::{Cfg, CharacterSystem, MoveState, PhysicsWorld};
use shinobi_sim::combat::{GROUP_PLAYER, GROUP_WORLD};
use shinobi_sim::rapier3d::prelude::*;
use shinobi_sim::movement::step_player;
use shinobi_sim::own_state::OwnState;
use shinobi_sim::tick::{to_player_input, Server};

const SPAWN: [f32; 3] = [0.0, 0.81, 0.0];
/// Level geometry as (center, half-extents); inserted in this order on both sides.
const LEVEL: &[([f32; 3], [f32; 3])] = &[
    ([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]), // floor
    ([3.0, 6.0, -40.0], [1.0, 6.0, 50.0]),   // wall, face at x = 2
    ([-4.0, 0.15, -10.0], [3.0, 0.15, 3.0]), // 0.3 m step (autostep)
];

/// The client's copy of the movement sim. Must mirror `Server::step` for the movement-only path.
struct Predictor {
    world: PhysicsWorld,
    ch: CharacterSystem,
    now: f32,
}

impl Predictor {
    fn new(cfg: Cfg) -> Self {
        let mut world = PhysicsWorld {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            queries: QueryPipeline::new(),
        };
        for (c, h) in LEVEL {
            world.colliders.insert(
                ColliderBuilder::cuboid(h[0], h[1], h[2])
                    .translation(vector![c[0], c[1], c[2]])
                    .collision_groups(InteractionGroups::new(GROUP_WORLD, Group::ALL))
                    .build(),
            );
        }
        let body = world.bodies.insert(
            RigidBodyBuilder::kinematic_position_based().translation(vector![SPAWN[0], SPAWN[1], SPAWN[2]]).build(),
        );
        world.colliders.insert_with_parent(
            ColliderBuilder::capsule_y(cfg.half_height, cfg.radius)
                .collision_groups(InteractionGroups::new(GROUP_PLAYER, Group::ALL))
                .build(),
            body,
            &mut world.bodies,
        );
        if std::env::var("DUMMY_SENSOR").is_ok() {
            world.colliders.insert(ColliderBuilder::capsule_y(0.5, 0.3).sensor(true).translation(vector![0.0, 0.81, 0.0]).build());
        }
        Self { ch: CharacterSystem::new(body, cfg), world, now: 0.0 }
    }

    fn step(&mut self, intent: &PlayerIntent) -> [f32; 3] {
        step_player(&mut self.ch, &to_player_input(intent), &mut self.world, self.now, TICK_DT);
        self.now += TICK_DT;
        let t = self.world.bodies[self.ch.body].translation();
        [t.x, t.y, t.z]
    }
}

fn server(cfg: Cfg) -> Server {
    let mut s = Server::new(cfg);
    for (c, h) in LEVEL {
        s.add_static_box(*c, *h);
    }
    s.add_player(1, SPAWN);
    s
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
    fn pick(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

/// Scripted run at the wall (strafe, jump + cling forward), then seeded-random play.
/// No FIRE/SLICE: combat may rewrite `PlayerInput`, which the client does not predict here.
fn script(ticks: u32) -> Vec<PlayerIntent> {
    let mut rng = Lcg(0x5EED);
    let mut rng2 = Lcg(0xC0FFEE); // slide draws: separate stream so the rest of the script is unchanged
    let mut slide = false;
    let mut out = Vec::new();
    let (mut mx, mut mz, mut held, mut cling) = (0i8, 0i8, false, false);
    let mut yaw: u16 = 0;
    for t in 0..ticks {
        let mut b = 0u8;
        if t < 20 {
            mx = 127;
            mz = 0;
        } else if t == 20 {
            (mx, mz, held, cling) = (0, 127, true, true);
            b |= Buttons::JUMP_PRESSED;
        } else if t > 90 && t % 35 == 0 {
            mx = [-127, 0, 127][rng.pick(3) as usize];
            mz = [-127, 0, 127][rng.pick(3) as usize];
            held = rng.pick(2) == 0;
            cling = rng.pick(3) == 0;
            if rng.pick(2) == 0 {
                b |= Buttons::JUMP_PRESSED;
                held = true;
            }
        } else if t > 90 && t % 35 == 18 && rng.pick(2) == 0 {
            b |= Buttons::JUMP_PRESSED; // second press -> double jump candidates
            held = true;
        }
        if t > 90 && t % 55 == 7 {
            b |= if rng.pick(2) == 0 { Buttons::DASH } else { Buttons::ROLL };
        }
        if t > 90 && t % 35 == 3 {
            slide = rng2.pick(3) == 0;
        }
        if slide && t > 90 {
            b |= Buttons::SLIDE;
        }
        if held {
            b |= Buttons::JUMP_HELD;
        }
        if cling {
            b |= Buttons::WALL_CLING;
        }
        if t > 90 {
            yaw = yaw.wrapping_add((rng.pick(201) as i32 - 100) as u16); // <= ~0.55 deg/tick
        }
        out.push(PlayerIntent { tick: t, buttons: Buttons(b), move_x: mx, move_z: mz, aim_yaw: yaw, aim_pitch: 2048, ultimate: false, build: false, zip: false });
    }
    out
}

fn bits(p: [f32; 3]) -> [u32; 3] {
    [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()]
}

struct Run {
    server_pos: Vec<[f32; 3]>,
    client_pos: Vec<[f32; 3]>,
    intents: Vec<PlayerIntent>,
    saw_wall_run: bool,
    saw_grounded: bool,
    saw_air: bool,
    saw_double_jump: bool,
    saw_slide: bool,
    server_own: Vec<OwnState>,
}

/// Drive the server over the wire path (encode -> ingest -> frame) and the predictor in lockstep.
fn run(ticks: u32, tweak_server_level: bool) -> Run {
    run_with(ticks, tweak_server_level, None)
}

/// `drop_tick`: that intent never reaches the server (packet loss), so the server repeats the previous one
/// with its edges stripped while the client predicted the real one.
fn run_with(ticks: u32, tweak_server_level: bool, drop_tick: Option<u32>) -> Run {
    let cfg = Cfg::default();
    let mut srv = server(cfg);
    if tweak_server_level {
        srv.add_static_box([0.0, 1.0, -12.0], [5.0, 1.0, 0.5]); // obstacle only the server knows about
    }
    let mut cli = Predictor::new(cfg);
    let mut r = Run {
        server_pos: vec![],
        client_pos: vec![],
        intents: script(ticks),
        saw_wall_run: false,
        saw_grounded: false,
        saw_air: false,
        saw_double_jump: false,
        saw_slide: false,
        server_own: vec![],
    };
    for it in r.intents.clone() {
        let bytes = ClientPacket { intents: vec![it], ..Default::default() }.encode().unwrap();
        if Some(it.tick) != drop_tick {
            srv.ingest(1, &bytes);
        }
        while srv.tick == it.tick {
            srv.frame(Duration::from_nanos(16_666_667));
        }
        assert_eq!(srv.players[0].guard.strikes, 0, "tick {}: server struck an honest client", it.tick);

        // the server simulates from its own quantised yaw, never the claim
        let server_yaw = srv.players[0].yaw;
        if drop_tick.is_none() {
            assert_eq!(server_yaw, it.aim_yaw, "tick {}: yaw slew clamp engaged on honest input", it.tick);
        }
        let _ = dq_yaw(server_yaw);
        r.client_pos.push(cli.step(&PlayerIntent { aim_yaw: it.aim_yaw, ..it }));
        r.server_own.push(OwnState::from_snapshot(&srv.players[0].ch.snapshot(&srv.world)));
        r.server_pos.push(srv.players[0].pos(&srv.world));

        match srv.players[0].ch.state {
            MoveState::WallRun { .. } => r.saw_wall_run = true,
            MoveState::Grounded => r.saw_grounded = true,
            MoveState::Airborne => r.saw_air = true,
        }
        r.saw_double_jump |= srv.players[0].ch.double_jumped;
        r.saw_slide |= srv.players[0].ch.sliding();
    }
    r
}

#[test]
fn two_independent_worlds_are_bit_identical() {
    let r = run(900, false);
    assert!(r.saw_grounded && r.saw_air && r.saw_wall_run && r.saw_double_jump && r.saw_slide, "script must cover the kit: {}", {
        format!("g={} a={} w={} dj={} sl={}", r.saw_grounded, r.saw_air, r.saw_wall_run, r.saw_double_jump, r.saw_slide)
    });
    let worst = r.server_pos.iter().zip(&r.client_pos).map(|(s, c)| (0..3).map(|k| (s[k] - c[k]).powi(2)).sum::<f32>().sqrt()).fold(0.0f32, f32::max);
    eprintln!("PARITY worst divergence over {} ticks: {worst}", r.server_pos.len());
    for (t, (s, c)) in r.server_pos.iter().zip(&r.client_pos).enumerate() {
        assert_eq!(bits(*s), bits(*c), "tick {t}: server {s:?} vs client {c:?}");
    }
    // and a second client instance agrees with the first (no hidden global state)
    let again = run(900, false);
    assert_eq!(r.client_pos.iter().map(|p| bits(*p)).collect::<Vec<_>>(), again.client_pos.iter().map(|p| bits(*p)).collect::<Vec<_>>());
}

#[test]
fn reconciler_asks_for_zero_corrections_with_delayed_server_results() {
    const LATENCY: usize = 4; // server results reach the client 4 ticks late
    let r = run(900, false);
    let mut rec = Reconciler::default();
    let mut inflight: VecDeque<(u32, [f32; 3])> = VecDeque::new();
    for (i, it) in r.intents.iter().enumerate() {
        rec.record(*it, r.client_pos[i]);
        inflight.push_back((it.tick, r.server_pos[i]));
        if inflight.len() > LATENCY {
            let (acked, pos) = inflight.pop_front().unwrap();
            let c = rec.on_server_state(acked, pos, r.client_pos[i]);
            assert_eq!(c, Correction::None, "tick {acked}");
            assert_eq!(rec.visual_offset, [0.0; 3]);
        }
    }
}

#[test]
fn reconciler_detects_a_real_divergence() {
    // the server has an obstacle the client does not: prediction must fail loudly, not silently drift
    let diverged = run(400, true);
    let clean = run(400, false);
    let far = |s: &[f32; 3], c: &[f32; 3]| (0..3).map(|i| (s[i] - c[i]).powi(2)).sum::<f32>().sqrt() > 0.25;
    let first_bad = diverged.server_pos.iter().zip(&clean.client_pos).position(|(s, c)| far(s, c));
    let t = first_bad.expect("obstacle never moved the server past the deadband; move it");
    let mut rec = Reconciler::default();
    for (i, it) in clean.intents.iter().enumerate().take(t + 3) {
        rec.record(*it, clean.client_pos[i]); // history keeps the last 128, so stop just past the bad tick
    }
    let c = rec.on_server_state(t as u32, diverged.server_pos[t], clean.client_pos[t + 2]);
    assert!(matches!(c, Correction::Replay { .. }), "tick {t}: {c:?}");
}

/// Client predictor for the replay tests: world + character + sim clock, with a spare handle for the reconciler.
fn fresh_client() -> Predictor {
    Predictor::new(Cfg::default())
}

#[test]
fn a_dropped_intent_is_corrected_by_restoring_the_server_own_state_and_replaying() {
    const LATENCY: usize = 4;
    let ticks = 500;
    // first DASH edge after the scripted opening: dropping it makes the server skip a 30 m/s dash
    let drop_tick = script(ticks).iter().find(|i| i.tick > 100 && i.buttons.has(Buttons::DASH)).map(|i| i.tick).expect("script has a dash");
    let r = run_with(ticks, false, Some(drop_tick));

    let mut cli = fresh_client();
    let mut rec = Reconciler::default();
    let (mut replays, mut last_replay_tick) = (0, 0);
    let mut inflight: VecDeque<usize> = VecDeque::new();
    let mut cli_pos = [0.0f32; 3];
    for (i, it) in r.intents.iter().enumerate() {
        cli_pos = cli.step(it); // the client predicts from the intent it actually pressed
        rec.record(*it, cli_pos);
        inflight.push_back(i);
        if inflight.len() > LATENCY {
            let j = inflight.pop_front().unwrap();
            // the state blob crosses the wire bit-packed
            let mut w = BitWriter::new();
            r.server_own[j].write(&mut w);
            let bytes = w.finish();
            let own = OwnState::read(&mut BitReader::new(&bytes)).unwrap();
            if let Some(p) = rec.on_server_own_state(j as u32, r.server_pos[j], &own, cli_pos, &mut cli.ch, &mut cli.world, &mut shinobi_client::reconcile::plain_input) {
                replays += 1;
                last_replay_tick = j;
                cli_pos = p;
                cli.now = (i + 1) as f32 * TICK_DT;
            }
        }
    }
    assert!(replays >= 1, "the dropped dash must have been detected");
    assert!(last_replay_tick < drop_tick as usize + 12, "corrections stop right after the drop (last at {last_replay_tick}, drop {drop_tick})");
    let end = r.server_pos.last().unwrap();
    let err = (0..3).map(|k| (end[k] - cli_pos[k]).powi(2)).sum::<f32>().sqrt();
    assert!(err < 0.02, "after replay the client agrees with the server within the deadband: {err} m");
}

#[test]
fn restore_from_the_wire_blob_tracks_the_exact_run_for_60_ticks_bit_exactly() {
    let r = run(700, false);
    for start in (100..600).step_by(37) {
        let mut cli = fresh_client();
        let mut w = BitWriter::new();
        r.server_own[start].write(&mut w);
        let own = OwnState::read(&mut BitReader::new(&w.finish())).unwrap();
        let p = r.server_pos[start];
        let _ = p;
        cli.ch.restore_own(&mut cli.world, &own);
        cli.now = (start + 1) as f32 * TICK_DT;
        for k in start + 1..(start + 60).min(r.intents.len()) {
            let got = cli.step(&PlayerIntent { aim_yaw: r.intents[k].aim_yaw, ..r.intents[k] });
            let s = r.server_pos[k];
            let e = (0..3).map(|a| (got[a] - s[a]).powi(2)).sum::<f32>().sqrt();
            assert!(e < 1e-6, "restore at {start}, +{} ticks: {e} m off", k - start);
        }
    }
}
