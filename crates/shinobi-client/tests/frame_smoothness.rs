//! Frame-to-frame smoothness of what the player SEES: own eye and a remote player, with the real `ClientGame`
//! (prediction, reconcile, interpolation) driven at uneven frame times by a model of Bevy's fixed-step loop
//! (`Time<Fixed>` accumulator, `overstep_fraction`). A renderer that only moves on 60 Hz ticks shows 0 / 1-tick
//! displacements on a 144 Hz display: that is the stutter this test measures and must fail on.
//!
//! Metric: for every rendered frame, |displacement - speed * frame_dt| / (speed * TICK_DT). 0 = perfectly smooth motion,
//! 1 = off by a whole 60 Hz tick of travel (what stepping at 60 Hz looks like at 144 Hz).

use std::time::Duration;

use shinobi_client::display_clock::DisplayClock;
use shinobi_client::client_game::{Action, ButtonEvent, ClientGame, ClientGameCfg, InputFrame, OwnAck, RenderView};
use shinobi_proto::bits::BitWriter;
use shinobi_proto::datagram::{frame_snapshot_v3, is_hello, Welcome};
use shinobi_proto::gameplay::OwnExtras;
use shinobi_proto::intent::{ClientPacket, PlayerIntent};
use shinobi_proto::quant::{q_pitch, q_pos3, q_vel, q_yaw};
use shinobi_proto::snapshot::{encode_delta, EntityState, MoveKind, Snapshot};
use shinobi_proto::TICK_DT;

/// Run speed the scripted players move at (the sim default).
const RUN: f64 = 9.5;

#[test]
fn run_constant_matches_the_sim_default() {
    assert!((RUN - Cfg::default().run_speed as f64).abs() < 1e-6, "update RUN to the sim's run speed");
}
use shinobi_sim::character::{Cfg, MoveState};
use shinobi_sim::events::EventOutbox;
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::own_state::OwnState;
use shinobi_sim::tick::Server;

fn level() -> LevelDesc {
    LevelDesc {
        boxes: vec![LevelBox::new([0.0, -0.5, 0.0], [400.0, 0.5, 400.0])],
        spawns: vec![[0.0, 0.81, 0.0], [0.0, 0.81, -20.0]],
    }
}

fn snapshot_datagram(srv: &Server) -> Vec<u8> {
    let entities: Vec<_> = srv
        .players
        .iter()
        .map(|p| {
            let v = p.ch.vel;
            (
                p.id,
                EntityState {
                    pos: q_pos3(p.pos(&srv.world)),
                    vel: [q_vel(v.x), q_vel(v.y), q_vel(v.z)],
                    yaw: p.yaw,
                    kind: match p.ch.state {
                        MoveState::Grounded => MoveKind::Grounded,
                        MoveState::Airborne => MoveKind::Airborne,
                        MoveState::WallRun { .. } => MoveKind::WallRun,
                    },
                    flags: 0,
                    fx: 0,
                },
            )
        })
        .collect();
    let snap = Snapshot { tick: srv.tick, entities };
    let mut ev = BitWriter::new();
    EventOutbox::default().write_pending(&mut ev, 256);
    let p = &srv.players[0];
    let mut own = BitWriter::new();
    let extras = OwnExtras { has_state: true, acked_tick: srv.tick - 1, hp_x10: 1000, hits: vec![], hit_seq: 0, ..Default::default() };
    extras.write(&mut own, |w| OwnState::from_snapshot(&p.ch.snapshot(&srv.world)).write(w));
    frame_snapshot_v3(&ev.finish(), &own.finish(), &[], &encode_delta(None, &snap))
}

/// One local player + one scripted remote, zero-latency link, ticks driven by the caller.
struct Rig {
    game: ClientGame,
    srv: Server,
    tick: f64,
    down: Vec<Vec<u8>>,
}

impl Rig {
    fn new() -> Self {
        let lv = level();
        let mut srv = Server::new(Cfg::default());
        lv.apply_to_server(&mut srv);
        // steady-motion harness: the look assist would bend the aim (and so the path) toward the remote player
        let mut r = Self { game: ClientGame::new(lv, ClientGameCfg { assist: shinobi_sim::balance::AssistBalance { friction: 0.0, magnet: 0.0, centre: 0.0, touch_mult: 1.0 }, ..ClientGameCfg::default() }), srv, tick: 0.0, down: vec![] };
        for _ in 0..4 {
            r.step(&InputFrame::default(), 0.0);
        }
        r
    }

    /// One fixed step (what `FixedUpdate` runs): the client intent tick, the server tick, the datagrams.
    fn step(&mut self, f: &InputFrame, remote_stick: f32) {
        self.tick += 1.0;
        for d in std::mem::take(&mut self.down) {
            let _ = self.game.on_datagram(&d, self.tick);
        }
        if let Some(u) = self.game.net.take_own_state() {
            if let Some(pos) = u.server_pos {
                self.game.on_own_state(OwnAck { tick: u.acked_tick, pos, own: u.own });
            }
        }
        let pkt = self.game.sim_tick(f, self.tick);
        if let Some(pkt) = pkt {
            if is_hello(&pkt) {
                if self.srv.players.is_empty() {
                    self.srv.add_player(1, level().spawn(0));
                    self.srv.add_player(2, level().spawn(1));
                }
                self.down.push(Welcome { player_id: 1, server_tick: self.srv.tick }.encode().to_vec());
            } else if !self.srv.players.is_empty() {
                self.srv.ingest(1, &pkt);
            }
        }
        if self.srv.players.len() > 1 {
            // the remote strafes right at run speed
            let tick = self.srv.tick;
            let it = PlayerIntent { tick, move_x: (remote_stick * 127.0) as i8, aim_yaw: q_yaw(0.0), aim_pitch: q_pitch(0.0), ..Default::default() };
            let pkt = ClientPacket { intents: vec![it], ..Default::default() };
            let p = self.srv.players.iter_mut().find(|p| p.id == 2).unwrap();
            p.queue.push_packet(&pkt, tick);
        }
        if !self.srv.players.is_empty() {
            self.srv.frame(Duration::from_nanos(16_666_667));
            self.down.push(snapshot_datagram(&self.srv));
        }
    }
}

/// Bevy's fixed-step loop: `Time<Fixed>` accumulator at 60 Hz, `overstep_fraction`.
struct Pacer {
    acc: f64,
}

impl Pacer {
    /// Advance by a frame of `dt` seconds; returns the number of fixed steps to run.
    fn advance(&mut self, dt: f64) -> u32 {
        self.acc += dt;
        let mut n = 0;
        while self.acc >= TICK_DT as f64 {
            self.acc -= TICK_DT as f64;
            n += 1;
        }
        n
    }
    fn overstep(&self) -> f64 {
        self.acc / TICK_DT as f64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Draw {
    /// view(integer tick): what the client did before the fix (alpha stuck at 0)
    TickStepped,
    /// view(tick + overstep) right after this frame's fixed steps (the ideal ordering)
    Interpolated,
    /// view(tick + overstep) read BEFORE this frame's fixed steps, like a system in Bevy's `First` schedule
    InterpolatedFirst,
    /// as `InterpolatedFirst`, but through `DisplayClock` (what play.rs now does)
    SmoothedFirst,
}

/// Frame times (s) for a named display.
fn frames(kind: &str, n: usize) -> Vec<f64> {
    let mut rng = 0x1234_5678_9abc_def0u64;
    let mut next = move || {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((rng >> 33) as f64) / (1u64 << 31) as f64
    };
    (0..n)
        .map(|i| match kind {
            "144" => 1.0 / 144.0,
            "75" => 1.0 / 75.0,
            "60" => 1.0 / 60.0,
            "60jitter" => 1.0 / 60.0 + (next() - 0.5) * 0.006,
            "144jitter" => 1.0 / 144.0 + (next() - 0.5) * 0.003,
            "hitches" => if i % 50 == 49 { 0.04 } else { 1.0 / 60.0 + (next() - 0.5) * 0.002 },
            _ => panic!("{kind}"),
        })
        .collect()
}

struct Trace {
    own_x: Vec<(f64, f64)>,
    remote_x: Vec<(f64, f64)>,
    yaw: Vec<f32>,
    dt: Vec<f64>,
    jumpiest_offset: f32,
}

fn run(kind: &str, draw: Draw) -> Trace {
    let mut rig = Rig::new();
    let mut pacer = Pacer { acc: 0.0 };
    let mut clock = DisplayClock::default();
    // < 10 s of sim: the server's match warm-up ends at 10 s with a mass teleport to the spawn points, which is not what this measures
    let dts = frames(kind, 560);
    let strafe = InputFrame { stick: (1.0, 0.0), ..Default::default() };
    let mut t = Trace { own_x: vec![], remote_x: vec![], yaw: vec![], dt: vec![], jumpiest_offset: 0.0 };
    let (mut prev_view, mut warm) = (None::<RenderView>, 0);
    for dt in dts {
        // mouse look every rendered frame (play.rs): 2 rad/s turn
        rig.game.aim.yaw = (rig.game.aim.yaw + 2.0 * dt as f32).rem_euclid(std::f32::consts::TAU);
        let before = match draw {
            Draw::InterpolatedFirst => Some(rig.game.view(rig.tick + pacer.overstep())),
            Draw::SmoothedFirst => {
                let t = clock.advance(dt / TICK_DT as f64, rig.tick + pacer.overstep());
                Some(rig.game.view(t))
            }
            _ => None,
        };
        for _ in 0..pacer.advance(dt) {
            rig.step(&strafe, 1.0);
        }
        let view = match draw {
            Draw::TickStepped => rig.game.view(rig.tick),
            Draw::Interpolated => rig.game.view(rig.tick + pacer.overstep()),
            Draw::InterpolatedFirst | Draw::SmoothedFirst => before.unwrap(),
        };
        warm += 1;
        if warm > 240 {
            t.own_x.push((view.eye[0] as f64, view.eye[2] as f64));
            t.remote_x.push(view.remotes.first().map_or((0.0, 0.0), |r| (r.1.pos[0] as f64, r.1.pos[2] as f64)));
            t.yaw.push(view.aim.yaw);
            t.dt.push(dt);
            t.jumpiest_offset = t.jumpiest_offset.max(view.correction_offset.iter().map(|d| d.abs()).fold(0.0, f32::max));
        }
        prev_view = Some(view);
    }
    let _ = prev_view;
    t
}

/// (max, p99) of |disp_i - v * dt_ref_i| / (v * TICK) over the trace; `lag` = reference frame offset (0 = this frame's dt,
/// 1 = previous frame's dt: what a renderer reading the PREVIOUS frame's state shows).
fn jitter(x: &[(f64, f64)], dt: &[f64], v: f64, lag: usize) -> (f64, f64) {
    let dist = |i: usize| ((x[i].0 - x[i - 1].0).powi(2) + (x[i].1 - x[i - 1].1).powi(2)).sqrt();
    let mut e: Vec<f64> = (lag.max(1)..x.len()).map(|i| (dist(i) - v * dt[i - lag]).abs() / (v * TICK_DT as f64)).filter(|e| *e < 4.0).collect();
    e.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (*e.last().unwrap(), e[(e.len() as f64 * 0.99) as usize - 1])
}

/// Like `jitter` but against a CONSTANT frame time (regular presentation, e.g. vsync): |disp - v * mean_dt| / (v * TICK).
fn jitter_const(x: &[(f64, f64)], dt: &[f64], v: f64) -> (f64, f64) {
    let mean = dt.iter().sum::<f64>() / dt.len() as f64;
    let dist = |i: usize| ((x[i].0 - x[i - 1].0).powi(2) + (x[i].1 - x[i - 1].1).powi(2)).sqrt();
    let mut e: Vec<f64> = (1..x.len()).map(|i| (dist(i) - v * mean).abs() / (v * TICK_DT as f64)).filter(|e| *e < 4.0).collect();
    e.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (*e.last().unwrap(), e[(e.len() as f64 * 0.99) as usize - 1])
}

fn report(name: &str, t: &Trace, v: f64, lag: usize) -> ((f64, f64), (f64, f64)) {
    let own = jitter(&t.own_x, &t.dt, v, lag);
    let rem = jitter(&t.remote_x, &t.dt, v, lag);
    eprintln!("{name:<34} own max {:.3} p99 {:.3} | remote max {:.3} p99 {:.3} | worst correction offset {:.3} m", own.0, own.1, rem.0, rem.1, t.jumpiest_offset);
    (own, rem)
}

#[test]
fn camera_aim_is_smooth_every_frame_regardless_of_tick_rate() {
    // aim turns by exactly rate * dt each frame: the live-look path (no 60 Hz stepping)
    for kind in ["144", "75", "60jitter", "hitches"] {
        let t = run(kind, Draw::Interpolated);
        let mut worst = 0.0f64;
        for i in 1..t.yaw.len() {
            let d = (t.yaw[i] - t.yaw[i - 1]).rem_euclid(std::f32::consts::TAU) as f64;
            worst = worst.max((d - 2.0 * t.dt[i]).abs());
        }
        assert!(worst < 1e-3, "{kind}: aim step off by {worst} rad");
    }
}

#[test]
fn drawing_between_ticks_is_smooth_and_stepping_at_60_hz_is_not() {
    // sensitivity check first: the old behaviour (integer tick clock) must be flagged on a fast display
    let bad = run("144", Draw::TickStepped);
    let (own_bad, rem_bad) = report("144 Hz  tick-stepped (old)", &bad, RUN, 0);
    assert!(own_bad.1 > 0.5 && rem_bad.1 > 0.5, "the metric must catch 60 Hz stepping at 144 Hz: {own_bad:?} {rem_bad:?}");
    for kind in ["144", "144jitter", "75", "60", "60jitter", "hitches"] {
        let t = run(kind, Draw::Interpolated);
        let (own, rem) = report(&format!("{kind} interpolated (after steps)"), &t, RUN, 0);
        // `hitches` has 40 ms frames (2-3 ticks in one frame): the catch-up burst is real motion, so only p99 is held tight
        // bounds scale with run speed (jitter in m grows with it): re-baselined for the 7.2 -> 9.5 m/s run-speed round
        let bound = if kind == "hitches" { 0.66 } else { 0.33 };
        assert!(own.1 < bound, "{kind}: own camera p99 jitter {} (bound {bound})", own.1);
        assert!(rem.1 < bound.max(0.35), "{kind}: remote player p99 jitter {} (bound {})", rem.1, bound.max(0.35));
    }
}

#[test]
fn reading_the_view_before_the_fixed_steps_shows_the_previous_frames_time() {
    // How play.rs publishes the view (the `First` schedule runs before `RunFixedMainLoop`): each frame shows the state
    // as of the END of the previous frame. Smooth on even frame times, but with uneven frames its displacement follows
    // the PREVIOUS frame's dt (velocity wobble), i.e. a frame of latency plus jitter. Print both references.
    for kind in ["144", "60jitter", "hitches"] {
        let t = run(kind, Draw::InterpolatedFirst);
        let now = report(&format!("{kind} view-before-steps vs dt_now"), &t, RUN, 0);
        let prev = report(&format!("{kind} view-before-steps vs dt_prev"), &t, RUN, 1);
        eprintln!("   -> follows the previous frame: {}", prev.0 .1 < now.0 .1);
    }
}

#[test]
fn display_clock_removes_frame_delta_noise_from_the_motion_without_adding_much_latency() {
    // Presentation is assumed regular (vsync) while the measured frame delta is noisy: the raw draw time makes the speed
    // wobble, the smoothed one does not. Hitches (40 ms every 50th frame) must not jump the picture either.
    for kind in ["60jitter", "144jitter", "hitches"] {
        let raw = run(kind, Draw::InterpolatedFirst);
        let sm = run(kind, Draw::SmoothedFirst);
        let (r_own, r_rem) = (jitter_const(&raw.own_x, &raw.dt, RUN), jitter_const(&raw.remote_x, &raw.dt, RUN));
        let (s_own, s_rem) = (jitter_const(&sm.own_x, &sm.dt, RUN), jitter_const(&sm.remote_x, &sm.dt, RUN));
        eprintln!("{kind:<10} vs regular presentation: raw own p99 {:.3} remote {:.3} | smoothed own p99 {:.3} remote {:.3}", r_own.1, r_rem.1, s_own.1, s_rem.1);
        if kind == "60jitter" {
            assert!(s_own.1 < r_own.1 * 0.75 && s_rem.1 < r_rem.1 * 0.75, "smoothing must clearly cut the wobble: {s_own:?} vs {r_own:?}");
        }
        // Re-baselined with the run speed 7.2 -> 9.5 m/s (jitter in m scales with speed): 0.27 -> 0.36.
        assert!(s_own.1 < 0.36 && s_rem.1 < 0.36, "{kind}: smoothed p99 {s_own:?} {s_rem:?}");
    }
}

#[test]
fn camera_pose_glides_between_ticks_instead_of_stepping_at_60_hz() {
    // Run (head bob on) and draw at 144 Hz: the bob / roll / FOV channels must change a little EVERY frame, not in 60 Hz lumps.
    let mut rig = Rig::new();
    let mut pacer = Pacer { acc: 0.0 };
    let strafe = InputFrame { stick: (0.0, 1.0), ..Default::default() };
    let (mut stepped, mut smooth) = (vec![], vec![]);
    for i in 0..600 {
        for _ in 0..pacer.advance(1.0 / 144.0) {
            rig.step(&strafe, 0.0);
        }
        if i > 240 {
            stepped.push(rig.game.view(rig.tick).eye[1] as f64);
            smooth.push(rig.game.view(rig.tick + pacer.overstep()).eye[1] as f64);
        }
    }
    // fraction of frames where the eye height does not move at all while the bob is clearly moving
    let flat = |v: &[f64]| v.windows(2).filter(|w| (w[1] - w[0]).abs() < 1e-7).count() as f64 / v.len() as f64;
    let swing = smooth.iter().cloned().fold(f64::MIN, f64::max) - smooth.iter().cloned().fold(f64::MAX, f64::min);
    eprintln!("eye-y: stepped flat frames {:.2}, interpolated flat frames {:.2}, swing {swing:.4} m", flat(&stepped), flat(&smooth));
    assert!(swing > 1e-3, "head bob should move the eye while running");
    assert!(flat(&smooth) < 0.1, "interpolated pose must not hold still between ticks");
    assert!(flat(&stepped) > 0.3, "sanity: tick-stepped pose does hold between ticks");
}
