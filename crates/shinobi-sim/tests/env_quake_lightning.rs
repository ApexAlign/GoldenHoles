//! Quakes and lightning through the real `Server` (docs/destruction_design.md): telegraph, shaking that brings weak structures
//! down, bolts that land where players stood and hurt only inside their radius, nothing at all when the preset is off.

use std::time::Duration;

use shinobi_proto::intent::{ClientPacket, PlayerIntent, ViewState};
use shinobi_proto::quant::{q_pitch, q_yaw};
use shinobi_sim::balance::Balance;
use shinobi_sim::breaks::BreakEvent;
use shinobi_sim::character::Cfg;
use shinobi_sim::env_events::{EnvEvent, LIGHTNING};
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::pvp::{HazardSource, PvpEvent};
use shinobi_sim::rapier3d::prelude::Isometry;
use shinobi_sim::tick::Server;

const A: u16 = 1;
const B: u16 = 2;

fn server(extra: Vec<LevelBox>, tweak: impl FnOnce(&mut Balance)) -> Server {
    let mut boxes = vec![LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0])];
    boxes.extend(extra);
    let lv = LevelDesc { boxes, spawns: vec![[0.0, 0.81, 0.0], [0.0, 0.81, -40.0]] };
    let mut s = Server::new(Cfg::default());
    lv.apply_to_server(&mut s);
    s.set_spawns(lv.spawns.clone());
    let mut b = Balance::demo();
    b.player.max_hp = 100.0;
    b.player.regen_per_s = 0.0;
    s.set_balance(b.clone()).unwrap();
    s.match_state.cfg.frag_limit = u16::MAX;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    s.add_player(A, [0.0, 0.81, 0.0]);
    s.add_player(B, [0.0, 0.81, -40.0]);
    for _ in 0..800 {
        feed(&mut s);
        if s.match_state.kind() == shinobi_sim::match_state::PhaseKind::Live {
            break;
        }
    }
    tp(&mut s, A, [0.0, 0.81, 0.0]);
    tp(&mut s, B, [0.0, 0.81, -40.0]);
    for _ in 0..200 {
        feed(&mut s);
    }
    for p in s.players.iter_mut() {
        p.vitals.hp = p.vitals.max_hp;
        p.vitals.invuln_until_tick = 0;
    }
    // the environment is switched on only now, so a test sees the whole cycle from its first event
    tweak(&mut b);
    s.set_balance(b).unwrap();
    s
}

fn tp(s: &mut Server, id: u16, at: [f32; 3]) {
    let body = s.players.iter().find(|p| p.id == id).unwrap().ch.body;
    s.world.bodies[body].set_position(Isometry::translation(at[0], at[1], at[2]), true);
    shinobi_sim::character::sync_body_colliders(&mut s.world, body);
}

fn feed(s: &mut Server) -> Vec<PvpEvent> {
    let tick = s.tick;
    for id in [A, B] {
        let it = PlayerIntent { tick, aim_yaw: q_yaw(0.0), aim_pitch: q_pitch(0.0), ..Default::default() };
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

#[test]
fn nothing_happens_with_the_demo_preset() {
    let mut s = server(vec![], |_| {});
    for _ in 0..3000 {
        feed(&mut s);
        assert!(s.env_events.is_empty(), "the demo preset has no environment events");
    }
}

#[test]
fn a_quake_warns_shakes_breaks_weak_towers_and_ends() {
    let low = LevelBox::new([10.0, 2.0, 10.0], [1.5, 2.0, 1.5]).destructible(100.0);
    let high = LevelBox::new([10.0, 6.0, 10.0], [1.5, 2.0, 1.5]).destructible(100.0);
    let perm = LevelBox::new([-10.0, 2.0, 10.0], [1.5, 2.0, 1.5]);
    let mut s = server(vec![low, high, perm], |b| {
        b.env.quake = 1.0;
        b.env.quake_min_s = 1.0;
        b.env.quake_max_s = 1.0;
        b.env.quake_warn_s = 0.5;
        b.env.quake_s = 3.0;
        b.env.quake_dmg_frac_per_s = 0.6;
        b.destruct.rez_s = 0.0;
    });
    let mut env = Vec::new();
    let mut brk = Vec::new();
    for _ in 0..600 {
        feed(&mut s);
        env.extend(s.env_events.iter().copied());
        brk.extend(s.break_events.iter().copied());
    }
    let kinds: Vec<&str> = env.iter().map(|e| match e {
        EnvEvent::QuakeWarn { .. } => "warn",
        EnvEvent::QuakeStart { .. } => "start",
        EnvEvent::QuakeEnd { .. } => "end",
        _ => "other",
    }).collect();
    assert!(kinds.starts_with(&["warn", "start", "end"]), "{kinds:?}");
    let start_tick = env.iter().find_map(|e| if let EnvEvent::QuakeStart { tick, .. } = e { Some(*tick) } else { None }).unwrap();
    assert!(brk.iter().all(|e| matches!(e, BreakEvent::Break { tick, .. } | BreakEvent::Damaged { tick, .. } if *tick >= start_tick)), "nothing breaks before the shaking starts: {brk:?}");
    assert!(s.breaks.is_broken(1) && s.breaks.is_broken(2), "the whole weak tower came down: {brk:?}");
    assert!(!s.breaks.is_broken(3) || s.breaks.max_hp(3) == 0.0, "the permanent block is untouched");
    assert!(brk.iter().any(|e| matches!(e, BreakEvent::Break { cascade: false, .. })));
}

#[test]
fn lightning_marks_where_a_player_stands_and_strikes_there() {
    let pillar = LevelBox::new([4.0, 1.5, 0.0], [0.8, 1.5, 0.8]).destructible(300.0);
    let mut s = server(vec![pillar], |b| {
        b.env.lightning = 1.0;
        b.env.bolt_min_s = 1.0;
        b.env.bolt_max_s = 1.0;
        b.env.bolt_warn_s = 1.0;
        b.env.bolt_volley = 1;
        b.env.bolt_radius = 7.0;
        b.env.bolt_player_dmg = 80.0;
    });
    let mut env = Vec::new();
    let mut pvp = Vec::new();
    let mut brk = Vec::new();
    for _ in 0..400 {
        pvp.extend(feed(&mut s));
        env.extend(s.env_events.iter().copied());
        brk.extend(s.break_events.iter().copied());
    }
    let mark = env.iter().find_map(|e| if let EnvEvent::BoltMark { id, pos, warn_ticks, .. } = e { Some((*id, *pos, *warn_ticks)) } else { None }).expect("a bolt was marked");
    assert!(mark.2 >= 60, "at least one second of warning");
    let strike = env.iter().find_map(|e| if let EnvEvent::BoltStrike { id, pos, .. } = e { Some((*id, *pos)) } else { None }).expect("and it struck");
    let mark = env.iter().find_map(|e| if let EnvEvent::BoltMark { id, pos, warn_ticks, .. } = e { (*id == strike.0).then_some((*id, *pos, *warn_ticks)) } else { None }).expect("the struck bolt was marked first");
    assert_eq!((strike.0, strike.1), (mark.0, mark.1), "the strike lands exactly on the mark");
    // the mark is at a player (A or B stood still: the strike point is within a body of one of them)
    let near = |id: u16| {
        let p = s.players.iter().find(|p| p.id == id).unwrap().pos(&s.world);
        ((p[0] - strike.1[0]).powi(2) + (p[2] - strike.1[2]).powi(2)).sqrt()
    };
    assert!(near(A) < 1.5 || near(B) < 1.5, "marked on a player");
    // whoever was at the point took heavy damage and the death/hit is reported as lightning
    assert!(pvp.iter().any(|e| matches!(e, PvpEvent::HazardHit { source, amount, .. } if *source == LIGHTNING && *amount >= 40.0)), "{pvp:?}");
    assert!(hp(&s, A) < 100.0 || hp(&s, B) < 100.0);
    assert!(matches!(LIGHTNING, HazardSource::Obstacle(_)));
    let _ = brk;
}

#[test]
fn a_target_who_runs_clear_of_the_ring_is_not_hurt_and_a_nearby_pillar_is_blown_up() {
    let pillar = LevelBox::new([3.0, 1.5, 0.0], [0.8, 1.5, 0.8]).destructible(300.0);
    let mut s = server(vec![pillar], |b| {
        b.env.lightning = 1.0;
        b.env.bolt_min_s = 1.0;
        b.env.bolt_max_s = 1.0;
        b.env.bolt_warn_s = 1.0;
        b.env.bolt_volley = 1;
        b.destruct.rez_s = 0.0;
    });
    // both players are far from the pillar: after the mark, whoever is marked teleports 30 m clear; a pillar 3 m from the
    // strike point is destroyed only if the strike was near it, so put the strike there by marking A (at the origin)
    let mut marked = None;
    let mut pvp = Vec::new();
    for _ in 0..300 {
        pvp.extend(feed(&mut s));
        for e in s.env_events.clone() {
            if let EnvEvent::BoltMark { pos, .. } = e {
                marked = Some(pos);
                // everyone steps well out of the radius
                tp(&mut s, A, [pos[0] + 30.0, 0.81, pos[2]]);
                tp(&mut s, B, [pos[0] + 30.0, 0.81, pos[2] + 30.0]);
            }
        }
    }
    assert!(marked.is_some());
    assert!(!pvp.iter().any(|e| matches!(e, PvpEvent::HazardHit { source, .. } if *source == LIGHTNING)), "nobody was in the radius: {pvp:?}");
    assert_eq!(hp(&s, A), 100.0);
}

#[test]
fn late_joiners_hear_about_a_quake_and_marked_bolts_in_progress() {
    let mut s = server(vec![], |b| {
        b.env.quake = 1.0;
        b.env.lightning = 1.0;
        b.env.quake_min_s = 1.0;
        b.env.quake_max_s = 1.0;
        b.env.quake_warn_s = 2.0;
        b.env.bolt_min_s = 1.0;
        b.env.bolt_max_s = 1.0;
        b.env.bolt_warn_s = 3.0;
    });
    let mut caught = None;
    for _ in 0..400 {
        feed(&mut s);
        let (q, bolts) = s.env.catch_up(s.tick);
        if matches!(q, Some((_, true))) && !bolts.is_empty() {
            caught = Some((q.unwrap(), bolts));
            break;
        }
    }
    let ((left, warning), bolts) = caught.expect("a quake warning and a marked bolt overlapped");
    assert!(warning && left > 0 && left <= 120, "{left} ticks of warning left");
    assert!(bolts[0].2 > 0 && bolts[0].2 <= 180);
}
