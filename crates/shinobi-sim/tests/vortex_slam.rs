//! Vortex Slam through the real `Server`: a dive (slide held in a fall) that lands hard sets off a damaging, knocking
//! shock around the diver; a plain fall or a dive that lands far away does not.

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


fn slide(yaw: f32) -> PlayerIntent {
    PlayerIntent { buttons: Buttons(Buttons::SLIDE), aim_yaw: q_yaw(yaw), aim_pitch: q_pitch(0.0), ..Default::default() }
}

fn dive_from(height: f32, hold_slide: bool) -> (f32, f32, f32) {
    let mut s = server(-2.5);
    tp(&mut s, B, [2.5, 0.81, 0.0]);
    tp(&mut s, A, [0.0, height, 0.0]);
    let mut b_peak_speed = 0.0f32;
    for _ in 0..120 {
        feed(&mut s, if hold_slide { slide(0.0) } else { idle(0.0) }, idle(B_YAW));
        let b = s.players.iter().find(|p| p.id == B).unwrap();
        b_peak_speed = b_peak_speed.max(b.ch.vel.x.abs());
    }
    (hp(&s, A), hp(&s, B), b_peak_speed)
}

#[test]
fn a_dive_that_lands_hard_hurts_and_throws_a_bystander_but_not_the_diver() {
    let (a, b, push) = dive_from(14.0, true);
    assert_eq!(a, 100.0, "the diver is unharmed");
    assert!(b < 100.0, "the bystander next to the landing takes the slam ({b})");
    assert!(push > 4.0, "and is thrown clear ({push} m/s)");
}

#[test]
fn a_plain_fall_or_a_slide_less_drop_sets_off_nothing() {
    let (a, b, _) = dive_from(14.0, false);
    assert_eq!((a, b), (100.0, 100.0), "no slide held: an ordinary fall");
}
