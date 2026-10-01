//! Server step cost with 16 players, ~1400 solid wall cuboids and every player laying a trail.
//! `cargo test --release -p shinobi-sim --test perf_step -- --ignored --nocapture`

use std::time::{Duration, Instant};

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent};
use shinobi_sim::character::Cfg;
use shinobi_sim::tick::Server;

fn build(n: u16, boxes: usize) -> Server {
    let mut s = Server::new(Cfg::default());
    s.add_static_box([0.0, -0.5, 0.0], [300.0, 0.5, 300.0]);
    for i in 0..boxes {
        let a = i as f32 * 0.37;
        s.add_static_box([a.cos() * (20.0 + (i % 40) as f32 * 5.0), 1.0, a.sin() * (20.0 + (i % 40) as f32 * 5.0)], [0.2, 1.0, 1.5]);
    }
    for id in 1..=n {
        s.add_player(id, [id as f32 * 2.0 - 16.0, 0.81, 0.0]);
    }
    s
}

fn run(n: u16, boxes: usize, ticks: u32, fight: bool) -> (f64, f64) {
    let mut s = build(n, boxes);
    let mut times = Vec::new();
    for t in 0..ticks {
        for id in 1..=n {
            let it = PlayerIntent {
                tick: t,
                buttons: Buttons(
                    (if t % 50 == 5 { Buttons::JUMP_PRESSED | Buttons::JUMP_HELD } else { 0 })
                        | (if fight && (t + id as u32 * 3) % 25 == 0 { Buttons::FIRE } else { 0 })
                        | (if fight && (t + id as u32 * 7) % 70 == 0 { Buttons::SLICE } else { 0 }),
                ),
                move_x: ((id as i32 * 37 + t as i32 / 20) % 3 - 1) as i8 * 100,
                move_z: 127,
                aim_yaw: (id as u32 * 4000 + t * 20) as u16,
                aim_pitch: 2048,
                ..Default::default()
            };
            let bytes = ClientPacket { intents: vec![it], ..Default::default() }.encode().unwrap();
            s.ingest(id, &bytes);
        }
        let t0 = Instant::now();
        s.frame(Duration::from_nanos(16_666_667));
        times.push(t0.elapsed().as_secs_f64() * 1e3);
    }
    let avg = times.iter().sum::<f64>() / times.len() as f64;
    (avg, times.iter().cloned().fold(0.0, f64::max))
}

#[test]
#[ignore]
fn step_cost_16_players() {
    let (avg, max) = run(16, 1400, 600, false);
    println!("PERF 16 players, 1400 boxes: step avg {avg:.2} ms, max {max:.2} ms");
    let (a1, _) = run(1, 1400, 600, false);
    println!("PERF 1 player, 1400 boxes: step avg {a1:.2} ms");
    let (af, mf) = run(16, 0, 600, true);
    println!("PERF 16 players FIGHTING, no boxes: step avg {af:.2} ms, max {mf:.2} ms");
    let (a0, _) = run(16, 0, 600, false);
    println!("PERF 16 players, no boxes: step avg {a0:.2} ms");
}
