//! Katana moves through the real `Server` (demo balance): heavy cut, air cut + slam, counter cut, whirl cut, execute.
//! A (id 1) at the origin facing -Z, B (id 2) in front.

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState, PITCH_LEVEL};
use shinobi_proto::quant::q_yaw;
use shinobi_sim::balance::Balance;
use shinobi_sim::character::Cfg;
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::tick::Server;

const A: u16 = 1;
const B: u16 = 2;

fn server(boxes: Vec<LevelBox>, b_z: f32) -> Server {
    let lv = LevelDesc { boxes, spawns: vec![[0.0, 0.81, 0.0], [0.0, 0.81, b_z]] };
    let mut s = Server::new(Cfg::default());
    lv.apply_to_server(&mut s);
    s.set_spawns(lv.spawns.clone());
    s.set_balance(Balance::demo()).unwrap();
    s.match_state.cfg.frag_limit = u16::MAX;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    s.add_player(A, [0.0, 0.81, 0.0]);
    s.add_player(B, [0.0, 0.81, b_z]);
    for _ in 0..800 {
        feed(&mut s, idle(0.0), idle(std::f32::consts::PI));
        if s.match_state.kind() == shinobi_sim::match_state::PhaseKind::Live {
            break;
        }
    }
    for (id, z) in [(A, 0.0f32), (B, b_z)] {
        let body = s.players.iter().find(|p| p.id == id).unwrap().ch.body;
        s.world.bodies[body].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(0.0, 0.81, z), true);
        shinobi_sim::character::sync_body_colliders(&mut s.world, body);
    }
    for _ in 0..200 {
        feed(&mut s, idle(0.0), idle(std::f32::consts::PI));
    }
    for p in s.players.iter_mut() {
        p.vitals.hp = p.vitals.max_hp;
        p.melee = Default::default();
    }
    s
}

fn floor() -> Vec<LevelBox> {
    vec![LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0])]
}

fn idle(yaw: f32) -> PlayerIntent {
    PlayerIntent { aim_yaw: q_yaw(yaw), aim_pitch: PITCH_LEVEL, ..Default::default() }
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

const A_YAW: f32 = 0.0;
const B_YAW: f32 = std::f32::consts::PI;

fn slice() -> PlayerIntent {
    PlayerIntent { buttons: Buttons(Buttons::SLICE), ..idle(A_YAW) }
}

fn hp(s: &Server, id: u16) -> f32 {
    s.players.iter().find(|p| p.id == id).unwrap().vitals.hp
}

fn pos(s: &Server, id: u16) -> [f32; 3] {
    s.players.iter().find(|p| p.id == id).unwrap().pos(&s.world)
}


fn held() -> PlayerIntent {
    slice()
}

fn teleport(s: &mut Server, id: u16, p: [f32; 3]) {
    let body = s.players.iter().find(|q| q.id == id).unwrap().ch.body;
    s.world.bodies[body].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(p[0], p[1], p[2]), true);
    s.world.bodies[body].set_linvel(shinobi_sim::rapier3d::na::Vector3::zeros(), true);
    shinobi_sim::character::sync_body_colliders(&mut s.world, body);
}

fn player(s: &mut Server, id: u16) -> &mut shinobi_sim::tick::Player {
    s.players.iter_mut().find(|p| p.id == id).unwrap()
}

#[test]
fn a_released_heavy_cut_hits_harder_than_a_light_one_and_a_tap_stays_light() {
    // light: a plain tap on B in reach
    let mut s = server(floor(), -1.5);
    for t in 0..30 {
        feed(&mut s, if t == 5 { slice() } else { idle(A_YAW) }, idle(B_YAW));
    }
    let light = 100.0 - hp(&s, B);
    // heavy: the press cuts at nothing, the button stays down ~1 s, B steps in, release
    let mut s = server(floor(), -30.0);
    for t in 0..90 {
        if t == 55 {
            let a = pos(&s, A);
            teleport(&mut s, B, [a[0], 0.81, a[2] - 2.0]);
        }
        feed(&mut s, if (5..70).contains(&t) { held() } else { idle(A_YAW) }, idle(B_YAW));
    }
    let heavy = 100.0 - hp(&s, B);
    eprintln!("KATANA heavy: light {light:.1}, heavy {heavy:.1}");
    assert!(heavy >= light * 1.5 && heavy <= light * 2.0 * 1.61, "heavy ~1.8x ({heavy} vs {light})");
    // a held button does not auto-repeat: a 0.2 s hold is one light cut, not two
    let mut s = server(floor(), -1.5);
    for t in 0..40 {
        feed(&mut s, if (5..17).contains(&t) { held() } else { idle(A_YAW) }, idle(B_YAW));
    }
    assert!(100.0 - hp(&s, B) < 20.0 * 1.61, "one cut only: {}", 100.0 - hp(&s, B));
}

#[test]
fn an_air_cut_hits_25_percent_harder_and_slams_instead_of_launching() {
    let mut s = server(floor(), -1.5);
    teleport(&mut s, A, [0.0, 3.0, 0.0]);
    let z0 = pos(&s, B)[2];
    let mut slammed = false;
    for t in 0..30 {
        feed(&mut s, if t == 2 { slice() } else { idle(A_YAW) }, idle(B_YAW));
        slammed |= player(&mut s, B).kmoves.slam.is_some();
    }
    let dmg = 100.0 - hp(&s, B);
    eprintln!("KATANA air: {dmg:.1}, B moved {:.2} m", z0 - pos(&s, B)[2]);
    assert!(dmg >= 20.0 * 1.2, "air cut +25 % ({dmg})");
    assert!(slammed, "the victim is slammed, a shock-wave waits for the landing");
    assert!(z0 - pos(&s, B)[2] < 1.0, "driven into the ground, not thrown back");
}

#[test]
fn a_counter_cut_after_a_parry_reaches_and_scales_with_the_parried_hit() {
    let mut s = server(floor(), -5.0);
    let t0 = s.tick;
    player(&mut s, A).kmoves.counter = Some((B, t0 + 60, 20.0));
    for t in 0..30 {
        feed(&mut s, if t == 3 { slice() } else { idle(A_YAW) }, idle(B_YAW));
    }
    let dmg = 100.0 - hp(&s, B);
    eprintln!("KATANA counter: {dmg:.1}");
    assert!((dmg - 30.0).abs() < 1.5, "20 hp parried -> 1 + 0.5 x1 = 1.5x ({dmg})");
    assert!(player(&mut s, A).kmoves.counter.is_none(), "the window is spent");
}

#[test]
fn a_whirl_cut_just_after_lowering_the_shield_hits_all_round() {
    // B stands BEHIND A: only a 360 sweep reaches
    let mut s = server(floor(), 1.5);
    let t0 = s.tick;
    player(&mut s, A).kmoves.unblocked_at = Some(t0 + 2);
    for t in 0..30 {
        feed(&mut s, if t == 3 { slice() } else { idle(A_YAW) }, idle(B_YAW));
    }
    let dmg = 100.0 - hp(&s, B);
    eprintln!("KATANA whirl: {dmg:.1}");
    assert!(dmg > 8.0 && dmg < 20.0, "~12 behind the swordsman ({dmg})");
    // without the shield trick the same cut whiffs
    let mut s = server(floor(), 1.5);
    for t in 0..30 {
        feed(&mut s, if t == 3 { slice() } else { idle(A_YAW) }, idle(B_YAW));
    }
    assert_eq!(hp(&s, B), 100.0, "a plain cut does not reach behind");
}

#[test]
fn a_cut_on_a_stunned_low_hp_player_executes_but_not_a_healthy_or_unstunned_one() {
    for (hp0, stunned, dies) in [(24.0f32, true, true), (60.0, true, false), (24.0, false, false)] {
        let mut s = server(floor(), -1.5);
        let t0 = s.tick;
        let b = player(&mut s, B);
        b.vitals.hp = hp0;
        if stunned {
            b.blk.stagger_until = t0 + 200;
        }
        let mut kills = 0;
        for t in 0..30 {
            let ev = feed(&mut s, if t == 3 { slice() } else { idle(A_YAW) }, idle(B_YAW));
            kills += ev.iter().filter(|e| matches!(e, PvpEvent::Kill { .. })).count();
        }
        let _ = kills;
        let b = player(&mut s, B);
        let dead = b.vitals.deaths > 0;
        assert_eq!(dead, dies, "hp {hp0} stunned {stunned}: hp now {}", b.vitals.hp);
    }
}
