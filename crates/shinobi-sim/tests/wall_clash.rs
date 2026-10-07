//! Server-side melee integration (docs/traversal_melee.md): hook-strike and wall-slash damage / reach, and the wall clash
//! between two players meeting on one face.

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent};
use shinobi_proto::quant::q_yaw;
use shinobi_sim::balance::Balance;
use shinobi_sim::character::{Cfg, MoveState};
use shinobi_sim::rapier3d::prelude::*;
use shinobi_sim::tick::Server;

const FRAME: Duration = Duration::from_nanos(16_666_667);

/// The demo preset with the katana at the 20 hp the melee-balance work targets (tests read damage from here, never hardcode).
fn balance() -> Balance {
    let mut b = Balance::demo();
    b.combat.katana.damage = 20.0;
    b.melee = shinobi_sim::balance::MeleeBalance::default(); // these tests pin exact damage: no momentum / knockback
    b
}

fn pkt(tick: u32, it: PlayerIntent) -> Vec<u8> {
    ClientPacket { intents: vec![PlayerIntent { tick, ..it }], ..Default::default() }.encode().unwrap()
}

fn send(s: &mut Server, id: u16, it: PlayerIntent) {
    let t = s.tick;
    s.ingest(id, &pkt(t, it));
}

fn put(s: &mut Server, id: u16, at: [f32; 3]) {
    let i = s.players.iter().position(|p| p.id == id).unwrap();
    let b = s.players[i].ch.body;
    s.world.bodies[b].set_position(Isometry::translation(at[0], at[1], at[2]), true);
    shinobi_sim::character::sync_body_colliders(&mut s.world, b);
}

fn hp(s: &Server, id: u16) -> f32 {
    s.players.iter().find(|p| p.id == id).unwrap().vitals.hp
}

/// Two players on the face x = 5 (a 100 m tall wall), A at z = 0 and B at z = 1.6, both wall-running.
fn clash_scene() -> Server {
    let mut s = Server::new(Cfg::default());
    s.set_balance(balance()).unwrap();
    s.add_static_box([6.0, 50.0, 0.0], [1.0, 50.0, 30.0]);
    s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
    s.add_player(1, [0.0, 0.81, 0.0]);
    s.add_player(2, [0.0, 0.81, 5.0]);
    // skip warmup / spawn protection so damage counts
    for _ in 0..400 {
        s.frame(FRAME);
    }
    for p in s.players.iter_mut() {
        p.vitals.invuln_until_tick = 0;
    }
    put(&mut s, 1, [4.4, 20.0, 0.0]);
    put(&mut s, 2, [4.4, 20.0, 1.6]);
    for id in [1u16, 2] {
        let i = s.players.iter().position(|p| p.id == id).unwrap();
        s.players[i].ch.vel = vector![0.0, 0.0, 0.0];
        s.players[i].ch.state = MoveState::WallRun { normal: vector![-1.0, 0.0, 0.0], tangent: vector![0.0, 0.0, 1.0], time_left: 3.0 };
    }
    s
}

fn hold(id_dir_z: i8) -> PlayerIntent {
    // wall-run held, facing +z (yaw pi), stick forward = along +z
    PlayerIntent { buttons: Buttons(Buttons::JUMP_HELD), move_z: id_dir_z, aim_yaw: q_yaw(std::f32::consts::PI), ..Default::default() }
}

#[test]
fn two_wall_runners_meeting_open_a_clash_and_the_first_slash_wins_and_throws_the_loser_off() {
    let mut s = clash_scene();
    send(&mut s, 1, hold(0));
    send(&mut s, 2, hold(0));
    s.frame(FRAME);
    let (c1, c2) = (s.players[0].clash, s.players[1].clash);
    assert!(c1.is_some() && c2.is_some(), "clash opened: {c1:?} {c2:?}");
    // A slashes B inside the window
    let mut slash = hold(0);
    slash.buttons = Buttons(Buttons::JUMP_HELD | Buttons::SLICE);
    send(&mut s, 1, slash);
    send(&mut s, 2, hold(0));
    let b = balance();
    let before = hp(&s, 2);
    s.frame(FRAME);
    s.frame(FRAME);
    let dealt = before - hp(&s, 2);
    let expect = b.combat.katana.damage * b.combat.katana.wall_damage_mult * Cfg::default().clash_mult;
    eprintln!("wall clash: winner's slash dealt {dealt:.1} hp (katana {} x wall {} x clash {} = {expect:.1})", b.combat.katana.damage, b.combat.katana.wall_damage_mult, Cfg::default().clash_mult);
    assert!((dealt - expect).abs() < 1.0, "dealt {dealt}, expected {expect}");
    let loser = &s.players[1];
    assert!(!matches!(loser.ch.state, MoveState::WallRun { .. }), "the loser is thrown off the wall");
    assert!(loser.ch.vel.x < -5.0, "knocked away from the face: {:?}", loser.ch.vel);
    assert!(loser.clash.is_none(), "clash over");
}

#[test]
fn a_clash_with_no_strike_throws_both_off_when_the_window_closes() {
    let mut s = clash_scene();
    for _ in 0..4 {
        send(&mut s, 1, hold(0));
        send(&mut s, 2, hold(0));
        s.frame(FRAME);
    }
    assert!(s.players[0].clash.is_some());
    let window_ticks = (Cfg::default().clash_window * 60.0) as u32 + 4;
    for _ in 0..window_ticks {
        send(&mut s, 1, hold(0));
        send(&mut s, 2, hold(0));
        s.frame(FRAME);
    }
    for p in &s.players {
        assert!(!matches!(p.ch.state, MoveState::WallRun { .. }), "player {} still on the wall after a drawn clash", p.id);
        assert!(p.clash.is_none());
    }
    assert_eq!(hp(&s, 1), hp(&s, 2), "a draw hurts nobody");
}

#[test]
fn players_on_different_faces_do_not_clash() {
    let mut s = clash_scene();
    // B on the opposite side of a second wall: normal +x
    s.add_static_box([-6.0, 50.0, 0.0], [1.0, 50.0, 30.0]);
    put(&mut s, 2, [-4.4, 20.0, 1.6]);
    s.players[1].ch.state = MoveState::WallRun { normal: vector![1.0, 0.0, 0.0], tangent: vector![0.0, 0.0, 1.0], time_left: 3.0 };
    send(&mut s, 1, hold(0));
    send(&mut s, 2, hold(0));
    s.frame(FRAME);
    assert!(s.players[0].clash.is_none() && s.players[1].clash.is_none());
}

/// Hook-strike: a swing while hooked reaches further and hits harder than a plain slash at the same range.
#[test]
fn hook_strike_reaches_further_and_hits_harder_than_a_plain_slash() {
    use shinobi_sim::character::GrappleState;
    let b = balance();
    let run = |hooked: bool, gap: f32| -> f32 {
        let mut s = Server::new(Cfg::default());
        s.set_balance(b.clone()).unwrap();
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        s.add_static_box([0.0, 10.0, 40.0], [20.0, 10.0, 1.0]); // an anchor wall behind the victim
        s.add_player(1, [0.0, 0.81, 0.0]);
        s.add_player(2, [0.0, 0.81, 30.0]);
        for _ in 0..400 {
            s.frame(FRAME);
        }
        for p in s.players.iter_mut() {
            p.vitals.invuln_until_tick = 0;
        }
        // hit boxes are lag-compensated from history: let the teleport settle in it before swinging
        put(&mut s, 1, [0.0, 0.81, 0.0]);
        put(&mut s, 2, [0.0, 0.81, gap]);
        for _ in 0..4 {
            send(&mut s, 1, PlayerIntent::default());
            send(&mut s, 2, PlayerIntent::default());
            s.frame(FRAME);
        }
        s.players[0].ch.vel = vector![0.0, 0.0, 14.0]; // closing on the target
        if hooked {
            s.players[0].ch.grapple = Some(GrappleState { anchor: vector![0.0, 10.0, 39.0], len: 38.0, time: 1.0 });
        }
        let before = hp(&s, 2);
        let mut slash = PlayerIntent { buttons: Buttons(Buttons::SLICE), aim_yaw: q_yaw(std::f32::consts::PI), ..Default::default() };
        slash.tick = s.tick;
        send(&mut s, 1, slash);
        send(&mut s, 2, PlayerIntent::default());
        s.frame(FRAME);
        s.frame(FRAME);
        before - hp(&s, 2)
    };
    let (plain_near, hook_far, plain_far) = (run(false, 2.5), run(true, 7.0), run(false, 7.0));
    eprintln!(
        "hook-strike: plain slash at 2.5 m deals {plain_near:.1}; at 7 m a plain slash deals {plain_far:.1}, a hook-strike deals {hook_far:.1} (katana {} x hook {})",
        b.combat.katana.damage, b.combat.katana.hook_damage_mult
    );
    assert!((plain_near - b.combat.katana.damage).abs() < 0.5, "plain slash damage {plain_near}");
    assert_eq!(plain_far, 0.0, "7 m is out of a plain slash's reach");
    assert!((hook_far - b.combat.katana.damage * b.combat.katana.hook_damage_mult).abs() < 0.5, "hook-strike damage {hook_far}");
}

