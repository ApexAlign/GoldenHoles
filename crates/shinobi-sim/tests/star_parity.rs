//! Prediction parity of the star variants: a client-style `CombatSystem` fed the intents a server player gets, in the
//! same world, throws the same star (kind, flight, landing, energy) tick for tick. The client never decides a variant's
//! effects (blast, white-out) -- only the throw and the pool -- so this is the part that has to match.

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState};
use shinobi_proto::quant::{dq_pitch, dq_yaw, q_pitch, q_yaw};
use shinobi_proto::TICK_DT;
use shinobi_sim::balance::Balance;
use shinobi_sim::character::{Cfg, PlayerInput};
use shinobi_sim::combat::stars::StarKind;
use shinobi_sim::combat::{Aim, CombatInput, CombatSystem};
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::rapier3d::prelude::*;
use shinobi_sim::tick::Server;

const A: u16 = 1;

fn run_kind(kind: StarKind) {
    let ticks = 150;
    let lv = LevelDesc { boxes: vec![LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]), LevelBox::new([0.0, 5.0, -30.0], [40.0, 5.0, 1.0])], spawns: vec![[0.0, 0.81, 0.0]] };
    let mut s = Server::new(Cfg::default());
    lv.apply_to_server(&mut s);
    s.set_spawns(lv.spawns.clone());
    let b = Balance::demo();
    s.set_balance(b.clone()).unwrap();
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    s.add_player(A, [0.0, 0.81, 0.0]);
    let feed = |s: &mut Server, it: PlayerIntent| {
        let tick = s.tick;
        let mut it = it;
        it.tick = tick;
        let from = tick.saturating_sub(6).max(1);
        let pkt = ClientPacket { ack_snapshot_tick: from, view: ViewState { from_tick: from, alpha: 0 }, intents: vec![it], ..Default::default() };
        s.ingest(A, &pkt.encode().unwrap());
        s.frame(Duration::from_nanos(16_666_667));
    };
    let idle = |pitch: f32| PlayerIntent { aim_yaw: q_yaw(0.0), aim_pitch: q_pitch(pitch), ..Default::default() };
    for _ in 0..800 {
        feed(&mut s, idle(0.0));
        if s.match_state.kind() == shinobi_sim::match_state::PhaseKind::Live {
            break;
        }
    }
    for _ in 0..120 {
        feed(&mut s, idle(0.0));
    }
    // the client's copy: its own CombatSystem with the same balance, fed the same input each tick
    let mut pred = CombatSystem::ninja(&Cfg::default());
    pred.apply_balance(&b, &Cfg::default());
    // start from the server's pool, as the client syncs to the server's report
    let mut mismatches = Vec::new();
    let pitch = -0.25;
    for t in 0..ticks as u32 {
        let fire = t == 3;
        let it = if fire { PlayerIntent { buttons: Buttons(Buttons::FIRE), star: kind as u8, ..idle(pitch) } } else { idle(pitch) };
        feed(&mut s, it);
        let me_s = s.players.iter().find(|p| p.id == A).unwrap();
        let pos = me_s.pos(&s.world);
        let aim = Aim { origin: vector![pos[0], pos[1] + me_s.ch.eye_above(), pos[2]], yaw: dq_yaw(it.aim_yaw), pitch: dq_pitch(it.aim_pitch) };
        // the real client has no hitbox for itself; here the server's own one is in the world, so list it as an ally
        pred.allies = vec![me_s.vitals.hitbox];
        let mut input = PlayerInput::default();
        let mut ev = Vec::new();
        // the predictor runs one tick behind the server frame in this harness, so give it the world as of now
        pred.update(
            TICK_DT,
            0.0,
            &CombatInput { fire, slice: false, ultimate: false, mark: false, star: StarKind::from_u8(it.star) },
            &aim,
            &s.world,
            &me_s.ch,
            &mut input,
            &mut ev,
        );
        let (sd, pd) = (me_s.combat.loadout.discs(), pred.loadout.discs());
        if sd.len() != pd.len() {
            mismatches.push(format!("tick {t}: {} server discs vs {} predicted", sd.len(), pd.len()));
            continue;
        }
        for (a, b) in sd.iter().zip(pd.iter()) {
            let same = a.kind == b.kind && (a.pos - b.pos).norm() < 1e-3 && (a.dir - b.dir).norm() < 1e-4;
            if !same {
                mismatches.push(format!("tick {t}: server {:?} {:?} vs predicted {:?} {:?}", a.kind, a.pos, b.kind, b.pos));
            }
        }
        let (es, ep) = (me_s.combat.shuriken_energy().unwrap().0, pred.shuriken_energy().unwrap().0);
        if (es - ep).abs() > 1e-3 {
            mismatches.push(format!("tick {t}: energy server {es} vs predicted {ep}"));
        }
    }
    assert!(mismatches.is_empty(), "{kind:?}: {:?}", &mismatches[..mismatches.len().min(5)]);
}

#[test]
fn a_predicted_standard_star_matches_the_server() {
    run_kind(StarKind::Standard);
}

#[test]
fn a_predicted_flash_matches_the_server() {
    run_kind(StarKind::Flash);
}
