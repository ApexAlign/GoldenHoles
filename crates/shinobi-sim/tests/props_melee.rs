//! Katana cuts smash small prop-like dboxes, including the whirl cut (which used to skip boxes).

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState};
use shinobi_proto::quant::{q_pitch, q_yaw};
use shinobi_sim::balance::Balance;
use shinobi_sim::character::Cfg;
use shinobi_sim::level::{LevelBox, LevelDesc};
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

fn crate_box(z: f32) -> LevelBox {
    // a 1 m crate on the floor, hp 40: one katana hit (piece_damage 60) smashes it
    LevelBox::new([0.0, 0.5, z], [0.5, 0.5, 0.5]).destructible(40.0)
}

#[test]
fn a_plain_swing_smashes_a_crate() {
    let mut s = server_with(-40.0, vec![crate_box(-1.6)], |_| {});
    for t in 0..120 {
        feed(&mut s, if t == 5 { swing(0.0) } else { idle(0.0) }, idle(B_YAW));
        if s.breaks.is_broken(1) {
            return;
        }
    }
    panic!("a katana swing must smash a 40 hp crate");
}

#[test]
fn a_whirl_cut_smashes_a_crate_behind_the_swordsman() {
    // crate BEHIND A (A looks down -Z, crate at +Z): only the 360 sweep reaches it
    let mut s = server_with(-40.0, vec![crate_box(1.6)], |_| {});
    let t0 = s.tick;
    let a = s.players.iter_mut().find(|p| p.id == A).unwrap();
    a.kmoves.unblocked_at = Some(t0 + 2);
    for t in 0..40 {
        feed(&mut s, if t == 3 { swing(0.0) } else { idle(0.0) }, idle(B_YAW));
    }
    assert!(s.breaks.is_broken(1), "whirl chips boxes all round");
    // control: a plain cut does not reach behind
    let mut s = server_with(-40.0, vec![crate_box(1.6)], |_| {});
    for t in 0..40 {
        feed(&mut s, if t == 3 { swing(0.0) } else { idle(0.0) }, idle(B_YAW));
    }
    assert!(!s.breaks.is_broken(1));
}

/// `python3 scripts/lvl_props.py levels/ffa_split.lvl levels/ffa_split_props.lvl` output: parses, within budget, props clear of spawns.
#[test]
fn the_generated_props_level_parses_fits_the_budget_and_keeps_spawns_clear() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../levels/");
    let base = shinobi_sim::level::parse_level_file(&std::fs::read_to_string(format!("{dir}ffa_split.lvl")).unwrap()).unwrap();
    let lv = shinobi_sim::level::parse_level_file(&std::fs::read_to_string(format!("{dir}ffa_split_props.lvl")).unwrap()).unwrap();
    let n = lv.level.boxes.len();
    assert!(n <= 1800, "{n} boxes");
    let props = &lv.level.boxes[base.level.boxes.len()..];
    assert!(props.len() >= 150, "{} prop boxes", props.len());
    assert!(props.iter().filter(|b| b.hp > 0.0 && b.hp <= 60.0).count() * 10 >= props.len() * 8, "most props die to one katana hit");
    assert!(props.iter().all(|b| b.half.iter().all(|h| *h <= 1.8)), "props are small");
    // base level untouched
    assert_eq!(&lv.level.boxes[..base.level.boxes.len()], &base.level.boxes[..]);
    assert_eq!(lv.level.spawns, base.level.spawns);
    for s in lv.level.spawns.iter().chain(lv.pickups.iter().map(|p| &p.pos)) {
        for b in props {
            let inside = (0..3).all(|k| (s[k] - b.center[k]).abs() < b.half[k] + if k == 1 { 1.0 } else { 0.6 });
            assert!(!inside, "prop {b:?} sits on spawn/pickup {s:?}");
        }
    }
}
