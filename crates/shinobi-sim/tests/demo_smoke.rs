//! Headless demo smoke: demo balance on the standard level, two scripted-but-honest duelists for 60 s of ticks.
//! Honest = legal speeds, yaw slew well under the anti-cheat ceiling, a real lag-comp view in every packet.
//! (A second scripted stream stands in for the bot crate: sim cannot depend on it.)

use std::collections::BTreeMap;
use std::f32::consts::{PI, TAU};
use std::time::Duration;

use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState, PITCH_LEVEL};
use shinobi_proto::quant::{dq_yaw, q_yaw};
use shinobi_sim::balance::Balance;
use shinobi_sim::character::Cfg;
use shinobi_sim::combat::CombatEvent;
use shinobi_sim::level::LevelDesc;
use shinobi_sim::match_state::{MatchEvent, MatchState, Phase};
use shinobi_sim::pvp::PvpEvent;
use shinobi_sim::tick::Server;

const TICKS: u32 = 60 * 60;
/// Honest turn rate: 12 deg per tick (720 deg/s), far below the 90 deg/tick ceiling.
const TURN: f32 = 12.0 * PI / 180.0;

fn angle_diff(to: f32, from: f32) -> f32 {
    (to - from + PI).rem_euclid(TAU) - PI
}

/// What a keen human might do: face the nearest living opponent, close in, strafe, throw at range, cut when near.
fn script(s: &Server, me: u16, tick: u32) -> PlayerIntent {
    let idle = PlayerIntent { tick, aim_pitch: PITCH_LEVEL, ..Default::default() };
    let Some(a) = s.players.iter().find(|p| p.id == me) else { return idle };
    // nobody to fight (or dead): an honest client keeps its view where it is, it does not snap to yaw 0
    let idle = PlayerIntent { aim_yaw: a.yaw, ..idle };
    let pa = a.pos(&s.world);
    let dist_to = |p: &shinobi_sim::tick::Player| {
        let q = p.pos(&s.world);
        ((q[0] - pa[0]).powi(2) + (q[2] - pa[2]).powi(2)).sqrt()
    };
    // nearest living opponent, ties -> lowest id
    let Some(b) = s
        .players
        .iter()
        .filter(|p| p.id != me && p.vitals.alive())
        .min_by(|x, y| dist_to(x).total_cmp(&dist_to(y)).then(x.id.cmp(&y.id)))
    else {
        return idle;
    };
    let pb = b.pos(&s.world);
    let (dx, dz) = (pb[0] - pa[0], pb[2] - pa[2]);
    let dist = (dx * dx + dz * dz).sqrt();
    let want = (-dx).atan2(-dz); // yaw 0 faces -Z, positive turns toward -X
    let cur = dq_yaw(a.yaw);
    let step = angle_diff(want, cur).clamp(-TURN, TURN);
    let strafe = if (tick / 50 + me as u32) % 2 == 0 { 90 } else { -90 };
    let mut buttons = 0u8;
    if a.vitals.alive() && dist < 5.0 && (tick + me as u32 * 13) % 45 == 0 {
        buttons |= Buttons::SLICE;
    }
    if a.vitals.alive() && dist > 2.0 && dist < 25.0 && (tick + me as u32 * 7) % 18 == 0 {
        buttons |= Buttons::FIRE;
    }
    PlayerIntent {
        tick,
        buttons: Buttons(buttons),
        move_x: if dist < 6.0 { strafe } else { 0 },
        move_z: if dist > 3.0 { 127 } else { 0 },
        aim_yaw: q_yaw(cur + step),
        aim_pitch: PITCH_LEVEL,
        ..Default::default()
    }
}

fn feed(s: &mut Server, ids: &[u16], tick: u32) {
    for &me in ids {
        let it = script(s, me, tick);
        let from = tick.saturating_sub(6).max(1);
        let pkt = ClientPacket {
            ack_snapshot_tick: from,
            view: ViewState { from_tick: from, alpha: 0 },
            intents: vec![it],
            ..Default::default()
        };
        s.ingest(me, &pkt.encode().unwrap());
    }
}

fn demo_server(n: u16) -> Server {
    let level = LevelDesc::standard();
    let mut s = Server::new(Cfg::default());
    level.apply_to_server(&mut s);
    s.set_spawns(level.spawns.clone());
    s.set_balance(Balance::demo()).unwrap();
    for id in 1..=n {
        let at = s.pick_spawn(None);
        s.add_player(id, at);
    }
    s
}

#[test]
fn sixty_seconds_of_duelling_is_consistent_and_honest() {
    let mut s = demo_server(2);

    let mut kills: Vec<(u32, u16, u16)> = Vec::new(); // (tick, killer, victim)
    let mut respawns = 0;
    let (mut hit_events, mut player_damage) = (0usize, 0usize);
    let mut started_at: Option<u32> = None;
    let mut ever_live_after_warmup = false;
    let mut saw_warmup_first = false;
    // per-match tallies, reset on MatchStart exactly like the server does
    let mut tally: BTreeMap<u16, (u16, u16)> = BTreeMap::new();

    for t in 0..TICKS {
        let tick = s.tick;
        assert_eq!(tick, t);
        feed(&mut s, &[1, 2], tick);
        s.frame(Duration::from_nanos(16_666_667));

        if t == 30 {
            saw_warmup_first = matches!(s.match_state.phase, Phase::Warmup { .. });
        }
        for e in &s.match_events {
            if let MatchEvent::MatchStart { tick, .. } = e {
                started_at.get_or_insert(*tick);
                tally.clear();
            }
        }
        if matches!(s.match_state.phase, Phase::Live { .. }) {
            ever_live_after_warmup = true;
        }
        for e in &s.pvp_events {
            match *e {
                PvpEvent::Kill { tick, killer, victim, .. } => {
                    assert_eq!(tick, t, "a Kill is reported in the frame it happened");
                    kills.push((tick, killer, victim));
                    tally.entry(killer).or_default().0 += 1;
                    tally.entry(victim).or_default().1 += 1;
                }
                PvpEvent::Respawn { .. } => respawns += 1,
                PvpEvent::Hit { tick: ht, .. } => {
                    assert_eq!(ht, t, "a Hit is reported in the same frame the damage happened");
                    hit_events += 1;
                }
                _ => {}
            }
        }
        // every landed weapon hit is visible to clients twice over: the raw combat event and the pvp Hit
        player_damage += s
            .events
            .iter()
            .filter(|(_, e)| matches!(e, CombatEvent::Damage { collider, .. } if s.players.iter().any(|p| p.vitals.hitbox == *collider)))
            .count();
    }

    eprintln!("demo smoke: {} kills, {} respawns, {} hits, {} raw damage events, match started at tick {:?}, final {:?}", kills.len(), respawns, hit_events, player_damage, started_at, MatchState::scoreboard(&s.players));
    assert!(saw_warmup_first, "starts in warmup");
    assert!(started_at.is_some_and(|t| (170..=200).contains(&t)), "demo warmup is 3 s after the first player: {started_at:?}");
    assert!(ever_live_after_warmup, "match went live");
    assert!(!kills.is_empty(), "60 s of duelling produced no kill at all (hits: {hit_events})");
    assert!(respawns >= 1, "someone died and came back");
    assert!(hit_events >= kills.len(), "every kill was preceded by hits");
    assert!(player_damage >= hit_events, "raw damage events cover every Hit ({player_damage} < {hit_events})");

    // scoreboard agrees with the Kill events of the match in progress / just finished
    for e in MatchState::scoreboard(&s.players) {
        let (k, d) = tally.get(&e.id).copied().unwrap_or((0, 0));
        assert_eq!((e.kills, e.deaths), (k, d), "player {} scoreboard vs kill events", e.id);
    }

    // honest input never trips the anti-cheat
    for p in &s.players {
        assert_eq!(p.guard.strikes, 0, "player {} collected strikes", p.id);
    }
    assert_eq!(s.players.len(), 2, "nobody was kicked");
}

/// Four scripted duelists, free-for-all, five minutes: hp / score / respawn / match bookkeeping stays exact
/// through many kills, several matches, intermissions and map changes.
#[test]
fn four_duelist_free_for_all_for_five_minutes_keeps_the_books() {
    const TICKS: u32 = 5 * 60 * 60;
    let ids = [1u16, 2, 3, 4];
    let mut s = demo_server(4);
    let max_hp = s.balance.player.max_hp;

    let mut tally: BTreeMap<u16, (u16, u16)> = BTreeMap::new();
    let mut alive_before: BTreeMap<u16, bool> = ids.iter().map(|i| (*i, true)).collect();
    let (mut kills, mut respawns, mut hits, mut matches_ended, mut maps) = (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut starts = 0u32;

    for t in 0..TICKS {
        let tick = s.tick;
        feed(&mut s, &ids, tick);
        s.frame(Duration::from_nanos(16_666_667));

        let mut victims_this_frame = Vec::new();
        for e in &s.match_events {
            match *e {
                MatchEvent::MatchStart { .. } => {
                    tally.clear();
                    starts += 1;
                    // a reset revives everybody without a Respawn event
                    for v in alive_before.values_mut() {
                        *v = true;
                    }
                }
                MatchEvent::NextMap { .. } => maps += 1,
                _ => {}
            }
        }
        for e in &s.pvp_events {
            match *e {
                PvpEvent::Kill { tick, killer, victim, .. } => {
                    assert_eq!(tick, t);
                    assert!(alive_before[&victim], "tick {t}: {victim} killed while already dead");
                    assert!(!victims_this_frame.contains(&victim), "tick {t}: {victim} killed twice in one frame");
                    assert_ne!(killer, victim);
                    victims_this_frame.push(victim);
                    kills += 1;
                    tally.entry(killer).or_default().0 += 1;
                    tally.entry(victim).or_default().1 += 1;
                }
                PvpEvent::Respawn { player, .. } => {
                    assert!(!alive_before[&player], "tick {t}: {player} respawned while alive");
                    alive_before.insert(player, true);
                    respawns += 1;
                }
                PvpEvent::Hit { tick, amount, attacker, victim, .. } => {
                    assert_eq!(tick, t);
                    assert!(amount >= 0.0 && attacker != victim, "a parried hit reports 0, nothing reports negative");
                    hits += 1;
                }
                // ring-outs and wall-splats: a death like any other, credited to whoever pushed
                PvpEvent::HazardKill { victim, killer, .. } => {
                    assert!(alive_before[&victim], "tick {t}: {victim} killed while already dead");
                    victims_this_frame.push(victim);
                    if let Some(k) = killer {
                        tally.entry(k).or_default().0 += 1;
                    }
                    tally.entry(victim).or_default().1 += 1;
                }
                _ => {}
            }
        }
        for v in victims_this_frame {
            alive_before.insert(v, false);
        }
        for e in &s.match_events {
            if let MatchEvent::MatchEnd { winner, .. } = *e {
                matches_ended += 1;
                let board = MatchState::scoreboard(&s.players);
                match winner {
                    Some(w) => {
                        let top = board[0].kills;
                        assert_eq!(board.iter().find(|e| e.id == w).unwrap().kills, top, "winner has the most kills");
                    }
                    None => assert!(board.len() < 2 || (board[0].kills, board[0].deaths) == (board[1].kills, board[1].deaths), "a draw means a true tie"),
                }
            }
        }

        // per-tick invariants
        for p in &s.players {
            let v = &p.vitals;
            assert!(v.hp >= 0.0 && v.hp <= max_hp, "tick {t}: hp {} out of range", v.hp);
            if v.alive() {
                assert!(v.hp > 0.0, "tick {t}: {} alive with no hp", p.id);
            } else {
                assert!(v.hp == 0.0 && v.dead_until_tick.is_some(), "tick {t}: {} dead but bookkeeping disagrees", p.id);
            }
            let pos = p.pos(&s.world);
            assert!(pos.iter().all(|c| c.is_finite()) && pos[0].abs() < 200.0 && pos[2].abs() < 200.0, "tick {t}: {pos:?}");
        }
    }

    eprintln!("ffa: {kills} kills, {respawns} respawns, {hits} hits, {starts} match starts, {matches_ended} ended, {maps} map changes");
    assert!(kills >= 20 && respawns >= 10 && hits >= kills, "a real fight happened");
    assert!(matches_ended >= 1 && maps >= 1 && starts >= 2, "matches cycled through intermission and a new map");

    // final scoreboard equals the Kill events of the match in progress / just finished
    for e in MatchState::scoreboard(&s.players) {
        let (k, d) = tally.get(&e.id).copied().unwrap_or((0, 0));
        assert_eq!((e.kills, e.deaths), (k, d), "player {}", e.id);
    }
    for p in &s.players {
        assert_eq!(p.guard.strikes, 0, "player {} collected strikes", p.id);
    }
    assert_eq!(s.players.len(), 4, "nobody was kicked");
}
