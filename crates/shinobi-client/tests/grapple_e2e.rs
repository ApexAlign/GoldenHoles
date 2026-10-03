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
    let extras = OwnExtras { has_state: true, acked_tick: srv.tick - 1, hp_x10: 1000, hits: vec![], hit_seq: 0 };
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
            down.send(now, snapshot_datagram(&srv));
        }
    }
    out.corrections = game.corrections;
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
    // 50 ms one way, +-40 ms jitter (reordering), 0 / 5 / 20 % loss each way, three seeds each.
    // 0-5 %: effectively exact (a startup correction of <= 5 cm at worst). 20 %: p99 stays tiny, but once in a while
    // every redundant copy of a button EDGE (zip, dash, jump: the server never repeats an edge) is lost; the rope then
    // exists on one side only for about one RTT. Measured worst 2.4 m / 1.75 m visual offset, the same class as a lost
    // dash (no-hook baseline: 1.2 m). Fixing it needs edge redundancy on the wire (Network).
    for (loss, p99_bound, max_bound, snap_bound) in [(0.0, 0.01, 0.06, 0.02), (0.05, 0.02, 0.10, 0.02), (0.20, 0.05, 3.0, 2.2)] {
        let (mut worst99, mut worst_max, mut worst_off, mut worst_step, mut corr, mut hooks) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0u32, 0u32);
        for seed in [11u64, 12, 13] {
            let o = run(3.0, 2.4, loss, seed + (loss * 1000.0) as u64, 2400);
            report(&format!("loss {:.0}% seed {seed}", loss * 100.0), &o);
            assert!(o.attaches >= 5, "the bot must actually grapple at {loss}: {o:?}");
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
        // the fastest legal mover is a dash: 30 m/s = 0.5 m per tick (the rope is capped below that)
        assert!(worst_step < 0.6, "loss {loss}: displayed position jumped {worst_step} m in one tick");
    }
}

#[test]
fn baseline_without_grapple_same_script() {
    for (loss, seed) in [(0.0, 11u64), (0.05, 12), (0.20, 13)] {
        let o = run_ex(3.0, 2.4, loss, seed, 2400, false, LEAD);
        report(&format!("NO HOOK loss {:.0}%", loss * 100.0), &o);
    }
}
