//! Stealth cloak (cloak.rs, docs/melee.md "Cloak") through the real `Server` (demo balance): activation while held,
//! breaking on an attack and on taking damage, the cooldown, and the stealth-kill bonus. A (id 1) at the origin
//! facing -Z, B (id 2) 1.5 m in front (within blade reach, same setup as `melee_duel.rs`).

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
const A_YAW: f32 = 0.0;
const B_YAW: f32 = std::f32::consts::PI;

fn server(b_z: f32) -> Server {
    let boxes = vec![LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0])];
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
        feed(&mut s, idle(A_YAW), idle(B_YAW));
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
        feed(&mut s, idle(A_YAW), idle(B_YAW));
    }
    for p in s.players.iter_mut() {
        p.vitals.hp = p.vitals.max_hp;
        p.melee = Default::default();
        p.cloak = Default::default();
    }
    s
}

fn idle(yaw: f32) -> PlayerIntent {
    PlayerIntent { aim_yaw: q_yaw(yaw), aim_pitch: PITCH_LEVEL, ..Default::default() }
}

/// Holding the cloak button, no attack.
fn cloaked(yaw: f32) -> PlayerIntent {
    PlayerIntent { cloak: true, ..idle(yaw) }
}

/// A katana swing with the cloak button still held (the swing itself breaks it this same tick).
fn slice_cloaked(yaw: f32) -> PlayerIntent {
    PlayerIntent { buttons: Buttons(Buttons::SLICE), cloak: true, ..idle(yaw) }
}

fn slice(yaw: f32) -> PlayerIntent {
    PlayerIntent { buttons: Buttons(Buttons::SLICE), ..idle(yaw) }
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

fn cloak_active(s: &Server, id: u16) -> bool {
    s.players.iter().find(|p| p.id == id).unwrap().cloak.active
}

#[test]
fn holding_the_button_raises_the_cloak() {
    let mut s = server(-1.5);
    assert!(!cloak_active(&s, A));
    for _ in 0..5 {
        feed(&mut s, cloaked(A_YAW), idle(B_YAW));
    }
    assert!(cloak_active(&s, A), "the cloak came up while the button was held");
}

#[test]
fn a_katana_swing_breaks_the_cloak_and_a_hit_landed_cloaked_is_a_guaranteed_stealth_kill() {
    let mut s = server(-1.5);
    // A holds the cloak, approaching; B stands still, unaware
    for _ in 0..30 {
        feed(&mut s, cloaked(A_YAW), idle(B_YAW));
    }
    assert!(cloak_active(&s, A), "cloak up before the swing");
    // the swing: a katana hit while still cloaked is a stealth kill (demo: 20 base * 6x stealth = 120 > 100 hp)
    let mut kills = Vec::new();
    for t in 0..10 {
        for e in feed(&mut s, if t == 0 { slice_cloaked(A_YAW) } else { idle(A_YAW) }, idle(B_YAW)) {
            if matches!(e, PvpEvent::Kill { killer: A, victim: B, .. }) {
                kills.push(e);
            }
        }
    }
    eprintln!("CLOAK stealth kill: {kills:?}, B hp left {:.1}", hp(&s, B));
    assert_eq!(kills.len(), 1, "exactly one stealth kill: {kills:?}");
    assert!(!cloak_active(&s, A), "the swing broke the cloak at once");
}

#[test]
fn an_uncloaked_swing_at_full_hp_is_not_a_one_shot_kill() {
    // control: the same swing, never cloaked, is the ordinary 20 (+ momentum) hp cut, not a kill
    let mut s = server(-1.5);
    for t in 0..10 {
        feed(&mut s, if t == 0 { slice(A_YAW) } else { idle(A_YAW) }, idle(B_YAW));
    }
    assert!(hp(&s, B) > 50.0, "an ordinary cut leaves B well above dead: {:.1} hp", hp(&s, B));
}

#[test]
fn taking_a_hit_breaks_the_cloak() {
    let mut s = server(-1.5);
    for _ in 0..30 {
        feed(&mut s, cloaked(A_YAW), idle(B_YAW));
    }
    assert!(cloak_active(&s, A));
    // B cuts A: A took damage, so A's cloak breaks even though A threw no attack
    for t in 0..10 {
        feed(&mut s, cloaked(A_YAW), if t == 0 { slice(B_YAW) } else { idle(B_YAW) });
    }
    assert!(!cloak_active(&s, A), "taking a hit broke the cloak");
    assert!(hp(&s, A) < 100.0, "and A did take damage");
}

#[test]
fn a_broken_cloak_waits_out_the_cooldown_even_if_the_button_stays_held() {
    let mut s = server(-1.5);
    // the swing breaks it on iteration 0 (cooldown set to 5 s = 300 ticks); the other 9 ticks of this loop each
    // tick the cooldown down by one, so 291 ticks of it are left once the loop ends
    for t in 0..10 {
        feed(&mut s, if t == 0 { slice_cloaked(A_YAW) } else { cloaked(A_YAW) }, idle(B_YAW));
    }
    assert!(!cloak_active(&s, A), "the swing broke it on the very tick it started");
    // held throughout all but the last tick of the remaining cooldown: still down
    for _ in 0..290 {
        feed(&mut s, cloaked(A_YAW), idle(B_YAW));
    }
    assert!(!cloak_active(&s, A), "cooldown not elapsed yet");
    feed(&mut s, cloaked(A_YAW), idle(B_YAW));
    assert!(cloak_active(&s, A), "cooldown elapsed: the held button raises it again");
}

#[test]
fn releasing_the_button_early_ends_the_cloak_and_still_pays_the_cooldown() {
    let mut s = server(-1.5);
    for _ in 0..10 {
        feed(&mut s, cloaked(A_YAW), idle(B_YAW));
    }
    assert!(cloak_active(&s, A));
    feed(&mut s, idle(A_YAW), idle(B_YAW)); // release
    assert!(!cloak_active(&s, A), "releasing the button ends the cloak");
    feed(&mut s, cloaked(A_YAW), idle(B_YAW));
    assert!(!cloak_active(&s, A), "still on cooldown immediately after a voluntary release");
}
