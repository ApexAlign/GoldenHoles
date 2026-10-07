//! How long does a held shield keep you alive? One or two attackers cut at B continuously while B holds the shield and
//! faces them. Reported for the OLD shield numbers (the first demo guard) and the CURRENT demo guard.
//!
//!   cargo test -p shinobi-sim --test shield_endurance -- --nocapture

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState, PITCH_LEVEL};
use shinobi_proto::quant::q_yaw;
use shinobi_sim::balance::{Balance, GuardBalance};
use shinobi_sim::character::Cfg;
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::tick::Server;

const B: u16 = 2;

/// The guard of the first demo build (what the user found too weak).
fn old_guard() -> GuardBalance {
    GuardBalance {
        front_deg: 70.0,
        front_mult: 0.15,
        parry_s: 0.25,
        max_hold_s: 2.5,
        regen_s: 3.0,
        hit_cost: 0.2,
        rearm_s: 1.0,
        stagger_s: 0.8,
        swing_open_s: 0.25,
        counter_open_s: 1.0,
        bonus_mult: 1.5,
        ..GuardBalance::default()
    }
}

fn send(s: &mut Server, id: u16, mut it: PlayerIntent) {
    it.tick = s.tick;
    let from = s.tick.saturating_sub(6).max(1);
    let pkt = ClientPacket { ack_snapshot_tick: from, view: ViewState { from_tick: from, alpha: 0 }, intents: vec![it], ..Default::default() };
    s.ingest(id, &pkt.encode().unwrap());
}

fn idle(yaw: f32) -> PlayerIntent {
    PlayerIntent { aim_yaw: q_yaw(yaw), aim_pitch: PITCH_LEVEL, ..Default::default() }
}

/// (seconds until the guard first broke, seconds until B died) with `attackers` cutting continuously; `None` = did not
/// happen within 30 s.
fn endure(guard: GuardBalance, attackers: u16) -> (Option<f32>, Option<f32>) {
    let mut boxes = vec![LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0])];
    boxes.push(LevelBox::new([0.0, 0.0, 60.0], [0.1, 0.1, 0.1])); // keeps the level non-trivial
    let lv = LevelDesc { boxes, spawns: vec![[0.0, 0.81, 0.0]; 4] };
    let mut s = Server::new(Cfg::default());
    lv.apply_to_server(&mut s);
    s.set_spawns(lv.spawns.clone());
    let mut b = Balance::demo();
    b.guard = guard;
    b.player.regen_per_s = 0.0;
    s.set_balance(b).unwrap();
    s.match_state.cfg.frag_limit = u16::MAX;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    // attackers stand in front of B (B at z=-1.5 facing +Z toward them)
    let spots: [(u16, [f32; 3]); 2] = [(1, [0.0, 0.81, 0.0]), (3, [0.9, 0.81, 0.0])];
    for (id, p) in spots.iter().take(attackers as usize) {
        s.add_player(*id, *p);
    }
    s.add_player(B, [0.0, 0.81, -1.5]);
    for _ in 0..900 {
        for (id, _) in spots.iter().take(attackers as usize) {
            send(&mut s, *id, idle(0.0));
        }
        send(&mut s, B, idle(std::f32::consts::PI));
        s.frame(Duration::from_nanos(16_666_667));
        if s.match_state.kind() == shinobi_sim::match_state::PhaseKind::Live {
            break;
        }
    }
    for (id, p) in spots.iter().take(attackers as usize) {
        let body = s.players.iter().find(|q| q.id == *id).unwrap().ch.body;
        s.world.bodies[body].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(p[0], p[1], p[2]), true);
        shinobi_sim::character::sync_body_colliders(&mut s.world, body);
    }
    let bb = s.players.iter().find(|q| q.id == B).unwrap().ch.body;
    s.world.bodies[bb].set_position(shinobi_sim::rapier3d::prelude::Isometry::translation(0.0, 0.81, -1.5), true);
    shinobi_sim::character::sync_body_colliders(&mut s.world, bb);
    for p in s.players.iter_mut() {
        p.vitals.hp = p.vitals.max_hp;
        p.vitals.invuln_until_tick = 0;
        p.blk = Default::default();
        p.melee = Default::default();
    }
    let (mut broke, mut died) = (None, None);
    let mut was_held = false;
    for t in 0..30 * 60u32 {
        let bpos = s.players.iter().find(|q| q.id == B).unwrap().pos(&s.world);
        for (id, _) in spots.iter().take(attackers as usize) {
            // face B, close in when knocked away, and keep the cut button down
            let ap = s.players.iter().find(|q| q.id == *id).unwrap().pos(&s.world);
            let (dx, dz) = (bpos[0] - ap[0], bpos[2] - ap[2]);
            let yaw = (-dx).atan2(-dz);
            let far = (dx * dx + dz * dz).sqrt() > 1.8;
            send(&mut s, *id, PlayerIntent { buttons: Buttons(Buttons::SLICE), move_z: if far { 127 } else { 0 }, ..idle(yaw) });
        }
        send(&mut s, B, PlayerIntent { block: true, ..idle(std::f32::consts::PI) });
        s.frame(Duration::from_nanos(16_666_667));
        let bp = s.players.iter().find(|q| q.id == B).unwrap();
        if was_held && !bp.blk.held && broke.is_none() {
            broke = Some(t as f32 / 60.0);
        }
        was_held |= bp.blk.held;
        if bp.vitals.deaths > 0 && died.is_none() {
            died = Some(t as f32 / 60.0);
            break;
        }
    }
    (broke, died)
}

#[test]
fn report_how_long_a_held_shield_survives_one_and_two_attackers() {
    for n in [1u16, 2] {
        let (ob, od) = endure(old_guard(), n);
        let (nb, nd) = endure(Balance::demo().guard, n);
        let f = |v: Option<f32>| v.map_or("never (30 s+)".to_string(), |s| format!("{s:.1} s"));
        eprintln!("SHIELD {n} attacker(s): OLD guard breaks at {}, B dies at {} | NEW guard breaks at {}, B dies at {}", f(ob), f(od), f(nb), f(nd));
        // the first demo build: 1 attacker broke the guard at 1.5 s and killed at 3.0 s; 2 attackers 1.5 s / 2.1 s
        assert!(nb.map_or(true, |t| t >= 3.0), "the guard holds at least 3 s against {n} continuous attacker(s): {nb:?}");
        assert!(nd.map_or(true, |t| t >= 5.0), "and you survive at least 5 s behind it: {nd:?}");
    }
}
