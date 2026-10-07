//! Block / parry / strike window through the real `Server` (demo balance): scripted intents, flat floor.
//! A (id 1) stands at the origin facing -Z. B (id 2) stands 1.5 m in front of A (inside blade reach), facing +Z.

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

fn server() -> Server {
    let lv = LevelDesc { boxes: vec![LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0])], spawns: vec![[0.0, 0.81, 0.0], [0.0, 0.81, -1.5]] };
    let mut s = Server::new(Cfg::default());
    lv.apply_to_server(&mut s);
    s.set_spawns(lv.spawns.clone());
    let mut b = Balance::demo();
    b.melee = shinobi_sim::balance::MeleeBalance::default(); // these tests pin the base damage numbers
    s.set_balance(b).unwrap();
    s.match_state.cfg.frag_limit = u16::MAX;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    s.add_player(A, [0.0, 0.81, 0.0]);
    s.add_player(B, [0.0, 0.81, -1.5]);
    // warmup out, then put both back and clear protection / yaw
    for _ in 0..600 {
        feed(&mut s, [idle(0, 0.0), idle(0, std::f32::consts::PI)]);
        if s.match_state.kind() == shinobi_sim::match_state::PhaseKind::Live {
            break;
        }
    }
    for (id, z) in [(A, 0.0f32), (B, -1.5)] {
        let body = s.players.iter().find(|p| p.id == id).unwrap().ch.body;
        s.world.bodies[body].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(0.0, 0.81, z), true);
        shinobi_sim::character::sync_body_colliders(&mut s.world, body);
    }
    for _ in 0..200 {
        feed(&mut s, [idle(0, 0.0), idle(0, std::f32::consts::PI)]);
    }
    for p in s.players.iter_mut() {
        p.vitals.hp = p.vitals.max_hp;
    }
    s
}

fn idle(tick: u32, yaw: f32) -> PlayerIntent {
    PlayerIntent { tick, aim_yaw: q_yaw(yaw), aim_pitch: PITCH_LEVEL, ..Default::default() }
}

fn feed(s: &mut Server, its: [PlayerIntent; 2]) -> Vec<PvpEvent> {
    let tick = s.tick;
    for (id, mut it) in [(A, its[0]), (B, its[1])] {
        it.tick = tick;
        let from = tick.saturating_sub(6).max(1);
        let pkt = ClientPacket { ack_snapshot_tick: from, view: ViewState { from_tick: from, alpha: 0 }, intents: vec![it], ..Default::default() };
        s.ingest(id, &pkt.encode().unwrap());
    }
    s.frame(Duration::from_nanos(16_666_667));
    s.pvp_events.clone()
}

const A_YAW: f32 = 0.0; // faces -Z, toward B
const B_YAW: f32 = std::f32::consts::PI; // faces +Z, toward A

fn hp(s: &Server, id: u16) -> f32 {
    s.players.iter().find(|p| p.id == id).unwrap().vitals.hp
}

/// A swings once (the slice button is an edge) while B does `b`; returns B's hp loss and the events seen.
fn a_cuts(b: impl Fn(u32) -> PlayerIntent) -> (f32, Vec<PvpEvent>) {
    let mut s = server();
    let before = hp(&s, B);
    let mut ev = Vec::new();
    for t in 0..70u32 {
        let a = PlayerIntent { buttons: Buttons(if t == 30 { Buttons::SLICE } else { 0 }), ..idle(0, A_YAW) };
        ev.extend(feed(&mut s, [a, b(t)]));
    }
    (before - hp(&s, B), ev)
}

#[test]
fn a_plain_cut_does_20_and_a_held_shield_cuts_it_down() {
    let (plain, _) = a_cuts(|_| idle(0, B_YAW));
    assert!((plain - 20.0).abs() < 0.01, "an unguarded cut does the user's 20 hp, got {plain}");

    // B holds the shield from tick 0, long before the cut lands at tick 30 (the parry window is long over): a plain
    // block, 3 % of 20 (the base cut: no bonus punches through a shield)
    let (blocked, _) = a_cuts(|_| PlayerIntent { block: true, ..idle(0, B_YAW) });
    assert!((blocked - 0.6).abs() < 0.01, "a frontal block leaves 3 % ({blocked})");
}

#[test]
fn a_shield_does_nothing_against_a_hit_from_behind() {
    // B shields but faces AWAY from A: the cut lands on the back
    let (hit, _) = a_cuts(|_| PlayerIntent { block: true, ..idle(0, A_YAW) });
    assert!((hit - 20.0).abs() < 0.01, "from behind the shield is no cover ({hit})");
}

#[test]
fn a_block_raised_just_before_the_cut_is_a_parry_that_staggers_the_attacker() {
    // B raises the shield on tick 29, one tick before A cuts (tick 30), and holds: well inside the parry window
    let (loss, ev) = a_cuts(|t| PlayerIntent { block: t >= 29, ..idle(0, B_YAW) });
    assert!(loss < 0.01, "a parry negates the cut ({loss})");
    assert!(ev.iter().any(|e| matches!(e, PvpEvent::Hit { attacker: A, victim: B, amount, .. } if *amount == 0.0)), "the parried hit is still reported: {ev:?}");

    // (B waits until the shield has been down a while: a cut right after lowering it is a whirl cut, see katana_moves.rs)
    // A is now staggered and open: B's counter-cut deals the bonus (20 x 1.5 = 30) and A cannot cut back meanwhile
    let mut s = server();
    let mut dealt = Vec::new();
    for t in 0..120u32 {
        let a = PlayerIntent { buttons: Buttons(if t == 5 || t == 30 { Buttons::SLICE } else { 0 }), ..idle(0, A_YAW) };
        let b = PlayerIntent { block: (4..12).contains(&t), buttons: Buttons(if t == 40 { Buttons::SLICE } else { 0 }), ..idle(0, B_YAW) };
        for e in feed(&mut s, [a, b]) {
            if let PvpEvent::Hit { attacker, victim, amount, bonus, .. } = e {
                dealt.push((t, attacker, victim, amount, bonus));
            }
        }
    }
    let counter = dealt.iter().find(|h| h.1 == B && h.2 == A).expect("B countered");
    assert!((counter.3 - 30.0).abs() < 0.01 && counter.4, "the counter hits the open attacker for 30 with the bonus flag: {dealt:?}");
    assert!(!dealt.iter().any(|h| h.1 == A && h.2 == B && h.3 > 0.0 && h.0 < 5 + 48), "A stays staggered ~0.8 s: no cut from A landed in that time: {dealt:?}");
}

#[test]
fn a_cut_leaves_the_swordsman_open_for_a_moment_and_a_late_hit_is_plain() {
    // A cuts at tick 5 and is open for 0.25 s (15 ticks); B strikes back inside that window: 30. Much later: 20.
    let strike = |when: u32| {
        let mut s = server();
        let mut got = None;
        for t in 0..80u32 {
            let a = PlayerIntent { buttons: Buttons(if t == 5 { Buttons::SLICE } else { 0 }), ..idle(0, A_YAW) };
            let b = PlayerIntent { buttons: Buttons(if t == when { Buttons::SLICE } else { 0 }), ..idle(0, B_YAW) };
            for e in feed(&mut s, [a, b]) {
                if let PvpEvent::Hit { attacker: B, victim: A, amount, bonus, .. } = e {
                    got = Some((amount, bonus));
                }
            }
        }
        got
    };
    let (amt, bonus) = strike(10).expect("a quick counter lands");
    assert!(bonus && (amt - 30.0).abs() < 0.01, "inside the strike window the counter is worth 1.5x: {amt} {bonus}");
    let (amt, bonus) = strike(50).expect("a late cut lands");
    assert!(!bonus && (amt - 20.0).abs() < 0.01, "outside it the cut is the base 20: {amt} {bonus}");
}

#[test]
fn the_view_is_locked_while_the_shield_is_up_but_the_body_still_moves() {
    let mut s = server();
    let yaw0 = s.players.iter().find(|p| p.id == B).unwrap().yaw;
    let z0 = s.players.iter().find(|p| p.id == B).unwrap().pos(&s.world)[2];
    for t in 0..60u32 {
        // B holds the shield, runs forward and spins its claimed aim by a legal 5 degrees a tick
        let b = PlayerIntent { block: true, move_z: 127, aim_yaw: q_yaw(B_YAW + 0.09 * (t as f32)), ..idle(0, B_YAW) };
        feed(&mut s, [idle(0, A_YAW), b]);
    }
    let p = s.players.iter().find(|p| p.id == B).unwrap();
    assert_eq!(p.yaw, yaw0, "the shield froze the aim at the direction it went up");
    assert!((p.pos(&s.world)[2] - z0).abs() > 1.0, "but the body kept moving at full speed while blocking");
}
