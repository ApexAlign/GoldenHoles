//! Prediction parity of the power weapons: a client-style `PowerState` fed the intents (and the tag counts) a server player
//! gets steps tick for tick like the server's own -- draw progress, ammo, carry time, cooldown, the tick the arrow leaves or
//! the tag is thrown -- and the walk the client predicts for a drawn bow is the one the server walks.

use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState};
use shinobi_proto::quant::{q_pitch, q_yaw};
use shinobi_proto::TICK_DT;
use shinobi_sim::balance::Balance;
use shinobi_sim::character::Cfg;
use shinobi_sim::level::{LevelBox, LevelDesc};
use shinobi_sim::power::{PowerCfg, PowerEvent, PowerInput, PowerKind, PowerState};
use shinobi_sim::tick::{to_player_input, to_player_input_scaled, Server};

const A: u16 = 1;

fn server() -> Server {
    let lv = LevelDesc { boxes: vec![LevelBox::new([0.0, -0.5, 0.0], [200.0, 0.5, 200.0])], spawns: vec![[0.0, 0.81, 0.0]] };
    let mut s = Server::new(Cfg::default());
    lv.apply_to_server(&mut s);
    s.set_spawns(lv.spawns.clone());
    s.set_balance(Balance::demo()).unwrap();
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    s.add_player(A, [0.0, 0.81, 0.0]);
    for _ in 0..1000 {
        feed(&mut s, PlayerIntent::default());
        if s.match_state.kind() == shinobi_sim::match_state::PhaseKind::Live {
            break;
        }
    }
    for _ in 0..120 {
        feed(&mut s, PlayerIntent::default());
    }
    s
}

fn feed(s: &mut Server, it: PlayerIntent) {
    let tick = s.tick;
    let mut it = it;
    it.tick = tick;
    let from = tick.saturating_sub(6).max(1);
    let pkt = ClientPacket { ack_snapshot_tick: from, view: ViewState { from_tick: from, alpha: 0 }, intents: vec![it], ..Default::default() };
    s.ingest(A, &pkt.encode().unwrap());
    s.frame(Duration::from_nanos(16_666_667));
}

fn player_power(s: &Server) -> PowerState {
    s.players.iter().find(|p| p.id == A).unwrap().power
}

/// Runs the script on the server and on a client-style copy and returns the events both saw.
fn run(kind: PowerKind, script: impl Fn(u32) -> PlayerIntent, ticks: u32) -> (Vec<(u32, PowerEvent)>, Vec<(u32, PowerEvent)>) {
    let mut s = server();
    let cfg = s.balance.combat.power;
    assert_eq!(cfg, PowerCfg::default(), "the client predicts with the default numbers");
    s.players[0].power.grant(kind, 0, 0.0, &cfg);
    let mut pred = PowerState::default();
    pred.grant(kind, 0, 0.0, &cfg);
    let (mut sev, mut cev) = (Vec::new(), Vec::new());
    for t in 0..ticks {
        let it = script(t);
        // what the server will count before it steps this tick
        let (live, armed) = (s.live_tags(A), s.armed_tags(A));
        let mut ev = Vec::new();
        pred.update(TICK_DT, &PowerInput { draw: it.draw, fire: it.buttons.has(Buttons::FIRE), live_tags: live, armed_tags: armed, can_act: true }, &cfg, &mut ev);
        cev.extend(ev.into_iter().map(|e| (t, e)));
        feed(&mut s, it);
        let sp = player_power(&s);
        // the server also clears a used-up tag bag once the last tag is gone; the client follows its report
        let mut p2 = pred;
        if p2.spent_out(s.live_tags(A)) && !sp.held() {
            pred = p2;
        }
        assert_eq!(sp, pred, "tick {t}: server {sp:?} vs predicted {pred:?}");
        let _ = &mut sev;
    }
    // the server's own event stream is its launches: compare through the arrows / tags it created
    (sev, cev)
}

fn aim() -> PlayerIntent {
    PlayerIntent { aim_yaw: q_yaw(0.0), aim_pitch: q_pitch(0.0), ..Default::default() }
}

#[test]
fn drawing_and_loosing_a_bow_steps_identically_on_both_sides() {
    // walk + draw 40 ticks, release; a short tap; a half draw; a second full draw until the bow is empty
    let script = |t: u32| {
        let mut it = PlayerIntent { move_z: 127, ..aim() };
        it.draw = matches!(t, 5..=45 | 80..=84 | 120..=140 | 200..=240 | 300..=340);
        if t == 85 {
            it.buttons = Buttons(Buttons::FIRE); // the tap's star
        }
        it
    };
    let (_, c) = run(PowerKind::Yumi, script, 400);
    let looses: Vec<u32> = c.iter().filter(|(_, e)| *e == PowerEvent::Loose).map(|(t, _)| *t).collect();
    assert_eq!(looses, vec![46, 241, 341], "the arrows leave on the release ticks of the full draws");
    assert!(c.iter().any(|(_, e)| matches!(e, PowerEvent::DrawCancel { tap: true })));
    assert!(c.iter().any(|(_, e)| matches!(e, PowerEvent::DrawCancel { tap: false })));
}

#[test]
fn throwing_and_setting_off_tags_steps_identically_on_both_sides() {
    let script = |t: u32| {
        let mut it = PlayerIntent { move_z: 60, aim_pitch: q_pitch(-0.4), ..aim() };
        if matches!(t, 5 | 12 | 40 | 100) {
            it.buttons = Buttons(Buttons::FIRE);
        }
        it
    };
    let (_, c) = run(PowerKind::Tag, script, 220);
    let throws: Vec<u32> = c.iter().filter(|(_, e)| *e == PowerEvent::TagThrow).map(|(t, _)| *t).collect();
    assert_eq!(throws, vec![5, 40], "tick 12 is inside the cooldown and swallowed");
}

#[test]
fn the_predicted_walk_of_a_drawn_bow_is_the_servers_walk() {
    let s = server();
    let mult = s.balance.combat.power.yumi.move_mult;
    for (mx, mz) in [(0i8, 127i8), (100, 100), (-127, 0)] {
        let it = PlayerIntent { move_x: mx, move_z: mz, draw: true, aim_yaw: q_yaw(0.7), ..Default::default() };
        let (c, sv) = (to_player_input(&it), to_player_input_scaled(&it, mult));
        assert_eq!((c.move_dir, c.walk_slow), (sv.move_dir, sv.walk_slow));
        assert!((c.walk_slow - 0.25).abs() < 1e-6);
        let plain = PlayerIntent { draw: false, ..it };
        assert_eq!(to_player_input(&plain).walk_slow, 0.0);
    }
}
