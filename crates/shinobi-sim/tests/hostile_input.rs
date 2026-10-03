//! Fuzz-ish regression: hostile intents (i8 extremes, random buttons, wild aim) against the full server sim with
//! thin SOLID Tron walls everywhere (owners' own trails, zero grace). One client must never be able to crash
//! the server or push the world into NaN. Deterministic (xorshift), so a failure reproduces exactly.
//!
//! Found by the 16-bot soak: a wall-run re-probe from inside a solid collider got a zero normal, which became a
//! zero-direction ray and a parry3d index panic.

use std::time::Duration;

use shinobi_proto::intent::{ClientPacket, PlayerIntent, ViewState};
use shinobi_sim::character::Cfg;
use shinobi_sim::tick::Server;
use shinobi_sim::walls::{OwnerTrail, WallId};
use shinobi_sim::walls_world::WallsWorld;

/// Few live walls per owner: `step_player` rebuilds the query BVH per player per tick, so wall count drives cost.
const WALL_CAP: usize = 16;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn chance(&mut self, one_in: u64) -> bool {
        self.next() % one_in == 0
    }
}

/// Per-player "held" input that is re-rolled occasionally, so sustained runs / wall-runs / dashes actually
/// happen instead of pure jitter.
#[derive(Clone, Copy, Default)]
struct Held {
    move_x: i8,
    move_z: i8,
    buttons: u8,
    yaw: u16,
    pitch: u16,
}

fn extreme_i8(r: &mut Rng) -> i8 {
    match r.next() % 6 {
        0 => i8::MIN,
        1 => i8::MAX,
        2 => 0,
        3 => -127,
        _ => r.next() as i8,
    }
}

fn reroll(r: &mut Rng, h: &mut Held) {
    h.move_x = extreme_i8(r);
    h.move_z = extreme_i8(r);
    h.buttons = (r.next() & 0x7f) as u8;
    if r.chance(3) {
        h.yaw = r.next() as u16; // wild slew: anticheat should clamp / strike, never panic
        h.pitch = r.next() as u16;
    }
}

fn fuzz(seed: u64, grace_ticks: u32, players: u16, ticks: u32) {
    let cfg = Cfg::default();
    let mut server = Server::new(cfg);
    server.add_static_box([0.0, -0.5, 0.0], [60.0, 0.5, 60.0]);
    // a few pillars so wall-run has something besides trail walls
    server.add_static_box([6.0, 2.0, 0.0], [0.5, 2.0, 6.0]);
    server.add_static_box([-6.0, 2.0, 0.0], [0.5, 2.0, 6.0]);
    let mut rng = Rng(seed);
    let life = (cfg.trail_life * 60.0) as u32;
    let mut walls = WallsWorld::new(life, grace_ticks, cfg.trail_height, cfg.trail_thick);
    let mut trails: Vec<OwnerTrail> = Vec::new();
    let mut held: Vec<Held> = Vec::new();
    let mut next_wall: WallId = 1;
    let mut seqs: Vec<u16> = Vec::new();
    let mut respawns = 0u32;

    let spawn = |i: u16| [(i % 4) as f32 * 1.5 - 2.0, 0.81, (i / 4) as f32 * 1.5 - 1.0];
    for i in 0..players {
        server.add_player(i + 1, spawn(i));
        trails.push(OwnerTrail::new(i as u8, cfg.trail_spacing, life, WALL_CAP));
        held.push(Held::default());
        seqs.push(0);
    }

    let started = std::time::Instant::now();
    for n in 0..ticks {
        if n % 2000 == 0 {
            eprintln!("  seed {seed:#x}: tick {n}/{ticks} ({:?}, walls {}, bodies {}, colliders {}, respawns {respawns})", started.elapsed(), walls.tracked(), server.world.bodies.len(), server.world.colliders.len());
        }
        for i in 0..players as usize {
            let id = i as u16 + 1;
            if server.players.iter().all(|p| p.id != id) {
                // kicked by anticheat (expected for hostile aim): a fresh client takes the slot
                server.add_player(id, spawn(i as u16));
                trails[i].clear();
                respawns += 1;
            }
            if rng.chance(12) {
                reroll(&mut rng, &mut held[i]);
            }
            let h = held[i];
            let t = server.tick + 1;
            let intents = (0..t.min(4))
                .map(|k| PlayerIntent {
                    tick: t - k,
                    buttons: shinobi_proto::intent::Buttons(h.buttons),
                    move_x: h.move_x,
                    move_z: h.move_z,
                    aim_yaw: h.yaw,
                    aim_pitch: h.pitch,
                    ultimate: false,
                    build: false, build_wall: false, build_ramp: false,
                    zip: false,
                })
                .collect();
            seqs[i] = seqs[i].wrapping_add(1);
            let pkt = ClientPacket { seq: seqs[i], view: ViewState { from_tick: t.saturating_sub(6), alpha: 255 }, intents, ..Default::default() };
            if let Ok(bytes) = pkt.encode() {
                server.ingest(id, &bytes);
            }
        }

        server.frame(Duration::from_nanos(16_666_667));

        let tick = server.tick.saturating_sub(1);
        for p in &server.players {
            let pos = p.pos(&server.world);
            assert!(pos.iter().all(|v| v.is_finite() && v.abs() < 2000.0), "player {} pos {pos:?} at tick {tick}", p.id);
            let v = p.ch.vel;
            assert!(v.iter().all(|c| c.is_finite()), "player {} vel {v:?} at tick {tick}", p.id);
            let i = p.id as usize - 1;
            let step = trails[i].sample(tick, [pos[0], pos[1] - 0.8, pos[2]], &mut next_wall);
            if let Some(w) = step.spawned {
                walls.add(w);
            }
            for id in step.killed {
                walls.kill(id, tick);
            }
        }
        walls.sync(tick, &mut server.world);
    }
    eprintln!("seed {seed:#x} grace {grace_ticks}: {ticks} ticks, {players} players, {respawns} anticheat respawns, walls tracked {}", walls.tracked());
}

#[test]
fn hostile_intents_with_solid_walls_never_panic_or_nan() {
    let t = std::time::Instant::now();
    fuzz(0x5EED_0001, 0, 3, 20_000); // zero grace: players constantly embedded in their own walls
    fuzz(0x5EED_0002, 15, 3, 6_000);
    fuzz(0xC0FF_EE11, 0, 6, 4_000);
    eprintln!("fuzz total {:?}", t.elapsed());
}
