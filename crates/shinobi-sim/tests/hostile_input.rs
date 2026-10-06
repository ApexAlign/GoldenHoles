//! Fuzz-ish regression: hostile intents (i8 extremes, random buttons, wild aim, build presses of every piece kind)
//! against the full server sim with printed floors / walls / ramps all around the players (they keep embedding
//! themselves in their own pieces). One client must never be able to crash the server or push the world into NaN.
//! Deterministic (xorshift), so a failure reproduces exactly.
//!
//! Found by the 16-bot soak: a wall-run re-probe from inside a solid collider got a zero normal, which became a
//! zero-direction ray and a parry3d index panic.

use std::time::Duration;

use shinobi_proto::intent::{ClientPacket, PlayerIntent, ViewState};
use shinobi_sim::character::Cfg;
use shinobi_sim::tick::Server;

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
    /// Build press held this period (the sim treats it as a per-tick edge only while the intent says so).
    build: bool,
    wall: bool,
    ramp: bool,
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
    h.build = r.chance(2);
    h.wall = r.chance(2);
    h.ramp = r.chance(2);
    if r.chance(3) {
        h.yaw = r.next() as u16; // wild slew: anticheat should clamp / strike, never panic
        h.pitch = r.next() as u16;
    }
}

fn fuzz(seed: u64, players: u16, ticks: u32) {
    let cfg = Cfg::default();
    let mut server = Server::new(cfg);
    server.add_static_box([0.0, -0.5, 0.0], [60.0, 0.5, 60.0]);
    // a few pillars so wall-run has something besides printed pieces
    server.add_static_box([6.0, 2.0, 0.0], [0.5, 2.0, 6.0]);
    server.add_static_box([-6.0, 2.0, 0.0], [0.5, 2.0, 6.0]);
    let mut rng = Rng(seed);
    let mut held: Vec<Held> = Vec::new();
    let mut seqs: Vec<u16> = Vec::new();
    let mut respawns = 0u32;

    let spawn = |i: u16| [(i % 4) as f32 * 1.5 - 2.0, 0.81, (i / 4) as f32 * 1.5 - 1.0];
    for i in 0..players {
        server.add_player(i + 1, spawn(i));
        held.push(Held::default());
        seqs.push(0);
    }

    let started = std::time::Instant::now();
    for n in 0..ticks {
        if n % 2000 == 0 {
            eprintln!("  seed {seed:#x}: tick {n}/{ticks} ({:?}, pieces {}, bodies {}, colliders {}, respawns {respawns})", started.elapsed(), server.blocks.len(), server.world.bodies.len(), server.world.colliders.len());
        }
        for i in 0..players as usize {
            let id = i as u16 + 1;
            if server.players.iter().all(|p| p.id != id) {
                // kicked by anticheat (expected for hostile aim): a fresh client takes the slot
                server.add_player(id, spawn(i as u16));
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
                    build: h.build,
                    build_wall: h.build && h.wall,
                    build_ramp: h.build && h.ramp,
                    zip: false,
        block: false,
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
        }
    }
    eprintln!("seed {seed:#x}: {ticks} ticks, {players} players, {respawns} anticheat respawns, pieces {}", server.blocks.len());
}

#[test]
fn hostile_intents_with_printed_pieces_never_panic_or_nan() {
    let t = std::time::Instant::now();
    fuzz(0x5EED_0001, 3, 20_000);
    fuzz(0x5EED_0002, 3, 6_000);
    fuzz(0xC0FF_EE11, 6, 4_000);
    eprintln!("fuzz total {:?}", t.elapsed());
}
