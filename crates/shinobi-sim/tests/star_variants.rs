//! Star variants through the real `Server` (demo balance): flash (white-out only for enemies who face it) and the energy
//! each variant costs.

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState};
use shinobi_proto::quant::{q_pitch, q_yaw};
use shinobi_sim::balance::Balance;
use shinobi_sim::character::Cfg;
use shinobi_sim::combat::stars::StarKind;
use shinobi_sim::combat::CombatEvent;
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::rapier3d::prelude::Isometry;
use shinobi_sim::tick::Server;

const A: u16 = 1;
const B: u16 = 2;
const B_YAW: f32 = std::f32::consts::PI;

fn server(b_z: f32) -> Server {
    let lv = LevelDesc {
        boxes: vec![LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]), LevelBox::new([0.0, 5.0, -41.0], [40.0, 5.0, 1.0])],
        spawns: vec![[0.0, 0.81, 0.0], [0.0, 0.81, b_z]],
    };
    let mut s = Server::new(Cfg::default());
    lv.apply_to_server(&mut s);
    s.set_spawns(lv.spawns.clone());
    let mut b = Balance::demo();
    b.player.max_hp = 100.0;
    b.player.regen_per_s = 0.0;
    b.melee.knock_speed = 0.0;
    s.set_balance(b).unwrap();
    s.match_state.cfg.frag_limit = u16::MAX;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    s.add_player(A, [0.0, 0.81, 0.0]);
    s.add_player(B, [0.0, 0.81, b_z]);
    for _ in 0..800 {
        feed(&mut s, idle(0.0), idle(B_YAW));
        if s.match_state.kind() == shinobi_sim::match_state::PhaseKind::Live {
            break;
        }
    }
    tp(&mut s, A, [0.0, 0.81, 0.0]);
    tp(&mut s, B, [0.0, 0.81, b_z]);
    for _ in 0..200 {
        feed(&mut s, idle(0.0), idle(B_YAW));
    }
    for p in s.players.iter_mut() {
        p.vitals.hp = p.vitals.max_hp;
        p.vitals.invuln_until_tick = 0;
        p.melee = Default::default();
    }
    s
}

fn tp(s: &mut Server, id: u16, at: [f32; 3]) {
    let body = s.players.iter().find(|p| p.id == id).unwrap().ch.body;
    s.world.bodies[body].set_position(Isometry::translation(at[0], at[1], at[2]), true);
    shinobi_sim::character::sync_body_colliders(&mut s.world, body);
}

fn idle(yaw: f32) -> PlayerIntent {
    PlayerIntent { aim_yaw: q_yaw(yaw), aim_pitch: q_pitch(0.0), ..Default::default() }
}

fn throw(yaw: f32, pitch: f32, kind: StarKind) -> PlayerIntent {
    PlayerIntent { buttons: Buttons(Buttons::FIRE), aim_yaw: q_yaw(yaw), aim_pitch: q_pitch(pitch), star: kind as u8, ..Default::default() }
}

fn feed(s: &mut Server, a: PlayerIntent, b: PlayerIntent) -> Vec<PvpEvent> {
    let tick = s.tick;
    for (id, mut it) in [(A, a), (B, b)] {
        it.tick = tick;
        let from = tick.saturating_sub(6).max(1);
        let pkt = ClientPacket { ack_snapshot_tick: from, view: ViewState { from_tick: from, alpha: 0 }, intents: vec![it], ..Default::default() };
        s.ingest(id, &pkt.encode().unwrap());
    }
    s.frame(Duration::from_nanos(16_666_667));
    s.pvp_events.clone()
}

fn wait(s: &mut Server, ticks: u32) {
    for _ in 0..ticks {
        feed(s, idle(0.0), idle(B_YAW));
    }
}

fn hp(s: &Server, id: u16) -> f32 {
    s.players.iter().find(|p| p.id == id).unwrap().vitals.hp
}

fn energy(s: &Server) -> (f32, bool) {
    let e = s.players.iter().find(|p| p.id == A).unwrap().combat.shuriken_energy().expect("a pool in the demo balance");
    (e.0, e.2)
}

fn objects(s: &Server, owner: u16, kind: StarKind) -> Vec<shinobi_sim::combat::shuriken::Disc> {
    s.projectiles().into_iter().filter(|(o, d)| *o == owner && d.kind == kind).map(|(_, d)| d).collect()
}

fn eye(s: &Server, id: u16) -> f32 {
    let p = s.players.iter().find(|p| p.id == id).unwrap();
    p.pos(&s.world)[1] + p.ch.eye_above()
}

#[test]
fn a_flash_whites_out_only_enemies_who_face_it() {
    let mut s = server(-12.0);
    // burst point: A throws at the wall 40 m ahead... too far for B: use B standing close to the wall instead
    tp(&mut s, B, [6.0, 0.81, -33.0]);
    tp(&mut s, A, [0.0, 0.81, -20.0]);
    wait(&mut s, 5);
    // the flash goes straight ahead and bursts on the wall at z = -40
    let burst = [0.0f32, eye(&s, A), -39.9];
    let (dx, dz) = (burst[0] - 6.0, burst[2] - (-33.0));
    let toward = (-dx).atan2(-dz); // yaw whose forward = (-sin, -cos) points at the burst
    let mut blind: Option<(u8, u16)> = None;
    for t in 0..40 {
        feed(&mut s, if t == 0 { throw(0.0, 0.0, StarKind::Flash) } else { idle(0.0) }, idle(toward));
        for (id, e) in s.events.iter() {
            if let (A, CombatEvent::StarBlind { victim: B, strength_q, ticks }) = (*id, e) {
                blind = Some((*strength_q, *ticks));
            }
        }
    }
    let (q, ticks) = blind.expect("B faces the burst and is blinded");
    assert!(q > 60, "facing it at ~9 m is a solid white-out ({q})");
    assert!((90..=215).contains(&ticks), "a few seconds of white-out ({ticks} ticks)");
    let b = s.players.iter().find(|p| p.id == B).unwrap();
    assert!(b.blind_until > s.tick - 40, "the server remembers the blind");
    assert_eq!(hp(&s, B), 100.0, "a flash does no damage");
    // facing away: nothing
    let mut s = server(-12.0);
    tp(&mut s, B, [6.0, 0.81, -33.0]);
    tp(&mut s, A, [0.0, 0.81, -20.0]);
    wait(&mut s, 5);
    let mut any = false;
    for t in 0..40 {
        feed(&mut s, if t == 0 { throw(0.0, 0.0, StarKind::Flash) } else { idle(0.0) }, idle(toward + std::f32::consts::PI));
        any |= s.events.iter().any(|(_, e)| matches!(e, CombatEvent::StarBlind { .. }));
    }
    assert!(!any, "an enemy looking away is not blinded");
    // a teammate who faces it: nothing
    let mut s = server(-12.0);
    for p in s.players.iter_mut() {
        p.team = 0;
    }
    tp(&mut s, B, [6.0, 0.81, -33.0]);
    tp(&mut s, A, [0.0, 0.81, -20.0]);
    wait(&mut s, 5);
    let mut any = false;
    for t in 0..40 {
        feed(&mut s, if t == 0 { throw(0.0, 0.0, StarKind::Flash) } else { idle(0.0) }, idle(toward));
        any |= s.events.iter().any(|(_, e)| matches!(e, CombatEvent::StarBlind { .. }));
    }
    assert!(!any, "allies are never blinded");
}

#[test]
fn a_blinded_player_cannot_lock_on() {
    let mut s = server(-14.0);
    // A looks straight at B: lock forms
    wait(&mut s, 40);
    let a = |s: &Server| s.players.iter().find(|p| p.id == A).unwrap().combat.lock.holding();
    assert!(a(&s), "positive control: A is locked onto B");
    // B flashes A (B stands in front of A and throws at the wall behind A? simpler: blind A directly)
    s.players.iter_mut().find(|p| p.id == A).unwrap().blind_until = s.tick + 60;
    wait(&mut s, 10);
    assert!(!a(&s), "white-out breaks the lock");
    wait(&mut s, 70);
    assert!(a(&s), "and it comes back when the white-out ends");
}

#[test]
fn each_star_draws_its_own_cost_from_the_same_pool() {
    let mut s = server(-30.0);
    let (e0, locked) = energy(&s);
    assert!(!locked && (e0 - 6.0).abs() < 1e-3);
    let spend = |s: &mut Server, k: StarKind| {
        let before = energy(s).0;
        feed(s, throw(0.0, -0.2, k), idle(B_YAW));
        before - energy(s).0
    };
    let f = spend(&mut s, StarKind::Flash);
    assert!((1.9..2.1).contains(&f), "flash costs 2 ({f})");
    wait(&mut s, 12);
    // wait for a refill: a standard star is 1
    wait(&mut s, 300);
    assert!((energy(&s).0 - 6.0).abs() < 1e-3);
    let st = spend(&mut s, StarKind::Standard);
    assert!((0.9..1.1).contains(&st), "a standard star is still 1 ({st})");
}

#[test]
fn a_star_is_a_close_range_tool_full_damage_inside_7_m_gone_by_14_m() {
    let lost = |dist: f32| {
        let mut s = server(-dist);
        feed(&mut s, throw(0.0, 0.0, StarKind::Standard), idle(B_YAW));
        wait(&mut s, 90);
        100.0 - hp(&s, B)
    };
    let (near, mid, edge, far) = (lost(4.0), lost(10.5), lost(13.5), lost(18.0));
    assert!(near > 5.0, "a close star hurts: {near}");
    assert!(mid < near && mid > near * 0.5, "falls off between 7 and 14 m: {near} -> {mid}");
    assert!(edge <= mid && edge >= near * 0.5 - 0.01, "about half at the edge of its range: {edge}");
    assert_eq!(far, 0.0, "past 14 m the star is gone before it arrives");
}
