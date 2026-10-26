//! Kinetic Battery counterplay and determinism (`docs/weapons/shield.md`).

use std::time::Duration;

use shinobi_proto::intent::{ClientPacket, PlayerIntent, ViewState};
use shinobi_proto::quant::{q_pitch, q_yaw};
use shinobi_sim::balance::Balance;
use shinobi_sim::character::Cfg;
use shinobi_sim::combat::DamageKind;
use shinobi_sim::level::{parse_level_file, LevelBox, LevelDesc};
use shinobi_sim::pvp::{apply_hits, Cause, PvpHit};
use shinobi_sim::shield::BATTERY_MAX;
use shinobi_sim::tick::Server;

const A: u16 = 1;
const B: u16 = 2;
const ONE_TICK: Duration = Duration::from_nanos(16_666_667);

fn duel() -> Server {
    let lv = LevelDesc { boxes: vec![LevelBox::new([0.0, -0.5, 0.0], [50.0, 0.5, 50.0])], spawns: vec![[0.0, 0.81, 0.0], [0.0, 0.81, 1.5]] };
    let mut s = Server::new(Cfg::default());
    lv.apply_to_server(&mut s);
    s.set_spawns(lv.spawns.clone());
    s.set_balance(Balance::default()).unwrap();
    s.add_player(A, [0.0, 0.81, 0.0]);
    s.add_player(B, [0.0, 0.81, 1.5]);
    for p in &mut s.players {
        p.vitals.invuln_until_tick = 0;
        p.vitals.hp = p.vitals.max_hp;
        p.yaw = q_yaw(0.0);
    }
    s
}

fn hit(s: &mut Server, victim: u16, amount: f32, kind: DamageKind, guarded: bool, tick: u32) -> f32 {
    let vi = s.players.iter().position(|p| p.id == victim).unwrap();
    let a = s.players[vi].vitals.hp;
    let (block, melee, cloak, shield) = (s.balance.guard.cfg(), s.balance.melee.cfg(), s.balance.combat.cloak, s.balance.shield.cfg());
    let h = PvpHit { attacker: A, victim: vi, amount, cause: Cause::Weapon(kind), head: false, stealth: false, point: [0.0, 0.81, 0.0], guarded };
    let mut out = Vec::new();
    apply_hits(&mut s.players, &mut s.world, &[h], tick, 150, &block, &melee, &cloak, &shield, None, &mut out);
    a - s.players[vi].vitals.hp
}

fn shield(s: &Server, id: u16) -> shinobi_sim::shield::ShieldState {
    s.players.iter().find(|p| p.id == id).unwrap().shield
}

fn hold_block(s: &mut Server, id: u16, since: u32) {
    let p = s.players.iter_mut().find(|p| p.id == id).unwrap();
    p.blk.held = true;
    p.blk.since = since;
    p.blk.energy = 1.0;
    p.yaw = q_yaw(0.0);
}

#[test]
fn blocking_a_star_or_arrow_charges_the_battery_but_blocking_a_cut_does_not() {
    let mut s = duel();
    hold_block(&mut s, B, 0);
    assert!((hit(&mut s, B, 7.0, DamageKind::Shuriken, false, 100) - 0.0).abs() < 0.01, "a star on the shield is deflected");
    assert_eq!(shield(&s, B).battery, 80);

    hold_block(&mut s, B, 0);
    assert!((hit(&mut s, B, 20.0, DamageKind::Slash, false, 100) - 3.0).abs() < 0.01, "a cut leaks 15%");
    assert_eq!(shield(&s, B).battery, 80, "a blocked cut never feeds the Battery");

    hold_block(&mut s, B, 0);
    assert!(hit(&mut s, B, 20.0, DamageKind::Arrow, true, 100) > 0.0, "the server already resolved the arrow block");
    assert_eq!(shield(&s, B).battery, 160, "a blocked arrow uses the same charge as a blocked star");
}

#[test]
fn a_parry_charges_and_still_negates_the_hit() {
    let mut s = duel();
    hold_block(&mut s, B, 99);
    assert_eq!(hit(&mut s, B, 20.0, DamageKind::Slash, false, 100), 0.0);
    assert_eq!(shield(&s, B).battery, 250);
    hold_block(&mut s, B, 99);
    assert_eq!(hit(&mut s, B, 7.0, DamageKind::Shuriken, false, 100), 0.0);
    assert_eq!(shield(&s, B).battery, 500);
}

#[test]
fn the_aura_boosts_only_unguarded_slash_damage() {
    let mut s = duel();
    let cfg = s.balance.shield.cfg();
    let ai = s.players.iter().position(|p| p.id == A).unwrap();
    s.players[ai].shield.battery = BATTERY_MAX;
    assert_eq!(s.players[ai].shield.update(100, &cfg), shinobi_sim::shield::ShieldTick::AuraStart);

    assert!((hit(&mut s, B, 20.0, DamageKind::Slash, false, 100) - 24.0).abs() < 0.01, "20 x 1.2");
    let bi = s.players.iter().position(|p| p.id == B).unwrap();
    s.players[bi].vitals.hp = s.players[bi].vitals.max_hp;
    hold_block(&mut s, B, 0);
    assert!((hit(&mut s, B, 20.0, DamageKind::Slash, false, 100) - 3.0).abs() < 0.01, "a shield is still 15%, never 18%");
    s.players[bi].vitals.hp = s.players[bi].vitals.max_hp;
    assert!((hit(&mut s, B, 7.0, DamageKind::Shuriken, false, 100) - 0.0).abs() < 0.01, "stars are not boosted");
    assert!(s.players[ai].shield.aura_active(399));
    s.players[ai].shield.update(400, &cfg);
    assert!(!s.players[ai].shield.aura_active(400));
    s.players[bi].vitals.hp = s.players[bi].vitals.max_hp;
    s.players[bi].blk.held = false;
    assert!((hit(&mut s, B, 20.0, DamageKind::Slash, false, 400) - 20.0).abs() < 0.01, "expired aura is back to base damage");
}

#[test]
fn core_vent_fills_over_fill_ticks_then_cools_down_per_player() {
    let text = "box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nspawn 10 0.81 0\n\
        zone 0 0.81 0 2 2 2 core_vent fill_ticks=5 cooldown_ticks=10\n";
    let f = parse_level_file(text).unwrap();
    let mut s = Server::new(Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_spawns(f.level.spawns.clone());
    s.set_level_features(f.features);
    s.add_player(A, [0.0, 0.81, 0.0]);
    s.add_player(B, [10.0, 0.81, 0.0]);
    let cfg = s.balance.shield.cfg();
    let sim_cfg = *s.cfg();

    for tick in 0..4 {
        s.level_features.end_tick(&s.world, &mut s.players, tick, &sim_cfg, true);
        for p in &mut s.players {
            p.shield.update(tick, &cfg);
        }
    }
    assert_eq!(shield(&s, A).battery, 800);
    assert!(!shield(&s, A).aura_active(3));
    assert_eq!(shield(&s, B).battery, 0, "a player outside the vent gets nothing");

    s.level_features.end_tick(&s.world, &mut s.players, 4, &sim_cfg, true);
    for p in &mut s.players {
        p.shield.update(4, &cfg);
    }
    assert_eq!(shield(&s, A).battery, 0, "full charge is consumed by the aura");
    assert_eq!(shield(&s, A).aura_ticks_left(4), 300);
    assert_eq!(shield(&s, A).vent_ready_tick, 14);
    assert_eq!(shield(&s, A).vent_reveal_until, 4 + 300, "a completed fill reveals the filler for reveal_ticks");
    assert_eq!(shield(&s, B).vent_reveal_until, 0);

    for tick in 5..14 {
        s.level_features.end_tick(&s.world, &mut s.players, tick, &sim_cfg, true);
    }
    assert_eq!(shield(&s, A).vent_fill_ticks, 0, "the vent is on cooldown");
    s.level_features.end_tick(&s.world, &mut s.players, 14, &sim_cfg, true);
    assert_eq!(shield(&s, A).vent_fill_ticks, 1, "cooldown ended");

    s.players.iter_mut().find(|p| p.id == A).unwrap().shield.on_death();
    assert_eq!(shield(&s, A).vent_reveal_until, 0, "death ends the reveal");
    assert_eq!(shield(&s, A).vent_ready_tick, 14, "but the cooldown survives it");
}

fn replay_server() -> Server {
    let text = "box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nspawn 20 0.81 0\n\
        zone 0 0.81 0 2 2 2 core_vent fill_ticks=30 cooldown_ticks=120\n";
    let f = parse_level_file(text).unwrap();
    let mut s = Server::new(Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_spawns(f.level.spawns.clone());
    s.set_level_features(f.features);
    let mut b = Balance::demo();
    b.player.invuln_ticks = 12;
    b.player.first_spawn_invuln_ticks = 0;
    s.set_balance(b).unwrap();
    s.match_state.cfg.warmup_ticks = 1;
    s.match_state.cfg.min_players = 1;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    s.match_state.cfg.frag_limit = u16::MAX;
    s.add_player(A, [0.0, 0.81, 0.0]);
    s.add_player(B, [20.0, 0.81, 0.0]);
    for p in &mut s.players {
        p.vitals.invuln_until_tick = 0;
    }
    s
}

#[test]
fn thousand_tick_replay_is_identical() {
    let mut a = replay_server();
    let mut b = replay_server();
    let mut saw_aura = false;
    for _ in 0..1000 {
        a.frame(ONE_TICK);
        b.frame(ONE_TICK);
        saw_aura |= a.players.iter().any(|p| p.shield.aura_active(a.tick));
        assert_eq!(a.players.iter().map(|p| (p.id, p.shield)).collect::<Vec<_>>(), b.players.iter().map(|p| (p.id, p.shield)).collect::<Vec<_>>());
        assert_eq!(a.players.iter().map(|p| p.pos(&a.world)).collect::<Vec<_>>(), b.players.iter().map(|p| p.pos(&b.world)).collect::<Vec<_>>());
    }
    assert!(saw_aura, "the replay exercised the core-vent fill and aura");
}

fn feed(s: &mut Server, id: u16, mut it: PlayerIntent) {
    let tick = s.tick;
    it.tick = tick;
    let from = tick.saturating_sub(6).max(1);
    let pkt = ClientPacket { ack_snapshot_tick: from, view: ViewState { from_tick: from, alpha: 0 }, intents: vec![it], ..Default::default() };
    s.ingest(id, &pkt.encode().unwrap());
}

fn movement_server() -> Server {
    let lv = LevelDesc { boxes: vec![LevelBox::new([0.0, -0.5, 0.0], [100.0, 0.5, 100.0])], spawns: vec![[0.0, 0.81, 0.0]] };
    let mut s = Server::new(Cfg::default());
    lv.apply_to_server(&mut s);
    s.set_spawns(lv.spawns.clone());
    let mut b = Balance::demo();
    b.player.first_spawn_invuln_ticks = 0;
    s.set_balance(b).unwrap();
    s.match_state.cfg.warmup_ticks = 1;
    s.match_state.cfg.min_players = 1;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    s.add_player(A, [0.0, 0.81, 0.0]);
    for _ in 0..120 {
        feed(&mut s, A, PlayerIntent::default());
        s.frame(ONE_TICK);
    }
    s.players[0].vitals.invuln_until_tick = 0;
    s
}

#[test]
fn an_active_aura_does_not_change_movement() {
    let mut plain = movement_server();
    let mut aura = movement_server();
    let cfg = aura.balance.shield.cfg();
    aura.players[0].shield.battery = BATTERY_MAX;
    assert_eq!(aura.players[0].shield.update(aura.tick, &cfg), shinobi_sim::shield::ShieldTick::AuraStart);
    for t in 0..120u32 {
        let it = PlayerIntent { move_z: 127, aim_yaw: q_yaw(0.3), aim_pitch: q_pitch(0.0), ..Default::default() };
        feed(&mut plain, A, it);
        feed(&mut aura, A, it);
        plain.frame(ONE_TICK);
        aura.frame(ONE_TICK);
        assert!(aura.players[0].shield.aura_active(aura.tick), "tick {t}: aura still on");
        assert_eq!(plain.players[0].pos(&plain.world), aura.players[0].pos(&aura.world), "tick {t}");
    }
}
