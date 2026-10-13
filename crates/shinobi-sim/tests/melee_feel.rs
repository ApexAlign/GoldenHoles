//! Melee accuracy, measured. A simulated human (noisy, lagging aim, a slice every half second whenever the target is
//! within 8 m) cuts at a target that circle-strafes at run speed, at 3, 6 and 10 m. Reported: swings that connected
//! BEFORE (box-only blade, no homing) and AFTER (target-centred cone + the client's homing of the aim onto the lock).
//!
//!   cargo test -p shinobi-sim --test melee_feel -- --nocapture

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState, PITCH_LEVEL};
use shinobi_proto::quant::{dq_yaw, q_yaw};
use shinobi_sim::balance::Balance;
use shinobi_sim::character::Cfg;
use shinobi_sim::combat::{CombatEvent, DamageKind};
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::tick::Server;

const A: u16 = 1;
const B: u16 = 2;

fn server(dist: f32, cone: bool) -> Server {
    let lv = LevelDesc { boxes: vec![LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0])], spawns: vec![[0.0, 0.81, 0.0], [0.0, 0.81, -dist]] };
    let mut s = Server::new(Cfg::default());
    lv.apply_to_server(&mut s);
    s.set_spawns(lv.spawns.clone());
    let mut b = Balance::demo();
    if !cone {
        b.combat.katana.cone_deg = 0.0;
        b.combat.katana.cone_extra = 0.0;
    }
    b.melee = shinobi_sim::balance::MeleeBalance::default(); // accuracy only: no knockback
    b.player.max_hp = 1.0e6; // nobody dies: this is about connecting, not killing
    b.player.regen_per_s = 0.0;
    s.set_balance(b).unwrap();
    s.match_state.cfg.frag_limit = u16::MAX;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    s.add_player(A, [0.0, 0.81, 0.0]);
    s.add_player(B, [0.0, 0.81, -dist]);
    s
}

fn send(s: &mut Server, id: u16, mut it: PlayerIntent) {
    it.tick = s.tick;
    let from = s.tick.saturating_sub(6).max(1);
    let pkt = ClientPacket { ack_snapshot_tick: from, view: ViewState { from_tick: from, alpha: 0 }, intents: vec![it], ..Default::default() };
    s.ingest(id, &pkt.encode().unwrap());
}

/// xorshift: deterministic noise
fn rnd(state: &mut u64) -> f32 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    ((*state >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
}

/// (swings, swings that connected) of the human over `secs`.
fn run(dist: f32, cone: bool, homing: bool, secs: u32, seed: u64) -> (u32, u32) {
    let mut s = server(dist, cone);
    for _ in 0..400 {
        // warmup out; both idle
        send(&mut s, A, PlayerIntent { aim_yaw: q_yaw(0.0), aim_pitch: PITCH_LEVEL, ..Default::default() });
        send(&mut s, B, PlayerIntent { aim_yaw: q_yaw(std::f32::consts::PI), aim_pitch: PITCH_LEVEL, ..Default::default() });
        s.frame(Duration::from_nanos(16_666_667));
        if s.match_state.kind() == shinobi_sim::match_state::PhaseKind::Live {
            break;
        }
    }
    let mut rng = seed | 1;
    let mut noise = 0.0f32;
    let mut hist: Vec<[f32; 3]> = Vec::new();
    let (mut swings, mut hits) = (0u32, 0u32);
    for t in 0..secs * 60 {
        let a = s.players.iter().find(|p| p.id == A).unwrap();
        let b = s.players.iter().find(|p| p.id == B).unwrap();
        let (pa, pb) = (a.pos(&s.world), b.pos(&s.world));
        hist.push(pb);
        // the human sees the target 6 ticks late and aims with error that is re-drawn every 8 ticks
        let seen = hist[hist.len().saturating_sub(7)];
        if t % 8 == 0 {
            noise = rnd(&mut rng) * 0.16;
        }
        let (dx, dz) = (pb[0] - pa[0], pb[2] - pa[2]);
        let d = (dx * dx + dz * dz).sqrt();
        let want = (-(seen[0] - pa[0])).atan2(-(seen[2] - pa[2]));
        let cur = dq_yaw(a.yaw);
        let turn = ((want + noise - cur + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI).clamp(-0.2, 0.2);
        let mut yaw = cur + turn;
        let slice = d <= dist + 1.5 && t % 30 == 3;
        if slice && homing {
            // the client's homing: a slice press with the target locked turns the aim onto it
            yaw = (-dx).atan2(-dz);
        }
        let run_in = 0; // the human stands and cuts: the lunge has to do the closing
        send(&mut s, A, PlayerIntent { aim_yaw: q_yaw(yaw), aim_pitch: PITCH_LEVEL, move_z: run_in, buttons: Buttons(if slice { Buttons::SLICE } else { 0 }), ..Default::default() });
        // the target circle-strafes around the human, flipping direction every 0.7 s, always facing them
        let sign = if (t / 42) % 2 == 0 { 127 } else { -127 };
        let face = (dx).atan2(dz);
        // ... and holds its distance: forward is toward the human, so push in when too far, back off when too close
        let radial: i8 = if d > dist + 0.4 { 127 } else if d < dist - 0.4 { -127 } else { 0 };
        send(&mut s, B, PlayerIntent { aim_yaw: q_yaw(face), aim_pitch: PITCH_LEVEL, move_x: sign, move_z: radial, ..Default::default() });
        s.frame(Duration::from_nanos(16_666_667));
        swings += s.events.iter().filter(|(id, e)| *id == A && matches!(e, CombatEvent::Slash { .. })).count() as u32;
        hits += s.pvp_events.iter().filter(|e| matches!(e, PvpEvent::Hit { attacker: A, kind: DamageKind::Slash, .. })).count() as u32;
    }
    (swings, hits)
}

#[test]
fn the_cone_and_homing_make_the_katana_connect_far_more_often() {
    let (mut before_total, mut after_total) = ((0u32, 0u32), (0u32, 0u32));
    let mut mid = (0.0f32, 0.0f32);
    for dist in [3.0f32, 6.0, 10.0] {
        let (mut b, mut a) = ((0u32, 0u32), (0u32, 0u32));
        for seed in 1..=4u64 {
            let r = run(dist, false, false, 40, seed);
            b = (b.0 + r.0, b.1 + r.1);
            let r = run(dist, true, true, 40, seed);
            a = (a.0 + r.0, a.1 + r.1);
        }
        eprintln!(
            "MELEE target strafing at {dist:>4} m: before {:>3}/{:>3} = {:>3.0}%   after {:>3}/{:>3} = {:>3.0}%",
            b.1, b.0, 100.0 * b.1 as f32 / b.0.max(1) as f32, a.1, a.0, 100.0 * a.1 as f32 / a.0.max(1) as f32
        );
        if dist == 6.0 {
            mid = (b.1 as f32 / b.0.max(1) as f32, a.1 as f32 / a.0.max(1) as f32);
        }
        before_total = (before_total.0 + b.0, before_total.1 + b.1);
        after_total = (after_total.0 + a.0, after_total.1 + a.1);
    }
    let (rb, ra) = (before_total.1 as f32 / before_total.0.max(1) as f32, after_total.1 as f32 / after_total.0.max(1) as f32);
    eprintln!("MELEE overall: before {:.0}% -> after {:.0}%", rb * 100.0, ra * 100.0);
    // 3 m is already easy (the blade box covers it), 10 m is beyond any swing (blade + lunge + cone = ~7.5 m): the gain is in
    // the mid range, where only the lunge reaches and the box used to slip off a moving target
    // margin re-baselined for the 7.2 -> 9.5 m/s run-speed round: the target's circle-strafe is also faster now, so the
    // cone's advantage over a plain swing is a bit smaller in absolute terms (still clearly there)
    assert!(mid.1 > mid.0 + 0.14, "at 6 m the connect rate must rise clearly ({:.2} -> {:.2})", mid.0, mid.1);
    assert!(ra > rb + 0.08, "and overall ({rb:.2} -> {ra:.2})");
}
