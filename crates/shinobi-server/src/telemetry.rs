//! Playtest telemetry: per-player combat / movement / building statistics for tuning, printed to the server log as
//! `TELEMETRY ...` lines every 60 s and when a match ends (and tee'd to a file with `--telemetry PATH`).
//!
//! Everything is computed on the server from events it already has, so there is no wire traffic and the numbers are
//! the authoritative ones. Per match: stats reset at `MatchStart`, the FINAL report is at `MatchEnd`.
//!
//! Definitions (read these before trusting a percentage):
//! * shuriken hit % = shuriken hits dealt / shurikens thrown. A hit is credited when it lands (a disc flies up to a
//!   second or so), so over a short window the ratio can exceed what the throws of that window earned.
//! * headshot % = headshots / shuriken hits; a headshot is a hit worth at least the midpoint of body and head damage.
//! * "locked" = the thrower's lock-on was fully locked (`LockPhase::Locked`) when the disc left the hand (throws) or
//!   fully locked on the victim when it landed (hits). Hit % locked / unlocked divide the matching counts.
//! * lock-on "engaged" = times the lock reached Locked; time locked is out of time alive.
//! * grapple fired = attaches + misses. Pieces lost = the player's own pieces that de-rezzed (anyone's damage).
//! * deaths by cause: shuriken / katana / grid beam / void (kill plane) / hazard (obstacle).
//! * "headroom" = ticks by which the newest intent beat the server clock (the client's adaptive intent lead is
//!   sized from it); late = share of reports where it arrived after its tick.
//! * "client: ping / corrections" are measured by the game client (round trip from the server's echo, reconcile
//!   corrections applied) and sent to the server about once a second (`ClientReport`); bots have no client.
use std::collections::BTreeMap;

use shinobi_sim::blocks::BlockEvent;
use shinobi_sim::combat::aim_assist::LockPhase;
use shinobi_sim::combat::{CombatEvent, DamageKind};
use shinobi_sim::match_state::MatchEvent;
use shinobi_sim::pvp::{PvpEvent, VOID};
use shinobi_sim::tick::Server;

use crate::config::BOT_ID_BASE;

const TICK_HZ: f64 = shinobi_proto::TICK_HZ as f64;
/// Periodic report cadence.
const REPORT_EVERY_TICKS: u32 = 60 * shinobi_proto::TICK_HZ;

#[derive(Clone, Debug, Default)]
pub struct PlayerStats {
    pub thrown: u32,
    pub thrown_locked: u32,
    pub shuriken_hits: u32,
    pub shuriken_hits_locked: u32,
    pub headshots: u32,
    pub swings: u32,
    pub katana_hits: u32,
    pub grapple_attach: u32,
    pub grapple_miss: u32,
    pub built: u32,
    pub lost: u32,
    pub kills: u32,
    /// shuriken, katana, grid beam, void, hazard
    pub deaths: [u32; 5],
    pub life_sum_ticks: u64,
    pub lives: u32,
    alive_since: Option<u32>,
    pub alive_ticks: u64,
    pub locked_ticks: u64,
    pub lock_engaged: u32,
    was_locked: bool,
    pub margin_sum: i64,
    pub margin_n: u32,
    pub margin_min: i32,
    pub late: u32,
    /// Client-reported round trip (ms) samples, about one per second.
    pub ping_sum: u64,
    pub ping_n: u32,
    pub ping_max: u16,
    /// Client-reported reconcile corrections (counter modulo 65536): first and latest value this match.
    corr_first: Option<u16>,
    corr_last: u16,
    report_tick: Option<u32>,
    /// Server-side input trouble this match: intents rejected as too far ahead, ticks synthesised for lost / late input.
    pub rejected_future: u32,
    pub repeated_ticks: u32,
    queue_base: Option<(u32, u32)>,
}

impl PlayerStats {
    /// Reconcile corrections the client applied during this match.
    pub fn corrections(&self) -> u32 {
        self.corr_first.map_or(0, |f| self.corr_last.wrapping_sub(f) as u32)
    }

    pub fn deaths_total(&self) -> u32 {
        self.deaths.iter().sum()
    }

    fn add(&mut self, o: &PlayerStats) {
        self.thrown += o.thrown;
        self.thrown_locked += o.thrown_locked;
        self.shuriken_hits += o.shuriken_hits;
        self.shuriken_hits_locked += o.shuriken_hits_locked;
        self.headshots += o.headshots;
        self.swings += o.swings;
        self.katana_hits += o.katana_hits;
        self.grapple_attach += o.grapple_attach;
        self.grapple_miss += o.grapple_miss;
        self.built += o.built;
        self.lost += o.lost;
        self.kills += o.kills;
        for i in 0..5 {
            self.deaths[i] += o.deaths[i];
        }
        self.life_sum_ticks += o.life_sum_ticks;
        self.lives += o.lives;
        self.alive_ticks += o.alive_ticks;
        self.locked_ticks += o.locked_ticks;
        self.lock_engaged += o.lock_engaged;
        self.margin_sum += o.margin_sum;
        self.margin_n += o.margin_n;
        self.margin_min = if self.margin_n == o.margin_n { o.margin_min } else { self.margin_min.min(o.margin_min) };
        self.late += o.late;
        self.rejected_future += o.rejected_future;
        self.repeated_ticks += o.repeated_ticks;
        self.ping_sum += o.ping_sum;
        self.ping_n += o.ping_n;
        self.ping_max = self.ping_max.max(o.ping_max);
        self.corr_last = self.corr_last.wrapping_add(o.corrections() as u16);
        self.corr_first.get_or_insert(0);
    }
}

#[derive(Default)]
pub struct Telemetry {
    stats: BTreeMap<u16, PlayerStats>,
    match_no: u32,
    last_tick: Option<u32>,
    next_report: u32,
    match_start: u32,
    text: Vec<String>,
    json: Vec<String>,
}

fn pct(a: u32, b: u32) -> String {
    if b == 0 {
        "-".into()
    } else {
        format!("{:.1}%", a as f64 * 100.0 / b as f64)
    }
}

impl Telemetry {
    fn stat(&mut self, id: u16) -> &mut PlayerStats {
        self.stats.entry(id).or_insert_with(|| PlayerStats { margin_min: i32::MAX, ..Default::default() })
    }

    #[cfg(test)]
    pub fn stats(&self, id: u16) -> Option<&PlayerStats> {
        self.stats.get(&id)
    }

    #[cfg(test)]
    pub fn match_number(&self) -> u32 {
        self.match_no
    }

    /// Server -> telemetry: the smallest intent headroom the server measured for `id` (see `Replicator::post_frame`).
    pub fn observe_margin(&mut self, id: u16, margin: i32) {
        let s = self.stat(id);
        s.margin_sum += margin as i64;
        s.margin_n += 1;
        s.margin_min = s.margin_min.min(margin);
        if margin < 0 {
            s.late += 1;
        }
    }

    /// Text lines (printed to the console) and JSON lines (file only) produced since the last call.
    pub fn take(&mut self) -> (Vec<String>, Vec<String>) {
        (std::mem::take(&mut self.text), std::mem::take(&mut self.json))
    }

    /// Call once per server frame BEFORE the replicator drains the event vectors.
    pub fn observe(&mut self, server: &Server) {
        let now = server.tick;
        let dt = now.saturating_sub(self.last_tick.unwrap_or(now)).min(10) as u64;
        self.last_tick = Some(now);
        if server.match_events.iter().any(|e| matches!(e, MatchEvent::MatchStart { .. })) {
            self.stats.clear();
            self.match_no += 1;
            self.match_start = now;
            self.next_report = now + REPORT_EVERY_TICKS;
        }
        let balance = &server.balance.combat;
        let head_threshold = balance.shuriken.damage * (1.0 + balance.headshot_mult.max(1.0)) / 2.0;
        // gauges first: lock state / alive time (this frame's events belong to the state at the end of the frame)
        for p in &server.players {
            let alive = p.vitals.alive();
            let locked = alive && p.combat.lock.phase == LockPhase::Locked;
            let s = self.stat(p.id);
            if alive {
                s.alive_ticks += dt;
                s.alive_since.get_or_insert(now);
            }
            if locked {
                s.locked_ticks += dt;
                if !s.was_locked {
                    s.lock_engaged += 1;
                }
            }
            s.was_locked = locked;
            let q = (p.queue.rejected_future, p.queue.repeated_ticks);
            let base = *s.queue_base.get_or_insert(q);
            s.rejected_future = q.0.saturating_sub(base.0);
            s.repeated_ticks = q.1.saturating_sub(base.1);
            // the client's own ping / correction report, once per report
            if let Some((r, at)) = p.client_report {
                if s.report_tick != Some(at) {
                    s.report_tick = Some(at);
                    if r.ping_ms != shinobi_proto::intent::PING_UNKNOWN {
                        s.ping_sum += r.ping_ms as u64;
                        s.ping_n += 1;
                        s.ping_max = s.ping_max.max(r.ping_ms);
                    }
                    s.corr_first.get_or_insert(r.corrections);
                    s.corr_last = r.corrections;
                }
            }
        }
        let locked_on = |server: &Server, who: u16, victim: Option<u16>| {
            server.players.iter().find(|p| p.id == who).map_or(false, |p| {
                p.combat.lock.phase == LockPhase::Locked && victim.map_or(true, |v| p.combat.lock.target_id == v as u128)
            })
        };
        for (id, ev) in &server.events {
            match ev {
                CombatEvent::ShurikenThrown { .. } => {
                    let locked = locked_on(server, *id, None);
                    let s = self.stat(*id);
                    s.thrown += 1;
                    s.thrown_locked += locked as u32;
                }
                CombatEvent::Slash { .. } => self.stat(*id).swings += 1,
                CombatEvent::GrappleAttach { .. } => self.stat(*id).grapple_attach += 1,
                CombatEvent::GrappleMiss { .. } => self.stat(*id).grapple_miss += 1,
                _ => {}
            }
        }
        for ev in &server.pvp_events {
            match *ev {
                PvpEvent::Hit { attacker, victim, amount, kind, .. } => match kind {
                    DamageKind::Shuriken => {
                        let locked = locked_on(server, attacker, Some(victim));
                        let s = self.stat(attacker);
                        s.shuriken_hits += 1;
                        s.shuriken_hits_locked += locked as u32;
                        s.headshots += (amount >= head_threshold && head_threshold > balance.shuriken.damage) as u32;
                    }
                    DamageKind::Slash => self.stat(attacker).katana_hits += 1,
                    DamageKind::GridBeam | DamageKind::Explosive | DamageKind::Arrow | DamageKind::Tag => {}
                },
                PvpEvent::Kill { killer, victim, kind, .. } => {
                    self.stat(killer).kills += 1;
                    let cause = match kind {
                        DamageKind::Shuriken => 0,
                        DamageKind::Slash => 1,
                        DamageKind::GridBeam => 2,
                        DamageKind::Explosive | DamageKind::Arrow | DamageKind::Tag => 0,
                    };
                    self.died(victim, cause, now);
                }
                PvpEvent::HazardKill { victim, source, killer, .. } => {
                    if let Some(k) = killer {
                        self.stat(k).kills += 1;
                    }
                    self.died(victim, if source == VOID { 3 } else { 4 }, now);
                }
                PvpEvent::Respawn { player, .. } => {
                    self.stat(player).alive_since = Some(now);
                }
                PvpEvent::HazardHit { .. } | PvpEvent::EnemyHazardHit { .. } => {}
            }
        }
        for ev in &server.block_events {
            match *ev {
                BlockEvent::Placed { owner, .. } => self.stat(owner).built += 1,
                BlockEvent::DeRez { owner, .. } => self.stat(owner).lost += 1,
                _ => {}
            }
        }
        if server.match_events.iter().any(|e| matches!(e, MatchEvent::MatchEnd { .. })) {
            self.report(now, true);
        } else if now >= self.next_report && self.next_report != 0 {
            self.next_report = now + REPORT_EVERY_TICKS;
            self.report(now, false);
        }
    }

    fn died(&mut self, victim: u16, cause: usize, now: u32) {
        let s = self.stat(victim);
        s.deaths[cause] += 1;
        if let Some(since) = s.alive_since.take() {
            s.life_sum_ticks += now.saturating_sub(since) as u64;
            s.lives += 1;
        }
    }

    fn report(&mut self, now: u32, last: bool) {
        let secs = now.saturating_sub(self.match_start) as f64 / TICK_HZ;
        let humans: Vec<u16> = self.stats.keys().copied().filter(|id| *id < BOT_ID_BASE).collect();
        let bots: Vec<u16> = self.stats.keys().copied().filter(|id| *id >= BOT_ID_BASE).collect();
        if humans.is_empty() && bots.is_empty() {
            return;
        }
        let head = format!(
            "TELEMETRY ===== match {} {} at {}:{:02} | {} human(s), {} bot(s)",
            self.match_no,
            if last { "FINAL" } else { "so far" },
            secs as u32 / 60,
            secs as u32 % 60,
            humans.len(),
            bots.len()
        );
        self.text.push(head);
        for id in &humans {
            let s = self.stats[id].clone();
            self.player_lines(&format!("p{id} (human)"), &s, true);
            self.json.push(Self::json_line(self.match_no, last, secs, &format!("p{id}"), true, &s));
        }
        if !bots.is_empty() {
            let mut all = PlayerStats { margin_min: i32::MAX, ..Default::default() };
            for id in &bots {
                all.add(&self.stats[id]);
            }
            self.player_lines(&format!("bots x{} (summed)", bots.len()), &all, false); // bots have no network link
            self.json.push(Self::json_line(self.match_no, last, secs, "bots", false, &all));
        }
    }

    fn player_lines(&mut self, who: &str, s: &PlayerStats, net: bool) {
        let life = if s.lives > 0 { format!("{:.1}s", s.life_sum_ticks as f64 / TICK_HZ / s.lives as f64) } else { "-".into() };
        let d = s.deaths;
        self.text.push(format!(
            "TELEMETRY {who}: kills {} deaths {} (shuriken {}, katana {}, beam {}, void {}, hazard {}) avg life {life}",
            s.kills,
            s.deaths_total(),
            d[0],
            d[1],
            d[2],
            d[3],
            d[4]
        ));
        self.text.push(format!(
            "TELEMETRY    shuriken: thrown {} hit {} ({}) headshots {} ({} of hits) | locked: thrown {} hit {} ({}) | unlocked: thrown {} hit {} ({})",
            s.thrown,
            s.shuriken_hits,
            pct(s.shuriken_hits, s.thrown),
            s.headshots,
            pct(s.headshots, s.shuriken_hits),
            s.thrown_locked,
            s.shuriken_hits_locked,
            pct(s.shuriken_hits_locked, s.thrown_locked),
            s.thrown - s.thrown_locked,
            s.shuriken_hits - s.shuriken_hits_locked.min(s.shuriken_hits),
            pct(s.shuriken_hits - s.shuriken_hits_locked.min(s.shuriken_hits), s.thrown - s.thrown_locked)
        ));
        self.text.push(format!(
            "TELEMETRY    lock-on: engaged {} times, {:.1}s locked of {:.1}s alive ({})",
            s.lock_engaged,
            s.locked_ticks as f64 / TICK_HZ,
            s.alive_ticks as f64 / TICK_HZ,
            pct(s.locked_ticks as u32, s.alive_ticks as u32)
        ));
        self.text.push(format!(
            "TELEMETRY    katana: swings {} hits {} ({}) | grapple: fired {} attached {} missed {} ({} attach) | pieces: built {} lost {}",
            s.swings,
            s.katana_hits,
            pct(s.katana_hits, s.swings),
            s.grapple_attach + s.grapple_miss,
            s.grapple_attach,
            s.grapple_miss,
            pct(s.grapple_attach, s.grapple_attach + s.grapple_miss),
            s.built,
            s.lost
        ));
        if net && s.ping_n > 0 {
            self.text.push(format!(
                "TELEMETRY    client: ping avg {:.0} max {} ms, reconcile corrections {} (client-measured)",
                s.ping_sum as f64 / s.ping_n as f64,
                s.ping_max,
                s.corrections()
            ));
        }
        if net && (s.rejected_future > 0 || s.repeated_ticks > 0) {
            self.text.push(format!(
                "TELEMETRY    server input trouble: {} intents rejected as too far ahead, {} ticks synthesised (late or lost input)",
                s.rejected_future, s.repeated_ticks
            ));
        }
        if net && s.margin_n > 0 {
            self.text.push(format!(
                "TELEMETRY    net: intent headroom avg {:.1} min {} ticks, late {} of {} reports",
                s.margin_sum as f64 / s.margin_n as f64,
                s.margin_min,
                s.late,
                s.margin_n
            ));
        }
    }

    fn json_line(match_no: u32, last: bool, secs: f64, who: &str, human: bool, s: &PlayerStats) -> String {
        format!(
            "{{\"match\":{match_no},\"final\":{last},\"secs\":{secs:.1},\"player\":\"{who}\",\"human\":{human},\"kills\":{},\"deaths\":{},\"deaths_by\":{{\"shuriken\":{},\"katana\":{},\"beam\":{},\"void\":{},\"hazard\":{}}},\"avg_life_s\":{:.1},\"thrown\":{},\"shuriken_hits\":{},\"headshots\":{},\"thrown_locked\":{},\"hits_locked\":{},\"lock_engaged\":{},\"locked_s\":{:.1},\"alive_s\":{:.1},\"swings\":{},\"katana_hits\":{},\"grapple_attach\":{},\"grapple_miss\":{},\"built\":{},\"lost\":{},\"rejected_future\":{},\"repeated_ticks\":{},\"ping_avg_ms\":{:.0},\"ping_max_ms\":{},\"corrections\":{},\"headroom_avg\":{:.2},\"headroom_min\":{},\"late\":{},\"reports\":{}}}",
            s.kills,
            s.deaths_total(),
            s.deaths[0],
            s.deaths[1],
            s.deaths[2],
            s.deaths[3],
            s.deaths[4],
            if s.lives > 0 { s.life_sum_ticks as f64 / TICK_HZ / s.lives as f64 } else { 0.0 },
            s.thrown,
            s.shuriken_hits,
            s.headshots,
            s.thrown_locked,
            s.shuriken_hits_locked,
            s.lock_engaged,
            s.locked_ticks as f64 / TICK_HZ,
            s.alive_ticks as f64 / TICK_HZ,
            s.swings,
            s.katana_hits,
            s.grapple_attach,
            s.grapple_miss,
            s.built,
            s.lost,
            s.rejected_future,
            s.repeated_ticks,
            if s.ping_n > 0 { s.ping_sum as f64 / s.ping_n as f64 } else { 0.0 },
            s.ping_max,
            s.corrections(),
            if s.margin_n > 0 { s.margin_sum as f64 / s.margin_n as f64 } else { 0.0 },
            if s.margin_n > 0 { s.margin_min } else { 0 },
            s.late,
            s.margin_n
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shinobi_sim::character::Cfg;
    use shinobi_sim::combat::aim_assist::LockPhase;
    use shinobi_sim::match_state::EndReason;
    use shinobi_sim::rapier3d::prelude::{Point, Vector};

    fn server() -> Server {
        let mut s = Server::new(Cfg::default());
        s.set_balance(shinobi_sim::balance::Balance::demo()).unwrap(); // headshots pay 1.5x
        s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        s.add_player(1, [0.0, 0.81, 0.0]);
        s.add_player(BOT_ID_BASE, [5.0, 0.81, 0.0]);
        s
    }

    fn thrown() -> CombatEvent {
        CombatEvent::ShurikenThrown { pos: Point::origin(), dir: Vector::z(), kind: Default::default() }
    }

    fn slash() -> CombatEvent {
        CombatEvent::Slash { flip: false, dir: Vector::z(), lunge: true, hits: 1, origin: Point::origin(), reach: 2.0, half_width: 1.0, half_height: 1.0, damage: 60.0, kind: shinobi_sim::combat::SlashKind::Light }
    }

    #[test]
    fn counts_every_weapon_lock_grapple_building_death_and_prints_a_final_report() {
        let mut s = server();
        let mut t = Telemetry::default();
        s.tick = 10;
        s.match_events = vec![MatchEvent::MatchStart { tick: 10, map: 0 }];
        t.observe(&s);
        // p1 is fully locked on the bot while it throws two shurikens
        s.tick = 20;
        s.match_events.clear();
        s.players[0].combat.lock.phase = LockPhase::Locked;
        s.players[0].combat.lock.target_id = BOT_ID_BASE as u128;
        let head = s.balance.combat.shuriken.damage * s.balance.combat.headshot_mult;
        s.events = vec![(1, thrown()), (1, thrown()), (1, slash()), (1, CombatEvent::GrappleAttach { origin: Point::origin(), anchor: Point::origin() }), (1, CombatEvent::GrappleMiss { origin: Point::origin(), dir: Vector::z() })];
        s.pvp_events = vec![
            PvpEvent::Hit { tick: 20, attacker: 1, victim: BOT_ID_BASE, amount: s.balance.combat.shuriken.damage, kind: DamageKind::Shuriken, head: false, bonus: false },
            PvpEvent::Hit { tick: 20, attacker: 1, victim: BOT_ID_BASE, amount: head, kind: DamageKind::Shuriken, head: false, bonus: false },
            PvpEvent::Hit { tick: 20, attacker: 1, victim: BOT_ID_BASE, amount: 60.0, kind: DamageKind::Slash, head: false, bonus: false },
            PvpEvent::Kill { tick: 20, killer: 1, victim: BOT_ID_BASE, kind: DamageKind::Slash, head: false },
        ];
        s.block_events = vec![
            BlockEvent::Placed { tick: 20, owner: 1, cell: [0, 0, 0], piece: shinobi_sim::blocks::Piece::FLOOR, scale_q: 16 },
            BlockEvent::DeRez { tick: 20, cell: [0, 0, 0], owner: 1, piece: shinobi_sim::blocks::Piece::FLOOR },
        ];
        t.observe(&s);
        // lock released; two more throws, one unlocked miss; then p1 falls into the void and later respawns
        s.tick = 80;
        s.players[0].combat.lock.phase = LockPhase::Search;
        s.events = vec![(1, thrown()), (1, thrown())];
        s.pvp_events = vec![PvpEvent::HazardKill { tick: 80, victim: 1, source: VOID, killer: None }];
        s.block_events.clear();
        t.observe_margin(1, 6);
        t.observe_margin(1, -1);
        t.observe(&s);
        let p = t.stats(1).unwrap().clone();
        assert_eq!((p.thrown, p.thrown_locked, p.shuriken_hits, p.shuriken_hits_locked, p.headshots), (4, 2, 2, 2, 1));
        assert_eq!((p.swings, p.katana_hits, p.kills), (1, 1, 1));
        assert_eq!((p.grapple_attach, p.grapple_miss, p.built, p.lost), (1, 1, 1, 1));
        assert_eq!(p.deaths, [0, 0, 0, 1, 0], "void death");
        assert_eq!((p.lock_engaged, p.margin_n, p.margin_min, p.late), (1, 2, -1, 1));
        assert!(p.locked_ticks >= 10 && p.alive_ticks > p.locked_ticks, "{p:?}");
        assert_eq!(p.lives, 1, "one life ended; avg life = time alive before the death");
        // the bot died to a katana kill
        assert_eq!(t.stats(BOT_ID_BASE).unwrap().deaths, [0, 1, 0, 0, 0]);
        // match end -> FINAL report with humans first and the bots as one summed row
        s.tick = 100;
        s.events.clear();
        s.pvp_events.clear();
        s.match_events = vec![MatchEvent::MatchEnd { tick: 100, winner: Some(1), reason: EndReason::FragLimit }];
        t.observe(&s);
        let (text, json) = t.take();
        let all = text.join("\n");
        assert!(all.contains("match 1 FINAL") && all.contains("1 human(s), 1 bot(s)"), "{all}");
        assert!(all.contains("p1 (human): kills 1 deaths 1 (shuriken 0, katana 0, beam 0, void 1, hazard 0)"), "{all}");
        assert!(all.contains("thrown 4 hit 2 (50.0%) headshots 1 (50.0% of hits) | locked: thrown 2 hit 2 (100.0%) | unlocked: thrown 2 hit 0 (0.0%)"), "{all}");
        assert!(all.contains("katana: swings 1 hits 1 (100.0%) | grapple: fired 2 attached 1 missed 1 (50.0% attach) | pieces: built 1 lost 1"), "{all}");
        assert!(all.contains("intent headroom avg 2.5 min -1 ticks, late 1 of 2 reports"), "{all}");
        assert!(all.contains("bots x1 (summed): kills 0 deaths 1"), "{all}");
        assert_eq!(json.len(), 2);
        assert!(json[0].starts_with("{\"match\":1,\"final\":true") && json[0].contains("\"player\":\"p1\"") && json[0].contains("\"thrown\":4"), "{}", json[0]);
    }

    /// The client's own report (ping, corrections) reaches the telemetry once per report, not once per frame.
    #[test]
    fn client_reported_ping_and_corrections_are_sampled_once_per_report() {
        use shinobi_proto::intent::ClientReport;
        let mut s = server();
        let mut t = Telemetry::default();
        s.tick = 5;
        s.match_events = vec![MatchEvent::MatchStart { tick: 5, map: 0 }];
        t.observe(&s);
        s.match_events.clear();
        for (tick, ping, corr) in [(70u32, 40u16, 65_534u16), (130, 100, 65_535), (190, 70, 2)] {
            s.tick = tick;
            s.players[0].client_report = Some((ClientReport { ping_ms: ping, corrections: corr }, tick));
            t.observe(&s);
            t.observe(&s); // a second frame with the same report must not double count
        }
        let p = t.stats(1).unwrap();
        assert_eq!((p.ping_n, p.ping_max, p.ping_sum), (3, 100, 210));
        assert_eq!(p.corrections(), 4, "65534 -> 2 wraps: 4 corrections");
        s.tick = 200;
        s.match_events = vec![MatchEvent::MatchEnd { tick: 200, winner: None, reason: EndReason::TimeLimit }];
        t.observe(&s);
        let (text, json) = t.take();
        let all = text.join("\n");
        assert!(all.contains("client: ping avg 70 max 100 ms, reconcile corrections 4"), "{all}");
        assert!(json[0].contains("\"ping_avg_ms\":70") && json[0].contains("\"corrections\":4"), "{}", json[0]);
    }

    #[test]
    fn stats_reset_at_match_start_and_a_progress_report_comes_every_minute() {
        let mut s = server();
        let mut t = Telemetry::default();
        s.tick = 5;
        s.match_events = vec![MatchEvent::MatchStart { tick: 5, map: 0 }];
        t.observe(&s);
        s.match_events.clear();
        s.events = vec![(1, thrown())];
        s.tick = 6;
        t.observe(&s);
        assert_eq!(t.stats(1).unwrap().thrown, 1);
        assert!(t.take().0.is_empty(), "no report before the minute is up");
        s.events.clear();
        s.tick = 5 + REPORT_EVERY_TICKS;
        t.observe(&s);
        let (text, _) = t.take();
        assert!(text[0].contains("match 1 so far at 1:00"), "{text:?}");
        // the next match starts from zero
        s.tick += 100;
        s.match_events = vec![MatchEvent::MatchStart { tick: s.tick, map: 1 }];
        t.observe(&s);
        assert_eq!((t.match_number(), t.stats(1).unwrap().thrown), (2, 0));
    }
}
