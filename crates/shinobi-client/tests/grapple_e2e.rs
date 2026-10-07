//! Grapple end to end on the real 8v8 map: `ClientGame` <-> real `Server` through lossy, jittery links.
//! A bot scripts full grapple cycles (hook, reel, swing, jump-release, re-fire) and the test measures how far the
//! client's prediction strays from the server and whether the reconciler ever has to move the player visibly.

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
use shinobi_sim::level::LevelDesc;
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
}

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
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels/8v8.lvl");
    LevelDesc::from_text(&std::fs::read_to_string(path).expect("levels/8v8.lvl")).expect("8v8.lvl parses")
}

fn snapshot_datagram(srv: &Server, margin: i8) -> Vec<u8> {
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
        fx: 0,
    };
    let snap = Snapshot { tick: srv.tick, entities: vec![(p.id, st)] };
    let mut ev = BitWriter::new();
    EventOutbox::default().write_pending(&mut ev, 256);
    let mut own = BitWriter::new();
    let extras = OwnExtras { has_state: true, acked_tick: srv.tick - 1, hp_x10: 1000, hits: vec![], hit_seq: 0, intent_complete: srv.players[0].queue.complete_tick(), lead_margin: margin,
        echo_seq: srv.players[0].last_pkt.map_or(0, |(s, _)| s),
        echo_age: srv.players[0].last_pkt.map_or(0, |(_, t)| srv.tick.saturating_sub(t).min(255) as u8),
        ..Default::default()
    };
    extras.write(&mut own, |w| OwnState::from_snapshot(&p.ch.snapshot(&srv.world)).write(w));
    frame_snapshot_v3(&ev.finish(), &own.finish(), &[], &encode_delta(None, &snap))
}

fn ev(action: Action, down: bool) -> ButtonEvent {
    ButtonEvent { action, down }
}

/// Slabs hanging over the lane mouths on the Cyan side (bottom faces at y = 10.6): the bot keeps hooking them.
/// Intent lead: must cover one-way delay + jitter + the clock estimate lag (default 8 is sized for 80 ms RTT).
const LEAD: u32 = 11;

const SLABS: [[f32; 3]; 2] = [[30.0, 10.6, -24.0], [-30.0, 10.6, -24.0]];

#[derive(Default, Debug)]
struct Out {
    /// |predicted - server| for every predicted tick the server also executed.
    errs: Vec<f32>,
    max_display_step: f32,
    max_visual_offset: f32,
    corrections: u32,
    grapple_ticks: u32,
    server_grapple_ticks: u32,
    attaches: u32,
    releases: u32,
    misses: u32,
    ropes_seen: u32,
    /// (tick, lead) every time the adaptive intent lead changed, and where it ended.
    lead_log: Vec<(u32, u32)>,
    final_lead: Option<u32>,
    /// Client-measured round trip (ms) at the end, and the correction count it would report.
    final_ping: Option<f32>,
}

impl Out {
    fn pct(&self, p: f64) -> f32 {
        let mut v = self.errs.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[((v.len() - 1) as f64 * p) as usize]
    }
    fn max(&self) -> f32 {
        self.errs.iter().copied().fold(0.0, f32::max)
    }
}

fn run(one_way: f64, jitter: f64, loss: f64, seed: u64, ticks: u32) -> Out {
    run_ex(one_way, jitter, loss, seed, ticks, true, LEAD)
}

fn run_ex(one_way: f64, jitter: f64, loss: f64, seed: u64, ticks: u32, hook: bool, lead: u32) -> Out {
    let lv = level();
    let mut game = ClientGame::new(lv.clone(), ClientGameCfg { lead_ticks: lead, ..ClientGameCfg::default() });
    let mut srv = Server::new(Cfg::default());
    lv.apply_to_server(&mut srv);
    let start = [30.0, 0.81, -30.0];
    let mut up = Link::new(seed, one_way, jitter, loss);
    let mut down = Link::new(seed ^ 0xABCD, one_way, jitter, loss);
    let mut rng = Lcg(seed ^ 0x77);
    let mut server_pos: std::collections::HashMap<u32, [f32; 3]> = std::collections::HashMap::new();
    let mut pred_pos: std::collections::HashMap<u32, [f32; 3]> = std::collections::HashMap::new();
    let mut out = Out::default();
    let mut last_display: Option<[f32; 3]> = None;
    let mut cycle_t: Option<u32> = None;
    let mut cycle = 0u32;
    let mut target = (0.0f32, 0.0f32);

    for i in 0..ticks {
        let now = i as f64;
        for d in down.recv(now) {
            let _ = game.on_datagram(&d, now);
        }
        if let Some(u) = game.net.take_own_state() {
            if let Some(pos) = u.server_pos {
                game.on_own_state(OwnAck { tick: u.acked_tick, pos, own: u.own });
            }
        }

        // ---- bot: every 140 ticks aim at a slab from wherever we are, fire, swing, jump-release ----
        let mut f = InputFrame::default();
        if let Some(p) = game.predicted_pos().filter(|_| game.predicting()) {
            let t0 = *cycle_t.get_or_insert(i + 30);
            let k = i as i64 - t0 as i64;
            if k >= 0 && k % 140 == 0 {
                cycle += 1;
                let s = SLABS[(cycle as usize) % 2];
                let eye = [p[0], p[1] + 1.4, p[2]];
                let (dx, dy, dz) = (s[0] - eye[0], s[1] - eye[1], s[2] - eye[2]);
                target = ((-dx).atan2(-dz).rem_euclid(std::f32::consts::TAU), dy.atan2((dx * dx + dz * dz).sqrt().max(0.01)));
            }
            // a human cannot turn 180 degrees in one tick (the server clamps it and strikes): slew <= 0.35 rad / tick
            let dyaw = (target.0 - game.aim.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            game.aim.yaw = (game.aim.yaw + dyaw.clamp(-0.35, 0.35)).rem_euclid(std::f32::consts::TAU);
            game.aim.pitch += (target.1 - game.aim.pitch).clamp(-0.35, 0.35);
            if hook && k >= 0 && k % 140 == 12 {
                f.buttons.push(ev(Action::Grapple, true));
                f.buttons.push(ev(Action::Grapple, false));
            }
            // swing: strafe while attached, jump to release after 55 ticks, dash once for composition
            if k >= 0 {
                let ph = k % 140;
                f.stick = if (20..55).contains(&ph) { (1.0, 0.0) } else if (60..100).contains(&ph) { (-1.0, 0.3) } else { (0.0, 0.0) };
                if ph == 55 {
                    f.buttons.push(ev(Action::Jump, true));
                    f.buttons.push(ev(Action::Jump, false));
                }
                if ph == 30 && cycle % 3 == 0 {
                    f.buttons.push(ev(Action::Dash, true));
                    f.buttons.push(ev(Action::Dash, false));
                }
                if hook && ph == 100 && rng.next() % 4 == 0 {
                    // E spam
                    for _ in 0..3 {
                        f.buttons.push(ev(Action::Grapple, true));
                        f.buttons.push(ev(Action::Grapple, false));
                    }
                }
            }
        }
        if let Some(l) = game.net.lead() {
            if out.lead_log.last().map_or(true, |x| x.1 != l) {
                out.lead_log.push((i, l));
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
        let view = game.view(now);
        out.attaches += view.fx.grapple_attached.is_some() as u32;
        out.releases += view.fx.grapple_released as u32;
        out.misses += view.fx.grapple_missed as u32;
        out.grapple_ticks += view.fx.grappling as u32;
        out.ropes_seen += view.rope.is_some() as u32;
        out.max_visual_offset = out.max_visual_offset.max(view.correction_offset.iter().map(|d| d.abs()).fold(0.0, f32::max));
        if let (Some(d), true) = (view.local_pos, game.predicting()) {
            if let Some(l) = last_display {
                out.max_display_step = out.max_display_step.max((0..3).map(|k| (d[k] - l[k]).powi(2)).sum::<f32>().sqrt());
            }
            last_display = Some(d);
        }

        for pkt in up.recv(now) {
            if is_hello(&pkt) {
                if srv.players.is_empty() {
                    srv.add_player(1, start);
                }
                down.send(now, Welcome { player_id: 1, server_tick: srv.tick }.encode().to_vec());
            } else if !srv.players.is_empty() {
                srv.ingest(1, &pkt);
            }
        }
        if !srv.players.is_empty() {
            let rep = srv.frame(Duration::from_nanos(16_666_667));
            if !rep.kicked.is_empty() {
                panic!("server kicked {:?} at tick {i}; client pos {:?}", rep.kicked, game.predicted_pos());
            }
            let executed = srv.tick - 1;
            server_pos.insert(executed, srv.players[0].pos(&srv.world));
            out.server_grapple_ticks += srv.players[0].ch.grapple.is_some() as u32;
            // what the real server reports in the owner block: headroom of the newest intent since the last report
            let margin = srv.players[0].queue.take_margin().map_or(15, |m| m.clamp(-16, 14) as i8);
            down.send(now, snapshot_datagram(&srv, margin));
        }
    }
    out.corrections = game.corrections;
    out.final_lead = game.net.lead();
    out.final_ping = game.net.ping_ms();
    let last = pred_pos.keys().copied().max().unwrap_or(0);
    for (tick, p) in &pred_pos {
        // skip the very first ticks (prediction warm-up) and the last window (acks still in flight)
        if *tick < 60 || *tick + 40 > last {
            continue;
        }
        if let Some(s) = server_pos.get(tick) {
            out.errs.push((0..3).map(|k| (p[k] - s[k]).powi(2)).sum::<f32>().sqrt());
        }
    }
    out
}

fn report(name: &str, o: &Out) {
    eprintln!(
        "{name}: n {} err p50 {:.4} p99 {:.4} max {:.4} | visual_offset max {:.3} display step max {:.3} | corrections {} | grapple ticks client {} server {} | attaches {} releases {} misses {} ropes {}",
        o.errs.len(), o.pct(0.5), o.pct(0.99), o.max(), o.max_visual_offset, o.max_display_step, o.corrections, o.grapple_ticks, o.server_grapple_ticks, o.attaches, o.releases, o.misses, o.ropes_seen
    );
}

#[test]
fn clean_link_grapple_prediction_is_exact() {
    let o = run_ex(0.0, 0.0, 0.0, 1, 1500, true, ClientGameCfg::default().lead_ticks);
    report("clean", &o);
    assert!(o.attaches >= 4 && o.releases >= 4, "the bot must actually grapple: {o:?}");
    assert!(o.server_grapple_ticks > 100, "server saw the rope");
    assert!(o.max() < 0.02, "max err {}", o.max());
    assert_eq!(o.corrections, 0, "no corrections on a clean link");
    assert!(o.max_visual_offset < 0.01);
}

#[test]
fn lossy_jittery_links_keep_grapple_prediction_tight_and_never_snap() {
    // 50 ms one way, +-40 ms jitter (reordering), 0 / 5 / 20 % loss each way, three seeds each. One-shot actions
    // (zip, dash, jump, build) are re-sent in every datagram until the server confirms them (`intent_complete`) and a
    // late one is applied exactly once on the next tick, so even at 20 % loss the worst error stays at decimetres
    // (it used to reach 2.4 m with the rope on one side for a whole RTT).
    // max/offset bounds are sized for far hooks that reel at up to 140 m/s (1000 m range, `grapple_far_speed`): a one-tick timing
    // difference on a lossy link is 2.3 m at that speed (it was 0.33 m at the old 20 m/s cap); p99 is still ~0
    for (loss, p99_bound, max_bound, snap_bound) in [(0.0, 0.01, 4.0, 3.0), (0.05, 0.02, 4.0, 3.0), (0.20, 0.05, 4.5, 3.5)] {
        let (mut worst99, mut worst_max, mut worst_off, mut worst_step, mut corr, mut hooks) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0u32, 0u32);
        for seed in [11u64, 12, 13] {
            let o = run(3.0, 2.4, loss, seed + (loss * 1000.0) as u64, 3000); // 3000 ticks: slower movement means fewer grapple attempts per tick
            report(&format!("loss {:.0}% seed {seed}", loss * 100.0), &o);
            assert!(o.attaches >= 4, "the bot must actually grapple at {loss}: {o:?}"); // >= 4: slower movement, fewer hooks per run
            assert!(o.server_grapple_ticks > 100, "server saw ropes at loss {loss}");
            worst99 = worst99.max(o.pct(0.99));
            worst_max = worst_max.max(o.max());
            worst_off = worst_off.max(o.max_visual_offset);
            worst_step = worst_step.max(o.max_display_step);
            corr += o.corrections;
            hooks += o.attaches;
        }
        eprintln!("== loss {:.0}%: worst p99 {worst99:.4} max {worst_max:.4} visual offset {worst_off:.3} step {worst_step:.3} corrections {corr} hooks {hooks}", loss * 100.0);
        assert!(worst99 < p99_bound, "loss {loss}: p99 err {worst99} (bound {p99_bound})");
        assert!(worst_max < max_bound, "loss {loss}: max err {worst_max} (bound {max_bound})");
        assert!(worst_off < snap_bound, "loss {loss}: visual offset {worst_off} (bound {snap_bound})");
        // the fastest legal mover is a far-hook reel: 140 m/s = 2.3 m per tick
        assert!(worst_step < 2.6, "loss {loss}: displayed position jumped {worst_step} m in one tick");
    }
}

#[test]
fn baseline_without_grapple_same_script() {
    for (loss, seed) in [(0.0, 11u64), (0.05, 12), (0.20, 13)] {
        let o = run_ex(3.0, 2.4, loss, seed, 2400, false, LEAD);
        report(&format!("NO HOOK loss {:.0}%", loss * 100.0), &o);
    }
}

/// The intent lead adapts to the measured headroom instead of a hand-tuned constant: from any starting value on a
/// 50 ms +-40 ms link it settles in a sane band within a couple of seconds, stops changing (no oscillation), and the
/// prediction error stays small afterwards.
#[test]
fn adaptive_lead_converges_from_any_start_without_oscillating() {
    for start in [4u32, 8, 16] {
        for loss in [0.0, 0.20] {
            let o = run_ex(3.0, 2.4, loss, 21 + start as u64, 2400, true, start);
            report(&format!("lead from {start}, loss {:.0}%", loss * 100.0), &o);
            let fin = o.final_lead.expect("lead known");
            assert!((8..=15).contains(&fin), "start {start} loss {loss}: settled at {fin} (log {:?})", o.lead_log);
            let late_changes = o.lead_log.iter().filter(|(t, _)| *t > 600).count();
            assert!(late_changes <= 2, "start {start} loss {loss}: still changing after 10 s: {:?}", o.lead_log);
            assert!(o.max() < 4.5, "start {start} loss {loss}: max err {} (log {:?})", o.max(), o.lead_log); // 140 m/s far hooks (was 0.35 at 20 m/s)
        }
    }
}

/// On a fast link the lead stays small (no wasted headroom) and prediction is exact.
#[test]
fn fast_link_keeps_the_lead_small() {
    let o = run_ex(0.0, 0.0, 0.0, 5, 1500, true, 8);
    let fin = o.final_lead.expect("lead known");
    assert!(fin <= 8, "lead {fin} on a zero-latency link");
    assert!(o.max() < 0.02 && o.corrections == 0, "{o:?}");
}

/// The client measures its own round trip from the server's echo: on a 50 ms one-way link (100 ms RTT, a tick is
/// 16.7 ms) the smoothed ping settles near 100 ms, and on a zero-latency link near 0. The server sees the client's report.
#[test]
fn client_measured_ping_matches_the_link() {
    let slow = run_ex(3.0, 0.0, 0.0, 5, 900, false, 8);
    let fast = run_ex(0.0, 0.0, 0.0, 5, 900, false, 8);
    let (s, f) = (slow.final_ping.expect("ping measured"), fast.final_ping.expect("ping measured"));
    // 3 ticks each way = 100 ms; the harness delivers on whole ticks, so allow a tick or two of slack
    assert!((80.0..140.0).contains(&s), "50 ms one way measured as {s} ms RTT");
    assert!(f < 40.0, "zero-latency link measured as {f} ms");
}

/// With 40 ms of jitter the echo names the newest packet to ARRIVE at the server and the client skips snapshots that were
/// overtaken, so both directions favour the fast paths: the figure is the latency of the FRESHEST data (what the player
/// feels), about half the mean round trip here. It must stay in a sane band (documented in PLAYTEST.md).
#[test]
fn client_measured_ping_under_jitter_is_biased_low_but_sane() {
    let o = run_ex(3.0, 2.4, 0.0, 9, 1500, false, 8);
    let p = o.final_ping.expect("ping measured");
    eprintln!("ping under 50 ms +-40 ms jitter (true RTT 100 ms): {p:.0} ms");
    assert!((35.0..110.0).contains(&p), "{p}");
}
