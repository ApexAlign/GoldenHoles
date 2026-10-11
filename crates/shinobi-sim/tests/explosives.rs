//! Thrown explosives and the flash through the real `Server` (demo balance, docs/stars.md): a barrel's blast hurts with
//! falloff and hurts its thrower too (a shield takes the edge off), shooting a resting barrel sets it off early, barrels
//! chain, destructible boxes take the blast, a canister sticks (to the floor or a player) and bursts after its short fuse,
//! and a flash dazes (extra damage taken, no lock-on).

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState};
use shinobi_proto::quant::{q_pitch, q_yaw};
use shinobi_sim::balance::Balance;
use shinobi_sim::breaks::BreakEvent;
use shinobi_sim::character::Cfg;
use shinobi_sim::combat::stars::StarKind;
use shinobi_sim::combat::DamageKind;
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::rapier3d::prelude::Isometry;
use shinobi_sim::tick::Server;

const A: u16 = 1;
const B: u16 = 2;
const B_YAW: f32 = std::f32::consts::PI;
/// Where A is parked when a test wants the thrower out of the blast.
const FAR: [f32; 3] = [40.0, 0.81, 40.0];

fn server_with(b_z: f32, extra: Vec<LevelBox>) -> Server {
    let mut boxes = vec![LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]), LevelBox::new([0.0, 5.0, -41.0], [40.0, 5.0, 1.0])];
    boxes.extend(extra);
    let lv = LevelDesc { boxes, spawns: vec![[0.0, 0.81, 0.0], [0.0, 0.81, b_z]] };
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

fn server(b_z: f32) -> Server {
    server_with(b_z, Vec::new())
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

fn objects(s: &Server, owner: u16, kind: StarKind) -> Vec<shinobi_sim::combat::shuriken::Disc> {
    s.projectiles().into_iter().filter(|(o, d)| *o == owner && d.kind == kind).map(|(_, d)| d).collect()
}

fn eye(s: &Server, id: u16) -> f32 {
    let p = s.players.iter().find(|p| p.id == id).unwrap();
    p.pos(&s.world)[1] + p.ch.eye_above()
}

/// Explosion damage events `(victim, amount)` among `evs`.
fn blasts(evs: &[PvpEvent]) -> Vec<(u16, u16, f32)> {
    evs.iter()
        .filter_map(|e| match *e {
            PvpEvent::Hit { attacker, victim, amount, kind: DamageKind::Explosive, .. } => Some((attacker, victim, amount)),
            _ => None,
        })
        .collect()
}

/// A throws `kind` at the floor ahead and waits for it to land; returns the first landed copy.
fn lob(s: &mut Server, kind: StarKind, pitch: f32) -> [f32; 3] {
    for t in 0..150 {
        feed(s, if t == 0 { throw(0.0, pitch, kind) } else { idle(0.0) }, idle(B_YAW));
        if let Some(d) = objects(s, A, kind).iter().find(|d| d.stuck) {
            return [d.pos.x, d.pos.y, d.pos.z];
        }
    }
    panic!("the {kind:?} never landed");
}

/// Run until a blast hits somebody (up to `max` ticks); returns the hits of the tick it went off.
fn until_blast(s: &mut Server, max: u32) -> Vec<(u16, u16, f32)> {
    for _ in 0..max {
        let b = blasts(&feed(s, idle(0.0), idle(B_YAW)));
        if !b.is_empty() {
            return b;
        }
    }
    Vec::new()
}

#[test]
fn a_barrel_lobs_rests_and_its_blast_falls_off_with_distance() {
    // near the centre
    let mut s = server(-30.0);
    let m = lob(&mut s, StarKind::Barrel, -0.3);
    assert!(m[1] < 0.8 && m[2] < -2.0, "rests on the floor ahead: {m:?}");
    tp(&mut s, A, FAR);
    tp(&mut s, B, [m[0] + 1.5, 0.81, m[2]]);
    let near = until_blast(&mut s, 150);
    let n = near.iter().find(|h| h.1 == B).expect("B in the blast").2;
    assert!(near.iter().all(|h| h.0 == A), "credited to the thrower");
    assert!((55.0..=90.0).contains(&n), "close to the barrel it is most of a life ({n})");
    assert_eq!(hp(&s, A), 100.0, "the thrower, far away, is untouched");
    assert!(objects(&s, A, StarKind::Barrel).is_empty(), "the barrel is spent");
    // near the edge
    let mut s = server(-30.0);
    let m = lob(&mut s, StarKind::Barrel, -0.3);
    tp(&mut s, A, FAR);
    tp(&mut s, B, [m[0] + 5.0, 0.81, m[2]]);
    let far = until_blast(&mut s, 150);
    let f = far.iter().find(|h| h.1 == B).expect("B at the edge is still in it").2;
    assert!(f < n * 0.6 && f > 90.0 * 0.15 * 0.9, "falls off towards the edge: {n} at 1.5 m, {f} at 5 m");
    // out of reach
    let mut s = server(-30.0);
    let m = lob(&mut s, StarKind::Barrel, -0.3);
    tp(&mut s, A, FAR);
    tp(&mut s, B, [m[0] + 8.0, 0.81, m[2]]);
    assert!(until_blast(&mut s, 150).is_empty() && hp(&s, B) == 100.0, "beyond 6 m nothing happens");
    // and a barrel that hits a player goes off at once
    let mut s = server(-12.0);
    let mut hits = Vec::new();
    for t in 0..60 {
        let evs = feed(&mut s, if t == 0 { throw(0.0, 0.2, StarKind::Barrel) } else { idle(0.0) }, idle(B_YAW));
        hits.extend(blasts(&evs));
        if !hits.is_empty() {
            assert!(t < 40, "bursts on impact, not after the fuse (tick {t})");
            break;
        }
    }
    assert!(hits.iter().any(|h| h.1 == B), "the barrel struck B and went off");
}

#[test]
fn too_close_hurts_the_thrower_and_a_shield_takes_the_edge_off() {
    // thrown at its own feet
    let mut s = server(-30.0);
    let mut dmg = 0.0;
    for t in 0..200 {
        let evs = feed(&mut s, if t == 0 { throw(0.0, -1.5, StarKind::Barrel) } else { idle(0.0) }, idle(B_YAW));
        if let Some(h) = blasts(&evs).iter().find(|h| h.1 == A) {
            assert_eq!(h.0, A, "own barrel: suicide-style attribution");
            dmg = h.2;
            break;
        }
    }
    assert!(dmg > 60.0, "a barrel at your own feet hurts badly ({dmg})");
    assert!(hp(&s, A) < 40.0 && hp(&s, B) == 100.0);
    // same throw behind a raised shield
    let mut s = server(-30.0);
    let mut shielded = 0.0;
    for t in 0..200 {
        let a = if t == 0 {
            throw(0.0, -1.5, StarKind::Barrel)
        } else if t >= 50 {
            PlayerIntent { block: true, ..idle(0.0) }
        } else {
            idle(0.0)
        };
        let evs = feed(&mut s, a, idle(B_YAW));
        if let Some(h) = blasts(&evs).iter().find(|h| h.1 == A) {
            shielded = h.2;
            break;
        }
    }
    assert!(shielded > 0.0, "the blast still reaches a shielded thrower");
    assert!(shielded < dmg * 0.5, "the shield reduces it ({shielded} vs {dmg} open)");
}

#[test]
fn shooting_a_resting_barrel_sets_it_off_early_and_the_shooter_gets_the_credit() {
    let mut s = server(-30.0);
    let m = lob(&mut s, StarKind::Barrel, -0.3);
    // the thrower stands near it; B takes aim from 8 m away (outside the blast) and throws a plain star at it
    tp(&mut s, A, [m[0] + 3.0, 0.81, m[2]]);
    tp(&mut s, B, [m[0], 0.81, m[2] - 8.0]);
    wait(&mut s, 2);
    let pitch = -((eye(&s, B) - m[1]) / 8.0).atan();
    let mut hits = Vec::new();
    for t in 0..40 {
        hits.extend(blasts(&feed(&mut s, idle(0.0), if t == 0 { throw(B_YAW, pitch, StarKind::Standard) } else { idle(B_YAW) })));
        if !hits.is_empty() {
            break;
        }
    }
    assert!(!hits.is_empty(), "the star set the barrel off long before its fuse");
    assert!(objects(&s, A, StarKind::Barrel).is_empty());
    let on_a = hits.iter().find(|h| h.1 == A).expect("the thrower was baited into its own blast");
    assert_eq!(on_a.0, B, "credit to the one who shot it");
    assert!(hp(&s, B) == 100.0, "the shooter stayed out of reach");
}

#[test]
fn a_katana_cut_also_sets_a_barrel_off() {
    let mut s = server(-30.0);
    let m = lob(&mut s, StarKind::Barrel, -0.3);
    tp(&mut s, A, FAR);
    tp(&mut s, B, [m[0], 0.81, m[2] - 2.0]);
    let slice = PlayerIntent { buttons: Buttons(Buttons::SLICE), ..idle(B_YAW) };
    let mut hits = Vec::new();
    for t in 0..8 {
        hits.extend(blasts(&feed(&mut s, idle(0.0), if t == 0 { slice } else { idle(B_YAW) })));
    }
    assert!(hits.iter().any(|h| h.1 == B), "B cut a barrel two metres away and ate the blast: {hits:?}");
}

#[test]
fn barrels_chain() {
    let mut s = server(-30.0);
    lob(&mut s, StarKind::Barrel, -0.3);
    let m2 = {
        // the second barrel lands a few metres short of the first
        let mut p = None;
        for t in 0..120 {
            feed(&mut s, if t == 0 { throw(0.0, -0.5, StarKind::Barrel) } else { idle(0.0) }, idle(B_YAW));
            let rest: Vec<_> = objects(&s, A, StarKind::Barrel).into_iter().filter(|d| d.stuck).collect();
            if rest.len() == 2 {
                p = Some(rest);
                break;
            }
        }
        p.expect("both barrels rest")
    };
    let gap = (m2[0].pos - m2[1].pos).norm();
    assert!(gap < 6.0, "the barrels lie within one blast radius ({gap:.1} m)");
    tp(&mut s, A, FAR);
    let (mut before, mut gone_at) = (2usize, None);
    for t in 0..200 {
        feed(&mut s, idle(0.0), idle(B_YAW));
        let n = objects(&s, A, StarKind::Barrel).len();
        if n != before {
            gone_at = Some((t, before, n));
            break;
        }
        before = n;
    }
    let (_, from, to) = gone_at.expect("the first fuse ran out");
    assert_eq!((from, to), (2, 0), "both barrels went off on the same tick");
}

#[test]
fn a_barrel_blast_breaks_destructible_boxes() {
    let mut s = server_with(-30.0, vec![LevelBox::new([3.0, 1.0, -6.0], [0.5, 1.0, 0.5]).destructible(30.0)]);
    let mut broke = false;
    for t in 0..220 {
        feed(&mut s, if t == 0 { throw(0.0, -0.3, StarKind::Barrel) } else if t == 20 { idle(0.0) } else { idle(0.0) }, idle(B_YAW));
        if t == 40 {
            tp(&mut s, A, FAR); // out of the blast
        }
        broke |= s.break_events.iter().any(|e| matches!(e, BreakEvent::Break { .. }));
    }
    assert!(broke, "the blast shattered the pillar a few metres from the barrel");
}

#[test]
fn a_canister_sticks_to_the_floor_and_bursts_after_its_short_fuse() {
    let mut s = server(-30.0);
    let m = lob(&mut s, StarKind::Canister, -0.3);
    assert!(m[1] < 0.2, "stuck on the floor: {m:?}");
    tp(&mut s, A, FAR);
    tp(&mut s, B, [m[0] + 1.0, 0.81, m[2]]);
    let mut t = 0;
    let mut hit = None;
    while t < 150 && hit.is_none() {
        hit = blasts(&feed(&mut s, idle(0.0), idle(B_YAW))).into_iter().find(|h| h.1 == B);
        t += 1;
    }
    let h = hit.expect("the canister burst");
    assert!((35.0..=60.0).contains(&h.2), "smaller than a barrel: {}", h.2);
    assert!((60..=85).contains(&t), "about 1.2 s after sticking ({t} ticks)");
    assert!(objects(&s, A, StarKind::Canister).is_empty());
    // a barrel's reach is bigger: 5 m from a canister is outside its 4 m
    let mut s = server(-30.0);
    let m = lob(&mut s, StarKind::Canister, -0.3);
    tp(&mut s, A, FAR);
    tp(&mut s, B, [m[0] + 5.0, 0.81, m[2]]);
    assert!(until_blast(&mut s, 150).is_empty() && hp(&s, B) == 100.0);
}

#[test]
fn a_canister_sticks_to_a_player_and_rides_along() {
    let mut s = server(-14.0);
    let mut stuck_to = None;
    for t in 0..30 {
        feed(&mut s, if t == 0 { throw(0.0, 0.0, StarKind::Canister) } else { idle(0.0) }, idle(B_YAW));
        if let Some(d) = objects(&s, A, StarKind::Canister).iter().find(|d| d.stuck) {
            stuck_to = d.attach;
            break;
        }
    }
    let host = s.players.iter().find(|p| p.id == B).unwrap().vitals.hitbox;
    assert_eq!(stuck_to, Some(host), "it stuck to B");
    // B runs off: the canister goes along
    tp(&mut s, B, [20.0, 0.81, -14.0]);
    wait(&mut s, 3);
    let d = objects(&s, A, StarKind::Canister)[0];
    assert!((d.pos.x - 20.0).abs() < 2.0, "it rides on B ({:?})", d.pos);
    let h = until_blast(&mut s, 120);
    let on_b = h.iter().find(|h| h.1 == B).expect("B eats it").2;
    assert!(on_b > 45.0, "stuck on the body is near full damage ({on_b})");
    assert_eq!(hp(&s, A), 100.0);
}

#[test]
fn a_flash_dazes_extra_damage_and_no_lock_on() {
    let locked = |s: &Server, id: u16| s.players.iter().find(|p| p.id == id).unwrap().combat.lock.holding();
    // a plain star at B, with and without the flash before it
    let star_damage = |flash: bool| -> f32 {
        let mut s = server(-12.0);
        wait(&mut s, 40);
        assert!(locked(&s, B), "positive control: B has A locked");
        if flash {
            for t in 0..30 {
                feed(&mut s, if t == 0 { throw(0.0, 0.0, StarKind::Flash) } else { idle(0.0) }, idle(B_YAW));
            }
            assert!(s.players.iter().find(|p| p.id == B).unwrap().blind_until > s.tick, "B is dazed");
            assert!(!locked(&s, B), "a white-out breaks B's lock");
        }
        for t in 0..30 {
            let evs = feed(&mut s, if t == 0 { throw(0.0, 0.0, StarKind::Standard) } else { idle(0.0) }, idle(B_YAW));
            for e in evs {
                if let PvpEvent::Hit { victim: B, amount, kind: DamageKind::Shuriken, .. } = e {
                    return amount;
                }
            }
        }
        panic!("the star never hit");
    };
    let plain = star_damage(false);
    let dazed = star_damage(true);
    assert!((dazed / plain - 1.25).abs() < 0.02, "dazed players take +25% ({plain} -> {dazed})");
}

#[test]
fn a_flash_blinds_the_thrower_only_when_looking_at_it_close_by() {
    // thrown 6 m ahead at a wall? the wall is far: use a steep throw down at the feet, looking at it
    let mut s = server(-30.0);
    let mut blind_self = false;
    for t in 0..40 {
        feed(&mut s, if t == 0 { throw(0.0, -1.0, StarKind::Flash) } else { idle(0.0) }, idle(B_YAW));
        blind_self |= s.players.iter().find(|p| p.id == A).unwrap().blind_until > s.tick;
    }
    assert!(blind_self, "a flash at your own feet, looking at it, whites you out too");
    // thrown far ahead: the burst is 30+ m away
    let mut s = server(-30.0);
    for t in 0..60 {
        feed(&mut s, if t == 0 { throw(0.0, 0.0, StarKind::Flash) } else { idle(0.0) }, idle(B_YAW));
    }
    assert!(s.players.iter().find(|p| p.id == A).unwrap().blind_until <= s.tick, "a distant burst leaves the thrower alone");
}
