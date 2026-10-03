//! Per-frame client cost: `ClientGame::sim_tick` (60 Hz) + `view` (144 Hz render) + snapshot decode (30 Hz) with
//! 16 remote players on the real arena. Release only:
//! `cargo test --release -p shinobi-client --test perf_client -- --ignored --nocapture`

use std::time::{Duration, Instant};

use shinobi_client::client_game::{ClientGame, ClientGameCfg, InputFrame, OwnAck};
use shinobi_proto::bits::BitWriter;
use shinobi_proto::datagram::{frame_snapshot_v3, Welcome};
use shinobi_proto::gameplay::OwnExtras;
use shinobi_proto::quant::{q_pos3, q_yaw};
use shinobi_proto::snapshot::{encode_delta, EntityState, MoveKind, Snapshot};
use shinobi_sim::character::Cfg;
use shinobi_sim::events::EventOutbox;
use shinobi_sim::level::parse_level_file;
use shinobi_sim::own_state::OwnState;
use shinobi_sim::tick::Server;

const REMOTES: u16 = 16;

fn stats(mut v: Vec<f64>) -> (f64, f64, f64) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    (mean, v[(v.len() as f64 * 0.99) as usize], *v.last().unwrap())
}

#[test]
#[ignore]
fn client_frame_cost_with_16_remotes_at_144hz() {
    let text = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels/arena.lvl")).unwrap();
    let level = parse_level_file(&text).unwrap().level;

    // a real authoritative own-state to start prediction from
    let mut srv = Server::new(Cfg::default());
    level.apply_to_server(&mut srv);
    srv.add_player(1, level.spawn(0));
    srv.frame(Duration::from_nanos(16_666_667));
    let own = OwnState::from_snapshot(&srv.players[0].ch.snapshot(&srv.world));
    let own_pos = own.pos;

    let mut game = ClientGame::new(level.clone(), ClientGameCfg::default());
    game.on_datagram(&Welcome { player_id: 1, server_tick: 1 }.encode(), 0.0).unwrap();

    let snapshot = |tick: u32, with_own: bool| -> Vec<u8> {
        let mut entities = vec![(1u16, EntityState { pos: q_pos3(own_pos), ..Default::default() })];
        for i in 0..REMOTES {
            let a = tick as f32 * 0.02 + i as f32;
            let p = [a.cos() * (10.0 + i as f32), 0.81, a.sin() * (10.0 + i as f32)];
            entities.push((2 + i, EntityState { pos: q_pos3(p), vel: [64, 0, 0], yaw: q_yaw(a), kind: MoveKind::Grounded, flags: 0 }));
        }
        let snap = Snapshot { tick, entities };
        let mut ev = BitWriter::new();
        EventOutbox::default().write_pending(&mut ev, 256);
        let mut ob = BitWriter::new();
        let extras = OwnExtras { has_state: with_own, acked_tick: tick.saturating_sub(1), hp_x10: 1000, hits: vec![], hit_seq: 0, ..Default::default() };
        extras.write(&mut ob, |w| own.write(w));
        frame_snapshot_v3(&ev.finish(), &ob.finish(), &[], &encode_delta(None, &snap)) // full snapshot: worst-case decode
    };

    // start prediction (first own-state), then run 20 s
    game.on_datagram(&snapshot(2, true), 2.0).unwrap();
    if let Some(u) = game.net.take_own_state() {
        game.on_own_state(OwnAck { tick: u.acked_tick, pos: u.own.pos, own: u.own });
    }
    assert!(game.net.player_id().is_some());

    let (mut sim, mut view, mut dgram) = (Vec::new(), Vec::new(), Vec::new());
    let render_hz = 144.0f64;
    let total_s = 20.0;
    let frames = (total_s * render_hz) as u32;
    let (mut next_sim, mut next_snap, mut tick) = (0.0f64, 0.0f64, 3u32);
    let mut intent_n = 0u32;
    for f in 0..frames {
        let t = f as f64 / render_hz;
        let local = 2.0 + t * 60.0;
        while next_sim <= t {
            intent_n += 1;
            let frame = InputFrame {
                stick: ((intent_n as f32 * 0.03).sin(), 1.0),
                look: (if intent_n % 3 == 0 { 2.0 } else { 0.0 }, 0.0),
                ..Default::default()
            };
            let t0 = Instant::now();
            let pkt = game.sim_tick(&frame, 2.0 + next_sim * 60.0);
            sim.push(t0.elapsed().as_secs_f64() * 1e3);
            std::hint::black_box(pkt);
            next_sim += 1.0 / 60.0;
        }
        while next_snap <= t {
            tick += 2;
            let d = snapshot(tick, false);
            let t0 = Instant::now();
            let _ = game.on_datagram(&d, 2.0 + next_snap * 60.0);
            dgram.push(t0.elapsed().as_secs_f64() * 1e3);
            next_snap += 1.0 / 30.0;
        }
        let t0 = Instant::now();
        let v = game.view(local);
        view.push(t0.elapsed().as_secs_f64() * 1e3);
        std::hint::black_box(v);
    }
    let (sm, sp, sx) = stats(sim);
    let (vm, vp, vx) = stats(view);
    let (dm, dp, dx) = stats(dgram);
    let per_frame = vm + sm * 60.0 / render_hz + dm * 30.0 / render_hz;
    println!("PERF sim_tick   mean {sm:.4} ms  p99 {sp:.4}  max {sx:.4}   (60 Hz)");
    println!("PERF view       mean {vm:.4} ms  p99 {vp:.4}  max {vx:.4}   (144 Hz, {REMOTES} remotes)");
    println!("PERF on_datagram mean {dm:.4} ms  p99 {dp:.4}  max {dx:.4}   (30 Hz, full snapshot of {} entities)", REMOTES + 1);
    println!("PERF amortised per rendered frame at 144 Hz: {per_frame:.4} ms (mobile budget 0.3 ms)");
    if !cfg!(debug_assertions) {
        assert!(per_frame < 0.3, "client frame cost {per_frame} ms exceeds the 0.3 ms mobile budget");
    }
}
