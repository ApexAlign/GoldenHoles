//! The yumi and the fuda tag through the real `Server` (docs/powerups.md): draw and release, damage and headshots, block /
//! parry-reflect / cut-down, tag stick / arm / fuse / detonate by hand, falloff, ring-out and wall-splat bookkeeping, self and
//! ally damage, disarming, the live cap, and the pedestals (pickup, respawn, drop on death).

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState};
use shinobi_proto::quant::{q_pitch, q_yaw};
use shinobi_sim::balance::Balance;
use shinobi_sim::character::Cfg;
use shinobi_sim::combat::{CombatEvent, DamageKind};
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::pickups::PickupEvent;
use shinobi_sim::power::PowerKind;
use shinobi_sim::power_pass::{Tag, FX_TAG_BLAST};
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::rapier3d::prelude::Isometry;
use shinobi_sim::tick::Server;

const A: u16 = 1;
const B: u16 = 2;
/// B stands at -Z looking back at A (+Z); A looks down -Z.
const B_YAW: f32 = std::f32::consts::PI;

fn server_with(b_z: f32, floor: LevelBox, tweak: impl FnOnce(&mut Balance)) -> Server {
    let lv = LevelDesc { boxes: vec![floor], spawns: vec![[0.0, 0.81, 0.0], [0.0, 0.81, b_z]] };
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

fn server(b_z: f32) -> Server {
    server_with(b_z, LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]), |_| {})
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

fn hp(s: &Server, id: u16) -> f32 {
    s.players.iter().find(|p| p.id == id).unwrap().vitals.hp
}

fn player(s: &mut Server, id: u16) -> &mut shinobi_sim::tick::Player {
    s.players.iter_mut().find(|p| p.id == id).unwrap()
}

fn give(s: &mut Server, id: u16, kind: PowerKind, ammo: u8) {
    let cfg = s.balance.combat.power;
    let p = player(s, id);
    p.power.clear();
    assert!(p.power.grant(kind, ammo, 0.0, &cfg));
}

/// A draws for `ticks` (aim `pitch`), releases, and the PvP events of the shot plus `settle` ticks come back.
fn shoot(s: &mut Server, ticks: u32, pitch: f32, settle: u32) -> Vec<PvpEvent> {
    let mut ev = Vec::new();
    for _ in 0..ticks {
        ev.extend(feed(s, drawing(0.0, pitch), idle(B_YAW)));
    }
    // the release tick keeps the aim of the draw (the arrow leaves along the live aim)
    ev.extend(feed(s, PlayerIntent { aim_pitch: q_pitch(pitch), ..idle(0.0) }, idle(B_YAW)));
    for _ in 0..settle {
        ev.extend(feed(s, idle(0.0), idle(B_YAW)));
    }
    ev
}

fn arrow_hits(ev: &[PvpEvent]) -> Vec<(f32, bool)> {
    ev.iter()
        .filter_map(|e| match e {
            PvpEvent::Hit { attacker: A, victim: B, amount, kind: DamageKind::Arrow, head, .. } => Some((*amount, *head)),
            _ => None,
        })
        .collect()
}

fn blasts(s: &Server) -> usize {
    s.events.iter().filter(|(_, e)| matches!(e, CombatEvent::PowerFx { what, .. } if *what == FX_TAG_BLAST)).count()
}

fn world_tag(s: &mut Server, owner: u16, at: [f32; 3]) {
    s.tags.push(Tag::stuck_at(owner, 7, at, [0.0, 1.0, 0.0]));
}

// ---- yumi ----

#[test]
fn a_full_draw_looses_an_arrow_for_80_body_or_120_head() {
    let mut hit_kinds = Vec::new();
    for pitch in [-0.03, -0.01, 0.0, 0.006, 0.012, 0.018] {
        let mut s = server(-30.0);
        give(&mut s, A, PowerKind::Yumi, 0);
        let hits = arrow_hits(&shoot(&mut s, 40, pitch, 40));
        assert!(hits.len() <= 1, "one arrow, at most one hit");
        assert!(player(&mut s, A).power.cooldown > 0.0, "the bow is reloading");
        for (amount, head) in hits {
            assert_eq!(amount, if head { 120.0 } else { 80.0 }, "80 body / x1.5 head");
            hit_kinds.push(head);
        }
    }
    assert!(hit_kinds.iter().any(|h| *h) && hit_kinds.iter().any(|h| !*h), "across the aims there is a head and a body hit: {hit_kinds:?}");
}

#[test]
fn a_full_draw_headshot_kills_an_unhurt_player_outright_on_the_demo_numbers() {
    let mut kills = 0;
    for pitch in [-0.03, -0.01, 0.0, 0.006, 0.012, 0.018] {
        // the demo preset as it ships (max hp from `Balance::demo()`, no override): only regen is off so the hp stays put
        let mut s = server_with(-30.0, LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]), |b| {
            *b = Balance::demo();
            b.player.regen_per_s = 0.0;
        });
        let ev = shoot(&mut s, 40, pitch, 40);
        let head = arrow_hits(&ev).iter().any(|(_, head)| *head);
        let killed = ev.iter().any(|e| matches!(e, PvpEvent::Kill { killer: A, victim: B, kind: DamageKind::Arrow, .. }));
        if head {
            assert!(killed, "the headshot killed (pitch {pitch})");
            kills += 1;
        } else if !arrow_hits(&ev).is_empty() {
            assert!(!killed && hp(&s, B) > 0.0, "a body shot hurts but does not kill (pitch {pitch})");
        }
    }
    assert!(kills >= 1, "at least one of the aims was a headshot");
}

#[test]
fn an_arrow_drops_a_little_over_distance_and_has_a_range() {
    // B stands far enough back that the faster arrow (280 m/s) cannot reach and consume it mid-measurement, but still
    // inside the level box
    let mut s = server(-80.0);
    give(&mut s, A, PowerKind::Yumi, 0);
    for _ in 0..37 {
        feed(&mut s, drawing(0.0, 0.0), idle(B_YAW));
    }
    feed(&mut s, idle(0.0), idle(B_YAW));
    let a0 = s.arrows.first().copied().expect("in flight");
    let y0 = a0.pos[1];
    for _ in 0..10 {
        feed(&mut s, idle(0.0), idle(B_YAW));
    }
    let a1 = s.arrows.first().copied().expect("still flying");
    assert!(a1.pos[1] < y0 && y0 - a1.pos[1] < 0.2, "slight drop: {y0} -> {}", a1.pos[1]);
    assert!(a1.pos[2] < a0.pos[2] - 12.0, "fast: {} m in 10 ticks", a0.pos[2] - a1.pos[2]);
    // shot away from B it flies out and is removed past its range, never hitting anyone
    let mut s = server(-30.0);
    give(&mut s, A, PowerKind::Yumi, 0);
    for _ in 0..60 {
        feed(&mut s, idle(std::f32::consts::FRAC_PI_2), idle(B_YAW)); // turn away first (the aim slew is limited)
    }
    for _ in 0..37 {
        feed(&mut s, drawing(std::f32::consts::FRAC_PI_2, 0.1), idle(B_YAW));
    }
    feed(&mut s, idle(std::f32::consts::FRAC_PI_2), idle(B_YAW));
    assert_eq!(s.arrows.len(), 1);
    for _ in 0..150 {
        feed(&mut s, idle(std::f32::consts::FRAC_PI_2), idle(B_YAW));
    }
    assert!(s.arrows.is_empty(), "past 140 m it is gone");
    assert_eq!((hp(&s, A), hp(&s, B)), (100.0, 100.0));
}

#[test]
fn a_short_draw_cancels_and_costs_no_arrow() {
    let mut s = server(-30.0);
    give(&mut s, A, PowerKind::Yumi, 0);
    let ev = shoot(&mut s, 20, -0.02, 40);
    assert!(arrow_hits(&ev).is_empty());
    assert_eq!(player(&mut s, A).power.cooldown, 0.0, "a cancelled draw does not start the reload");
    assert!(s.arrows.is_empty());
}

#[test]
fn drawing_slows_the_walk_by_a_quarter() {
    let mut walk = PlayerIntent { move_z: 127, aim_yaw: q_yaw(0.0), aim_pitch: q_pitch(0.0), ..Default::default() };
    let mut s = server(60.0);
    for _ in 0..90 {
        feed(&mut s, walk, idle(B_YAW));
    }
    let free = -s.players.iter().find(|p| p.id == A).unwrap().pos(&s.world)[2];
    let mut s = server(60.0);
    give(&mut s, A, PowerKind::Yumi, 0);
    walk.draw = true;
    for _ in 0..90 {
        feed(&mut s, walk, idle(B_YAW));
    }
    let slow = -s.players.iter().find(|p| p.id == A).unwrap().pos(&s.world)[2];
    assert!(free > 3.0 && slow > 1.0, "both walked: {free} {slow}");
    let ratio = slow / free;
    assert!((0.6..0.9).contains(&ratio), "about -25 %: {slow} / {free} = {ratio}");
    // the bit means nothing while a tag bag is in the hands instead of the bow
    let mut s = server(60.0);
    give(&mut s, A, PowerKind::Tag, 0);
    for _ in 0..90 {
        feed(&mut s, walk, idle(B_YAW));
    }
    let bare = -s.players.iter().find(|p| p.id == A).unwrap().pos(&s.world)[2];
    assert!((bare - free).abs() < 0.01, "tags in hand, no slowdown: {bare} {free}");
}

#[test]
fn a_raised_shield_blocks_an_arrow_and_loses_a_lot_of_guard() {
    let mut s = server(-30.0);
    give(&mut s, A, PowerKind::Yumi, 0);
    let mut dmg = 0.0;
    let mut guard_before = 1.0f32;
    let mut guard_after = 1.0f32;
    for t in 0..120 {
        let b = if t >= 10 { guard(B_YAW) } else { idle(B_YAW) };
        let a = if t < 40 { drawing(0.0, -0.02) } else { idle(0.0) };
        let g0 = s.players.iter().find(|p| p.id == B).unwrap().blk.energy;
        for e in feed(&mut s, a, b) {
            if let PvpEvent::Hit { victim: B, amount, kind: DamageKind::Arrow, .. } = e {
                dmg += amount;
                guard_before = g0;
                guard_after = s.players.iter().find(|p| p.id == B).unwrap().blk.energy;
            }
        }
    }
    let want = 80.0 * s.balance.guard.front_mult;
    assert!((dmg - want).abs() < 0.01, "a blocked arrow does 80 x front_mult = {want}: {dmg}");
    assert!(guard_before - guard_after > 0.6, "and bites ~70 % off the guard: {guard_before} -> {guard_after}");
    assert!(s.arrows.is_empty());
}

#[test]
fn a_perfect_parry_reflects_the_arrow_back_at_the_shooter() {
    let mut s = server(-30.0);
    give(&mut s, A, PowerKind::Yumi, 0);
    let mut raised = false;
    let mut reflected = false;
    let (mut a_dmg, mut b_dmg) = (0.0, 0.0);
    for t in 0..200 {
        let a = if t < 40 { drawing(0.0, -0.02) } else { idle(0.0) };
        // raise the shield as the arrow closes to ~6 m, so the parry window covers the impact
        raised |= s.arrows.first().map_or(false, |ar| ar.owner == A && (ar.pos[2] + 30.0).abs() < 6.0);
        let b = if raised && !reflected { guard(B_YAW) } else { idle(B_YAW) };
        for e in feed(&mut s, a, b) {
            if let PvpEvent::Hit { victim, amount, .. } = e {
                if victim == A {
                    a_dmg += amount;
                } else {
                    b_dmg += amount;
                }
            }
        }
        reflected |= s.arrows.first().map_or(false, |ar| ar.reflected);
    }
    assert!(reflected, "the arrow turned around");
    assert_eq!(b_dmg, 0.0, "the parrier takes nothing");
    assert!(a_dmg == 80.0 || a_dmg == 120.0, "the shooter eats their own arrow: {a_dmg}");
}

#[test]
fn a_katana_swing_cuts_an_arrow_out_of_the_air() {
    let mut s = server(-30.0);
    give(&mut s, A, PowerKind::Yumi, 0);
    let mut cut = false;
    for t in 0..200 {
        let a = if t < 40 { drawing(0.0, -0.02) } else { idle(0.0) };
        // B swings when the arrow is a few metres out
        let near = s.arrows.first().map_or(false, |ar| (ar.pos[2] + 30.0).abs() < 3.5);
        let b = if near { swing(B_YAW) } else { idle(B_YAW) };
        feed(&mut s, a, b);
        cut |= near && s.arrows.is_empty();
    }
    assert!(cut, "the arrow vanished under the blade");
    assert_eq!(hp(&s, B), 100.0, "and did no damage");
}

#[test]
fn a_drawn_bow_throws_no_stars_but_a_tap_still_does() {
    let mut s = server(-30.0);
    give(&mut s, A, PowerKind::Yumi, 0);
    for _ in 0..10 {
        feed(&mut s, PlayerIntent { draw: true, buttons: Buttons(Buttons::FIRE), aim_yaw: q_yaw(0.0), aim_pitch: q_pitch(0.0), ..Default::default() }, idle(B_YAW));
    }
    assert!(s.projectiles().iter().all(|(o, _)| *o != A), "a drawn bow throws nothing");
    // a tap: draw for 3 ticks, then release with a FIRE edge
    let mut s = server(-30.0);
    give(&mut s, A, PowerKind::Yumi, 0);
    for _ in 0..3 {
        feed(&mut s, drawing(0.0, 0.0), idle(B_YAW));
    }
    feed(&mut s, press(0.0, 0.0), idle(B_YAW));
    assert!(s.projectiles().iter().any(|(o, _)| *o == A), "a tap is a star");
    assert_eq!(player(&mut s, A).power.cooldown, 0.0);
}

// ---- tags ----

#[test]
fn a_thrown_tag_sticks_to_a_foe_arms_and_goes_off_after_the_fuse() {
    let mut s = server(-10.0);
    give(&mut s, A, PowerKind::Tag, 0);
    feed(&mut s, press(0.0, -0.02), idle(B_YAW));
    assert_eq!(player(&mut s, A).power.ammo, 1);
    let mut stuck_tick = None;
    let mut dmg = Vec::new();
    for t in 0..240 {
        for e in feed(&mut s, idle(0.0), idle(B_YAW)) {
            if let PvpEvent::Hit { attacker: A, victim: B, amount, kind: DamageKind::Tag, .. } = e {
                dmg.push((t, amount));
            }
        }
        if stuck_tick.is_none() && s.tags.first().map_or(false, |g| g.stuck) {
            stuck_tick = Some(t);
            assert_eq!(s.tags[0].on.map(|o| o.0), Some(B), "it stuck on the player, not the floor");
        }
    }
    let st = stuck_tick.expect("stuck");
    assert!(st < 40, "a 10 m throw lands inside 40 ticks: {st}");
    assert_eq!(dmg.len(), 1, "one blast: {dmg:?}");
    let (bt, amount) = dmg[0];
    assert!((bt as i32 - st as i32 - 120).abs() <= 3, "2.0 s after sticking: stuck {st}, blast {bt}");
    assert!(amount > 40.0 && amount <= 50.0, "centre damage ~50: {amount}");
    assert!(hp(&s, B) < 60.0 && s.tags.is_empty());
    assert_eq!(hp(&s, A), 100.0, "10 m away: untouched");
}

#[test]
fn a_second_press_sets_off_an_armed_tag_early_but_not_before_it_is_armed() {
    let mut s = server(-30.0);
    give(&mut s, A, PowerKind::Tag, 1);
    feed(&mut s, press(0.0, -0.1), idle(B_YAW)); // thrown at the floor ahead
    assert_eq!(player(&mut s, A).power.ammo, 0);
    let mut landed = false;
    for _ in 0..80 {
        wait(&mut s, 1);
        if s.tags.first().map_or(false, |t| t.stuck) {
            landed = true;
            break;
        }
    }
    assert!(landed, "the tag landed");
    // inside the 0.5 s arming time the press does nothing (and is not a star either)
    feed(&mut s, press(0.0, 0.0), idle(B_YAW));
    assert_eq!(s.tags.len(), 1, "not armed yet");
    assert!(s.projectiles().iter().all(|(o, _)| *o != A));
    wait(&mut s, 40);
    assert_eq!(s.tags.len(), 1, "armed, waiting for the fuse");
    feed(&mut s, press(0.0, 0.0), idle(B_YAW));
    assert!(s.tags.is_empty(), "the second press set it off");
    assert_eq!(blasts(&s), 1);
    wait(&mut s, 2);
    assert!(!player(&mut s, A).power.held(), "the bag is used up once its tags are gone");
}

#[test]
fn the_blast_has_falloff_a_strong_launch_and_leaves_the_wall_splat_and_ring_out_marks() {
    let ticks = 125;
    let mut by_dist = Vec::new();
    for off in [1.0f32, 2.5, 3.8, 4.6] {
        let mut s = server(-30.0);
        tp(&mut s, A, [0.0, 0.81, 20.0]);
        world_tag(&mut s, A, [0.0, 0.03, -30.0 + off]);
        let (mut dmg, mut top) = (0.0, 0.0f32);
        for _ in 0..ticks {
            for e in feed(&mut s, idle(0.0), idle(B_YAW)) {
                if let PvpEvent::Hit { victim: B, amount, kind: DamageKind::Tag, .. } = e {
                    dmg += amount;
                }
            }
            let v = player(&mut s, B).ch.vel;
            top = top.max((v.x * v.x + v.z * v.z).sqrt());
        }
        by_dist.push((off, dmg, top));
        if off == 1.0 {
            let b = player(&mut s, B);
            assert_eq!(b.melee.last_hit.map(|h| h.0), Some(A), "ring-out credit");
            assert_eq!(b.melee.knock_by, A, "wall-splat window names the thrower");
            assert!(b.melee.knock_until > 0 && b.melee.knock_speed > 8.0);
        }
    }
    let d = |i: usize| by_dist[i].1;
    assert!(d(0) > d(1) && d(1) > d(2) && d(2) > 19.0 && d(3) == 0.0, "50 at the heart, ~40 % at the edge, nothing outside 4 m: {by_dist:?}");
    assert!(by_dist[0].2 > 9.0, "launched hard: {by_dist:?}");
    assert!(by_dist[0].2 > by_dist[2].2, "and less at the edge: {by_dist:?}");
}

#[test]
fn a_blast_on_the_platform_edge_throws_the_victim_into_the_void_for_the_thrower() {
    let mut s = server_with(0.0, LevelBox::new([0.0, -0.5, 0.0], [3.0, 0.5, 3.0]), |_| {});
    tp(&mut s, A, [-2.5, 0.81, 0.0]);
    tp(&mut s, B, [2.4, 0.81, 0.0]);
    wait(&mut s, 5);
    world_tag(&mut s, A, [1.0, 0.03, 0.0]);
    let mut killed = false;
    for _ in 0..400 {
        for e in feed(&mut s, idle(0.0), idle(B_YAW)) {
            if let PvpEvent::HazardKill { killer: Some(A), victim: B, .. } = e {
                killed = true;
            }
        }
    }
    assert!(killed, "B went over the edge and the ring-out is A's kill");
}

#[test]
fn the_thrower_takes_30_percent_and_teammates_nothing() {
    let near = [0.0, 0.03, -1.5];
    // free for all
    let mut s = server(-3.0);
    world_tag(&mut s, A, near);
    wait(&mut s, 125);
    let (a, b) = (100.0 - hp(&s, A), 100.0 - hp(&s, B));
    assert!(b > 30.0, "the foe takes the blast: {b}");
    assert!(a > 5.0 && (a - b * 0.3).abs() < 1.5, "the thrower takes 30 % of the same: {a} vs {b}");
    // teammates
    let mut s = server(-3.0);
    for p in s.players.iter_mut() {
        p.team = 0;
    }
    world_tag(&mut s, A, near);
    wait(&mut s, 125);
    assert_eq!(hp(&s, B), 100.0, "no damage to a teammate");
    assert!(hp(&s, A) < 100.0, "the thrower still pays");
}

#[test]
fn a_blade_cuts_a_tag_off_but_the_owners_own_swing_does_not() {
    // B's swing at A's tag stuck in front of B
    let mut s = server(-30.0);
    world_tag(&mut s, A, [0.0, 1.0, -28.5]);
    feed(&mut s, idle(0.0), swing(B_YAW));
    assert!(s.tags.is_empty(), "cut off");
    wait(&mut s, 130);
    assert_eq!((hp(&s, A), hp(&s, B)), (100.0, 100.0), "no blast ever");
    // the owner swinging at their own tag changes nothing
    let mut s = server(-30.0);
    tp(&mut s, B, [0.0, 0.81, 20.0]);
    world_tag(&mut s, A, [0.0, 1.0, -1.5]);
    feed(&mut s, swing(0.0), idle(B_YAW));
    assert_eq!(s.tags.len(), 1);
    // a tag stuck to a player is cut by that player's swing
    let mut s = server(-30.0);
    let mut t = Tag::stuck_at(A, 7, [0.0, 1.0, -29.6], [0.0, 0.0, 1.0]);
    t.on = Some((B, [0.0, 0.2, 0.4]));
    s.tags.push(t);
    feed(&mut s, idle(0.0), swing(B_YAW));
    assert!(s.tags.is_empty());
}

#[test]
fn at_most_two_tags_are_live_per_player() {
    let mut s = server_with(-30.0, LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]), |b| b.combat.power.tag.count = 3);
    give(&mut s, A, PowerKind::Tag, 0);
    assert_eq!(player(&mut s, A).power.ammo, 3);
    for _ in 0..3 {
        feed(&mut s, press(0.0, -0.3), idle(B_YAW));
        wait(&mut s, 30);
        assert!(s.tags.len() <= 2);
    }
    assert!(s.tags.len() <= 2, "{} live", s.tags.len());
}

// ---- pedestals ----

#[derive(Default)]
struct Seen {
    spawned: u32,
    taken: Vec<(u16, PowerKind)>,
}

fn watch(s: &mut Server, a: PlayerIntent, b: PlayerIntent, n: u32, seen: &mut Seen) {
    for _ in 0..n {
        feed(s, a, b);
        for e in s.pickup_events.clone() {
            match e {
                PickupEvent::Spawned { weapon, .. } if weapon != PowerKind::None => seen.spawned += 1,
                PickupEvent::Taken { player, weapon, .. } if weapon != PowerKind::None => seen.taken.push((player, weapon)),
                _ => {}
            }
        }
    }
}

#[test]
fn a_tag_pedestal_gives_the_bag_respawns_on_its_timer_and_wants_the_bow_hand_free() {
    let mut s = server(-30.0);
    let at = [10.0, 0.81, 0.0];
    // a yumi pedestal in a level file is ignored: everybody has the bow already
    s.add_weapon_pickup(100, [20.0, 0.81, 0.0], PowerKind::Yumi, 3.0);
    s.add_weapon_pickup(101, at, PowerKind::Tag, 3.0);
    let mut seen = Seen::default();
    watch(&mut s, idle(0.0), idle(B_YAW), 2, &mut seen);
    assert_eq!(seen.spawned, 1, "only the tag pedestal announces itself");
    assert_eq!(player(&mut s, A).power.kind, PowerKind::Yumi, "the bow is in hand from the start");
    tp(&mut s, A, at);
    watch(&mut s, idle(0.0), idle(B_YAW), 3, &mut seen);
    assert_eq!(seen.taken, vec![(A, PowerKind::Tag)]);
    let (kind, ammo, left) = {
        let p = player(&mut s, A);
        (p.power.kind, p.power.ammo, p.power.left)
    };
    assert_eq!((kind, ammo), (PowerKind::Tag, 2));
    assert!((59.0..=60.0).contains(&left), "60 s carry: {left}");
    // the pedestal is back 3 s later and B (bow in hand) takes it
    tp(&mut s, A, [0.0, 0.81, 0.0]);
    watch(&mut s, idle(0.0), idle(B_YAW), 200, &mut seen);
    assert_eq!(s.pickups.active_weapons().filter(|w| w.2 == PowerKind::Tag).count(), 1, "respawned");
    assert_eq!(seen.spawned, 2);
    tp(&mut s, B, at);
    watch(&mut s, idle(0.0), idle(B_YAW), 3, &mut seen);
    assert_eq!(seen.taken.last(), Some(&(B, PowerKind::Tag)));
}

#[test]
fn the_weapon_of_a_dead_player_lies_where_they_fell_with_what_was_left() {
    let mut s = server(-30.0);
    give(&mut s, A, PowerKind::Tag, 2);
    // A is one scratch from death and stands on their own tag
    player(&mut s, A).vitals.hp = 4.0;
    world_tag(&mut s, A, [0.0, 0.03, 0.0]);
    let mut dropped = None;
    for _ in 0..140 {
        feed(&mut s, idle(0.0), idle(B_YAW));
        for e in s.pickup_events.clone() {
            if let PickupEvent::Spawned { id, pos, weapon, .. } = e {
                if weapon != PowerKind::None {
                    dropped = Some((id, pos, weapon));
                }
            }
        }
        if dropped.is_some() {
            break;
        }
    }
    let (_, pos, weapon) = dropped.expect("a drop was announced");
    assert_eq!(weapon, PowerKind::Tag);
    assert!(!player(&mut s, A).power.held(), "the owner lost it");
    // B takes it with the two tags that were left
    tp(&mut s, B, [pos[0], 0.81, pos[2]]);
    for _ in 0..3 {
        feed(&mut s, idle(0.0), idle(B_YAW));
    }
    let b = player(&mut s, B);
    assert_eq!((b.power.kind, b.power.ammo), (PowerKind::Tag, 2));
}
