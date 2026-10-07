//! Star energy and the MARK star through the real `Server` (demo balance): a pool of 6 charges that regenerates, a lockout
//! when it runs dry (no spamming), star-only kills that need more than a volley, and the charged mark star.

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState, PITCH_LEVEL};
use shinobi_proto::quant::q_yaw;
use shinobi_sim::balance::Balance;
use shinobi_sim::character::Cfg;
use shinobi_sim::combat::CombatEvent;
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::tick::Server;

const A: u16 = 1;
const B: u16 = 2;

fn server(b_z: f32, hp: f32) -> Server {
    let lv = LevelDesc { boxes: vec![LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0])], spawns: vec![[0.0, 0.81, 0.0], [0.0, 0.81, b_z]] };
    let mut s = Server::new(Cfg::default());
    lv.apply_to_server(&mut s);
    s.set_spawns(lv.spawns.clone());
    let mut b = Balance::demo();
    b.player.max_hp = hp;
    b.player.regen_per_s = 0.0;
    b.melee.knock_speed = 0.0; // keep the target where it is
    s.set_balance(b).unwrap();
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
        p.vitals.invuln_until_tick = 0;
        p.melee = Default::default();
    }
    s
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

const B_YAW: f32 = std::f32::consts::PI;

fn fire() -> PlayerIntent {
    PlayerIntent { buttons: Buttons(Buttons::FIRE), ..idle(0.0) }
}

fn throws(s: &Server) -> usize {
    s.events.iter().filter(|(id, e)| *id == A && matches!(e, CombatEvent::ShurikenThrown { .. })).count()
}

fn energy(s: &Server) -> (f32, bool) {
    let e = s.players.iter().find(|p| p.id == A).unwrap().combat.shuriken_energy().expect("a pool in the demo balance");
    (e.0, e.2)
}

#[test]
fn spamming_throw_is_capped_to_a_volley_a_lockout_and_a_slow_trickle() {
    let mut s = server(30.0, 1.0e6);
    let mut times = Vec::new();
    for t in 0..600u32 {
        feed(&mut s, fire(), idle(B_YAW));
        for _ in 0..throws(&s) {
            times.push(t);
        }
    }
    eprintln!("STAR throws in 10 s of mashing: {} at ticks {:?}", times.len(), &times[..times.len().min(14)]);
    // 6 charges + one per 0.45 s over 10 s = about 28 at the very most
    assert!((6..=28).contains(&times.len()), "10 s of mashing throws at most 6 + 10/0.45 stars ({})", times.len());
    // the first volley is the full pool, then there is a lockout gap before the trickle (2 charges = 0.9 s)
    assert!(times.len() >= 7 && times[5] < 80, "the first six stars go out as a quick volley: {times:?}");
    let gap = times.windows(2).map(|w| w[1] - w[0]).max().unwrap();
    assert!(gap >= 40, "running dry locks the throw for a while (longest gap {gap} ticks): {times:?}");
}

#[test]
fn a_measured_volley_is_fine_and_the_pool_refills_when_you_wait() {
    let mut s = server(30.0, 1.0e6);
    for t in 0..52u32 {
        feed(&mut s, if t % 13 == 0 { fire() } else { idle(0.0) }, idle(B_YAW)); // four stars at the weapon's own cadence
    }
    let (e, locked) = energy(&s);
    assert!(!locked && e > 3.3 && e < 4.5, "four stars cost 4 charges, about 2 came back meanwhile: {e}");
    for _ in 0..240 {
        feed(&mut s, idle(0.0), idle(B_YAW));
    }
    let (e, locked) = energy(&s);
    assert!(!locked && (e - 6.0).abs() < 1e-3, "four seconds of waiting refills the pool: {e}");
}

#[test]
fn a_star_only_kill_needs_far_more_than_one_volley() {
    // B stands 8 m away in plain sight; A mashes the throw (the lock homes every star)
    let mut s = server(-8.0, 100.0);
    let (mut hits, mut dealt, mut killed_at) = (0u32, 0.0f32, None);
    for t in 0..1800u32 {
        for e in feed(&mut s, fire(), idle(B_YAW)) {
            if let PvpEvent::Hit { attacker: A, victim: B, amount, .. } = e {
                hits += 1;
                dealt += amount;
            }
            if matches!(e, PvpEvent::Kill { killer: A, victim: B, .. }) {
                killed_at.get_or_insert(t);
            }
        }
        if killed_at.is_some() {
            break;
        }
    }
    eprintln!("STAR star-only kill: {hits} hits, {dealt:.0} dealt, kill at tick {killed_at:?} ({:.1} s)", killed_at.unwrap_or(0) as f32 / 60.0);
    let t = killed_at.expect("mashing stars eventually kills") as f32 / 60.0;
    assert!(hits >= 14, "a hundred hp is at least 14 stars of 7: {hits}");
    assert!(t >= 4.0, "a star-only kill takes several volleys' worth of time, not one burst ({t:.1} s)");
}

#[test]
fn a_mark_star_costs_two_does_no_damage_and_makes_the_next_cut_hit_40_percent_harder() {
    let mut s = server(-8.0, 100.0);
    let mark = PlayerIntent { buttons: Buttons(Buttons::FIRE), mark: true, ..idle(0.0) };
    let (e0, _) = energy(&s);
    let mut hp_events = 0;
    for t in 0..60u32 {
        for e in feed(&mut s, if t == 0 { mark } else { idle(0.0) }, idle(B_YAW)) {
            if matches!(e, PvpEvent::Hit { attacker: A, victim: B, .. }) {
                hp_events += 1;
            }
        }
    }
    let e1 = energy(&s).0;
    let b = s.players.iter().find(|p| p.id == B).unwrap();
    assert!(e1 <= e0 - 2.0 + 60.0 / 60.0 / 0.45 + 0.1, "the mark star cost 2 charges (regen since: {e0} -> {e1})");
    assert_eq!(hp_events, 0, "a mark star deals no damage");
    assert_eq!(b.vitals.hp, b.vitals.max_hp);
    assert!(b.melee.marked_by == A && b.melee.marked(s.tick), "B is marked by A");
    // now close in and cut: the marked cut is worth 1.4x, once
    let body = s.players.iter().find(|p| p.id == B).unwrap().ch.body;
    s.world.bodies[body].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(0.0, 0.81, -1.5), true);
    shinobi_sim::character::sync_body_colliders(&mut s.world, body);
    for _ in 0..30 {
        feed(&mut s, idle(0.0), idle(B_YAW)); // let the lag-compensation history catch up with the teleport
    }
    let slice = PlayerIntent { buttons: Buttons(Buttons::SLICE), ..idle(0.0) };
    let mut cuts = Vec::new();
    for t in 0..120u32 {
        for e in feed(&mut s, if t == 5 || t == 40 { slice } else { idle(0.0) }, idle(B_YAW)) {
            if let PvpEvent::Hit { attacker: A, victim: B, amount, kind: shinobi_sim::combat::DamageKind::Slash, .. } = e {
                cuts.push(amount);
            }
        }
    }
    eprintln!("STAR marked cut {cuts:?}");
    assert!(cuts.len() >= 2, "both cuts landed: {cuts:?}");
    assert!((cuts[0] - 28.0).abs() < 0.5, "the marked cut is 20 x 1.4 ({})", cuts[0]);
    assert!((cuts[1] - 20.0).abs() < 0.5, "and the mark is spent: the next cut is the base 20 ({})", cuts[1]);
}
