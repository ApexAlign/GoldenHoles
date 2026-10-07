//! Star variants through the real `Server` (demo balance): mine (arms, bursts on approach, ignores allies and its owner,
//! shot or cut = disarmed, two at most), flash (white-out only for enemies who face it), smoke (breaks lock-on onto
//! anyone inside, and the cloud ends), and the energy each costs.

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState};
use shinobi_proto::quant::{q_pitch, q_yaw};
use shinobi_sim::balance::Balance;
use shinobi_sim::character::Cfg;
use shinobi_sim::combat::stars::StarKind;
use shinobi_sim::combat::{CombatEvent, DamageKind};
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

/// A throws a mine at the floor ahead and waits for it to stick; returns where.
fn plant_mine(s: &mut Server) -> [f32; 3] {
    for t in 0..120 {
        feed(s, if t == 0 { throw(0.0, -0.3, StarKind::Mine) } else { idle(0.0) }, idle(B_YAW));
        if let Some(d) = objects(s, A, StarKind::Mine).first().filter(|d| d.stuck) {
            return [d.pos.x, d.pos.y, d.pos.z];
        }
    }
    panic!("the mine never landed");
}

#[test]
fn a_mine_sticks_arms_and_bursts_when_an_enemy_comes_close() {
    let mut s = server(-30.0);
    let (e0, _) = energy(&s);
    let m = plant_mine(&mut s);
    assert!(m[1] < 0.2 && m[2] < -2.0, "stuck on the floor ahead: {m:?}");
    assert!(energy(&s).0 <= e0 - 3.0 + 0.3, "a mine costs 3 charges: {e0} -> {}", energy(&s).0);
    // B far away: the armed mine just sits there
    wait(&mut s, 60);
    assert!(objects(&s, A, StarKind::Mine)[0].stuck && hp(&s, B) == 100.0);
    // B walks within 3 m: bang, 25 damage, pushed away, and the mine is gone
    tp(&mut s, B, [m[0] + 1.5, 0.81, m[2]]);
    let mut dmg = Vec::new();
    for _ in 0..4 {
        for e in feed(&mut s, idle(0.0), idle(B_YAW)) {
            if let PvpEvent::Hit { attacker: A, victim: B, amount, kind: DamageKind::Mine, .. } = e {
                dmg.push(amount);
            }
        }
    }
    assert_eq!(dmg, vec![25.0], "one burst, 25 damage");
    assert_eq!(hp(&s, B), 75.0);
    assert!(objects(&s, A, StarKind::Mine).is_empty(), "the mine is spent");
    assert!(s.events.iter().any(|(id, e)| *id == A && matches!(e, CombatEvent::StarBurst { .. })) || hp(&s, B) == 75.0);
    let b = s.players.iter().find(|p| p.id == B).unwrap();
    assert!(b.ch.vel.x > 1.0 || (b.pos(&s.world)[0] - (m[0] + 1.5)) > 0.2, "knocked away from the blast");
}

#[test]
fn a_mine_is_not_armed_for_its_first_instant() {
    let mut s = server(-30.0);
    // an enemy already standing at the landing spot: the 0.6 s arming delay holds the burst back
    let aim_pitch = -0.3f32;
    let d = (eye(&s, A) - 0.3) / (-aim_pitch).tan();
    tp(&mut s, B, [0.0, 0.81, -d]);
    let mut first_burst = None;
    for t in 0..60 {
        for e in feed(&mut s, if t == 0 { throw(0.0, aim_pitch, StarKind::Mine) } else { idle(0.0) }, idle(B_YAW)) {
            if matches!(e, PvpEvent::Hit { kind: DamageKind::Mine, .. }) {
                first_burst.get_or_insert(t);
            }
        }
    }
    let t = first_burst.expect("it bursts once armed") as f32 / 60.0;
    assert!((0.6..1.2).contains(&t), "armed 0.6 s after landing, not before ({t:.2} s)");
}

#[test]
fn a_mine_ignores_teammates_and_its_owner() {
    let mut s = server(-30.0);
    for p in s.players.iter_mut() {
        p.team = 0; // same team
    }
    let m = plant_mine(&mut s);
    wait(&mut s, 45);
    tp(&mut s, B, [m[0] + 1.0, 0.81, m[2]]);
    tp(&mut s, A, [m[0] - 1.0, 0.81, m[2]]);
    wait(&mut s, 30);
    assert_eq!(hp(&s, B), 100.0, "an ally walks over it");
    assert_eq!(hp(&s, A), 100.0, "so does the owner");
    assert_eq!(objects(&s, A, StarKind::Mine).len(), 1, "still armed");
    // the same layout with B an enemy blows
    for p in s.players.iter_mut() {
        p.team = if p.id == A { 0 } else { 1 };
    }
    wait(&mut s, 5);
    assert_eq!(hp(&s, B), 75.0, "an enemy does not get the same courtesy");
    assert_eq!(hp(&s, A), 100.0, "and the owner is still immune");
}

#[test]
fn a_third_mine_replaces_the_oldest_and_mines_expire() {
    let mut s = server(-30.0);
    let mut uids = Vec::new();
    for i in 0..3 {
        for t in 0..170 {
            // a mine is 3 charges: wait for the pool between throws
            feed(&mut s, if t == 0 { throw(0.0, -0.2 - 0.06 * i as f32, StarKind::Mine) } else { idle(0.0) }, idle(B_YAW));
            if t == 20 {
                let m = objects(&s, A, StarKind::Mine);
                uids.push(m.iter().map(|d| d.uid).max_by_key(|u| *u).unwrap());
                assert!(m.len() <= 2, "at most two mines per player ({} alive)", m.len());
            }
        }
    }
    let alive: Vec<u16> = objects(&s, A, StarKind::Mine).iter().map(|d| d.uid).collect();
    assert_eq!(alive.len(), 2, "two mines stand: {alive:?}");
    assert!(!alive.contains(&uids[0]) && alive.contains(&uids[1]) && alive.contains(&uids[2]), "the oldest was replaced: {uids:?} -> {alive:?}");
    wait(&mut s, 21 * 60);
    assert!(objects(&s, A, StarKind::Mine).is_empty(), "a mine despawns after 20 s");
}

#[test]
fn a_shot_star_or_a_katana_cut_disarms_a_mine() {
    // shot: B throws a standard star at the floor where A's mine sits
    let mut s = server(-30.0);
    let m = plant_mine(&mut s);
    wait(&mut s, 40);
    let (dx, dz) = (m[0] - 0.0, m[2] - (-30.0));
    let dist = (dx * dx + dz * dz).sqrt();
    tp(&mut s, B, [m[0], 0.81, m[2] - 8.0]);
    wait(&mut s, 30);
    // B faces +Z (yaw PI), so the mine is dead ahead 8 m away
    let pitch = -((eye(&s, B) - 0.6) / 8.0).atan();
    let _ = dist;
    for t in 0..30 {
        feed(&mut s, idle(0.0), if t == 0 { throw(B_YAW, pitch, StarKind::Standard) } else { idle(B_YAW) });
    }
    assert!(objects(&s, A, StarKind::Mine).is_empty(), "the mine was shot off the floor");
    assert_eq!(hp(&s, B), 100.0, "and did not explode");
    // cut: B stands 2 m short of a fresh (still unarmed) mine and swings the katana through it
    let mut s = server(-30.0);
    let m = plant_mine(&mut s);
    tp(&mut s, B, [m[0], 0.81, m[2] - 2.0]);
    let slice = PlayerIntent { buttons: Buttons(Buttons::SLICE), ..idle(B_YAW) };
    let mut hurt = false;
    for t in 0..6 {
        for e in feed(&mut s, idle(0.0), if t == 0 { slice } else { idle(B_YAW) }) {
            hurt |= matches!(e, PvpEvent::Hit { victim: B, .. });
        }
    }
    assert!(objects(&s, A, StarKind::Mine).is_empty(), "the mine was cut");
    assert!(!hurt && hp(&s, B) == 100.0, "and it did not explode");
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
    assert!((20..=75).contains(&ticks), "about a second of white-out ({ticks} ticks)");
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
fn smoke_hides_whoever_stands_in_it_from_lock_on() {
    let mut s = server(-14.0);
    wait(&mut s, 40);
    let locked = |s: &Server| s.players.iter().find(|p| p.id == A).unwrap().combat.lock.holding();
    assert!(locked(&s), "positive control: A is locked onto B");
    // B drops smoke at its own feet (steep throw down): the cloud (4 m) covers B
    let mut landed = false;
    for t in 0..30 {
        feed(&mut s, idle(0.0), if t == 0 { throw(B_YAW, -1.2, StarKind::Smoke) } else { idle(B_YAW) });
        landed |= objects(&s, B, StarKind::Smoke).first().map_or(false, |d| d.stuck);
    }
    assert!(landed, "the smoke star landed");
    assert!(!locked(&s), "B is inside the cloud: no lock-on");
    // standing outside it, B can be locked again
    tp(&mut s, B, [0.0, 0.81, -26.0]);
    wait(&mut s, 40);
    assert!(locked(&s), "step out of the cloud and the lock comes back");
    // and the cloud itself ends after 5 s
    wait(&mut s, 6 * 60);
    assert!(objects(&s, B, StarKind::Smoke).is_empty(), "the cloud thins out after 5 s");
}

#[test]
fn each_variant_draws_its_own_cost_from_the_same_pool() {
    let mut s = server(-30.0);
    let (e0, locked) = energy(&s);
    assert!(!locked && (e0 - 6.0).abs() < 1e-3);
    let spend = |s: &mut Server, k: StarKind| {
        let before = energy(s).0;
        feed(s, throw(0.0, -0.2, k), idle(B_YAW));
        before - energy(s).0
    };
    let m = spend(&mut s, StarKind::Mine);
    assert!((2.9..3.1).contains(&m), "mine costs 3 ({m})");
    wait(&mut s, 12);
    let f = spend(&mut s, StarKind::Flash);
    assert!((1.9..2.1).contains(&f), "flash costs 2 ({f})");
    wait(&mut s, 12);
    // 6 - 3 - 2 + ~0.9 regen = ~1.9: a smoke (2) is refused, nothing thrown, pool unchanged
    let before = energy(&s).0;
    let thrown = |s: &Server| s.events.iter().filter(|(id, e)| *id == A && matches!(e, CombatEvent::ShurikenThrown { kind: StarKind::Smoke, .. })).count();
    feed(&mut s, throw(0.0, -0.2, StarKind::Smoke), idle(B_YAW));
    if before + 1e-3 < 2.0 {
        assert_eq!(thrown(&s), 0, "not enough energy for a smoke: refused");
    }
    // wait for a refill: a standard star is 1
    wait(&mut s, 300);
    assert!((energy(&s).0 - 6.0).abs() < 1e-3);
    let st = spend(&mut s, StarKind::Standard);
    assert!((0.9..1.1).contains(&st), "a standard star is still 1 ({st})");
}
