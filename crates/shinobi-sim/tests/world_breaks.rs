//! Destructible level boxes through the real `Server` (docs/destructible.md): a thrown star and a katana swing wear a
//! `dbox` pillar down and break it, a tag blast shatters what is near, a tower cut at the foot brings its top down, the
//! pieces come back after the re-form time, and the break events carry what a client needs.

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState};
use shinobi_proto::quant::{q_pitch, q_yaw};
use shinobi_sim::balance::Balance;
use shinobi_sim::breaks::BreakEvent;
use shinobi_sim::character::Cfg;
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::power_pass::Tag;
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::rapier3d::prelude::Isometry;
use shinobi_sim::tick::Server;

const A: u16 = 1;
const B: u16 = 2;
/// B stands at -Z looking back at A (+Z); A looks down -Z.
const B_YAW: f32 = std::f32::consts::PI;

fn server_with(b_z: f32, extra: Vec<LevelBox>, tweak: impl FnOnce(&mut Balance)) -> Server {
    let mut boxes = vec![LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0])];
    boxes.extend(extra);
    let lv = LevelDesc { boxes, spawns: vec![[0.0, 0.81, 0.0], [0.0, 0.81, b_z]] };
    let mut s = Server::new(Cfg::default());
    lv.apply_to_server(&mut s);
    s.set_spawns(lv.spawns.clone());
    let mut b = Balance::demo();
    b.player.max_hp = 100.0;
    b.player.regen_per_s = 0.0;
    b.melee.knock_speed = 0.0;
    tweak(&mut b);
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

fn drawing(yaw: f32, pitch: f32) -> PlayerIntent {
    PlayerIntent { draw: true, aim_yaw: q_yaw(yaw), aim_pitch: q_pitch(pitch), ..Default::default() }
}

fn press(yaw: f32, pitch: f32) -> PlayerIntent {
    PlayerIntent { buttons: Buttons(Buttons::FIRE), aim_yaw: q_yaw(yaw), aim_pitch: q_pitch(pitch), ..Default::default() }
}

fn guard(yaw: f32) -> PlayerIntent {
    PlayerIntent { block: true, aim_yaw: q_yaw(yaw), aim_pitch: q_pitch(0.0), ..Default::default() }
}

fn swing(yaw: f32) -> PlayerIntent {
    PlayerIntent { buttons: Buttons(Buttons::SLICE), aim_yaw: q_yaw(yaw), aim_pitch: q_pitch(0.0), ..Default::default() }
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


/// Box index of `extra[0]` is 1 (the floor is 0).
fn breaks_seen(s: &Server, store: &mut Vec<BreakEvent>) {
    store.extend(s.break_events.iter().copied());
}

#[test]
fn thrown_stars_chip_a_pillar_until_it_breaks() {
    let mut s = server_with(-40.0, vec![LevelBox::new([0.0, 1.5, -6.0], [1.0, 1.5, 0.3]).destructible(14.0)], |_| {});
    assert_eq!(s.breaks.len(), 1);
    let mut ev = Vec::new();
    let mut threw = 0;
    for t in 0..900 {
        // a star every 20 ticks at the pillar (A looks down -Z), aimed level at pillar height
        let it = if t % 20 == 0 { threw += 1; press(0.0, 0.0) } else { idle(0.0) };
        feed(&mut s, it, idle(B_YAW));
        breaks_seen(&s, &mut ev);
        if s.breaks.is_broken(1) {
            break;
        }
    }
    assert!(s.breaks.is_broken(1), "{threw} stars and the 14 hp pillar still stands; events {ev:?}");
    assert!(ev.iter().any(|e| matches!(e, BreakEvent::Damaged { idx: 1, .. })), "it cracked first: {ev:?}");
    assert!(ev.iter().any(|e| matches!(e, BreakEvent::Break { idx: 1, cascade: false, .. })));
    // a star now flies on through where it stood
    s.world.queries.update(&s.world.colliders);
    assert!(!shinobi_sim::spawn::blocked(&s.world, [0.0, 1.5, -3.0], [0.0, 1.5, -9.0]));
}

#[test]
fn a_katana_swing_cuts_a_light_wall() {
    let mut s = server_with(-40.0, vec![LevelBox::new([0.0, 1.5, -1.8], [1.5, 1.5, 0.3]).destructible(50.0)], |_| {});
    let mut ev = Vec::new();
    for t in 0..240 {
        let it = if t % 40 == 5 { swing(0.0) } else { idle(0.0) };
        feed(&mut s, it, idle(B_YAW));
        breaks_seen(&s, &mut ev);
        if s.breaks.is_broken(1) {
            break;
        }
    }
    assert!(s.breaks.is_broken(1), "a katana swing (piece damage 60 default) cuts a 50 hp wall; events {ev:?}");
}

#[test]
fn a_tag_blast_shatters_nearby_boxes_and_cascades_up_a_tower_then_all_return() {
    let low = LevelBox::new([8.0, 2.0, -10.0], [1.5, 2.0, 1.5]).destructible(150.0);
    let high = LevelBox::new([8.0, 6.0, -10.0], [1.5, 2.0, 1.5]).destructible(150.0);
    let far = LevelBox::new([-30.0, 2.0, -10.0], [1.5, 2.0, 1.5]).destructible(150.0);
    let mut s = server_with(-40.0, vec![low, high, far], |_| {});
    assert_eq!(s.breaks.len(), 3);
    let mut ev = Vec::new();
    let mut t = Tag::stuck_at(A, 1, [6.0, 1.0, -10.0], [0.0, 1.0, 0.0]);
    t.age = 100.0; // past the fuse: goes off on the next tick
    s.tags.push(t);
    for _ in 0..4 {
        feed(&mut s, idle(0.0), idle(B_YAW));
        breaks_seen(&s, &mut ev);
    }
    assert!(s.breaks.is_broken(1), "the foot of the tower is within the blast: {ev:?}");
    assert!(!s.breaks.is_broken(3), "a box 36 m away is untouched");
    // the top (not in range of the foot's blast damage? it is 4 m up) falls either way; wait for the chain
    for _ in 0..60 {
        feed(&mut s, idle(0.0), idle(B_YAW));
        breaks_seen(&s, &mut ev);
    }
    assert!(s.breaks.is_broken(2), "the top comes down with the foot: {ev:?}");
    // re-form after rez_ticks
    for _ in 0..(s.breaks.cfg.rez_ticks + 200) {
        feed(&mut s, idle(0.0), idle(B_YAW));
        breaks_seen(&s, &mut ev);
    }
    assert!(!s.breaks.is_broken(1) && !s.breaks.is_broken(2), "both came back: {ev:?}");
    assert!(ev.iter().filter(|e| matches!(e, BreakEvent::Rez { .. })).count() >= 2);
    assert!(s.breaks.broken().is_empty());
}

#[test]
fn a_broken_box_stops_blocking_rays_so_a_rope_on_it_is_cut() {
    // the grapple's "anchor still there?" check and its line-of-sight check are collider queries: gone box = no hit
    let mut s = server_with(-40.0, vec![LevelBox::new([0.0, 8.0, -12.0], [2.0, 1.0, 2.0]).destructible(30.0)], |_| {});
    let mut ev = Vec::new();
    s.world.queries.update(&s.world.colliders);
    assert!(shinobi_sim::spawn::blocked(&s.world, [0.0, 8.0, -5.0], [0.0, 8.0, -15.0]));
    assert!(s.breaks.damage(&mut s.world, 1, 99.0, s.tick, &mut ev));
    s.world.queries.update(&s.world.colliders);
    assert!(!shinobi_sim::spawn::blocked(&s.world, [0.0, 8.0, -5.0], [0.0, 8.0, -15.0]));
}

#[test]
fn arrows_chip_a_box_they_hit_and_a_few_break_it() {
    use shinobi_sim::power_pass::Arrow;
    let mut s = server_with(-40.0, vec![LevelBox::new([0.0, 1.5, -12.0], [2.0, 1.5, 0.4]).destructible(60.0)], |_| {});
    let mut ev = Vec::new();
    let mut shots = 0;
    for _ in 0..8 {
        shots += 1;
        s.arrows.push(Arrow { owner: A, uid: shots, pos: [0.0, 1.5, -2.0], vel: [0.0, 0.0, -60.0], origin: [0.0, 1.5, -2.0], age: 0.0, reflected: false });
        for _ in 0..40 {
            feed(&mut s, idle(0.0), idle(B_YAW));
            breaks_seen(&s, &mut ev);
        }
        if s.breaks.is_broken(1) {
            break;
        }
    }
    assert!(ev.iter().any(|e| matches!(e, BreakEvent::Damaged { idx: 1, .. })), "the first arrow cracks it: {ev:?}");
    assert!(s.breaks.is_broken(1), "{shots} arrows and a 60 hp wall still stands: {ev:?}");
    assert!(shots >= 2, "one arrow must not be enough");
}
