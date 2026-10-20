//! Frame hitches end to end: `ClientGame` <-> real `Server`, with the client run the way the game shell runs it
//! (render frames of uneven length, a 60 Hz fixed-step accumulator that catches up after a stall, capped at 250 ms
//! like Bevy's virtual clock). An honest player must never collect an anti-cheat strike from a slow or hitching
//! machine, and the intent lead must not run into the server's future-tick ceiling.

use std::collections::VecDeque;
use std::time::Duration;

use shinobi_client::client_game::{Action, ButtonEvent, ClientGame, ClientGameCfg, InputFrame, OwnAck};
use shinobi_proto::bits::BitWriter;
use shinobi_proto::datagram::{frame_snapshot_v3, is_hello, Welcome};
use shinobi_proto::gameplay::OwnExtras;
use shinobi_proto::quant::{q_pos3, q_vel};
use shinobi_proto::snapshot::{encode_delta, EntityState, MoveKind, Snapshot};
use shinobi_sim::character::{Cfg, MoveState};
use shinobi_sim::events::EventOutbox;
use shinobi_sim::level::LevelDesc;
use shinobi_sim::own_state::OwnState;
use shinobi_sim::tick::Server;

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
    let extras = OwnExtras {
        has_state: true,
        acked_tick: srv.tick - 1,
        hp_x10: 1000,
        hits: vec![],
        hit_seq: 0,
        intent_complete: p.queue.complete_tick(),
        lead_margin: margin,
        echo_seq: p.last_pkt.map_or(0, |(s, _)| s),
        echo_age: p.last_pkt.map_or(0, |(_, t)| srv.tick.saturating_sub(t).min(255) as u8),
        ..Default::default()
    };
    extras.write(&mut own, |w| OwnState::from_snapshot(&p.ch.snapshot(&srv.world)).write(w));
    frame_snapshot_v3(&ev.finish(), &own.finish(), &[], &encode_delta(None, &snap))
}

fn level() -> LevelDesc {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels/8v8.lvl");
    LevelDesc::from_text(&std::fs::read_to_string(path).expect("levels/8v8.lvl")).expect("8v8.lvl parses")
}

/// How the render loop behaves: frame length in 60 Hz ticks, and a periodic stall.
#[derive(Clone, Copy)]
struct Profile {
    frame_ticks: f64,
    /// Every `hitch_every` ticks of wall time one frame takes `hitch_ticks` instead.
    hitch_every: f64,
    hitch_ticks: f64,
    /// Big flick: yaw radians injected in a single frame every second.
    flick: f32,
}

#[derive(Default, Debug)]
struct Res {
    strikes: u32,
    rejected_future: u32,
    synthesised: u32,
    repeated: u32,
    corrections: u32,
    max_visual_offset: f32,
    kicked: bool,
    log: Vec<String>,
    final_lead: Option<u32>,
}

fn run(p: Profile, one_way: usize, seconds: u32, seed_moves: bool, wall_sync: bool) -> Res {
    let lv = level();
    let mut game = ClientGame::new(lv.clone(), ClientGameCfg::default());
    let mut srv = Server::new(Cfg::default());
    lv.apply_to_server(&mut srv);
    let start = [30.0, 0.81, -30.0];
    let mut up: VecDeque<(u64, Vec<u8>)> = VecDeque::new();
    let mut down: VecDeque<(u64, Vec<u8>)> = VecDeque::new();
    let mut res = Res::default();
    let (mut next_frame, mut last_frame, mut acc) = (0.0f64, 0.0f64, 0.0f64);
    let mut next_hitch = p.hitch_every;
    let mut local_tick = 0.0f64;
    let mut next_flick = 60u64;
    let mut stalled_since_srv_tick = 0u32;
    let _ = &mut stalled_since_srv_tick;

    for wall in 0..(seconds as u64 * 60) {
        // ---- client frame(s) due now ----
        while next_frame <= wall as f64 {
            let dt = next_frame - last_frame;
            last_frame = next_frame;
            while let Some((t, _)) = down.front() {
                if *t > wall {
                    break;
                }
                let (_, d) = down.pop_front().unwrap();
                if wall_sync {
                    game.net.sync_wall(local_tick, next_frame);
                }
                let _ = game.on_datagram(&d, local_tick);
            }
            if let Some(u) = game.net.take_own_state() {
                if let Some(pos) = u.server_pos {
                    game.on_own_state(OwnAck { tick: u.acked_tick, pos, own: u.own });
                }
            }
            acc += dt.min(15.0); // Bevy's virtual clock never advances more than 250 ms per frame
            let mut look = (0.0f32, 0.0f32);
            if seed_moves {
                look.0 = 12.0; // steady turn
                if p.flick != 0.0 && wall >= next_flick {
                    look.0 += p.flick / 0.0022; // the whole flick lands in one frame (device units)
                    next_flick += 60;
                }
            }
            let mut first = true;
            while acc >= 1.0 {
                acc -= 1.0;
                local_tick += 1.0;
                if wall_sync {
                    game.net.sync_wall(local_tick, next_frame);
                }
                let mut f = InputFrame::default();
                if seed_moves {
                    f.stick = (1.0, 0.0);
                    if first {
                        f.look = look;
                    }
                    if (wall / 45) % 3 == 0 && first {
                        f.buttons.push(ButtonEvent { action: Action::Jump, down: true });
                        f.buttons.push(ButtonEvent { action: Action::Jump, down: false });
                    }
                }
                first = false;
                if let Some(pkt) = game.sim_tick(&f, local_tick) {
                    up.push_back((wall + one_way as u64, pkt));
                }
            }
            let hitch = wall as f64 >= next_hitch && p.hitch_every > 0.0;
            if hitch {
                next_hitch += p.hitch_every;
            }
            next_frame += if hitch { p.hitch_ticks } else { p.frame_ticks };
        }

        // ---- server: one tick per wall tick ----
        while let Some((t, _)) = up.front() {
            if *t > wall {
                break;
            }
            let (_, pkt) = up.pop_front().unwrap();
            if is_hello(&pkt) {
                if srv.players.is_empty() {
                    srv.add_player(1, start);
                }
                down.push_back((wall + one_way as u64, Welcome { player_id: 1, server_tick: srv.tick, ..Default::default() }.encode().to_vec()));
            } else if !srv.players.is_empty() {
                srv.ingest(1, &pkt);
            }
        }
        if !srv.players.is_empty() {
            let rep = srv.frame(Duration::from_nanos(16_666_667));
            res.log.append(&mut srv.strike_log);
            if !rep.kicked.is_empty() {
                res.kicked = true;
                break;
            }
            let margin = srv.players[0].queue.take_margin().map_or(15, |m| m.clamp(-16, 14) as i8);
            down.push_back((wall + one_way as u64, snapshot_datagram(&srv, margin)));
        }
    }
    if let Some(pl) = srv.players.first() {
        res.strikes = pl.guard.strikes;
        res.rejected_future = pl.queue.rejected_future;
        res.repeated = pl.queue.repeated_ticks;
    }
    res.corrections = game.corrections;
    res.final_lead = game.net.lead();
    res
}

const PROFILES: [(&str, Profile); 7] = [
    ("60fps", Profile { frame_ticks: 1.0, hitch_every: 0.0, hitch_ticks: 0.0, flick: 0.0 }),
    ("30fps", Profile { frame_ticks: 2.0, hitch_every: 0.0, hitch_ticks: 0.0, flick: 0.0 }),
    ("15fps", Profile { frame_ticks: 4.0, hitch_every: 0.0, hitch_ticks: 0.0, flick: 0.0 }),
    ("60fps+100ms hitch", Profile { frame_ticks: 1.0, hitch_every: 90.0, hitch_ticks: 6.0, flick: 0.0 }),
    ("60fps+200ms hitch", Profile { frame_ticks: 1.0, hitch_every: 90.0, hitch_ticks: 12.0, flick: 0.0 }),
    ("30fps+300ms hitch", Profile { frame_ticks: 2.0, hitch_every: 120.0, hitch_ticks: 18.0, flick: 0.0 }),
    ("30fps+300ms hitch+flick", Profile { frame_ticks: 2.0, hitch_every: 120.0, hitch_ticks: 18.0, flick: 3.0 }),
];

#[test]
fn honest_slow_or_hitching_clients_are_never_struck() {
    let mut bad = Vec::new();
    for (name, p) in PROFILES {
        for one_way in [0usize, 2] {
            let r = run(p, one_way, 40, true, true);
            eprintln!("{name} ow={one_way}: {r:?}");
            if r.kicked || r.strikes > 0 || r.rejected_future > 0 {
                bad.push(format!("{name} ow={one_way}: {r:?}"));
            }
        }
    }
    assert!(bad.is_empty(), "false strikes:\n{}", bad.join("\n"));
}
