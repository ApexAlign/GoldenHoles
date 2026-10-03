//! Balance review harness (not a regression test): scripted human duelist vs bots of each skill on
//! `levels/arena.lvl` with `Balance::demo()` and the replicator's light-wall trails.
//!   cargo test --offline -p shinobi-ai --test balance_review -- --ignored --nocapture
//! Prints win rates, K/D, kill-time, death causes, shot accuracy and bot idle/stuck numbers.
mod common;
use std::f32::consts::{PI, TAU};

use common::*;
use shinobi_ai::{Archetype, BotSkill};
use shinobi_proto::intent::{Buttons, PlayerIntent, PITCH_LEVEL};
use shinobi_proto::quant::{dq_yaw, q_yaw};
use shinobi_sim::balance::Balance;
use shinobi_sim::combat::powerup::PowerupKind;
use shinobi_sim::combat::{CombatEvent, DamageKind};
use shinobi_sim::level::parse_level_file;
use shinobi_sim::match_state::PhaseKind;
use shinobi_sim::pvp::{HazardSource, PvpEvent, VOID_SOURCE_ID};
use shinobi_sim::tick::Server;

const HUMAN: u16 = 1;
const TURN: f32 = 12.0 * PI / 180.0;
const LIVE_TICKS: u32 = 3600;

fn arena() -> Server {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../levels/arena.lvl");
    let f = parse_level_file(&std::fs::read_to_string(path).expect("levels/arena.lvl")).expect("parses");
    let mut s = Server::new(shinobi_sim::character::Cfg::default());
    f.level.apply_to_server(&mut s);
    s.set_spawns(f.level.spawns.clone());
    s.set_balance(Balance::demo()).unwrap();
    for (i, p) in f.pickups.iter().enumerate() {
        s.add_pickup(i as u32 + 1, p.pos, PowerupKind::GridBeam, p.respawn_secs);
    }
    // one long continuous match: no intermissions in the statistics
    s.match_state.cfg.frag_limit = u16::MAX;
    s.match_state.cfg.time_limit_ticks = u32::MAX / 2;
    s
}

/// A human-ish duelist: faces the nearest living opponent with aim noise, closes in, strafes, throws at range,
/// cuts when near, only when roughly lined up. `noise` = max aim error in radians (re-drawn every 8 ticks).
struct Human {
    noise: f32,
    rng: u64,
    err: f32,
}

impl Human {
    fn new(noise: f32, seed: u64) -> Self {
        Self { noise, rng: seed | 1, err: 0.0 }
    }
    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }

    fn intent(&mut self, s: &Server, tick: u32) -> PlayerIntent {
        let Some(a) = s.players.iter().find(|p| p.id == HUMAN) else { return idle(0.0) };
        let hold = PlayerIntent { aim_yaw: a.yaw, aim_pitch: PITCH_LEVEL, ..Default::default() };
        let pa = a.pos(&s.world);
        let d = |p: &shinobi_sim::tick::Player| {
            let q = p.pos(&s.world);
            ((q[0] - pa[0]).powi(2) + (q[2] - pa[2]).powi(2)).sqrt()
        };
        let Some(b) = s.players.iter().filter(|p| p.id != HUMAN && p.vitals.alive()).min_by(|x, y| d(x).total_cmp(&d(y))) else {
            return hold;
        };
        if !a.vitals.alive() {
            return hold;
        }
        if tick % 8 == 0 {
            self.err = self.rand() * self.noise;
        }
        let pb = b.pos(&s.world);
        let (dx, dz) = (pb[0] - pa[0], pb[2] - pa[2]);
        let dist = (dx * dx + dz * dz).sqrt();
        let want = (-dx).atan2(-dz) + self.err;
        let cur = dq_yaw(a.yaw);
        let delta = (want - cur + PI).rem_euclid(TAU) - PI;
        let step = delta.clamp(-TURN, TURN);
        let aligned = delta.abs() < 0.2;
        let strafe = if (tick / 50) % 2 == 0 { 90 } else { -90 };
        let mut buttons = 0u8;
        if aligned && dist < 5.0 && tick % 45 == 13 {
            buttons |= Buttons::SLICE;
        }
        if aligned && dist > 2.0 && dist < 25.0 && tick % 18 == 7 {
            buttons |= Buttons::FIRE;
        }
        PlayerIntent {
            buttons: Buttons(buttons),
            move_x: if dist < 6.0 { strafe } else { 0 },
            move_z: if dist > 3.0 { 127 } else { 0 },
            aim_yaw: q_yaw(cur + step),
            aim_pitch: PITCH_LEVEL,
            ..Default::default()
        }
    }
}

#[derive(Default, Clone)]
struct Stats {
    kills: [u32; 2], // [human, bots]
    deaths: [u32; 2],
    wall_deaths: [u32; 2],
    obstacle_deaths: [u32; 2],
    void_deaths: [u32; 2],
    shots: [u32; 2],
    shot_hits: [u32; 2],
    slices: [u32; 2],
    slice_hits: [u32; 2],
    first_blood_to_death: Vec<f32>, // seconds, victims of weapon kills
    life_secs: Vec<f32>,            // alive time per death
    bot_alive_ticks: u32,
    bot_idle_ticks: u32,
    bot_stuck_windows: u32,
    bot_windows: u32,
    own_wall_hits: u32,
    dmg_wall: [f32; 2],   // damage TAKEN by [human, bots] from walls
    dmg_weapon: [f32; 2], // ... and from weapons
}

fn side(id: u16) -> usize {
    (id >= 200) as usize
}

fn run(skill: BotSkill, arch: Archetype, noise: f32, seed: u64, n_bots: u16) -> Stats {
    let mut s = arena();
    let mut h = host(seed);
    let at = s.pick_spawn(None);
    s.add_player(HUMAN, at);
    for i in 0..n_bots {
        assert!(h.add_bot(&mut s, 201 + i, arch, skill));
    }
    let mut human = Human::new(noise, seed ^ 0xBEEF);
    let mut st = Stats::default();
    let mut live = 0u32;
    let mut first_hit: std::collections::BTreeMap<u16, u32> = Default::default();
    let mut born: std::collections::BTreeMap<u16, u32> = Default::default();
    let mut trail: std::collections::BTreeMap<u16, Vec<[f32; 3]>> = Default::default();
    let mut prev: std::collections::BTreeMap<u16, [f32; 3]> = Default::default();
    for tick in 0..(LIVE_TICKS + 1200) {
        let it = human.intent(&s, tick);
        push_human(&mut s, HUMAN, it);
        step(&mut h, &mut s);
        let t = s.tick;
        let is_live = s.match_state.kind() == PhaseKind::Live;
        if is_live {
            live += 1;
        }
        for (_, e) in &s.events {
            match e {
                CombatEvent::ShurikenThrown { .. } => {}
                _ => {}
            }
        }
        for (pid, e) in &s.events {
            match e {
                CombatEvent::ShurikenThrown { .. } => st.shots[side(*pid)] += 1,
                CombatEvent::Slash { .. } => st.slices[side(*pid)] += 1,
                _ => {}
            }
        }
        for e in &s.pvp_events {
            match *e {
                PvpEvent::HazardHit { victim, amount, source: HazardSource::Wall { .. }, .. } if is_live => {
                    st.dmg_wall[side(victim)] += amount;
                }
                PvpEvent::Hit { attacker, victim, kind, amount, .. } if is_live => {
                    st.dmg_weapon[side(victim)] += amount;
                    first_hit.entry(victim).or_insert(t);
                    match kind {
                        DamageKind::Shuriken => st.shot_hits[side(attacker)] += 1,
                        DamageKind::Slash => st.slice_hits[side(attacker)] += 1,
                        _ => {}
                    }
                }
                PvpEvent::Kill { killer, victim, .. } if is_live => {
                    st.kills[side(killer)] += 1;
                    st.deaths[side(victim)] += 1;
                    if let Some(f) = first_hit.remove(&victim) {
                        st.first_blood_to_death.push(t.saturating_sub(f) as f32 / 60.0);
                    }
                    st.life_secs.push(t.saturating_sub(*born.get(&victim).unwrap_or(&0)) as f32 / 60.0);
                    born.insert(victim, t + 180);
                }
                PvpEvent::HazardKill { victim, source, killer, .. } if is_live => {
                    st.deaths[side(victim)] += 1;
                    match source {
                        HazardSource::Wall { owner, .. } => {
                            st.wall_deaths[side(victim)] += 1;
                            if owner as u16 == victim {
                                st.own_wall_hits += 1;
                            }
                        }
                        HazardSource::Obstacle(id) if id == VOID_SOURCE_ID => st.void_deaths[side(victim)] += 1,
                        HazardSource::Obstacle(_) => st.obstacle_deaths[side(victim)] += 1,
                    }
                    if let Some(k) = killer {
                        st.kills[side(k)] += 1;
                    }
                    st.life_secs.push(t.saturating_sub(*born.get(&victim).unwrap_or(&0)) as f32 / 60.0);
                    born.insert(victim, t + 180);
                    first_hit.remove(&victim);
                }
                PvpEvent::Respawn { player, .. } => {
                    first_hit.remove(&player);
                }
                _ => {}
            }
        }
        // bot idleness / stuck windows (only while alive and an enemy lives)
        if is_live {
            for p in &s.players {
                if p.id < 200 || !p.vitals.alive() {
                    continue;
                }
                let pos = p.pos(&s.world);
                st.bot_alive_ticks += 1;
                if let Some(q) = prev.get(&p.id) {
                    let v = ((pos[0] - q[0]).powi(2) + (pos[2] - q[2]).powi(2)).sqrt() * 60.0;
                    let ph = h.bot(p.id).map_or("gone", |b| b.phase.name());
                    if v < 0.5 && matches!(ph, "advance" | "flank" | "zone") {
                        st.bot_idle_ticks += 1;
                    }
                }
                prev.insert(p.id, pos);
                let w = trail.entry(p.id).or_default();
                w.push(pos);
                if w.len() > 300 {
                    let old = w[w.len() - 301];
                    let moved = ((pos[0] - old[0]).powi(2) + (pos[2] - old[2]).powi(2)).sqrt();
                    if tick % 300 == 0 {
                        st.bot_windows += 1;
                        if moved < 1.5 {
                            st.bot_stuck_windows += 1;
                        }
                    }
                }
            }
        }
        if live >= LIVE_TICKS {
            break;
        }
    }
    st
}

fn pct(a: u32, b: u32) -> f32 {
    if b == 0 { 0.0 } else { 100.0 * a as f32 / b as f32 }
}

fn mean(v: &[f32]) -> f32 {
    if v.is_empty() { 0.0 } else { v.iter().sum::<f32>() / v.len() as f32 }
}

#[test]
#[ignore = "balance review: run with --ignored --nocapture"]
fn human_vs_each_bot_skill_on_the_arena() {
    let skills = [("easy", BotSkill::Easy), ("normal", BotSkill::Normal), ("hard", BotSkill::Hard)];
    let noises = [("aimbot 0.00", 0.0f32), ("decent 0.06", 0.06), ("sloppy 0.15", 0.15)];
    eprintln!("\n=== 1v1, 60 s live, arena.lvl, Balance::demo(); 10 runs per cell (5 seeds x light/heavy) ===");
    eprintln!("{:<7} {:<12} {:>6} {:>10} {:>9} {:>9} {:>8} {:>10} {:>9} {:>10} {:>10} {:>7} {:>8}", "skill", "human aim", "win%", "K/D(h:b)", "ttk(s)", "life(s)", "wall%", "wallDmg%hb", "acc h/b%", "slices h/b", "slHit h/b%", "idle%", "stuck%");
    for (sn, skill) in skills {
        for (nn, noise) in noises {
            let (mut agg, mut wins, mut runs) = (Stats::default(), 0u32, 0u32);
            for seed in 0..5u64 {
                for arch in [Archetype::Light, Archetype::Heavy] {
                    let r = run(skill, arch, noise, 0x1000 + seed, 1);
                    runs += 1;
                    if r.kills[0] > r.kills[1] {
                        wins += 1;
                    }
                    for i in 0..2 {
                        agg.kills[i] += r.kills[i];
                        agg.deaths[i] += r.deaths[i];
                        agg.wall_deaths[i] += r.wall_deaths[i];
                        agg.void_deaths[i] += r.void_deaths[i];
                        agg.obstacle_deaths[i] += r.obstacle_deaths[i];
                        agg.shots[i] += r.shots[i];
                        agg.shot_hits[i] += r.shot_hits[i];
                        agg.slices[i] += r.slices[i];
                        agg.slice_hits[i] += r.slice_hits[i];
                        agg.dmg_wall[i] += r.dmg_wall[i];
                        agg.dmg_weapon[i] += r.dmg_weapon[i];
                    }
                    agg.first_blood_to_death.extend(r.first_blood_to_death);
                    for i in 0..2 {
                        agg.wall_deaths[i] += 0;
                    }
                    agg.life_secs.extend(r.life_secs);
                    agg.bot_alive_ticks += r.bot_alive_ticks;
                    agg.bot_idle_ticks += r.bot_idle_ticks;
                    agg.bot_windows += r.bot_windows;
                    agg.bot_stuck_windows += r.bot_stuck_windows;
                    agg.own_wall_hits += r.own_wall_hits;
                }
            }
            let deaths = agg.deaths[0] + agg.deaths[1];
            let walls = agg.wall_deaths[0] + agg.wall_deaths[1];
            eprintln!(
                "{:<7} {:<12} {:>5.0}% {:>4}:{:<5} {:>9.2} {:>9.1} {:>7.0}% {:>4.0}/{:<5.0} {:>4.0}/{:<4.0} {:>4}/{:<5} {:>4.0}/{:<5.0} {:>6.1}% {:>7.1}%",
                sn,
                nn,
                pct(wins, runs),
                agg.kills[0],
                agg.kills[1],
                mean(&agg.first_blood_to_death),
                mean(&agg.life_secs),
                pct(walls, deaths),
                100.0 * agg.dmg_wall[0] / (agg.dmg_wall[0] + agg.dmg_weapon[0]).max(1.0),
                100.0 * agg.dmg_wall[1] / (agg.dmg_wall[1] + agg.dmg_weapon[1]).max(1.0),
                pct(agg.shot_hits[0], agg.shots[0]),
                pct(agg.shot_hits[1], agg.shots[1]),
                agg.slices[0],
                agg.slices[1],
                pct(agg.slice_hits[0], agg.slices[0]),
                pct(agg.slice_hits[1], agg.slices[1]),
                pct(agg.bot_idle_ticks, agg.bot_alive_ticks),
                pct(agg.bot_stuck_windows, agg.bot_windows),
            );
        }
    }

    eprintln!("\n=== free-for-all: human (decent 0.06) + 3 bots of one skill, 60 s live, 5 seeds ===");
    for (sn, skill) in skills {
        let mut agg = Stats::default();
        for seed in 0..5u64 {
            let r = run(skill, if seed % 2 == 0 { Archetype::Light } else { Archetype::Heavy }, 0.06, 0x2000 + seed, 3);
            for i in 0..2 {
                agg.kills[i] += r.kills[i];
                agg.deaths[i] += r.deaths[i];
                agg.wall_deaths[i] += r.wall_deaths[i];
                agg.dmg_wall[i] += r.dmg_wall[i];
                agg.dmg_weapon[i] += r.dmg_weapon[i];
                agg.shots[i] += r.shots[i];
                agg.slices[i] += r.slices[i];
            }
            agg.bot_alive_ticks += r.bot_alive_ticks;
            agg.bot_idle_ticks += r.bot_idle_ticks;
            agg.bot_windows += r.bot_windows;
            agg.bot_stuck_windows += r.bot_stuck_windows;
        }
        eprintln!(
            "{sn:<7} human {}K/{}D   bots (3x5 runs) {}K/{}D   wall-death share {:.0}% (human {}/{} deaths, bots {}/{})   wall dmg share human {:.0}% bots {:.0}%   shots h/b {}/{} slices h/b {}/{}   bot idle {:.1}%  stuck windows {:.1}%",
            agg.kills[0], agg.deaths[0], agg.kills[1], agg.deaths[1],
            pct(agg.wall_deaths[0] + agg.wall_deaths[1], agg.deaths[0] + agg.deaths[1]),
            agg.wall_deaths[0], agg.deaths[0], agg.wall_deaths[1], agg.deaths[1],
            100.0 * agg.dmg_wall[0] / (agg.dmg_wall[0] + agg.dmg_weapon[0]).max(1.0),
            100.0 * agg.dmg_wall[1] / (agg.dmg_wall[1] + agg.dmg_weapon[1]).max(1.0),
            agg.shots[0], agg.shots[1], agg.slices[0], agg.slices[1],
            pct(agg.bot_idle_ticks, agg.bot_alive_ticks),
            pct(agg.bot_stuck_windows, agg.bot_windows),
        );
    }
}
