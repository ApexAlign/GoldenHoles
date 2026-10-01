mod common;
use common::*;
use shinobi_ai::fsm::Phase;
use shinobi_ai::{Archetype, BotSkill};
use shinobi_proto::quant::q_yaw;
use shinobi_sim::pvp::PvpEvent;

#[test]
fn normal_bot_kills_a_stationary_dummy_and_never_hurts_itself() {
    for arch in [Archetype::Heavy, Archetype::Light] {
        let mut s = flat_server(vec![[0.0, 0.81, 0.0]]);
        s.add_player(1, [16.0, 0.81, 0.0]); // dummy human: never sends input
        let mut h = host(42);
        spawn_bot(&mut h, &mut s, 201, [0.0, 0.81, 0.0], arch, BotSkill::Normal);
        let (mut kills, mut self_dmg) = (0, 0.0);
        let mut seen: Vec<&'static str> = Vec::new();
        for _ in 0..1500 {
            step(&mut h, &mut s);
            for e in &s.pvp_events {
                if let PvpEvent::Kill { killer: 201, victim: 1, .. } = e {
                    kills += 1;
                }
            }
            self_dmg += damage_to(&s.pvp_events, 201);
            let n = phase_name(&h, 201);
            if seen.last() != Some(&n) {
                seen.push(n);
            }
        }
        assert!(kills >= 1, "{arch:?} bot never killed the dummy; phases: {seen:?}");
        assert_eq!(self_dmg, 0.0, "{arch:?} bot hurt itself");
        assert_eq!(s.players.iter().find(|p| p.id == 201).unwrap().guard.strikes, 0, "{arch:?} bot earned anti-cheat strikes");
    }
}

#[test]
fn off_screen_bots_never_exceed_the_ticket_cap() {
    let mut s = flat_server(vec![[0.0, 0.81, 12.0], [3.0, 0.81, 12.0], [-3.0, 0.81, 12.0], [6.0, 0.81, 14.0], [-6.0, 0.81, 14.0]]);
    s.add_player(1, [0.0, 0.81, 0.0]); // human faces -Z (yaw 0); the bots spawn behind at +Z
    s.players[0].vitals.hp = 1e6;
    s.players[0].vitals.max_hp = 1e6;
    let mut h = host(11);
    for id in 201..=205 {
        assert!(h.add_bot(&mut s, id, Archetype::Light, BotSkill::Hard));
    }
    let cap = h.tickets.cfg.max_active;
    let (mut max_active, mut max_strikers, mut total_strikes) = (0usize, 0usize, 0u32);
    for _ in 0..900 {
        push_human(&mut s, 1, idle(0.0));
        step(&mut h, &mut s);
        max_active = max_active.max(h.tickets.active_count());
        // bots that walked into the player's view need no ticket; the cap is on strikers outside the frustum
        let f = shinobi_ai::view::Frustum::default();
        let me = pos(&s, 1);
        let eye = shinobi_sim::rapier3d::prelude::Vector::new(me[0], me[1] + 0.6, me[2]);
        let strikers = (201..=205)
            .filter(|&id| is_strike(&h, id))
            .filter(|&id| {
                let b = pos(&s, id);
                !f.contains(eye, 0.0, 0.0, shinobi_sim::rapier3d::prelude::Vector::new(b[0], b[1], b[2]), 0.0)
            })
            .count();
        max_strikers = max_strikers.max(strikers);
    }
    for id in 201..=205 {
        total_strikes += h.bot(id).unwrap().stats.strikes;
    }
    assert!(total_strikes >= 2, "sanity: bots should have struck at all ({total_strikes})");
    assert!(max_active <= cap, "tickets exceeded: {max_active} > {cap}");
    assert!(max_strikers <= cap, "{max_strikers} off-screen bots slicing at once, cap {cap}");
    assert!(h.tickets.granted_total >= 2);
}

#[test]
fn bot_in_the_players_view_breaks_it_quickly() {
    use shinobi_ai::view::Frustum;
    use shinobi_sim::rapier3d::prelude::*;
    let mut evaded = 0;
    let mut broke_tracking = 0;
    for seed in 0..8u64 {
        let mut s = flat_server(vec![[0.0, 0.81, -16.0]]);
        s.add_player(1, [0.0, 0.81, 0.0]);
        s.players[0].vitals.hp = 1e6;
        s.players[0].vitals.max_hp = 1e6;
        let mut h = host(seed);
        h.add_bot(&mut s, 201, Archetype::Light, BotSkill::Hard);
        let f = Frustum::default();
        let (mut did_evade, mut h_min, mut h_max) = (false, f32::MAX, f32::MIN);
        for _ in 0..70 {
            push_human(&mut s, 1, idle(0.0));
            step(&mut h, &mut s);
            did_evade |= matches!(h.bot(201).unwrap().phase, Phase::Evade { .. });
            let (me, bot) = (pos(&s, 1), pos(&s, 201));
            let eye = vector![me[0], me[1] + 0.6, me[2]];
            let bp = vector![bot[0], bot[1], bot[2]];
            let h = f.angles(eye, 0.0, 0.0, bp).h;
            h_min = h_min.min(h);
            h_max = h_max.max(h);
        }
        evaded += did_evade as u32;
        // a sharp crosshair-relative jump (>= 18 deg) is what actually breaks a player's tracking
        broke_tracking += ((h_max - h_min).to_degrees() >= 18.0) as u32;
    }
    assert!(evaded >= 6, "Hard bot evaded in only {evaded}/8 seeded runs");
    assert!(broke_tracking >= 6, "bot swung >=18 deg off the crosshair in only {broke_tracking}/8 runs");
}

#[test]
fn runs_are_bit_identical() {
    fn run() -> Vec<u64> {
        let mut s = flat_server(vec![[0.0, 0.81, 10.0], [8.0, 0.81, 10.0], [-8.0, 0.81, 10.0]]);
        s.add_player(1, [0.0, 0.81, -10.0]);
        s.players[0].vitals.hp = 1e6;
        s.players[0].vitals.max_hp = 1e6;
        let mut h = host(99);
        h.add_bot(&mut s, 201, Archetype::Heavy, BotSkill::Normal);
        h.add_bot(&mut s, 202, Archetype::Light, BotSkill::Normal);
        h.add_bot(&mut s, 203, Archetype::Light, BotSkill::Hard);
        let mut hashes = Vec::new();
        for t in 0..1000u32 {
            let yaw = (t as f32 * 0.03).sin();
            push_human(&mut s, 1, shinobi_proto::intent::PlayerIntent {
                move_z: 90,
                move_x: ((t / 40) % 2) as i8 * 120 - 60,
                aim_yaw: q_yaw(yaw),
                ..idle(yaw)
            });
            step(&mut h, &mut s);
            let mut acc = 0xcbf29ce484222325u64;
            for id in [1u16, 201, 202, 203] {
                for c in pos(&s, id) {
                    acc = (acc ^ c.to_bits() as u64).wrapping_mul(0x100000001b3);
                }
            }
            hashes.push(acc);
        }
        hashes
    }
    let (a, b) = (run(), run());
    assert_eq!(a, b, "bot runs diverged");
    assert!(a.windows(2).any(|w| w[0] != w[1]), "world was static: test proves nothing");
}

#[test]
fn advance_matches_one_step_per_tick_and_merges_events() {
    let mut s = flat_server(vec![[0.0, 0.81, 0.0]]);
    s.add_player(1, [12.0, 0.81, 0.0]);
    let mut h = host(1);
    spawn_bot(&mut h, &mut s, 201, [0.0, 0.81, 0.0], Archetype::Heavy, BotSkill::Normal);
    let frame = std::time::Duration::from_nanos(1_000_000_000 / 144);
    let mut ticks = 0u32;
    let mut saw_combat = false;
    for _ in 0..144 * 6 {
        let r = h.advance(&mut s, frame);
        ticks += r.ticks;
        saw_combat |= !s.events.is_empty();
    }
    assert_eq!(s.tick, ticks, "server tick must equal the steps the host ran");
    assert!((ticks as i32 - 360).abs() <= 1, "ticks {ticks}");
    assert!(saw_combat, "merged combat events never surfaced");
}

#[test]
fn five_minute_ffa_soak_earns_zero_anticheat_strikes_and_no_kicks() {
    let mut s = flat_server(vec![[0.0, 0.81, 10.0], [8.0, 0.81, -10.0], [-8.0, 0.81, 6.0], [14.0, 0.81, 14.0]]);
    s.add_player(1, [0.0, 0.81, 0.0]);
    let mut h = host(5);
    h.add_bot(&mut s, 201, Archetype::Heavy, BotSkill::Normal);
    h.add_bot(&mut s, 202, Archetype::Light, BotSkill::Hard);
    h.add_bot(&mut s, 203, Archetype::Light, BotSkill::Easy);
    let (mut kicked, mut worst) = (0, 0);
    for t in 0..18_000u32 {
        // scripted human: wanders and sweeps its aim at a legal rate
        let yaw = (t as f32 * 0.05).sin() * 2.0;
        push_human(&mut s, 1, shinobi_proto::intent::PlayerIntent { move_z: 100, move_x: ((t / 50) % 3) as i8 * 60 - 60, ..idle(yaw) });
        kicked += step(&mut h, &mut s).kicked.len();
        for p in s.players.iter().filter(|p| p.id >= 200) {
            worst = worst.max(p.guard.strikes);
        }
    }
    assert_eq!(kicked, 0);
    assert_eq!(worst, 0, "bots accumulated anti-cheat strikes");
    assert_eq!(h.bot_count(), 3);
}


#[test]
fn bots_never_target_or_hurt_teammates_and_fill_both_sides_to_the_team_size() {
    use shinobi_sim::team::{TEAM_CYAN, TEAM_ORANGE};
    let mut s = common::flat_server(vec![[-20.0, 0.81, 0.0], [20.0, 0.81, 0.0]]);
    let mut h = common::host(7);
    s.add_player(1, [0.0, 0.81, 0.0]); // a human, balanced onto Cyan
    let human_team = s.players[0].team;
    let (added, removed) = h.fill_teams(&mut s, 4, 201, Archetype::Light, BotSkill::Normal);
    assert_eq!(added.len(), 7, "4 per side minus the human");
    assert!(removed.is_empty());
    assert_eq!((s.team_size(TEAM_CYAN), s.team_size(TEAM_ORANGE)), (4, 4));
    // a second human on the human's side pushes one bot of that team out
    s.add_player_on(2, [5.0, 0.81, 0.0], human_team);
    let (_, removed) = h.fill_teams(&mut s, 4, 201, Archetype::Light, BotSkill::Normal);
    assert_eq!(removed.len(), 1);
    assert_eq!((s.team_size(TEAM_CYAN), s.team_size(TEAM_ORANGE)), (4, 4));

    let (mut friendly, mut foe) = (0.0f32, 0.0f32);
    for _ in 0..1800 {
        common::step(&mut h, &mut s);
        for e in &s.pvp_events {
            if let shinobi_sim::pvp::PvpEvent::Hit { attacker, victim, amount, .. } = *e {
                let t = |id| s.players.iter().find(|p| p.id == id).map(|p| p.team);
                if t(attacker).is_some() && t(attacker) == t(victim) {
                    friendly += amount;
                } else {
                    foe += amount;
                }
            }
        }
    }
    assert!(foe > 0.0, "the sides actually fought");
    assert_eq!(friendly, 0.0, "no friendly damage in 30 s of bot fighting");
}

#[test]
fn bots_get_past_a_player_built_block_wall_and_still_kill() {
    use shinobi_sim::blocks::Blocks;
    for arch in [Archetype::Heavy, Archetype::Light] {
        let mut s = flat_server(vec![[-14.0, 0.81, 0.0]]);
        s.add_player(1, [14.0, 0.81, 0.0]); // dummy human on the far side
        // a 3-high, 12-wide wall of blocks across the middle (z -6..6): going around is ~7 m, over needs a jump
        let mut b = Blocks::default();
        for z in -6..6 {
            for y in 0..3 {
                b.place(&mut s.world, 1, [0, y, z], &[]).unwrap();
            }
        }
        s.blocks = b;
        let mut h = host(5);
        spawn_bot(&mut h, &mut s, 201, [-14.0, 0.81, 0.0], arch, BotSkill::Normal);
        let mut dealt = 0.0f32;
        for _ in 0..1800 {
            step(&mut h, &mut s);
            for e in &s.pvp_events {
                if let PvpEvent::Hit { attacker: 201, victim: 1, amount, .. } = e {
                    dealt += amount;
                }
            }
        }
        assert!(dealt > 0.0, "{arch:?} bot never hurt the dummy behind the block wall (phase {})", phase_name(&h, 201));
    }
}
