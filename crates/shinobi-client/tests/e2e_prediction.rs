//! End to end, no sockets: `ClientGame` <-> real `shinobi_sim::tick::Server` through in-memory links with
//! latency, jitter, loss and reordering. The server side replicates what `shinobi-server` sends (v2 snapshot
//! framing with the owner's `OwnState`); everything client-side is the production `ClientGame`.

use std::time::Duration;

use shinobi_client::client_game::{Action, ButtonEvent, ClientGame, ClientGameCfg, InputFrame, OwnAck};
use shinobi_proto::bits::BitWriter;
use shinobi_proto::datagram::{frame_snapshot_v3, is_hello, Welcome};
use shinobi_proto::gameplay::OwnExtras;
use shinobi_proto::intent::ClientPacket;
use shinobi_proto::quant::{q_pos3, q_vel};
use shinobi_proto::snapshot::{encode_delta, EntityState, MoveKind, Snapshot};
use shinobi_sim::character::{Cfg, MoveState};
use shinobi_sim::events::EventOutbox;
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::own_state::OwnState;
use shinobi_sim::tick::Server;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
    fn unit(&mut self) -> f64 {
        self.next() as f64 / (1u64 << 31) as f64
    }
    fn pick(&mut self, n: u32) -> u32 {
        self.next() % n
    }
}

/// One direction of an unreliable link, measured in sim ticks.
struct Link {
    q: Vec<(f64, Vec<u8>)>,
    rng: Lcg,
    one_way: f64,
    jitter: f64,
    loss: f64,
}

impl Link {
    fn new(seed: u64, one_way: f64, jitter: f64, loss: f64) -> Self {
        Self { q: vec![], rng: Lcg(seed), one_way, jitter, loss }
    }
    fn send(&mut self, now: f64, b: Vec<u8>) {
        if self.rng.unit() < self.loss {
            return;
        }
        let t = now + self.one_way + (self.rng.unit() * 2.0 - 1.0) * self.jitter;
        self.q.push((t, b));
    }
    fn recv(&mut self, now: f64) -> Vec<Vec<u8>> {
        let mut due: Vec<_> = self.q.iter().filter(|(t, _)| *t <= now).cloned().collect();
        self.q.retain(|(t, _)| *t > now);
        due.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        due.into_iter().map(|(_, b)| b).collect()
    }
}

fn level() -> LevelDesc {
    LevelDesc {
        boxes: vec![
            LevelBox::new([0.0, -0.5, 0.0], [200.0, 0.5, 200.0]),  // floor
            LevelBox::new([3.0, 6.0, -60.0], [1.0, 6.0, 70.0]),    // wall, face at x = 2
            LevelBox::new([-4.0, 0.15, -12.0], [3.0, 0.15, 3.0]),  // 0.3 m step
            LevelBox::new([-20.0, -0.5, -20.0], [10.0, 0.5, 10.0]).rolled(-0.35), // ramp
        ],
        spawns: vec![[0.0, 0.81, 0.0]],
    }
}

/// What the server sends the owner after executing tick `server.tick - 1`.
fn snapshot_datagram(srv: &Server) -> Vec<u8> {
    let p = &srv.players[0];
    let v = p.ch.vel;
    let st = EntityState {
        pos: q_pos3(p.pos(&srv.world)),
        vel: [q_vel(v.x), q_vel(v.y), q_vel(v.z)],
        yaw: p.yaw,
        kind: match p.ch.state {
            MoveState::Grounded => MoveKind::Grounded,
            MoveState::Airborne => MoveKind::Airborne,
            MoveState::WallRun { .. } => MoveKind::WallRun,
        },
        flags: 0,
    };
    let snap = Snapshot { tick: srv.tick, entities: vec![(p.id, st)] };
    let mut ev = BitWriter::new();
    EventOutbox::default().write_pending(&mut ev, 256);
    let mut own = BitWriter::new();
    let extras = OwnExtras { has_state: true, acked_tick: srv.tick - 1, hp_x10: 1000, hits: vec![] };
    extras.write(&mut own, |w| OwnState::from_snapshot(&p.ch.snapshot(&srv.world)).write(w));
    frame_snapshot_v3(&ev.finish(), &own.finish(), &[], &encode_delta(None, &snap))
}

fn ev(action: Action, down: bool) -> ButtonEvent {
    ButtonEvent { action, down }
}

/// Opening: strafe into the wall, jump + cling + forward (wall-run); then seeded random play.
fn frame(t: u32, rng: &mut Lcg, stick: &mut (f32, f32), held: &mut [bool; 3]) -> InputFrame {
    let mut f = InputFrame::default();
    if t == 120 {
        *stick = (1.0, 0.0);
    } else if t == 140 {
        *stick = (0.0, 1.0);
        f.buttons.push(ev(Action::Jump, true));
        f.buttons.push(ev(Action::Cling, true));
        held[0] = true;
        held[1] = true;
    } else if t > 200 && t % 35 == 0 {
        *stick = [(-1.0, 0.0), (0.0, 1.0), (1.0, 0.0), (0.0, -1.0), (0.7, 0.7), (0.0, 0.0)][rng.pick(6) as usize];
        for (i, action) in [(0, Action::Jump), (1, Action::Cling), (2, Action::Slide)] {
            let want = rng.pick(3) == 0;
            if want != held[i] {
                f.buttons.push(ev(action, want));
                held[i] = want;
            }
        }
        if rng.pick(2) == 0 {
            f.buttons.push(ev(Action::Jump, true)); // a fresh press (edge) even if already "held"
        }
    } else if t > 200 && t % 35 == 18 && rng.pick(2) == 0 {
        f.buttons.push(ev(Action::Jump, false));
        f.buttons.push(ev(Action::Jump, true)); // second press mid-air: double-jump candidates
    }
    if t > 200 && t % 55 == 7 {
        f.buttons.push(ev(if rng.pick(2) == 0 { Action::Dash } else { Action::Roll }, true));
        f.buttons.push(ev(if rng.pick(2) == 0 { Action::Dash } else { Action::Roll }, false));
    }
    if t > 200 {
        f.look = ((rng.pick(11) as f32 - 5.0) * 0.6, 0.0); // slow yaw wander
    }
    f.stick = *stick;
    f
}

struct Outcome {
    max_pred_err: f32,
    /// Worst prediction error over the last 120 predicted ticks (did it converge after any corrections?).
    tail_err: f32,
    max_display_step: f32,
    corrections: u32,
    predicted_ticks: usize,
    strikes: u32,
    /// Server ticks where the player moved further in one tick than any legal mover can (a teleport).
    teleports: Vec<u32>,
    /// (tick, |predicted - server|) for ticks after the first teleport.
    after_teleport: Vec<(u32, f32)>,
    saw: [bool; 3], // wall-run, airborne, grounded on the server
    fx: Fx,
}

/// Counts of local FX edges seen through `view()`.
#[derive(Default, Debug)]
struct Fx {
    jumped: u32,
    double_jumped: u32,
    landed: u32,
    wall_run_started: u32,
    dash_started: u32,
    slide_started: u32,
    slashes: u32,
    lunges: u32,
    throws: u32,
    max_throw_cd: f32,
    max_slice_cd: f32,
    hp_seen: bool,
    max_landing: f32,
    dash_ends: u32,
}

fn run(one_way: f64, jitter: f64, loss: f64, secs: u32) -> Outcome {
    run_ex(one_way, jitter, loss, secs, None, Combat::None)
}

#[derive(Clone, Copy, PartialEq)]
enum Combat {
    None,
    /// Shuriken taps (no movement effect on the server).
    Throw,
    /// Katana slices (the server's lunge moves the player; the client does not predict that yet).
    Slice,
}

/// `join2`: tick at which a second (idle) player joins; with 2 players the match warmup countdown starts.
fn run_ex(one_way: f64, jitter: f64, loss: f64, secs: u32, join2: Option<u32>, combat: Combat) -> Outcome {
    let level = level();
    let mut game = ClientGame::new(level.clone(), ClientGameCfg::default());
    let mut srv = Server::new(Cfg::default());
    level.apply_to_server(&mut srv);
    let mut up = Link::new(1, one_way, jitter, loss);
    let mut down = Link::new(2, one_way, jitter, loss);
    let (mut rng, mut stick, mut held) = (Lcg(0xFEED), (0.0f32, 0.0f32), [false; 3]);

    let mut server_pos: std::collections::HashMap<u32, [f32; 3]> = std::collections::HashMap::new();
    let mut pred_pos: std::collections::HashMap<u32, [f32; 3]> = std::collections::HashMap::new();
    let mut out = Outcome { max_pred_err: 0.0, tail_err: 0.0, max_display_step: 0.0, corrections: 0, predicted_ticks: 0, strikes: 0, teleports: vec![], after_teleport: vec![], saw: [false; 3], fx: Fx::default() };
    let mut last_display: Option<[f32; 3]> = None;

    for i in 0..(secs * 60 + 300) {
        let now = i as f64;
        for d in down.recv(now) {
            let _ = game.on_datagram(&d, now);
        }
        if let Some(u) = game.net.take_own_state() {
            if let Some(pos) = u.server_pos {
                game.on_own_state(OwnAck { tick: u.acked_tick, pos, own: u.own });
            }
        }
        let mut f = frame(i, &mut rng, &mut stick, &mut held);
        if i > 200 && i % 90 == 10 {
            match combat {
                Combat::Throw => {
                    f.buttons.push(ev(Action::Fire, true));
                    f.buttons.push(ev(Action::Fire, false));
                }
                Combat::Slice => {
                    f.buttons.push(ev(Action::Slice, true));
                    f.buttons.push(ev(Action::Slice, false));
                }
                Combat::None => {}
            }
        }
        if let Some(pkt) = game.sim_tick(&f, now) {
            if !is_hello(&pkt) {
                if let (Some(p), Ok(cp)) = (game.predicted_pos(), ClientPacket::decode(&pkt)) {
                    pred_pos.insert(cp.intents[0].tick, p);
                }
            }
            up.send(now, pkt);
        }
        // draw: the displayed position must never pop
        let view = game.view(now);
        {
            let x = &view.fx;
            out.fx.jumped += x.jumped as u32;
            out.fx.double_jumped += x.double_jumped as u32;
            out.fx.landed += x.landed as u32;
            out.fx.wall_run_started += x.wall_run_started as u32;
            out.fx.dash_started += x.dash_started as u32;
            out.fx.slide_started += x.slide_started as u32;
            out.fx.slashes += x.slash.is_some() as u32;
            out.fx.lunges += x.slash.map_or(false, |s| s.lunge) as u32;
            out.fx.throws += x.threw.is_some() as u32;
            out.fx.max_throw_cd = out.fx.max_throw_cd.max(x.throw_cooldown);
            out.fx.max_slice_cd = out.fx.max_slice_cd.max(x.slice_cooldown);
            out.fx.hp_seen |= view.hp.is_some();
            out.fx.max_landing = out.fx.max_landing.max(x.landing_speed);
            out.fx.dash_ends += x.dash_ended as u32;
        }
        if let (Some(d), true) = (view.local_pos, game.predicting()) {
            if let Some(l) = last_display {
                let step = (0..3).map(|k| (d[k] - l[k]).powi(2)).sum::<f32>().sqrt();
                out.max_display_step = out.max_display_step.max(step);
            }
            last_display = Some(d);
        }

        for pkt in up.recv(now) {
            if is_hello(&pkt) {
                if srv.players.is_empty() {
                    srv.add_player(1, level.spawn(0));
                }
                down.send(now, Welcome { player_id: 1, server_tick: srv.tick }.encode().to_vec());
            } else if !srv.players.is_empty() {
                srv.ingest(1, &pkt);
            }
        }
        if join2 == Some(i) {
            srv.add_player(2, level.spawn(0));
        }
        if !srv.players.is_empty() {
            srv.frame(Duration::from_nanos(16_666_667));
            let executed = srv.tick - 1;
            let sp = srv.players[0].pos(&srv.world);
            if let Some(prev) = server_pos.get(&executed.wrapping_sub(1)) {
                let d = (0..3).map(|k| (sp[k] - prev[k]).powi(2)).sum::<f32>().sqrt();
                if d > 2.0 {
                    out.teleports.push(executed);
                }
            }
            server_pos.insert(executed, sp);
            out.strikes = srv.players[0].guard.strikes;
            match srv.players[0].ch.state {
                MoveState::WallRun { .. } => out.saw[0] = true,
                MoveState::Airborne => out.saw[1] = true,
                MoveState::Grounded => out.saw[2] = true,
            }
            down.send(now, snapshot_datagram(&srv));
        }
    }
    out.corrections = game.corrections;
    if let Some(&t0) = out.teleports.first() {
        let mut ticks: Vec<_> = pred_pos.keys().copied().filter(|t| *t > t0).collect();
        ticks.sort_unstable();
        for t in ticks {
            if let Some(sv) = server_pos.get(&t) {
                let p = pred_pos[&t];
                out.after_teleport.push((t, (0..3).map(|k| (p[k] - sv[k]).powi(2)).sum::<f32>().sqrt()));
            }
        }
    }
    let last = pred_pos.keys().copied().max().unwrap_or(0);
    for (tick, p) in &pred_pos {
        if let Some(s) = server_pos.get(tick) {
            let e = (0..3).map(|k| (p[k] - s[k]).powi(2)).sum::<f32>().sqrt();
            out.max_pred_err = out.max_pred_err.max(e);
            if *tick + 120 > last && *tick + 40 < last {
                out.tail_err = out.tail_err.max(e);
            }
            out.predicted_ticks += 1;
        }
    }
    out
}

#[test]
fn clean_link_prediction_matches_server_exactly_enough() {
    let o = run(0.0, 0.0, 0.0, 20);
    eprintln!("clean: err {:.4} step {:.3} corr {} n {}", o.max_pred_err, o.max_display_step, o.corrections, o.predicted_ticks);
    assert!(o.predicted_ticks > 1000);
    assert!(o.saw.iter().all(|s| *s), "script must cover wall-run / air / ground: {:?}", o.saw);
    assert!(o.max_pred_err < 0.05, "pred err {}", o.max_pred_err);
    assert_eq!(o.strikes, 0);
}

#[test]
fn lossy_laggy_jittery_link_stays_within_5cm_with_no_visible_snaps() {
    // 80 ms RTT (40 ms one way) +- 15 ms jitter, 5% loss each way, reordering from the jitter
    let o = run(2.4, 0.9, 0.05, 20);
    eprintln!("lossy: err {:.4} step {:.3} corr {} n {}", o.max_pred_err, o.max_display_step, o.corrections, o.predicted_ticks);
    assert!(o.predicted_ticks > 1000, "predicted ticks {}", o.predicted_ticks);
    assert!(o.max_pred_err < 0.05, "predicted vs server error {} m", o.max_pred_err);
    // fastest legal mover is a dash at 30 m/s = 0.5 m/tick; anything past that + 5 cm is a visible pop
    assert!(o.max_display_step < 0.5 + 0.05, "displayed position jumped {} m in one tick", o.max_display_step);
    assert_eq!(o.strikes, 0, "anti-cheat must not strike an honest laggy client");
}

#[test]
fn match_start_teleport_is_followed_without_strikes_or_kicks() {
    // a second player joins at 1 s; the 10 s warmup countdown then ends in a mass reset to spawn points,
    // a server-side teleport of the local player. (Graphics saw z -36 -> -1.6 in a headless run.)
    let o = run_ex(2.4, 0.9, 0.05, 20, Some(60), Combat::None);
    eprintln!("teleports at {:?}, corrections {}", o.teleports, o.corrections);
    assert!(!o.teleports.is_empty(), "expected the warmup -> live reset to teleport the player");
    assert!(o.teleports.len() <= 2, "teleports {:?}", o.teleports);
    assert!(o.corrections >= 1, "the client must be corrected, not silently diverge");
    assert_eq!(o.strikes, 0, "a legitimate server teleport must not look like a speed hack");
    // within RTT + lead (~20 ticks) of the teleport the prediction is back on the server
    let settled: Vec<_> = o.after_teleport.iter().filter(|(t, _)| *t > o.teleports[0] + 30).collect();
    assert!(!settled.is_empty());
    let worst = settled.iter().map(|(_, e)| *e).fold(0.0f32, f32::max);
    assert!(worst < 0.05, "30+ ticks after the teleport the prediction is still {worst} m off");
}

#[test]
fn local_fx_edges_and_flags_cover_the_kit() {
    let o = run(2.4, 0.9, 0.05, 20);
    eprintln!("fx {:?}", o.fx);
    assert!(o.fx.jumped > 0 && o.fx.landed > 0, "{:?}", o.fx);
    assert!(o.fx.wall_run_started > 0, "the scripted wall approach must trigger a wall-run: {:?}", o.fx);
    assert!(o.fx.dash_started > 0 && o.fx.double_jumped > 0 && o.fx.slide_started > 0, "{:?}", o.fx);
    assert!(o.fx.hp_seen, "v3 own extras reach the view");
    assert!(o.fx.max_landing > 4.0, "landings carry their fall speed for the camera dip: {:?}", o.fx);
    assert!(o.fx.dash_ends > 0 && o.fx.dash_ends <= o.fx.dash_started, "dash-end edges: {:?}", o.fx);
    assert_eq!(o.max_pred_err, 0.0, "cosmetic camera feel must never change what the simulation does");
    // edges are per interval, never inflated by replays: at most one jump per 3 ticks is physically possible
    assert!(o.fx.jumped < 20 * 60 / 3);
}

#[test]
fn predicted_throws_have_a_cooldown_mirror_and_do_not_disturb_prediction() {
    let o = run_ex(2.4, 0.9, 0.05, 20, None, Combat::Throw);
    eprintln!("throw fx {:?} err {}", o.fx, o.max_pred_err);
    assert!(o.fx.throws >= 10, "{:?}", o.fx);
    assert!(o.fx.max_throw_cd > 0.5, "cooldown mirror engaged: {}", o.fx.max_throw_cd);
    assert!(o.max_pred_err < 0.05, "a throw must not move the character: {}", o.max_pred_err);
}

#[test]
fn predicted_slices_show_a_slash_and_arm_the_katana_cooldown() {
    let o = run_ex(2.4, 0.9, 0.05, 20, None, Combat::Slice);
    eprintln!("slice fx {:?}", o.fx);
    assert!(o.fx.slashes >= 10 && o.fx.lunges >= 3, "{:?}", o.fx);
    assert!(o.fx.max_slice_cd > 0.9, "cooldown mirror engaged: {}", o.fx.max_slice_cd);
    // the lunge is predicted now: a slice must not rubber-band the player
    eprintln!("slice: pred err {} corrections {} step {}", o.max_pred_err, o.corrections, o.max_display_step);
    assert!(o.max_pred_err < 0.05, "predicted vs server error {} m", o.max_pred_err);
    assert_eq!(o.corrections, 0, "no corrections for lunges the client predicted");
    assert_eq!(o.strikes, 0);
}

#[test]
fn replays_that_contain_slices_converge_under_heavy_loss_and_lateness() {
    // 10% loss + jitter of +-30 ms (wider than the lead margin): some intents arrive late, the server repeats
    // stale ones (edge bits stripped, so it also skips slices the client predicted), and the client must replay
    // lunges from the restored katana cooldown. (At 20%+ loss / +-37 ms the stream is wrong faster than any
    // client can correct it; that is a network problem, not a prediction one.)
    let o = run_ex(2.4, 1.8, 0.10, 20, None, Combat::Slice);
    eprintln!("heavy: corrections {} tail_err {} worst {}", o.corrections, o.tail_err, o.max_pred_err);
    assert!(o.corrections >= 3, "the stress must actually cause corrections: {}", o.corrections);
    assert!(o.tail_err < 0.05, "prediction must be back on the server after corrections: {}", o.tail_err);
}
