//! `RenderView` -> `HudState`: maps what the predicted game and the server report (own hp, cooldowns, hit
//! messages, scoreboard) onto the HUD's transient effects and state. Renderer-agnostic and unit-tested.

use shinobi_proto::gameplay::{PHASE_INTERMISSION, PHASE_LIVE};
use shinobi_sim::match_state::{PhaseKind, ScoreEntry};
use shinobi_sim::pvp::MAX_HP;

use crate::client_game::RenderView;
use std::collections::HashMap;

use crate::hud::{
    BuildHud, BuildSlot, GrappleHud, KilledBy, Winner, SPAWN_SHIELD_SECS,
    bearing_to, callsign, damage_kind, killfeed_tick, project, score_rows_from_entries, AllyTag, HudFx, HudState, KillEntry, LocalStats, RadarPing, TargetHud,
    Telemetry, TeamScores,
    BROW_H, CHIN_TOP, KILLFEED_MAX, SIDE_CLEAR,
};
use crate::netclient::NetClient;

/// Kills that win a team match (docs/8v8.md); the server does not send the limit.
pub const TEAM_SCORE_LIMIT: u16 = 50;

/// Team-match data for the score bar, team-coloured names and ally tags. Normally derived from the `NetClient`
/// each frame (`from_net`); set `HudBridge::teams` to override it (tests, offline shots). `None` = free-for-all.
#[derive(Clone, Debug, Default)]
pub struct TeamInfo {
    /// The local player's team (0 cyan, 1 orange).
    pub my_team: u8,
    pub scores: [u16; 2],
    pub score_limit: u16,
    /// `TEAM_RESULT_*` from the server (running / winner / draw / sudden death).
    pub result: u8,
    /// Team per player id.
    pub team_of: HashMap<u16, u8>,
}

impl TeamInfo {
    /// Teams as the server told us, for the players visible this frame. `None` before our own team is known or
    /// when the server runs no teams (free-for-all, `TEAM_NONE`).
    pub fn from_net(net: &NetClient, view: &RenderView) -> Option<Self> {
        let my_team = net.my_team().filter(|t| *t < shinobi_sim::team::TEAM_COUNT)?;
        let mut ids: Vec<u16> = view.remotes.iter().map(|(id, _)| *id).collect();
        ids.extend(view.board.iter().flat_map(|b| b.rows.iter().map(|r| r.id as u16)));
        for e in &view.events {
            if let shinobi_sim::events::NetEvent::Kill { killer, victim, .. } = e {
                ids.extend([*killer, *victim]);
            }
        }
        let team_of = ids.into_iter().filter_map(|id| net.team_of(id).filter(|t| *t < shinobi_sim::team::TEAM_COUNT).map(|t| (id, t))).collect();
        Some(Self { my_team, scores: net.team_scores(), score_limit: net.team_frag_limit().unwrap_or(TEAM_SCORE_LIMIT), result: net.team_result(), team_of })
    }
}

/// Lock-on state as combat publishes it (`LockOn { target, pos, progress, locked }`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LockInfo {
    pub target: u16,
    pub pos: [f32; 3],
    pub progress: f32,
    pub locked: bool,
}

/// What the client has seen of the current match, for the end-of-match awards.
#[derive(Clone, Debug, Default)]
pub struct MatchStats {
    /// Kills per `(killer, weapon)`.
    kills: HashMap<(u16, u8), u32>,
    /// Longest kill seen: `(killer, metres)`, measured only when killer and victim were both visible.
    longest: Option<(u16, f32)>,
    /// Headshots we landed.
    pub my_headshots: u32,
}

fn weapon_index(k: shinobi_sim::combat::DamageKind) -> u8 {
    use shinobi_sim::combat::DamageKind::*;
    match k {
        Shuriken => 0,
        Slash => 1,
        GridBeam => 2,
    }
}

impl MatchStats {
    /// Award lines for the results screen from what was seen: sharpshooter (most shuriken kills), blademaster (most
    /// katana kills), longest kill. Headshots per player and pieces destroyed are not on the wire, so no such awards.
    pub fn awards(&self, name: impl Fn(u16) -> String, colour: impl Fn(u16) -> crate::hud::Rgba) -> Vec<crate::hud::Award> {
        let mut out = Vec::new();
        for (idx, title) in [(0u8, "SHARPSHOOTER"), (1u8, "BLADEMASTER")] {
            let best = self.kills.iter().filter(|((_, w), n)| *w == idx && **n > 0).max_by_key(|((id, _), n)| (**n, std::cmp::Reverse(*id)));
            if let Some(((id, _), n)) = best {
                out.push(crate::hud::Award { title, name: name(*id), colour: Some(colour(*id)), value: n.to_string() });
            }
        }
        if let Some((id, m)) = self.longest {
            out.push(crate::hud::Award { title: "LONGEST KILL", name: name(id), colour: Some(colour(id)), value: format!("{m:.0} M") });
        }
        out
    }
}

/// Radar reach in metres; enemies show only inside `RADAR_ENEMY_RANGE` or for `RADAR_FIRE_SECS` after a throw.
pub const RADAR_RANGE: f32 = 90.0;
pub const RADAR_ENEMY_RANGE: f32 = 25.0;
/// Free-for-all kill limit when `HudBridge::kill_limit` is unset: the demo rule from `Balance::demo()`. The server
/// does not send its limit yet, so a differently configured server shows this number wrongly.
/// Kill limit to show: an explicit override, else the server's rules (`NetClient::frag_limit`), else the demo default.
pub fn kill_limit_to_show(override_limit: Option<u16>, from_server: Option<u16>) -> u16 {
    override_limit.or(from_server).unwrap_or_else(ffa_kill_limit)
}

/// Seconds out of the fight before health regeneration starts (`Balance::player.regen_delay_s`).
/// Flat shuriken speed (m/s) the lead marker assumes: the demo balance (`combat.shuriken.speed`).
const SHURIKEN_SPEED: f32 = 70.0;

/// 0..=1 closeness of `p` to the crosshair: 1 dead centre, 0 at `CANDIDATE_CONE_DEG` or beyond (or behind us).
fn crosshair_heat(eye: [f32; 3], yaw: f32, pitch: f32, p: [f32; 3]) -> f32 {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let fwd = [-sy * cp, sp, -cy * cp];
    let d = [p[0] - eye[0], p[1] - eye[1], p[2] - eye[2]];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-3);
    let cos = (d[0] * fwd[0] + d[1] * fwd[1] + d[2] * fwd[2]) / len;
    let ang = cos.clamp(-1.0, 1.0).acos().to_degrees();
    (1.0 - ang / crate::hud::CANDIDATE_CONE_DEG).clamp(0.0, 1.0)
}

/// Rate (1/s) the lock brackets chase the target: matches the 3D lock dots' `smooth_toward`.
pub const LOCK_SMOOTH_RATE: f32 = 28.0;

pub fn regen_delay_s() -> f32 {
    shinobi_sim::balance::Balance::demo().player.regen_delay_s
}
pub fn ffa_kill_limit() -> u16 {
    shinobi_sim::balance::Balance::demo().matches.frag_limit.min(u16::MAX as u32) as u16
}
/// Structures a player may own at once (`BlockCfg::max_per_owner`).
pub const BUILD_MAX: u8 = 64;
pub const RADAR_FIRE_SECS: f32 = 2.0;

#[derive(Default)]
pub struct HudBridge {
    pub teams: Option<TeamInfo>,
    /// Free-for-all colour per player id (defaults to `colour::player_rgba`).
    pub colour_of: Option<fn(u16) -> crate::hud::Rgba>,
    /// Kills that end a free-for-all match (server limit, if known).
    pub kill_limit: Option<u16>,
    /// Half extent of the map floor (`LevelDesc::floor_half`): the radar draws the void edge when it is in range.
    pub map_half: Option<f32>,
    /// Pickup points from the level file (static): shown on the radar while in range.
    pub pickups: Vec<[f32; 3]>,
    /// Build / grapple readouts, filled by the shell from the combat and movement state once those expose it.
    pub build: Option<crate::hud::BuildHud>,
    pub grapple: Option<crate::hud::GrappleHud>,
    /// Lock-on target from combat (`RenderView.lock` once it exists): world position + progress. Stub input.
    pub lock: Option<LockInfo>,
    boot: Option<f32>,
    grapple_flash: f32,
    /// The last kill that eliminated us (shown while dead).
    last_death: Option<KilledBy>,
    /// World position of the killer when the kill landed.
    last_death_pos: Option<[f32; 3]>,
    /// Match statistics from the kill events seen since the match began (reset after each intermission).
    stats: MatchStats,
    prev_phase: Option<u8>,
    /// Seconds we have been invulnerable (spawn protection) since last becoming so.
    invuln_t: f32,
    /// Master volume and mute from the audio settings, for the speaker glyph (the shell sets it each frame).
    pub audio: Option<(f32, bool)>,
    /// Seconds without damage before health regeneration starts (`None` = the demo balance's value).
    pub regen_delay: Option<f32>,
    prev_hp: Option<f32>,
    /// Render-smoothed lock target position (and whose): the sim steps at 60 Hz, the brackets should not.
    lock_smooth: Option<(u16, [f32; 3])>,
    since_damage: f32,
    was_dead: bool,
    booted: bool,
    /// Seconds of enemy-reveal left after our last throw.
    fire_reveal: f32,
    /// Viewport width / height for projecting ally tags (<= 0 means 16:9).
    pub aspect: f32,
    pub fx: HudFx,
    killfeed: Vec<KillEntry>,
    time: f32,
}

impl HudBridge {
    /// The kill log as the HUD holds it (newest last): for the console's `/log`.
    pub fn killfeed(&self) -> &[KillEntry] {
        &self.killfeed
    }

    /// Once per rendered frame. `tab_held` shows the scoreboard.
    pub fn update(&mut self, dt: f32, view: &RenderView, net: &NetClient, tab_held: bool) -> HudState {
        self.time += dt;
        self.fx.tick(dt);
        let colour_of = self.colour_of.unwrap_or(crate::colour::player_rgba);
        let teams = self.teams.clone().or_else(|| TeamInfo::from_net(net, view));

        let me = view.local_pos.unwrap_or([0.0; 3]);
        // predicted katana hit: confirm at once, the server's HitMsg (with the damage number) follows a round trip later
        if let Some(sh) = view.fx.slash_hit {
            self.fx.on_hit_amount(shinobi_sim::combat::DamageKind::Slash, sh.killed, 0);
        }
        for h in &view.hits {
            if h.taken && h.other == 0 {
                // environmental damage (obstacle / void): nobody to point at, so no direction arrow
            } else if h.taken {
                // bearing to the attacker if we can see them, else straight ahead
                let src = view.remotes.iter().find(|(id, _)| *id == h.other).map(|(_, s)| s.pos);
                self.fx.on_hurt_amount(src.map_or(0.0, |p| bearing_to(me, view.aim.yaw, p)), h.amount);
            } else {
                self.fx.on_hit_full(damage_kind(h.kind), h.kill, h.amount, h.head);
                if h.head {
                    self.stats.my_headshots += 1;
                }
            }
        }

        let local_id = net.player_id().unwrap_or(0);
        // reliable events: match banners, pickup toast, kill feed
        let name = |id: u16| callsign(id);
        for e in &view.events {
            self.fx.on_net_event(e, local_id, name);
            if let Some(mut k) = KillEntry::from_net(e, local_id, name) {
                if let (Some(t), shinobi_sim::events::NetEvent::Kill { killer, victim, .. }) = (&teams, e) {
                    k.killer_team = (!k.void).then(|| t.team_of.get(killer).copied()).flatten();
                    k.victim_team = t.team_of.get(victim).copied();
                } else if teams.is_none() {
                    if let shinobi_sim::events::NetEvent::Kill { killer, victim, .. } = e {
                        k.killer_colour = (!k.void).then(|| colour_of(*killer));
                        k.victim_colour = Some(colour_of(*victim));
                    }
                }
                if let shinobi_sim::events::NetEvent::Kill { killer, victim, .. } = e {
                    if !k.void {
                        *self.stats.kills.entry((*killer, weapon_index(k.weapon))).or_insert(0) += 1;
                        // distance between killer and victim when both are in view this frame
                        let pos_of = |id: u16| if id == local_id { view.local_pos } else { view.remotes.iter().find(|(r, _)| *r == id).map(|(_, s)| s.pos) };
                        if let (Some(a), Some(c)) = (pos_of(*killer), pos_of(*victim)) {
                            let d = ((a[0] - c[0]).powi(2) + (a[1] - c[1]).powi(2) + (a[2] - c[2]).powi(2)).sqrt();
                            if self.stats.longest.map_or(true, |(_, best)| d > best) {
                                self.stats.longest = Some((*killer, d));
                            }
                        }
                    }
                }
                if let shinobi_sim::events::NetEvent::Kill { victim, .. } = e {
                    if *victim == local_id {
                        let tint = k.killer_team.map(|t| crate::hud::Palette::TRON.team(t)).or(k.killer_colour);
                        self.last_death = Some(KilledBy { name: k.killer.clone(), colour: tint, weapon: k.weapon, void: k.void, bearing: None, dist: None, kd: None });
                        // where the killer stood (if we can see them this frame)
                        self.last_death_pos = if k.void { None } else if let shinobi_sim::events::NetEvent::Kill { killer, .. } = e { view.remotes.iter().find(|(id, _)| id == killer).map(|(_, s)| s.pos) } else { None };
                    }
                }
                self.killfeed.push(k);
                if self.killfeed.len() > KILLFEED_MAX {
                    self.killfeed.remove(0);
                }
            }
        }
        killfeed_tick(&mut self.killfeed, dt);

        let f = &view.fx;
        let scoreboard = view
            .board
            .as_ref()
            .map(|b| {
                let entries: Vec<_> = b
                    .rows
                    .iter()
                    .map(|r| ScoreEntry { id: r.id as u16, kills: r.kills, deaths: r.deaths, alive: r.alive })
                    .collect();
                score_rows_from_entries(&entries, local_id, callsign, |_| 0)
            })
            .unwrap_or_default();
        let mut scoreboard = scoreboard;
        let mut team_scores = None;
        let mut ally_tags = Vec::new();
        if let (Some(t), Some(b)) = (&teams, view.board.as_ref()) {
            for (row, r) in scoreboard.iter_mut().zip(b.rows.iter()) {
                row.team = t.team_of.get(&(r.id as u16)).copied();
            }
        } else if let Some(b) = view.board.as_ref() {
            for (row, r) in scoreboard.iter_mut().zip(b.rows.iter()) {
                row.colour = Some(colour_of(r.id as u16));
            }
        }
        if let Some(t) = &teams {
            team_scores = Some(TeamScores { mine: t.my_team, scores: t.scores, limit: t.score_limit, result: t.result });
            let aspect = if self.aspect > 0.0 { self.aspect } else { 16.0 / 9.0 };
            let (yaw, pitch) = view.look_angles();
            let fov = if view.camera.fov > 1.0 { view.camera.fov } else { 90.0 };
            for (id, smp) in &view.remotes {
                // allies only: enemies never get a tag, even through walls
                if t.team_of.get(id).copied().map(|x| x != 0) != Some(t.my_team != 0) || *id == local_id {
                    continue;
                }
                let head = [smp.pos[0], smp.pos[1] + 1.1, smp.pos[2]];
                if let Some((x, y, dist)) = project(view.eye, yaw, pitch, fov, aspect, head) {
                    // keep tags in the central band (the side columns stay empty); an off-screen ally pins to the edge
                    let (cx, cy) = (x.clamp(SIDE_CLEAR + 0.04, 1.0 - SIDE_CLEAR - 0.04), y.clamp(BROW_H + 0.12, CHIN_TOP - 0.05));
                    ally_tags.push(AllyTag { pos: (cx, cy), name: callsign(*id), dist, clamped: (cx, cy) != (x, y) });
                }
            }
        }
        // enemies within range but off screen -> edge ticks (teams: the other team; free-for-all: everyone else)
        let mut enemy_ticks = Vec::new();
        let mut enemy_tags: Vec<crate::hud::EnemyTag> = Vec::new();
        let locked_id = self.lock.map(|l| l.target).or_else(|| view.lock.map(|l| l.target as u16));
        {
            let aspect = if self.aspect > 0.0 { self.aspect } else { 16.0 / 9.0 };
            let (yaw, pitch) = view.look_angles();
            let fov = if view.camera.fov > 1.0 { view.camera.fov } else { 90.0 };
            for (id, smp) in &view.remotes {
                if *id == local_id {
                    continue;
                }
                if let Some(t) = &teams {
                    if t.team_of.get(id).copied().map(|x| x != 0) == Some(t.my_team != 0) {
                        continue;
                    }
                }
                if view.board.as_ref().and_then(|b| b.rows.iter().find(|r| r.id as u16 == *id)).map_or(false, |r| !r.alive) {
                    continue;
                }
                let chest = [smp.pos[0], smp.pos[1] + 0.9, smp.pos[2]];
                let on_screen = project(view.eye, yaw, pitch, fov, aspect, chest).filter(|(x, y, _)| (0.0..=1.0).contains(x) && (0.0..=1.0).contains(y));
                let d = ((chest[0] - view.eye[0]).powi(2) + (chest[1] - view.eye[1]).powi(2) + (chest[2] - view.eye[2]).powi(2)).sqrt();
                if on_screen.is_none() && d <= crate::hud::ENEMY_TICK_RANGE {
                    enemy_ticks.push(crate::hud::EnemyTick { bearing: bearing_to(me, yaw, smp.pos), dist: d });
                }
                // no tags through walls: the client game ray-checks eye -> chest against the level
                // the lock target already wears the lock label: no second name over it
                if d <= crate::hud::MARKER_RANGE && view.los_ids.contains(id) && Some(*id) != locked_id {
                    let head = [smp.pos[0], smp.pos[1] + 1.1, smp.pos[2]];
                    if let Some((x, y, dist)) = project(view.eye, yaw, pitch, fov, aspect, head) {
                        if (0.08..=0.92).contains(&x) && (BROW_H + 0.05..=CHIN_TOP - 0.03).contains(&y) {
                            enemy_tags.push(crate::hud::EnemyTag {
                                pos: (x, y),
                                name: callsign(*id),
                                dist,
                                team: teams.as_ref().and_then(|t| t.team_of.get(id).copied()),
                                colour: Some(colour_of(*id)),
                                hp: None, // NetClient::remote_hp once the snapshot carries it
                                heat: crosshair_heat(view.eye, yaw, pitch, chest),
                            });
                        }
                    }
                }
            }
        }
        let stats = LocalStats {
            hp: view.hp.map_or(1.0, |h| h / MAX_HP),
            dash_ready: if f.dash_ready { 1.0 } else { 1.0 - f.slice_cooldown },
            shuriken_ready: 1.0 - f.throw_cooldown,
            slice_ready: 1.0 - f.slice_cooldown,
            // reticle opens while moving fast, airborne, or right after a throw
            spread: ((f.speed / 12.0).clamp(0.0, 1.0) * 0.5 + f.throw_cooldown.clamp(0.0, 1.0) * 0.3 + if f.grounded { 0.0 } else { 0.2 }).clamp(0.0, 1.0),
            ult: 0.0,
            killfeed: self.killfeed.clone(),
            // Tab shows it; the intermission always does (final standings stay up until the next match)
            show_scoreboard: tab_held || view.board.as_ref().map_or(false, |b| b.phase == PHASE_INTERMISSION),
            scoreboard,
            match_clock: view.board.as_ref().map(|b| {
                let kind = match b.phase {
                    PHASE_LIVE => PhaseKind::Live,
                    PHASE_INTERMISSION => PhaseKind::Intermission,
                    _ => PhaseKind::Warmup,
                };
                (kind, b.ticks_left.map(|t| t.div_ceil(shinobi_proto::TICK_HZ as u32)))
            }),
            match_map: view.board.as_ref().map(|b| b.map),
            ..Default::default()
        };
        let mut hs = HudState::from_frame(net, &stats, &self.fx, self.time);
        // regeneration: the delay counts from the last damage taken (a hit or any hp drop), then hp climbs back
        if view.hits.iter().any(|h| h.taken) || self.prev_hp.map_or(false, |p| stats.hp < p - 1e-4) {
            self.since_damage = 0.0;
        } else {
            self.since_damage += dt;
        }
        self.prev_hp = Some(stats.hp);
        if !hs.dead && stats.hp < 0.999 && view.hp.is_some() {
            let delay = self.regen_delay.unwrap_or_else(regen_delay_s).max(0.01);
            hs.regen = Some(if self.since_damage >= delay { crate::hud::Regen::Healing } else { crate::hud::Regen::Waiting((self.since_damage / delay).clamp(0.0, 1.0)) });
        }
        // --- cyber layer: telemetry, radar, compass, lock brackets, boot-up ---
        let (yaw, pitch) = view.look_angles();
        hs.heading = Some(yaw);
        hs.zone = view.district.map(|d| (d.name.to_string(), d.tint));
        hs.telemetry = Some(Telemetry {
            speed: view.fx.speed,
            altitude: view.eye[1],
            rope: view.rope.map(|(a, bb)| ((a[0] - bb[0]).powi(2) + (a[1] - bb[1]).powi(2) + (a[2] - bb[2]).powi(2)).sqrt()),
        });
        if view.fx.threw.is_some() {
            self.fire_reveal = RADAR_FIRE_SECS;
        } else {
            self.fire_reveal = (self.fire_reveal - dt).max(0.0);
        }
        let firing = self.fire_reveal > 0.0;
        let (sy, cy) = yaw.sin_cos();
        let (fwd, right) = ([-sy, -cy], [cy, -sy]);
        for (id, smp) in &view.remotes {
            let ally = teams.as_ref().map_or(false, |t| t.team_of.get(id).copied().map(|x| x != 0) == Some(t.my_team != 0));
            let (dx, dz) = (smp.pos[0] - me[0], smp.pos[2] - me[2]);
            let dist = (dx * dx + dz * dz).sqrt();
            // no wallhack: enemies appear only when close or just after we fired; allies always
            if dist > RADAR_RANGE || (!ally && dist > RADAR_ENEMY_RANGE && !firing) {
                continue;
            }
            let pos = ((dx * right[0] + dz * right[1]) / RADAR_RANGE, (dx * fwd[0] + dz * fwd[1]) / RADAR_RANGE);
            let colour = match &teams {
                Some(t) => t.team_of.get(id).map_or(crate::hud::Palette::TRON.magenta, |x| crate::hud::Palette::TRON.team(*x)),
                None => colour_of(*id),
            };
            hs.radar.push(RadarPing { pos, colour, ally });
        }
        // our own printed pieces, in radar space (view-relative, range = 1)
        let to_radar = |x: f32, z: f32| {
            let (dx, dz) = (x - me[0], z - me[2]);
            ((dx * right[0] + dz * right[1]) / RADAR_RANGE, (dx * fwd[0] + dz * fwd[1]) / RADAR_RANGE)
        };
        for pk in &self.pickups {
            let p = to_radar(pk[0], pk[2]);
            if p.0 * p.0 + p.1 * p.1 <= 1.0 {
                hs.radar_pickups.push(p);
            }
        }
        for bl in view.blocks.iter().filter(|bl| bl.owner == local_id) {
            let c = bl.piece.center(bl.cell, view.block_size, view.block_thickness);
            let p = to_radar(c[0], c[2]);
            if p.0 * p.0 + p.1 * p.1 <= 1.0 {
                hs.radar_pieces.push(p);
            }
        }
        // the void edge: the map boundary sampled along each side, kept where it falls inside the radar disc
        if let Some(h) = self.map_half {
            let corners = [(-h, -h), (h, -h), (h, h), (-h, h)];
            for k in 0..4 {
                let (a, bb) = (corners[k], corners[(k + 1) % 4]);
                let mut prev: Option<(f32, f32)> = None;
                for s in 0..=48 {
                    let t = s as f32 / 48.0;
                    let p = to_radar(a.0 + (bb.0 - a.0) * t, a.1 + (bb.1 - a.1) * t);
                    let inside = p.0 * p.0 + p.1 * p.1 <= 1.0;
                    if let (true, Some(q)) = (inside, prev) {
                        hs.radar_edge.push((q, p));
                    }
                    prev = inside.then_some(p);
                }
            }
        }
        let lock = self.lock.or_else(|| view.lock.map(|l| LockInfo { target: l.target as u16, pos: l.pos, progress: l.progress, locked: l.locked }));
        if let Some(l) = lock {
            let aspect = if self.aspect > 0.0 { self.aspect } else { 16.0 / 9.0 };
            let fov = if view.camera.fov > 1.0 { view.camera.fov } else { 90.0 };
            // target health from the replicated snapshot flags (31 steps), when the target is in the newest snapshot
            let target_hp = net.interp.newest_tick().and_then(|t| net.interp.get(t)).and_then(|s| s.get(l.target)).map(|e| shinobi_proto::snapshot::hp_frac(e.flags));
            // exp smoothing (rate 28, frame-rate independent) with the target's velocity as feed-forward, as the 3D lock
            // dots do; a new target starts on itself (no sweep across the map)
            let vel = view.lock.map_or([0.0; 3], |lk| lk.target_vel);
            let k = 1.0 - (-LOCK_SMOOTH_RATE * dt).exp();
            let sm = match self.lock_smooth {
                Some((id, cur)) if id == l.target => std::array::from_fn(|i| {
                    let lead = cur[i] + vel[i] * dt;
                    lead + (l.pos[i] - lead) * k
                }),
                _ => l.pos,
            };
            self.lock_smooth = Some((l.target, sm));
            if let Some((x, y, dist)) = project(view.eye, yaw, pitch, fov, aspect, sm) {
                let colour = match &teams {
                    Some(t) => crate::hud::Palette::TRON.team(t.team_of.get(&l.target).copied().unwrap_or(1)),
                    None => colour_of(l.target),
                };
                // keep the brackets in the central band, away from the side columns
                let pos = (x.clamp(SIDE_CLEAR + 0.06, 1.0 - SIDE_CLEAR - 0.06), y.clamp(BROW_H + 0.15, CHIN_TOP - 0.1));
                // where a star thrown now arrives: the aim point carried along the target's velocity for the flight time
                let flight = dist / SHURIKEN_SPEED;
                let lead = (vel[0] * vel[0] + vel[2] * vel[2] > 1.0)
                    .then(|| project(view.eye, yaw, pitch, fov, aspect, std::array::from_fn(|i| sm[i] + vel[i] * flight)))
                    .flatten()
                    .map(|(lx, ly, _)| (lx.clamp(SIDE_CLEAR + 0.06, 1.0 - SIDE_CLEAR - 0.06), ly.clamp(BROW_H + 0.15, CHIN_TOP - 0.1)));
                let (since, breaking) = view.lock.map_or((1.0, false), |lk| (lk.t_since_phase, lk.phase == shinobi_sim::combat::aim_assist::LockPhase::Break));
                hs.target = Some(TargetHud { pos, name: callsign(l.target), colour, hp: target_hp, dist, progress: l.progress, locked: l.locked, since, breaking, lead });
            }
        }
        if lock.is_none() {
            self.lock_smooth = None;
        }
        // boot-up plays on first link and on every respawn
        if hs.dead {
            self.was_dead = true;
            self.boot = None;
        } else if view.connected && (self.was_dead || !self.booted) {
            self.was_dead = false;
            self.booted = true;
            self.boot = Some(0.0);
        }
        if let Some(t) = self.boot.as_mut() {
            *t += dt;
            if *t >= crate::hud::BOOT_TIME {
                self.boot = None;
            }
        }
        hs.boot = self.boot;
        // death overlay, spawn shield, match result, audio glyph
        hs.killed_by = if hs.dead { self.last_death.clone() } else { None };
        if let Some(k) = hs.killed_by.as_mut() {
            if let Some(pos) = self.last_death_pos {
                k.bearing = Some(bearing_to(me, view.aim.yaw, pos));
                k.dist = Some(((pos[0] - me[0]).powi(2) + (pos[2] - me[2]).powi(2)).sqrt());
            }
            k.kd = hs.scoreboard.iter().find(|r| r.local).map(|r| (r.kills, r.deaths));
        }
        if hs.invulnerable && !hs.dead {
            self.invuln_t += dt;
            hs.shield = Some((1.0 - self.invuln_t / SPAWN_SHIELD_SECS).clamp(0.0, 1.0));
        } else {
            self.invuln_t = 0.0;
        }
        if let Some(bd) = view.board.as_ref().filter(|bd| bd.phase == PHASE_INTERMISSION) {
            let id = bd.winner;
            hs.winner = Some(Winner {
                name: if id == 0 { String::new() } else { callsign(id) },
                colour: (id != 0).then(|| match &teams {
                    Some(t) => crate::hud::Palette::TRON.team(t.team_of.get(&id).copied().unwrap_or(0)),
                    None => colour_of(id),
                }),
                local: id != 0 && id == local_id,
                draw: id == 0,
            });
        }
        // match stats reset when the intermission ends; awards are shown only while it lasts
        let phase = view.board.as_ref().map(|bd| bd.phase);
        if self.prev_phase == Some(PHASE_INTERMISSION) && phase.is_some() && phase != Some(PHASE_INTERMISSION) {
            self.stats = MatchStats::default();
        }
        self.prev_phase = phase.or(self.prev_phase);
        if phase == Some(PHASE_INTERMISSION) {
            hs.awards = self.stats.awards(callsign, |id| match &teams {
                Some(t) => crate::hud::Palette::TRON.team(t.team_of.get(&id).copied().unwrap_or(0)),
                None => colour_of(id),
            });
            hs.my_headshots = self.stats.my_headshots;
        }
        hs.audio = self.audio;
        hs.aspect = self.aspect;
        hs.ffa = teams.is_none();
        hs.kill_limit = if teams.is_none() { Some(kill_limit_to_show(self.kill_limit, net.frag_limit())) } else { None };
        hs.time_limit_secs = net.time_limit_secs();
        // build readout from the predicted ghost (an explicit `build` overrides it)
        hs.build = self.build.or_else(|| {
            let g = view.build_ghost?;
            Some(BuildHud {
                selected: match g.piece.kind {
                    shinobi_sim::blocks::PieceKind::Floor => BuildSlot::Floor,
                    shinobi_sim::blocks::PieceKind::Wall => BuildSlot::Wall,
                    shinobi_sim::blocks::PieceKind::Ramp => BuildSlot::Ramp,
                },
                ready: if g.ready { 1.0 } else { 0.0 },
                placed: view.blocks.iter().filter(|bl| bl.owner == local_id).count().min(BUILD_MAX as usize) as u8,
                max: BUILD_MAX,
                ghost: Some(g.valid),
            })
        });
        // grapple readout from the predicted movement state (an explicit `grapple` overrides it)
        let gf = &view.fx;
        if gf.grapple_missed {
            self.grapple_flash = 0.4;
        } else {
            self.grapple_flash = (self.grapple_flash - dt).max(0.0);
        }
        hs.grapple = self.grapple.or(Some(GrappleHud {
            ready: if gf.grappling { 0.0 } else { 1.0 - gf.grapple_cooldown.clamp(0.0, 1.0) },
            hookable: gf.grapple_hookable,
            on_rope: gf.grappling,
            missed: self.grapple_flash > 0.0,
        }));
        hs.team_scores = team_scores;
        hs.ally_tags = ally_tags;
        hs.enemy_ticks = enemy_ticks;
        enemy_tags.sort_by(|a, b| a.dist.total_cmp(&b.dist));
        enemy_tags.truncate(crate::hud::ENEMY_TAG_MAX);
        hs.enemy_tags = enemy_tags;
        hs.controls = view.controls.clone();
        hs
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn lock_brackets_smooth_between_sim_steps_lead_the_target_and_hide_its_name_tag() {
        use crate::client_game::LockOn;
        use shinobi_sim::combat::aim_assist::LockPhase;
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.eye = [0.0, 1.5, 0.0];
        v.camera.fov = 90.0;
        let lock = |x: f32, vx: f32, target| LockOn { target, pos: [x, 1.5, -20.0], progress: 1.0, locked: true, phase: LockPhase::Locked, eased: 1.0, t_since_phase: 1.0, target_vel: [vx, 0.0, 0.0], blend: 1.0 };
        v.lock = Some(lock(0.0, 0.0, 5));
        let x0 = b.update(1.0 / 60.0, &v, &net(), false).target.unwrap().pos.0;
        // the sim target jumps 1 m in one step: the bracket moves part of the way, not all of it
        v.lock = Some(lock(1.0, 0.0, 5));
        let x1 = b.update(1.0 / 144.0, &v, &net(), false).target.unwrap().pos.0;
        let raw = crate::hud::project(v.eye, 0.0, 0.0, 90.0, 16.0 / 9.0, [1.0, 1.5, -20.0]).unwrap().0;
        assert!(x1 > x0 && x1 < raw - 1e-4, "smoothed: {x0} < {x1} < {raw}");
        // frame-rate independent: 1 s later (any step size) it has arrived
        for _ in 0..144 {
            b.update(1.0 / 144.0, &v, &net(), false);
        }
        let xe = b.update(1.0 / 144.0, &v, &net(), false).target.unwrap().pos.0;
        assert!((xe - raw).abs() < 1e-3, "{xe} vs {raw}");
        // a new target starts on itself
        v.lock = Some(lock(-3.0, 0.0, 6));
        let xn = b.update(1.0 / 144.0, &v, &net(), false).target.unwrap().pos.0;
        let rawn = crate::hud::project(v.eye, 0.0, 0.0, 90.0, 16.0 / 9.0, [-3.0, 1.5, -20.0]).unwrap().0;
        assert!((xn - rawn).abs() < 1e-4, "{xn} vs {rawn}");
        // the lock target gets no world name tag (the lock label already names it)
        let smp = |p: [f32; 3]| crate::interp::Sample { pos: p, yaw: 0.0, extrapolated: false };
        v.remotes = vec![(6, smp([-3.0, 0.0, -10.0])), (7, smp([2.0, 0.0, -10.0]))];
        v.los_ids = vec![6, 7];
        let s = b.update(1.0 / 144.0, &v, &net(), false);
        assert_eq!(s.enemy_tags.len(), 1);
        assert_eq!(s.enemy_tags[0].name, callsign(7));
    }

    #[test]
    fn enemy_markers_for_visible_enemies_in_range_nearest_six_with_crosshair_heat() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.eye = [0.0, 1.6, 0.0];
        v.local_pos = Some([0.0, 0.0, 0.0]);
        let smp = |p: [f32; 3]| crate::interp::Sample { pos: p, yaw: 0.0, extrapolated: false };
        for (k, z) in [-5.0, -8.0, -11.0, -14.0, -17.0, -20.0].iter().enumerate() {
            v.remotes.push((2 + k as u16, smp([(k as f32 - 2.5) * 0.5, 0.0, *z])));
        }
        v.remotes.push((20, smp([0.0, 0.0, -90.0]))); // beyond the marker range
        v.remotes.push((21, smp([0.0, 0.0, 6.0]))); // behind: tick, not tag
        v.remotes.push((22, smp([1.0, 0.0, -7.0]))); // in range and on screen but behind a wall
        v.los_ids = v.remotes.iter().map(|r| r.0).filter(|id| *id != 22).collect();
        let s = b.update(0.016, &v, &net(), false);
        assert!(s.enemy_tags.iter().all(|t| t.name != callsign(22)), "walled-off enemy gets no tag");
        assert_eq!(s.enemy_tags.len(), 6, "{:?}", s.enemy_tags.iter().map(|t| t.dist).collect::<Vec<_>>());
        assert!(s.enemy_tags.iter().all(|t| t.name != callsign(20)), "out of range gets no marker");
        let heats: Vec<f32> = s.enemy_tags.iter().map(|t| t.heat).collect();
        assert!(heats.iter().all(|h| (0.0..=1.0).contains(h)) && heats.iter().cloned().fold(0.0, f32::max) > 0.8, "enemies near the crosshair run hot: {heats:?}");
        assert_eq!(crosshair_heat([0.0; 3], 0.0, 0.0, [0.0, 0.0, -10.0]), 1.0);
        assert_eq!(crosshair_heat([0.0; 3], 0.0, 0.0, [10.0, 0.0, 0.0]), 0.0, "90 degrees off is cold");
        assert_eq!(crosshair_heat([0.0; 3], 0.0, 0.0, [0.0, 0.0, 10.0]), 0.0, "behind is cold");
        assert!(s.enemy_tags.windows(2).all(|w| w[0].dist <= w[1].dist), "nearest first");
        assert!(s.enemy_tags.iter().all(|t| t.hp.is_none()), "no hp until the network reports it");
        assert_eq!(s.enemy_ticks.len(), 1);
    }

    #[test]
    fn enemy_ticks_only_for_close_offscreen_enemies() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.eye = [0.0, 1.6, 0.0];
        v.local_pos = Some([0.0, 0.0, 0.0]);
        let smp = |p: [f32; 3]| crate::interp::Sample { pos: p, yaw: 0.0, extrapolated: false };
        v.remotes.push((2, smp([0.0, 0.0, -10.0]))); // ahead: on screen
        v.remotes.push((3, smp([0.0, 0.0, 10.0]))); // behind, close
        v.remotes.push((4, smp([0.0, 0.0, 80.0]))); // behind, far
        let s = b.update(0.016, &v, &net(), false);
        assert_eq!(s.enemy_ticks.len(), 1, "{:?}", s.enemy_ticks);
        assert!(s.enemy_ticks[0].bearing.abs() > 2.9 && s.enemy_ticks[0].dist < 12.0);
    }

    use super::*;
    use shinobi_proto::gameplay::{Board, BoardRow, HitMsg, KIND_SLASH};
    use shinobi_sim::combat::DamageKind;

    fn net() -> NetClient {
        NetClient::default()
    }

    fn hit(taken: bool, other: u16) -> HitMsg {
        HitMsg { other, amount: 30, kind: KIND_SLASH, taken, kill: false, head: false }
    }

    #[test]
    fn hp_and_cooldowns_map_to_unit_fractions() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.hp = Some(MAX_HP * 0.25);
        v.fx.dash_ready = false;
        v.fx.slice_cooldown = 0.4;
        v.fx.throw_cooldown = 1.0;
        let s = b.update(0.016, &v, &net(), false);
        assert!((s.hp - 0.25).abs() < 1e-5);
        assert!((s.dash_ready - 0.6).abs() < 1e-5);
        assert_eq!(s.shuriken_ready, 0.0);
        // before the first extras block everything reads full
        let s = HudBridge::default().update(0.016, &RenderView::default(), &net(), false);
        assert_eq!((s.hp, s.dash_ready, s.shuriken_ready), (1.0, 0.0 + 1.0, 1.0));
    }

    #[test]
    fn dealt_hit_makes_a_marker_that_expires() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.hits.push(hit(false, 7));
        let s = b.update(0.016, &v, &net(), false);
        assert!(s.since_hit.is_some() && s.hit_kind == Some(DamageKind::Slash));
        v.hits.clear();
        let s = b.update(1.0, &v, &net(), false);
        assert!(s.since_hit.is_none(), "marker gone after its short life");
    }

    #[test]
    fn taken_hit_points_at_the_attacker() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.local_pos = Some([0.0, 0.8, 0.0]);
        v.aim.yaw = 0.0; // facing -Z
        v.remotes.push((9, crate::interp::Sample { pos: [10.0, 0.8, 0.0], yaw: 0.0, extrapolated: false })); // to the right (+X)
        v.hits.push(hit(true, 9));
        let s = b.update(0.016, &v, &net(), false);
        let (_, bearing) = s.since_damage.expect("damage indicator");
        assert!(bearing > 1.0, "attacker on the right => clockwise bearing: {bearing}");
        // unknown attacker: straight ahead
        let mut v2 = RenderView::default();
        v2.hits.push(hit(true, 99));
        let s = HudBridge::default().update(0.016, &v2, &net(), false);
        assert_eq!(s.since_damage.unwrap().1, 0.0);
    }

    #[test]
    fn kill_events_fill_the_feed_and_expire() {
        use shinobi_sim::events::NetEvent;
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.events.push(NetEvent::Kill { tick: 5, killer: 2, victim: 3, kind: KIND_SLASH });
        let s = b.update(0.016, &v, &net(), false);
        assert_eq!(s.killfeed.len(), 1);
        assert_eq!((s.killfeed[0].killer.as_str(), s.killfeed[0].victim.as_str()), (callsign(2).as_str(), callsign(3).as_str()));
        v.events.clear();
        let s = b.update(crate::hud::KILLFEED_TIME + 0.1, &v, &net(), false);
        assert!(s.killfeed.is_empty(), "entries expire");
    }

    #[test]
    fn scoreboard_rows_only_while_tab_is_held() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.board = Some(Board { rows: vec![BoardRow { id: 1, kills: 3, deaths: 1, alive: true }, BoardRow { id: 2, kills: 0, deaths: 3, alive: true }], ..Default::default() });
        let s = b.update(0.016, &v, &net(), true);
        assert!(s.show_scoreboard && s.scoreboard.len() == 2);
        assert_eq!(s.scoreboard[0].kills, 3);
        assert!(!b.update(0.016, &v, &net(), false).show_scoreboard);
    }

    #[test]
    fn intermission_forces_the_final_scoreboard_with_timer_and_match_over_banner() {
        use shinobi_proto::gameplay::PHASE_WARMUP;
        use shinobi_sim::events::NetEvent;
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        let board = |phase, ticks| Board { phase, map: 1, ticks_left: ticks, winner: 0, rows: vec![BoardRow { id: 1, kills: 20, deaths: 2, alive: true }] };
        v.board = Some(board(PHASE_LIVE, Some(125 * 60 - 30)));
        let s = b.update(0.016, &v, &net(), false);
        assert!(!s.show_scoreboard, "live: Tab only");
        assert_eq!(s.match_clock, Some((PhaseKind::Live, Some(125))), "ticks round up to whole seconds");
        assert_eq!(s.match_map, Some(1));
        v.board = Some(board(PHASE_WARMUP, None));
        assert_eq!(b.update(0.016, &v, &net(), false).match_clock, Some((PhaseKind::Warmup, None)));
        v.events.push(NetEvent::MatchEnd { tick: 9, winner: 1, reason: 0 });
        v.board = Some(board(PHASE_INTERMISSION, Some(540)));
        let s = b.update(0.016, &v, &net(), false);
        assert!(s.show_scoreboard && s.scoreboard.len() == 1);
        assert_eq!(s.match_clock, Some((PhaseKind::Intermission, Some(9))));
        assert_eq!(s.banner.as_ref().map(|x| x.text.as_str()), Some("MATCH OVER"));
    }

    #[test]
    fn regen_waits_for_the_delay_then_heals_and_resets_on_damage() {
        use crate::hud::Regen;
        let mut b = HudBridge::default();
        b.regen_delay = Some(4.0);
        let mut v = RenderView::default();
        v.hp = Some(MAX_HP);
        assert_eq!(b.update(0.1, &v, &net(), false).regen, None, "full health: nothing to show");
        v.hp = Some(MAX_HP * 0.5);
        let s = b.update(0.1, &v, &net(), false);
        assert_eq!(s.regen, Some(Regen::Waiting(0.0)), "the drop itself restarts the delay");
        let s = b.update(2.0, &v, &net(), false);
        match s.regen {
            Some(Regen::Waiting(f)) => assert!((f - 0.5).abs() < 1e-4, "{f}"),
            r => panic!("{r:?}"),
        }
        assert_eq!(b.update(2.5, &v, &net(), false).regen, Some(Regen::Healing));
        v.hp = Some(MAX_HP * 0.7); // climbing: still healing, not a damage event
        assert_eq!(b.update(0.1, &v, &net(), false).regen, Some(Regen::Healing));
        v.hp = Some(MAX_HP * 0.6); // hit again
        assert_eq!(b.update(0.1, &v, &net(), false).regen, Some(Regen::Waiting(0.0)));
        v.hp = Some(MAX_HP);
        assert_eq!(b.update(0.1, &v, &net(), false).regen, None);
    }

    #[test]
    fn dealt_kill_flows_through_as_a_kill_marker_and_low_hp_pulses() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.hp = Some(MAX_HP * 0.1);
        v.hits.push(HitMsg { other: 7, amount: 50, kind: KIND_SLASH, taken: false, kill: true, head: false });
        let s = b.update(0.016, &v, &net(), false);
        assert!(s.hit_kill && s.hp < 0.3);
        assert!(crate::hud::Hud::build(&s).iter().any(|p| matches!(p, crate::hud::Prim::Ring { .. })), "kill ring drawn");
    }

    #[test]
    fn hazard_damage_shows_no_direction_arrow_but_a_weapon_hit_does() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.hits.push(HitMsg { other: 0, amount: 20, kind: 3 /* environmental */, taken: true, kill: false, head: false });
        assert!(b.update(0.016, &v, &net(), false).since_damage.is_none());
        v.hits.clear();
        v.hits.push(hit(true, 5));
        assert!(b.update(0.016, &v, &net(), false).since_damage.is_some());
    }

    #[test]
    fn katana_cooldown_and_reticle_spread_come_from_local_fx() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.fx.grounded = true;
        v.fx.slice_cooldown = 0.25;
        let still = b.update(0.016, &v, &net(), false);
        assert!((still.slice_ready - 0.75).abs() < 1e-5 && still.spread == 0.0);
        v.fx.speed = 12.0;
        v.fx.grounded = false;
        v.fx.throw_cooldown = 1.0;
        let busy = b.update(0.016, &v, &net(), false);
        assert_eq!(busy.spread, 1.0);
    }

    #[test]
    fn team_info_gives_a_score_bar_team_rows_and_tags_for_allies_only() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.eye = [0.0, 1.5, 0.0];
        v.camera.fov = 90.0;
        let smp = |x: f32, z: f32| shinobi_client_sample([x, 0.5, z]);
        v.remotes = vec![(2, smp(0.0, -12.0)), (3, smp(3.0, -12.0)), (4, smp(0.0, 12.0))];
        v.board = Some(Board { phase: PHASE_LIVE, ticks_left: None, map: 0, winner: 0, rows: vec![BoardRow { id: 2, kills: 1, deaths: 0, alive: true }, BoardRow { id: 3, kills: 2, deaths: 1, alive: true }] });
        b.teams = Some(TeamInfo { my_team: 0, scores: [4, 9], score_limit: 50, result: 0, team_of: [(2, 0), (3, 1), (4, 0)].into_iter().collect() });
        let s = b.update(0.016, &v, &net(), false);
        assert_eq!(s.team_scores, Some(TeamScores { mine: 0, scores: [4, 9], limit: 50, result: 0 }));
        assert_eq!(s.scoreboard.iter().map(|r| r.team).collect::<Vec<_>>(), vec![Some(0), Some(1)]);
        // ally 2 ahead: tagged; enemy 3 ahead: never; ally 4 behind: not projected
        assert_eq!(s.ally_tags.len(), 1);
        assert_eq!(s.ally_tags[0].name, callsign(2));
        assert!(!s.ally_tags[0].clamped);
        // no team info -> free-for-all: nothing team-shaped
        b.teams = None;
        assert!(TeamInfo::from_net(&net(), &v).is_none(), "no server team yet -> free-for-all");
        let s = b.update(0.016, &v, &net(), false);
        assert!(s.team_scores.is_none() && s.ally_tags.is_empty());
    }

    fn shinobi_client_sample(pos: [f32; 3]) -> crate::interp::Sample {
        crate::interp::Sample { pos, yaw: 0.0, extrapolated: false }
    }

    #[test]
    fn free_for_all_gets_unique_colours_a_leaderboard_flag_and_passes_build_and_grapple_through() {
        use crate::colour::player_rgba as ffa_colour;
        use crate::hud::{BuildHud, BuildSlot};
        
        let mut b = HudBridge::default();
        b.kill_limit = Some(25);
        b.build = Some(BuildHud { selected: BuildSlot::Wall, ready: 0.5, placed: 2, max: 6, ghost: Some(false) });
        b.grapple = Some(GrappleHud { ready: 1.0, hookable: true, on_rope: false, missed: false });
        let mut v = RenderView::default();
        v.board = Some(Board { phase: PHASE_LIVE, ticks_left: None, map: 0, winner: 0, rows: vec![BoardRow { id: 2, kills: 1, deaths: 0, alive: true }, BoardRow { id: 5, kills: 0, deaths: 1, alive: true }] });
        let s = b.update(0.016, &v, &net(), false);
        assert!(s.ffa && s.kill_limit == Some(25) && s.team_scores.is_none());
        assert_eq!(s.scoreboard[0].colour, Some(ffa_colour(2)));
        assert_eq!(s.scoreboard[1].colour, Some(ffa_colour(5)));
        assert_eq!(s.build, b.build);
        assert_eq!(s.grapple, b.grapple);
        // teams on: no FFA panel, no kill limit
        b.teams = Some(TeamInfo { my_team: 0, scores: [0, 0], score_limit: 50, result: 0, team_of: Default::default() });
        let s = b.update(0.016, &v, &net(), false);
        assert!(!s.ffa && s.kill_limit.is_none());
    }

    #[test]
    fn cyber_layer_telemetry_radar_without_wallhack_lock_brackets_and_boot() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.connected = true;
        v.eye = [0.0, 2.5, 0.0];
        v.camera.fov = 90.0;
        v.fx.speed = 9.0;
        v.local_pos = Some([0.0, 1.0, 0.0]);
        let smp = |x: f32, z: f32| crate::interp::Sample { pos: [x, 0.5, z], yaw: 0.0, extrapolated: false };
        // near enemy (6 m ahead), far enemy (60 m right), an ally 30 m behind
        v.remotes = vec![(2, smp(0.0, -6.0)), (3, smp(60.0, 0.0)), (4, smp(0.0, 30.0))];
        b.teams = Some(TeamInfo { my_team: 0, scores: [0, 0], score_limit: 50, result: 0, team_of: [(2, 1), (3, 1), (4, 0)].into_iter().collect() });
        let s = b.update(0.016, &v, &net(), false);
        let t = s.telemetry.unwrap();
        assert!((t.speed - 9.0).abs() < 1e-6 && (t.altitude - 2.5).abs() < 1e-6 && t.rope.is_none());
        assert_eq!(s.heading, Some(0.0));
        // enemy 2 close: shown ahead; enemy 3 far and we have not fired: hidden; ally 4 always shown, behind
        assert_eq!(s.radar.len(), 2);
        assert!(s.radar[0].pos.1 > 0.0 && !s.radar[0].ally);
        assert!(s.radar[1].pos.1 < 0.0 && s.radar[1].ally);
        // a throw reveals the far enemy for a couple of seconds
        v.fx.threw = Some([0.0, 0.0, -1.0]);
        let s = b.update(0.016, &v, &net(), false);
        assert_eq!(s.radar.len(), 3);
        v.fx.threw = None;
        // boot-up plays on link-up and ends
        assert!(s.boot.is_some());
        let mut last = s;
        for _ in 0..200 {
            last = b.update(0.02, &v, &net(), false);
        }
        assert!(last.boot.is_none());
        // lock brackets: projected, in the central band
        b.lock = Some(LockInfo { target: 2, pos: [0.0, 1.0, -10.0], progress: 0.4, locked: false });
        let s = b.update(0.016, &v, &net(), false);
        let tg = s.target.expect("target brackets");
        assert!(tg.pos.0 >= SIDE_CLEAR && tg.pos.0 <= 1.0 - SIDE_CLEAR && (tg.dist - 10.1).abs() < 0.3 && !tg.locked);
    }

    #[test]
    fn grapple_readout_follows_the_predicted_state_and_flashes_on_a_miss() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.fx.grapple_cooldown = 0.25;
        v.fx.grapple_hookable = true;
        let g = b.update(0.016, &v, &net(), false).grapple.unwrap();
        assert!((g.ready - 0.75).abs() < 1e-6 && g.hookable && !g.on_rope && !g.missed);
        v.fx.grappling = true;
        assert!(b.update(0.016, &v, &net(), false).grapple.unwrap().on_rope);
        v.fx.grappling = false;
        v.fx.grapple_missed = true;
        assert!(b.update(0.016, &v, &net(), false).grapple.unwrap().missed);
        v.fx.grapple_missed = false;
        assert!(b.update(0.016, &v, &net(), false).grapple.unwrap().missed, "flash lingers briefly");
        for _ in 0..40 {
            b.update(0.016, &v, &net(), false);
        }
        assert!(!b.update(0.016, &v, &net(), false).grapple.unwrap().missed);
    }

    #[test]
    fn build_readout_comes_from_the_predicted_ghost_and_owned_block_count() {
        use crate::client_game::BuildGhost;
        use shinobi_sim::blocks::{BlockInfo, Piece};
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        assert!(b.update(0.016, &v, &net(), false).build.is_none(), "no ghost, no readout");
        v.build_ghost = Some(BuildGhost { cell: [0, 0, 0], piece: Piece::WALL_X, center: [0.0; 3], half: [1.0; 3], rot: [0.0; 3], valid: false, ready: true });
        v.blocks = vec![
            BlockInfo { owner: 0, cell: [0, 0, 0].into(), piece: Piece::FLOOR, scale_q: 0 },
            BlockInfo { owner: 9, cell: [1, 0, 0].into(), piece: Piece::FLOOR, scale_q: 0 },
        ];
        let s = b.update(0.016, &v, &net(), false);
        let bh = s.build.expect("build readout");
        assert_eq!((bh.selected, bh.ghost, bh.max), (BuildSlot::Wall, Some(false), 64));
        assert_eq!(bh.placed, 1, "only our own pieces count (local id 0 offline)");
        assert_eq!(s.kill_limit, Some(ffa_kill_limit()), "ffa default limit");
    }

    #[test]
    fn lock_brackets_follow_the_published_lock_on() {
        use crate::client_game::LockOn;
        use shinobi_sim::combat::aim_assist::LockPhase;
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.eye = [0.0, 1.5, 0.0];
        v.camera.fov = 90.0;
        assert!(b.update(0.016, &v, &net(), false).target.is_none());
        v.lock = Some(LockOn { target: 5, pos: [0.0, 1.5, -20.0], progress: 1.0, locked: true, phase: LockPhase::Locked, eased: 1.0, t_since_phase: 1.0, target_vel: [0.0; 3], blend: 1.0 });
        let t = b.update(0.016, &v, &net(), false).target.expect("brackets");
        assert!(t.locked && t.name == callsign(5) && (t.dist - 20.0).abs() < 0.1);
        assert!((t.pos.0 - 0.5).abs() < 0.02, "dead ahead sits at the centre column");
    }

    #[test]
    fn death_shield_winner_and_audio_reach_the_hud_state() {
        use shinobi_sim::events::NetEvent;
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.connected = true;
        // we (id 0 offline) are killed by player 7 with a slash
        v.events = vec![NetEvent::Kill { killer: 7, victim: 0, kind: shinobi_proto::gameplay::KIND_SLASH, tick: 1 }];
        let s = b.update(0.016, &v, &net(), false);
        assert!(s.killed_by.is_none(), "not dead yet (no snapshot flag), so no overlay");
        b.last_death.as_ref().map(|k| assert_eq!(k.name, callsign(7))).expect("death recorded");
        // intermission: the winner comes from the board; the local player won when ids match
        v.events.clear();
        v.board = Some(Board { phase: PHASE_INTERMISSION, ticks_left: Some(480), map: 0, winner: 7, rows: vec![] });
        let s = b.update(0.016, &v, &net(), false);
        let w = s.winner.expect("winner");
        assert_eq!((w.name.as_str(), w.local, w.draw), (callsign(7).as_str(), false, false));
        v.board = Some(Board { phase: PHASE_INTERMISSION, ticks_left: Some(480), map: 0, winner: 0, rows: vec![] });
        assert!(b.update(0.016, &v, &net(), false).winner.unwrap().draw);
        v.board = Some(Board { phase: PHASE_LIVE, ticks_left: None, map: 0, winner: 0, rows: vec![] });
        assert!(b.update(0.016, &v, &net(), false).winner.is_none(), "no result outside intermission");
        // audio passes through
        b.audio = Some((0.5, true));
        assert_eq!(b.update(0.016, &v, &net(), false).audio, Some((0.5, true)));
    }

    #[test]
    fn a_predicted_katana_hit_confirms_before_the_server_message() {
        use crate::client_game::PredictedSlashHit;
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.fx.slash_hit = Some(PredictedSlashHit { point: [0.0; 3], killed: false });
        let s = b.update(0.016, &v, &net(), false);
        assert!(s.since_hit.is_some() && s.hit_kind == Some(shinobi_sim::combat::DamageKind::Slash) && !s.hit_kill);
        assert_eq!(s.hit_amount, 0, "no number until the server confirms the damage");
        v.fx.slash_hit = Some(PredictedSlashHit { point: [0.0; 3], killed: true });
        assert!(b.update(0.016, &v, &net(), false).hit_kill, "predicted kill shows the kill marker");
    }

    #[test]
    fn kill_limit_prefers_override_then_server_rules_then_the_demo_default() {
        assert_eq!(kill_limit_to_show(Some(3), Some(7)), 3);
        assert_eq!(kill_limit_to_show(None, Some(7)), 7);
        assert_eq!(kill_limit_to_show(None, None), ffa_kill_limit());
        // no rules received yet (default client): the default shows, no time limit
        let mut b = HudBridge::default();
        let s = b.update(0.016, &RenderView::default(), &net(), false);
        assert_eq!((s.kill_limit, s.time_limit_secs), (Some(ffa_kill_limit()), None));
    }

    #[test]
    fn the_killers_position_is_remembered_for_the_death_arrow() {
        use shinobi_sim::events::NetEvent;
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.remotes = vec![(7, crate::interp::Sample { pos: [10.0, 0.5, -20.0], yaw: 0.0, extrapolated: false })];
        v.events = vec![NetEvent::Kill { killer: 7, victim: 0, kind: shinobi_proto::gameplay::KIND_SHURIKEN, tick: 1 }];
        b.update(0.016, &v, &net(), false);
        assert_eq!(b.last_death_pos, Some([10.0, 0.5, -20.0]));
        // a void death has no killer to point at
        v.events = vec![NetEvent::Kill { killer: 0, victim: 0, kind: 200, tick: 2 }];
        b.update(0.016, &v, &net(), false);
        assert_eq!(b.last_death_pos, None);
    }

    #[test]
    fn headshot_flag_reaches_the_hud_state() {
        let mut b = HudBridge::default();
        let mut v = RenderView::default();
        v.hits.push(HitMsg { other: 7, amount: 18, kind: shinobi_proto::gameplay::KIND_SHURIKEN, taken: false, kill: false, head: true });
        let s = b.update(0.016, &v, &net(), false);
        assert!(s.hit_head && s.hit_amount == 18);
        v.hits.clear();
        v.hits.push(HitMsg { other: 7, amount: 12, kind: shinobi_proto::gameplay::KIND_SHURIKEN, taken: false, kill: false, head: false });
        assert!(!b.update(0.016, &v, &net(), false).hit_head, "a body hit replaces the headshot marker");
    }

    #[test]
    fn radar_shows_own_pieces_and_the_void_edge_but_not_others_pieces() {
        use shinobi_sim::blocks::{BlockInfo, Piece};
        let mut b = HudBridge::default();
        b.map_half = Some(60.0);
        let mut v = RenderView::default();
        v.local_pos = Some([0.0, 1.0, 0.0]);
        v.block_size = 4.0;
        v.block_thickness = 0.4;
        v.blocks = vec![
            BlockInfo { owner: 0, cell: [0, 0, -3], piece: Piece::FLOOR, scale_q: 0 },
            BlockInfo { owner: 9, cell: [1, 0, -3], piece: Piece::FLOOR, scale_q: 0 },
        ];
        let s = b.update(0.016, &v, &net(), false);
        assert_eq!(s.radar_pieces.len(), 1, "only our own piece");
        assert!(s.radar_pieces[0].1 > 0.0, "cell z -3 is ahead (north) of us");
        // map edge 60 m away inside the 90 m radar: segments exist and all stay inside the disc
        assert!(!s.radar_edge.is_empty());
        for (p, q) in &s.radar_edge {
            assert!(p.0 * p.0 + p.1 * p.1 <= 1.0001 && q.0 * q.0 + q.1 * q.1 <= 1.0001);
        }
        // no map extent known (or far from every edge): no edge drawn
        b.map_half = None;
        assert!(b.update(0.016, &v, &net(), false).radar_edge.is_empty());
        b.map_half = Some(500.0);
        assert!(b.update(0.016, &v, &net(), false).radar_edge.is_empty(), "edge beyond radar range");
    }

    #[test]
    fn awards_pickups_and_headshots_come_from_the_match_data() {
        use shinobi_proto::gameplay::{KIND_SHURIKEN, KIND_SLASH};
        use shinobi_sim::events::NetEvent;
        let mut b = HudBridge::default();
        b.pickups = vec![[0.0, 1.0, -30.0], [500.0, 1.0, 0.0]];
        let mut v = RenderView::default();
        v.local_pos = Some([0.0, 1.0, 0.0]);
        let smp = |x: f32, z: f32| crate::interp::Sample { pos: [x, 0.5, z], yaw: 0.0, extrapolated: false };
        v.remotes = vec![(7, smp(0.0, -50.0)), (8, smp(10.0, 0.0))];
        v.events = vec![
            NetEvent::Kill { killer: 7, victim: 8, kind: KIND_SHURIKEN, tick: 1 },
            NetEvent::Kill { killer: 7, victim: 8, kind: KIND_SHURIKEN, tick: 2 },
            NetEvent::Kill { killer: 8, victim: 7, kind: KIND_SLASH, tick: 3 },
        ];
        v.hits.push(HitMsg { other: 8, amount: 30, kind: KIND_SHURIKEN, taken: false, kill: false, head: true });
        v.board = Some(Board { phase: PHASE_LIVE, ticks_left: None, map: 0, winner: 0, rows: vec![] });
        let s = b.update(0.016, &v, &net(), false);
        assert_eq!(s.radar_pickups.len(), 1, "only the pickup inside radar range");
        assert!(s.awards.is_empty(), "awards appear only at intermission");
        // intermission: awards from the events
        v.events.clear();
        v.hits.clear();
        v.board = Some(Board { phase: PHASE_INTERMISSION, ticks_left: Some(480), map: 0, winner: 7, rows: vec![] });
        let s = b.update(0.016, &v, &net(), false);
        let by = |t: &str| s.awards.iter().find(|a| a.title == t).cloned();
        assert_eq!(by("SHARPSHOOTER").map(|a| (a.name, a.value)), Some((callsign(7), "2".to_string())));
        assert_eq!(by("BLADEMASTER").map(|a| (a.name, a.value)), Some((callsign(8), "1".to_string())));
        assert_eq!(by("LONGEST KILL").map(|a| a.name), Some(callsign(7)), "7 shot 8 from ~51 m");
        assert_eq!(s.my_headshots, 1);
        // next match: stats reset
        v.board = Some(Board { phase: PHASE_LIVE, ticks_left: None, map: 1, winner: 0, rows: vec![] });
        b.update(0.016, &v, &net(), false);
        v.board = Some(Board { phase: PHASE_INTERMISSION, ticks_left: Some(480), map: 1, winner: 0, rows: vec![] });
        let s = b.update(0.016, &v, &net(), false);
        assert!(s.awards.is_empty() && s.my_headshots == 0, "a fresh match starts from zero");
    }
}
