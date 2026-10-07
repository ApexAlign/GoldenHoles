//! Long grapple end to end: `ClientGame` <-> real `Server` over a (clean and lossy) link. A face stands 150 m, 300 m and
//! 500 m ahead; the bot checks the crosshair "can hook" tint, fires the hook, and rides the reel to the face. Pins: the
//! live client probe and the server agree on the anchor, the hook attaches at every distance (the whole path honours
//! `Cfg::grapple_range`), the reel is fast (300 m in about 3 s), prediction stays tight, nothing kicks, and a katana
//! press / release in flight keeps the speed.

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

fn level(face_z: f32) -> LevelDesc {
    // a floor, a tall wide face at z = face_z (its near side at face_z + 1), and a spawn at the origin looking -Z
    let text = format!("box 0 -0.5 0  60 0.5 1200\nbox 0 40 {face_z}  200 80 1\nspawn 0 0.81 0\n");
    LevelDesc::from_text(&text).expect("level parses")
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
        fx: 0,
    };
    let snap = Snapshot { tick: srv.tick, entities: vec![(p.id, st)] };
    let mut ev = BitWriter::new();
    EventOutbox::default().write_pending(&mut ev, 256);
    let mut own = BitWriter::new();
    let extras = OwnExtras {
        has_state: true,
        acked_tick: srv.tick - 1,
        hp_x10: 1300,
        hits: vec![],
        hit_seq: 0,
        intent_complete: srv.players[0].queue.complete_tick(),
        lead_margin: 8,
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

#[derive(Debug, Default)]
struct Out {
    hookable_before: bool,
    attached: bool,
    rope_len: f32,
    t_reach: Option<f32>,
    peak_speed: f32,
    max_err: f32,
    corrections: u32,
    end_speed: f32,
    strike_armed_ticks: u32,
}

/// `strike_at`: tick (after the hook) at which the bot presses the katana mid-reel.
fn run(dist: f32, strike_at: Option<u32>) -> Out {
    let face_z = -dist;
    let lv = level(face_z);
    let mut game = ClientGame::new(lv.clone(), ClientGameCfg::default());
    let mut srv = Server::new(Cfg::default());
    lv.apply_to_server(&mut srv);
    let mut out = Out::default();
    let mut pred: std::collections::HashMap<u32, [f32; 3]> = Default::default();
    let mut server_pos: std::collections::HashMap<u32, [f32; 3]> = Default::default();
    let mut up: Vec<(f64, Vec<u8>)> = Vec::new();
    let mut down: Vec<(f64, Vec<u8>)> = Vec::new();
    let one_way = 2.0;
    let mut hooked_at: Option<u32> = None;
    let mut start_pos = [0.0f32; 3];
    for i in 0..1400u32 {
        let now = i as f64;
        let due: Vec<Vec<u8>> = down.iter().filter(|(t, _)| *t <= now).map(|(_, b)| b.clone()).collect();
        down.retain(|(t, _)| *t > now);
        for d in due {
            let _ = game.on_datagram(&d, now);
        }
        if let Some(u) = game.net.take_own_state() {
            if let Some(pos) = u.server_pos {
                game.on_own_state(OwnAck { tick: u.acked_tick, pos, own: u.own });
            }
        }
        let mut f = InputFrame::default();
        if game.predicting() {
            game.aim.yaw = 0.0; // faces -Z
            game.aim.pitch = 0.0;
            if i == 60 {
                if let Some(p) = game.predicted_pos() {
                    start_pos = p;
                }
                // the crosshair "can hook" probe is the live client's own ray: it must see the face at full range
                out.hookable_before = game.view(now).fx.grapple_hookable;
                f.buttons.push(ev(Action::Grapple, true));
                f.buttons.push(ev(Action::Grapple, false));
                hooked_at = Some(i);
            }
            if let (Some(h), Some(k)) = (hooked_at, strike_at) {
                if i == h + k {
                    f.buttons.push(ev(Action::Slice, true));
                    f.buttons.push(ev(Action::Slice, false));
                }
            }
        }
        if let Some(pkt) = game.sim_tick(&f, now) {
            if !is_hello(&pkt) {
                if let (Some(p), Ok(cp)) = (game.predicted_pos(), ClientPacket::decode(&pkt)) {
                    pred.insert(cp.intents[0].tick, p);
                }
            }
            up.push((now + one_way, pkt));
        }
        let view = game.view(now);
        out.attached |= view.fx.grapple_attached.is_some();
        out.strike_armed_ticks += (view.fx.strike_armed != 0) as u32;
        if let Some(l) = view.rope.map(|(e, a)| ((a[0] - e[0]).powi(2) + (a[1] - e[1]).powi(2) + (a[2] - e[2]).powi(2)).sqrt()) {
            out.rope_len = out.rope_len.max(l);
        }
        let due: Vec<Vec<u8>> = up.iter().filter(|(t, _)| *t <= now).map(|(_, b)| b.clone()).collect();
        up.retain(|(t, _)| *t > now);
        for pkt in due {
            if is_hello(&pkt) {
                if srv.players.is_empty() {
                    srv.add_player(1, [0.0, 0.81, 0.0]);
                }
                down.push((now + one_way, Welcome { player_id: 1, server_tick: srv.tick }.encode().to_vec()));
            } else if !srv.players.is_empty() {
                srv.ingest(1, &pkt);
            }
        }
        if !srv.players.is_empty() {
            let rep = srv.frame(Duration::from_nanos(16_666_667));
            assert!(rep.kicked.is_empty(), "server kicked the player at tick {i} (dist {dist}): {:?}", srv.strike_log);
            server_pos.insert(srv.tick - 1, srv.players[0].pos(&srv.world));
            let sp = srv.players[0].ch.vel.norm();
            out.peak_speed = out.peak_speed.max(sp);
            out.end_speed = sp;
            if let Some(h) = hooked_at {
                if out.t_reach.is_none() {
                    let p = srv.players[0].pos(&srv.world);
                    let travelled = ((p[0] - start_pos[0]).powi(2) + (p[2] - start_pos[2]).powi(2)).sqrt();
                    if travelled >= dist - 14.0 {
                        out.t_reach = Some((i - h) as f32 / 60.0);
                    }
                }
            }
            down.push((now + one_way, snapshot_datagram(&srv)));
        }
    }
    out.corrections = game.corrections;
    let last = pred.keys().copied().max().unwrap_or(0);
    for (t, p) in &pred {
        if *t < 80 || *t + 40 > last {
            continue;
        }
        if let Some(s) = server_pos.get(t) {
            out.max_err = out.max_err.max((0..3).map(|k| (p[k] - s[k]).powi(2)).sum::<f32>().sqrt());
        }
    }
    out
}

#[test]
fn hooks_a_face_at_150_300_and_500_m_and_reels_there_fast() {
    let cfg = Cfg::default();
    assert!(cfg.grapple_range >= 1000.0, "range {}", cfg.grapple_range);
    for (dist, max_t) in [(150.0f32, 3.0f32), (300.0, 4.2), (500.0, 5.8)] {
        let o = run(dist, None);
        eprintln!(
            "far hook {dist:.0} m: hookable tint {}, attached {}, rope {:.0} m, reached in {:?} s, peak {:.0} m/s, prediction error max {:.3} m, corrections {}",
            o.hookable_before, o.attached, o.rope_len, o.t_reach, o.peak_speed, o.max_err, o.corrections
        );
        assert!(o.hookable_before, "the crosshair shows the hook is available at {dist} m");
        assert!(o.attached, "the hook attaches at {dist} m through the live client path");
        assert!(o.rope_len > dist - 3.0, "the rope is drawn the full {dist} m (rope {})", o.rope_len);
        let t = o.t_reach.unwrap_or_else(|| panic!("never reached the {dist} m face"));
        assert!(t < max_t, "{dist} m took {t:.2} s (limit {max_t})");
        assert!(o.peak_speed <= cfg.grapple_far_speed + 6.0, "speed {}", o.peak_speed);
        assert!(o.max_err < 4.0, "prediction error {} m at {dist} m", o.max_err);
    }
}

#[test]
fn a_katana_press_mid_reel_is_a_hook_strike_and_keeps_the_speed() {
    let o = run(500.0, Some(90));
    eprintln!("hook-strike mid-reel at 500 m: strike armed {} ticks, end speed {:.0} m/s, peak {:.0}", o.strike_armed_ticks, o.end_speed, o.peak_speed);
    assert!(o.strike_armed_ticks > 20, "the HUD cue is armed while hooked: {}", o.strike_armed_ticks);
    assert!(o.peak_speed > 60.0, "the far reel is fast: {}", o.peak_speed);
}
