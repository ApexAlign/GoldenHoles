//! Bots and the power weapons (docs/powerups.md): the bow every bot carries draws (the telegraph) and looses at a target 12-40 m off,
//! tags go out at a target in range and go off, a bot walks to a pedestal now and then, and an arrow coming at a bot is
//! sometimes met with a raised shield.
mod common;
use common::*;
use shinobi_ai::{Archetype, BotSkill};
use shinobi_sim::combat::{CombatEvent, DamageKind};
use shinobi_sim::power::PowerKind;
use shinobi_sim::power_pass::{FX_LOOSE, FX_TAG_BLAST, FX_TAG_THROW};
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::tick::Server;

const HUMAN: u16 = 1;

fn arena(human_at: [f32; 3]) -> Server {
    let mut s = flat_server(vec![[0.0, 0.81, 0.0]]);
    s.match_state.cfg.frag_limit = 1000;
    s.match_state.cfg.time_limit_ticks = 60 * 60 * 30;
    s.balance.player.regen_per_s = 0.0;
    s.add_player(HUMAN, human_at);
    // the human is a punching bag: nothing here may end early because they died
    let hp = s.players.iter_mut().find(|p| p.id == HUMAN).unwrap();
    hp.vitals.max_hp = 1.0e6;
    hp.vitals.hp = 1.0e6;
    s
}

fn give(s: &mut Server, id: u16, kind: PowerKind) {
    let cfg = s.balance.combat.power;
    let p = s.players.iter_mut().find(|p| p.id == id).unwrap();
    p.power.clear();
    assert!(p.power.grant(kind, 0, 0.0, &cfg));
}

fn human_faces_bot() -> shinobi_proto::intent::PlayerIntent {
    idle(0.0) // the human stands looking down -Z; the bot is behind them on +Z, so it never sees the draw on its screen
}

#[test]
fn a_bot_draws_the_bow_where_it_can_be_seen_then_looses_at_a_target_12_to_40_m_off() {
    // a bot starts a draw now and then rather than on every sighting, so add up a handful of seeds
    let (mut looses, mut hits, mut longest_draw) = (0u32, 0u32, 0u32);
    for seed in 1..=60u64 {
        let mut s = arena([0.0, 0.81, -30.0]);
        let mut h = host(seed);
        spawn_bot(&mut h, &mut s, 201, [0.0, 0.81, 0.0], Archetype::Light, BotSkill::Hard);
        let mut draw_run = 0u32;
        for _ in 0..60 * 12 {
            push_human(&mut s, HUMAN, human_faces_bot());
            step(&mut h, &mut s);
            let me = s.players.iter().find(|p| p.id == 201).unwrap();
            draw_run = if me.power.drawing { draw_run + 1 } else { 0 };
            longest_draw = longest_draw.max(draw_run);
            looses += s.events.iter().filter(|(id, e)| *id == 201 && matches!(e, CombatEvent::PowerFx { what, .. } if *what == FX_LOOSE)).count() as u32;
            hits += s.pvp_events.iter().filter(|e| matches!(e, PvpEvent::Hit { attacker: 201, victim: HUMAN, kind: DamageKind::Arrow, .. })).count() as u32;
        }
    }
    assert!(looses >= 3, "bots loosed arrows: {looses}");
    assert!(longest_draw >= 36, "and drew the full 0.6 s first, a telegraph a target can read: {longest_draw} ticks");
    assert!(hits >= 1, "a Hard bot standing off from a still target hits with at least one arrow: {hits} of {looses}");
}

#[test]
fn a_bot_with_tags_throws_them_at_a_target_in_range_and_they_go_off() {
    let mut s = arena([0.0, 0.81, -11.0]);
    let mut h = host(3);
    spawn_bot(&mut h, &mut s, 201, [0.0, 0.81, 0.0], Archetype::Light, BotSkill::Hard);
    give(&mut s, 201, PowerKind::Tag);
    let (mut throws, mut blasts) = (0u32, 0u32);
    for _ in 0..60 * 30 {
        push_human(&mut s, HUMAN, human_faces_bot());
        step(&mut h, &mut s);
        for (_, e) in &s.events {
            if let CombatEvent::PowerFx { what, .. } = e {
                throws += (*what == FX_TAG_THROW) as u32;
                blasts += (*what == FX_TAG_BLAST) as u32;
            }
        }
    }
    assert!(throws >= 1, "the bot threw a tag");
    assert!(blasts >= 1, "and it went off (fuse or the bot's own press): {throws} thrown, {blasts} blasts");
}

#[test]
fn bots_walk_to_a_pedestal_and_some_of_them_take_it() {
    // six bots 90 m apart (out of each other's sight, so they are not busy fighting) with a pedestal 12 m in front of each
    let mut s = arena([0.0, 0.81, -250.0]);
    let mut h = host(21);
    for i in 0..6u16 {
        let x = -225.0 + 90.0 * i as f32;
        s.add_weapon_pickup(900 + i as u32, [x + 12.0, 0.81, 0.0], PowerKind::Tag, 45.0);
        spawn_bot(&mut h, &mut s, 201 + i, [x, 0.81, 0.0], Archetype::Light, BotSkill::Normal);
    }
    let mut taken_by = None;
    for _ in 0..60 * 25 {
        push_human(&mut s, HUMAN, human_faces_bot());
        step(&mut h, &mut s);
        if taken_by.is_none() {
            taken_by = s.players.iter().find(|p| p.id != HUMAN && p.power.kind == PowerKind::Tag).map(|p| p.id);
        }
    }
    assert!(taken_by.is_some(), "one of six bots fetched the tags");
}

#[test]
fn a_bot_sometimes_raises_its_shield_against_an_arrow_and_parries_it_back() {
    // an arrow comes at each of 12 bots from 30 m: some of them guard in time, and a fresh guard sends the arrow home
    let (mut reflected, mut blocked_or_parried) = (0u32, 0u32);
    for seed in 0..12u64 {
        let mut s = arena([0.0, 0.81, -300.0]);
        let mut h = host(100 + seed);
        spawn_bot(&mut h, &mut s, 201, [0.0, 0.81, 0.0], Archetype::Light, BotSkill::Hard);
        let a = shinobi_sim::power_pass::Arrow { owner: HUMAN, uid: 77, pos: [0.0, 1.4, -30.0], vel: [0.0, 0.0, 90.0], origin: [0.0, 1.4, -31.0], age: 0.0, reflected: false, damage_mult: 1.0, tether: false };
        s.arrows.push(a);
        let mut dmg = 0.0;
        for _ in 0..60 {
            push_human(&mut s, HUMAN, idle(0.0));
            step(&mut h, &mut s);
            dmg += damage_to(&s.pvp_events, 201);
            if s.arrows.iter().any(|a| a.reflected) {
                reflected += 1;
                break;
            }
        }
        blocked_or_parried += (dmg < 80.0 * 0.5) as u32;
    }
    assert!(reflected >= 1, "at least one of 12 bots parried an arrow back ({reflected})");
    assert!(blocked_or_parried >= reflected, "{blocked_or_parried}");
}

/// The real thing: eight bots in a free-for-all on ffa_small (pedestals from the level file) pick the weapons up and use them.
#[test]
fn eight_bots_on_the_small_map_take_and_use_the_power_weapons() {
    use shinobi_sim::level::parse_level_file;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../levels/ffa_small.lvl");
    let f = parse_level_file(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut s = Server::new(shinobi_sim::character::Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_spawns(f.level.spawns.clone());
    s.set_balance(shinobi_sim::balance::Balance::demo()).unwrap();
    s.match_state.cfg.frag_limit = 1000;
    s.match_state.cfg.time_limit_ticks = 60 * 60 * 10;
    for (i, p) in f.pickups.iter().enumerate() {
        match p.kind {
            PowerKind::None => s.add_pickup(i as u32 + 1, p.pos, shinobi_sim::combat::powerup::PowerupKind::GridBeam, p.respawn_secs),
            k => s.add_weapon_pickup(i as u32 + 1, p.pos, k, p.respawn_secs),
        }
    }
    let mut h = host(9);
    h.rebuild_perches(&s);
    for i in 0..8u16 {
        assert!(h.add_bot(&mut s, 201 + i, if i % 3 == 0 { Archetype::Heavy } else { Archetype::Light }, BotSkill::Normal));
    }
    let (mut taken, mut loosed, mut thrown, mut blasts, mut arrow_hits) = (0u32, 0u32, 0u32, 0u32, 0u32);
    for _ in 0..60 * 150 {
        step(&mut h, &mut s);
        taken += s.pickup_events.iter().filter(|e| matches!(e, shinobi_sim::pickups::PickupEvent::Taken { weapon, .. } if *weapon != PowerKind::None)).count() as u32;
        for (_, e) in &s.events {
            if let CombatEvent::PowerFx { what, .. } = e {
                loosed += (*what == FX_LOOSE) as u32;
                thrown += (*what == FX_TAG_THROW) as u32;
                blasts += (*what == FX_TAG_BLAST) as u32;
            }
        }
        arrow_hits += s.pvp_events.iter().filter(|e| matches!(e, PvpEvent::Hit { kind: DamageKind::Arrow, .. })).count() as u32;
        for p in &s.players {
            assert!(p.ch.vel.iter().all(|v| v.is_finite()));
        }
    }
    eprintln!("ffa_small, 8 bots x 150 s: {taken} weapons taken, {loosed} arrows ({arrow_hits} hits), {thrown} tags thrown, {blasts} blasts");
    assert!(taken >= 2, "bots took weapons: {taken}");
    assert!(loosed + thrown >= 1, "and used them: {loosed} arrows, {thrown} tags");
}
