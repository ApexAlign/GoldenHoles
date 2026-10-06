//! Melee action heighteners through the real `Server` (demo balance): momentum damage, knockback + air juggle, wall-splat,
//! ring-out kill credit and the kill streak. A (id 1) at the origin facing -Z, B (id 2) 1.5 m in front.

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

#[test]
fn a_fast_swordsman_cuts_harder_than_one_standing_still() {
    // standing: B is inside blade reach, the cut is in place at the base 20
    let mut s = server(floor(), -1.5);
    for t in 0..30 {
        feed(&mut s, if t == 5 { slice() } else { idle(A_YAW) }, idle(B_YAW));
    }
    let standing = 100.0 - hp(&s, B);
    // lunging in from 5 m: the dash speed rides the cut
    let mut s = server(floor(), -5.0);
    for t in 0..30 {
        feed(&mut s, if t == 5 { slice() } else { idle(A_YAW) }, idle(B_YAW));
    }
    let lunging = 100.0 - hp(&s, B);
    eprintln!("MELEE momentum: standing cut {standing:.1}, lunging cut {lunging:.1}");
    assert!((standing - 20.0).abs() < 0.5, "standing cut is the base 20 ({standing})");
    assert!(lunging > standing * 1.25 && lunging <= 20.0 * 1.61, "a lunging cut carries momentum (+25..60 %): {lunging}");
}

#[test]
fn a_cut_launches_the_victim_away() {
    let mut s = server(floor(), -1.5);
    let z0 = pos(&s, B)[2];
    for t in 0..40 {
        feed(&mut s, if t == 5 { slice() } else { idle(A_YAW) }, idle(B_YAW));
    }
    let moved = z0 - pos(&s, B)[2];
    eprintln!("MELEE knockback: victim launched {moved:.2} m");
    assert!(moved > 2.0, "the victim is thrown back (moved {moved:.2} m)");
}

#[test]
fn a_launched_victim_stopped_by_a_wall_takes_splat_damage_and_is_stunned() {
    // a wall 1.6 m behind B: the launch hits it almost at once
    let mut boxes = floor();
    boxes.push(LevelBox::new([0.0, 2.0, -3.4], [4.0, 2.0, 0.3]));
    let mut s = server(boxes, -1.5);
    let mut kinds = Vec::new();
    for t in 0..60 {
        for e in feed(&mut s, if t == 5 { slice() } else { idle(A_YAW) }, idle(B_YAW)) {
            if let PvpEvent::HazardHit { victim: B, amount, .. } = e {
                kinds.push(amount);
            }
        }
    }
    eprintln!("MELEE splat: hazard hits {kinds:?}, hp left {:.1}", hp(&s, B));
    assert_eq!(kinds, vec![15.0], "exactly one 15 hp splat");
    let b = s.players.iter().find(|p| p.id == B).unwrap();
    assert!(b.blk.staggered(s.tick.saturating_sub(40)) || b.blk.stagger_until > 0, "and the victim was stunned");
    // the same cut with open floor behind never splats
    let mut s = server(floor(), -1.5);
    let mut splats = 0;
    for t in 0..60 {
        for e in feed(&mut s, if t == 5 { slice() } else { idle(A_YAW) }, idle(B_YAW)) {
            splats += matches!(e, PvpEvent::HazardHit { victim: B, .. }) as u32;
        }
    }
    assert_eq!(splats, 0, "open floor: no splat");
}

#[test]
fn shoving_someone_off_the_edge_is_a_kill_for_the_pusher() {
    // a 4 m square island: B stands 0.5 m from the -Z edge
    let island = vec![LevelBox::new([0.0, -0.5, 0.0], [4.0, 0.5, 4.0])];
    let mut s = server(island, -3.4);
    let (mut credit, mut killer_seen) = (None, None);
    let kills0 = s.players.iter().find(|p| p.id == A).unwrap().vitals.kills;
    for t in 0..400 {
        for e in feed(&mut s, if t == 5 { slice() } else { idle(A_YAW) }, idle(B_YAW)) {
            if let PvpEvent::HazardKill { victim: B, killer, .. } = e {
                killer_seen = Some(killer);
                credit = Some(t);
            }
        }
    }
    eprintln!("MELEE ring-out: void kill at tick {credit:?}, killer {killer_seen:?}");
    assert_eq!(killer_seen, Some(Some(A)), "the fall is credited to A");
    assert_eq!(s.players.iter().find(|p| p.id == A).unwrap().vitals.kills, kills0 + 1, "and scores for A");
}

#[test]
fn katana_kills_stack_a_streak_that_heals_and_hardens_the_next_cut() {
    let mut s = server(floor(), -1.5);
    s.players.iter_mut().find(|p| p.id == A).unwrap().vitals.hp = 50.0;
    s.players.iter_mut().find(|p| p.id == B).unwrap().vitals.hp = 10.0;
    for t in 0..30 {
        feed(&mut s, if t == 5 { slice() } else { idle(A_YAW) }, idle(B_YAW));
    }
    assert_eq!(s.players.iter().find(|p| p.id == B).unwrap().vitals.deaths, 1, "B was cut down");
    assert!((hp(&s, A) - 70.0).abs() < 0.5, "a katana kill heals 20 ({})", hp(&s, A));
    assert_eq!(s.players.iter().find(|p| p.id == A).unwrap().melee.streak, 1);
    // a second target right away is cut harder: 20 x 1.08
    let b = s.players.iter().find(|p| p.id == B).unwrap().ch.body;
    let _ = b;
    s.players.iter_mut().find(|p| p.id == A).unwrap().melee.streak_until = u32::MAX;
    let mut s2 = server(floor(), -1.5);
    s2.players.iter_mut().find(|p| p.id == A).unwrap().melee.streak = 2;
    s2.players.iter_mut().find(|p| p.id == A).unwrap().melee.streak_until = u32::MAX;
    for t in 0..30 {
        feed(&mut s2, if t == 5 { slice() } else { idle(A_YAW) }, idle(B_YAW));
    }
    let dealt = 100.0 - hp(&s2, B);
    assert!((dealt - 20.0 * 1.16).abs() < 0.5, "two stacked kills make the cut 16 % harder: {dealt}");
}
