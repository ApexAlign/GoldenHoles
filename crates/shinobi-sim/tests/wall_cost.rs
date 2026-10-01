//! Cost probe: ms per server step with 16 running players, solid trail walls on/off. Prints numbers; asserts
//! only sanity. Run: `cargo test --release -p shinobi-sim --test wall_cost -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent};
use shinobi_sim::character::Cfg;
use shinobi_sim::tick::Server;
use shinobi_sim::walls::{OwnerTrail, WallId};
use shinobi_sim::walls_world::{WallsWorld, WALL_GRACE_TICKS};

fn run(solid: bool, players: u16, ticks: u32) -> (f64, usize, f64) {
    let cfg = Cfg::default();
    let mut server = Server::new(cfg);
    server.add_static_box([0.0, -0.5, 0.0], [200.0, 0.5, 200.0]);
    let life = (cfg.trail_life * 60.0) as u32;
    let mut walls = WallsWorld::new(life, WALL_GRACE_TICKS, cfg.trail_height, cfg.trail_thick);
    let mut trails = Vec::new();
    for i in 0..players {
        server.add_player(i + 1, [(i % 4) as f32 * 6.0 - 9.0, 0.81, (i / 4) as f32 * 6.0 - 9.0]);
        trails.push(OwnerTrail::new(i as u8, cfg.trail_spacing, life, 128));
    }
    let mut next: WallId = 1;
    let mut total = Duration::ZERO;
    let mut sync_total = Duration::ZERO;
    let mut peak = 0;
    for _ in 0..ticks {
        let t = server.tick + 1;
        for i in 0..players {
            // circles: constant forward stick, yaw sweeps (bot "Circles")
            let yaw = ((t as f32 / 60.0) * 1.2 + i as f32 * 0.4).rem_euclid(std::f32::consts::TAU);
            let it = PlayerIntent { tick: t, buttons: Buttons(0), move_x: 0, move_z: 127, aim_yaw: shinobi_proto::quant::q_yaw(yaw), ..Default::default() };
            let pkt = ClientPacket { intents: vec![it], ..Default::default() };
            server.ingest(i + 1, &pkt.encode().unwrap());
        }
        let t0 = Instant::now();
        server.frame(Duration::from_nanos(16_666_667));
        total += t0.elapsed();
        let tick = server.tick.saturating_sub(1);
        if solid {
            let poses: Vec<_> = server.players.iter().map(|p| (p.id, p.pos(&server.world))).collect();
            for (id, pos) in poses {
                let step = trails[id as usize - 1].sample(tick, [pos[0], pos[1] - 0.8, pos[2]], &mut next);
                if let Some(w) = step.spawned {
                    walls.add(w);
                }
                for k in step.killed {
                    walls.kill(k, tick);
                }
            }
            let t1 = Instant::now();
            walls.sync(tick, &mut server.world);
            sync_total += t1.elapsed();
            peak = peak.max(walls.active());
        }
    }
    (total.as_secs_f64() * 1e3 / ticks as f64, peak, sync_total.as_secs_f64() * 1e3 / ticks as f64)
}

#[test]
#[ignore = "perf probe (slow in dev): cargo test --release -p shinobi-sim --test wall_cost -- --ignored --nocapture"]
fn wall_cost_probe() {
    let (a, _, _) = run(false, 16, 900);
    let (b, peak, sync) = run(true, 16, 900);
    eprintln!(
        "16 players, 900 ticks: no solid walls {a:.2} ms/step; solid walls {b:.2} ms/step (peak {peak} armed walls); WallsWorld::sync alone {sync:.3} ms/tick"
    );
    assert!(a.is_finite() && b.is_finite());
}
