//! A star thrown at a raised shield (block or parry) does no damage and ricochets straight back at its thrower: the
//! reflected star is the shield-holder's (it can hit the original thrower and credits the holder), the shield pays.

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState};
use shinobi_proto::quant::{q_pitch, q_yaw};
use shinobi_sim::balance::Balance;
use shinobi_sim::character::Cfg;
use shinobi_sim::combat::DamageKind;
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::rapier3d::prelude::Isometry;
use shinobi_sim::tick::Server;

const A: u16 = 1;
const B: u16 = 2;
/// B stands at -Z looking back at A (+Z); A looks down -Z.
const B_YAW: f32 = std::f32::consts::PI;

fn server(b_z: f32) -> Server {
    let lv = LevelDesc { boxes: vec![LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0])], spawns: vec![[0.0, 0.81, 0.0], [0.0, 0.81, b_z]] };
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

fn press(yaw: f32) -> PlayerIntent {
    PlayerIntent { buttons: Buttons(Buttons::FIRE), aim_yaw: q_yaw(yaw), aim_pitch: q_pitch(0.0), ..Default::default() }
}

fn guard(yaw: f32) -> PlayerIntent {
    PlayerIntent { block: true, aim_yaw: q_yaw(yaw), aim_pitch: q_pitch(0.0), ..Default::default() }
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

fn hp(s: &Server, id: u16) -> f32 {
    s.players.iter().find(|p| p.id == id).unwrap().vitals.hp
}

fn guard_energy(s: &Server, id: u16) -> f32 {
    s.players.iter().find(|p| p.id == id).unwrap().blk.energy
}

/// A throws `n` stars at B (12 m away, facing A); B does `b` every tick. Returns (events, A hp, B hp, guard energy B lost).
fn a_throws(b: impl Fn() -> PlayerIntent, n: u32) -> (Vec<PvpEvent>, f32, f32, f32) {
    let mut s = server(-12.0);
    let e0 = guard_energy(&s, B);
    let mut ev = Vec::new();
    let mut thrown = 0;
    for t in 0..400u32 {
        let a = if thrown < n && t % 30 == 0 {
            thrown += 1;
            press(0.0)
        } else {
            idle(0.0)
        };
        ev.extend(feed(&mut s, a, b()));
    }
    let lost = e0 - guard_energy(&s, B);
    (ev, hp(&s, A), hp(&s, B), lost)
}

#[test]
fn an_open_target_takes_the_star_and_nothing_comes_back() {
    let (ev, a_hp, b_hp, _) = a_throws(|| idle(B_YAW), 3);
    assert!(b_hp < 100.0, "no shield: the stars land (B {b_hp})");
    assert_eq!(a_hp, 100.0, "and nothing comes back");
    assert!(ev.iter().any(|e| matches!(e, PvpEvent::Hit { attacker: A, victim: B, kind: DamageKind::Shuriken, .. })));
}

#[test]
fn a_shield_deflects_stars_back_at_the_thrower_for_no_damage_to_itself() {
    let (ev, a_hp, b_hp, lost) = a_throws(|| guard(B_YAW), 3);
    assert_eq!(b_hp, 100.0, "the shield-holder took no damage from three stars");
    assert!(lost > 0.0, "but the shield paid for them ({lost})");
    assert!(
        ev.iter().any(|e| matches!(e, PvpEvent::Hit { attacker: A, victim: B, kind: DamageKind::Shuriken, amount, .. } if *amount == 0.0)),
        "the deflection is a zero-damage hit event: {ev:?}"
    );
    assert!(a_hp < 100.0, "the thrower ate its own star (A {a_hp})");
    assert!(
        ev.iter().any(|e| matches!(e, PvpEvent::Hit { attacker: B, victim: A, kind: DamageKind::Shuriken, amount, .. } if *amount > 0.0)),
        "the reflected star is credited to the shield-holder: {ev:?}"
    );
}

#[test]
fn a_shield_facing_away_gives_no_cover() {
    let (_, a_hp, b_hp, _) = a_throws(|| guard(0.0), 3);
    assert!(b_hp < 100.0, "back to the thrower: hit (B {b_hp})");
    assert_eq!(a_hp, 100.0);
}
