//! Server-side world effects of the star variants (docs/stars.md). The launcher (`combat/shuriken.rs`) flies, lands and
//! ages the objects; everything that touches other players happens here, once per tick after every player has moved:
//!
//! * FLASH bursts white out enemies who face them (`flash_strength`) with a line of sight to the burst.
//! * MINES: shot / cut mines are disarmed; armed mines whose trigger radius holds an enemy burst (damage + knockback).
//! * SMOKE: `Server::in_smoke` says who is hidden from lock-on this tick (the player loop feeds it to combat).

use rapier3d::prelude::*;
use shinobi_proto::quant::{dq_pitch, dq_yaw};

use crate::combat::stars::{flash_strength, view_dir, within, StarKind};
use crate::combat::{solid_groups, CombatEvent, DamageKind, GROUP_PROJECTILE};
use crate::match_state::TICK_HZ;
use crate::pvp::{Cause, PvpHit};
use crate::tick::{allied, Player, Server};

fn hittable(p: &Player, tick: u32) -> bool {
    p.vitals.alive() && !p.vitals.spawn_protected(tick) && !p.ch.invulnerable()
}

/// Distance from `p` to the segment `a`..`b`.
fn seg_dist(a: [f32; 3], b: [f32; 3], p: [f32; 3]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ap = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
    let t = if len2 < 1e-9 { 0.0 } else { ((ap[0] * ab[0] + ap[1] * ab[1] + ap[2] * ab[2]) / len2).clamp(0.0, 1.0) };
    let q = [a[0] + ab[0] * t - p[0], a[1] + ab[1] * t - p[1], a[2] + ab[2] * t - p[2]];
    (q[0] * q[0] + q[1] * q[1] + q[2] * q[2]).sqrt()
}

enum MineFate {
    Disarm,
    Burst(Vec<usize>),
}

impl Server {
    /// Clear line (no solid geometry) between two points.
    fn los_clear(&self, a: [f32; 3], b: [f32; 3]) -> bool {
        let d = vector![b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let len = d.norm();
        if len < 1e-3 {
            return true;
        }
        let ray = Ray::new(Point::from(vector![a[0], a[1], a[2]]), d / len);
        let pass = |h: ColliderHandle, c: &Collider| crate::combat::passes_projectile(h, c);
        let filter = QueryFilter::default().predicate(&pass).groups(InteractionGroups::new(GROUP_PROJECTILE, solid_groups()));
        self.world.queries.cast_ray(&self.world.bodies, &self.world.colliders, &ray, (len - 0.05).max(0.0), true, filter).is_none()
    }

    /// Per player (in `players` order): standing inside any smoke cloud right now.
    pub(crate) fn in_smoke(&self) -> Vec<bool> {
        let r = self.balance.combat.star.smoke_radius;
        let clouds: Vec<[f32; 3]> = self
            .players
            .iter()
            .flat_map(|p| p.combat.loadout.discs())
            .filter(|d| d.kind == StarKind::Smoke && d.stuck)
            .map(|d| d.cloud_centre())
            .collect();
        if clouds.is_empty() {
            return vec![false; self.players.len()];
        }
        self.players.iter().map(|p| clouds.iter().any(|c| within(p.pos(&self.world), *c, r))).collect()
    }

    /// Flash bursts, mine disarms and bursts for this tick. `ev_start` = index in `self.events` where this tick's
    /// combat events begin; the resulting hits are appended to `hits` (settled by `apply_hits` with everyone else's).
    pub(crate) fn star_pass(&mut self, tick: u32, ev_start: usize, hits: &mut Vec<PvpHit>) {
        let sc = self.balance.combat.star;
        let new_events: Vec<(u16, CombatEvent)> = self.events[ev_start.min(self.events.len())..].to_vec();
        let idx_of = |players: &[Player], id: u16| players.iter().position(|p| p.id == id);

        // ---- flash: white-out everyone who faces the burst ----
        let mut out_events: Vec<(u16, CombatEvent)> = Vec::new();
        for (owner, e) in &new_events {
            let CombatEvent::StarFlash { pos } = *e else { continue };
            let burst = [pos.x, pos.y, pos.z];
            let Some(oi) = idx_of(&self.players, *owner) else { continue };
            let oteam = self.players[oi].team;
            for qi in 0..self.players.len() {
                let q = &self.players[qi];
                if qi == oi || allied(oteam, q.team) || !q.vitals.alive() {
                    continue;
                }
                let at = q.pos(&self.world);
                let eye = [at[0], at[1] + q.ch.eye_above(), at[2]];
                let s = flash_strength(&sc, eye, view_dir(dq_yaw(q.yaw), dq_pitch(q.pitch)), burst);
                if s <= 0.0 || !self.los_clear(eye, burst) {
                    continue;
                }
                let ticks = ((sc.flash_secs * (0.4 + 0.6 * s) * TICK_HZ as f32) as u32).min(u16::MAX as u32);
                let until = tick + ticks;
                let q = &mut self.players[qi];
                q.blind_until = q.blind_until.max(until);
                out_events.push((*owner, CombatEvent::StarBlind { victim: q.id, strength_q: (s * 255.0).round() as u8, ticks: ticks as u16 }));
            }
        }

        // ---- mines ----
        // every landed mine as (owner index, uid, position, armed)
        let mut mines: Vec<(usize, u16, [f32; 3], bool)> = Vec::new();
        // enemy discs flying this tick as (owner index, previous position, position)
        let mut flying: Vec<(usize, [f32; 3], [f32; 3])> = Vec::new();
        let speed = self.balance.combat.shuriken.speed;
        for (pi, p) in self.players.iter().enumerate() {
            for d in p.combat.loadout.discs() {
                let pos = [d.pos.x, d.pos.y, d.pos.z];
                if d.kind == StarKind::Mine && d.stuck {
                    mines.push((pi, d.uid, pos, d.armed(&sc)));
                } else if d.kind == StarKind::Standard {
                    let back = d.dir * (speed * d.speed_scale / TICK_HZ as f32);
                    flying.push((pi, [pos[0] - back.x, pos[1] - back.y, pos[2] - back.z], pos));
                }
            }
        }
        if mines.is_empty() && out_events.is_empty() {
            return;
        }
        let slashes: Vec<(usize, [f32; 3], [f32; 3], f32)> = new_events
            .iter()
            .filter_map(|(id, e)| match *e {
                CombatEvent::Slash { origin, dir, reach, half_width, .. } => {
                    let a = [origin.x, origin.y, origin.z];
                    Some((idx_of(&self.players, *id)?, a, [a[0] + dir.x * reach, a[1] + dir.y * reach, a[2] + dir.z * reach], half_width))
                }
                _ => None,
            })
            .collect();

        let mut fates: Vec<(usize, u16, [f32; 3], MineFate)> = Vec::new();
        for (oi, uid, mpos, armed) in mines {
            let oteam = self.players[oi].team;
            let foe = |pj: usize| pj != oi && !allied(oteam, self.players[pj].team);
            // shot by a star or cut by a katana: disarmed, no bang
            let shot = flying.iter().any(|(pj, a, b)| foe(*pj) && seg_dist(*a, *b, mpos) <= sc.mine_disarm_radius);
            let cut = slashes.iter().any(|(pj, a, b, hw)| foe(*pj) && seg_dist(*a, *b, mpos) <= hw + 0.3);
            if shot || cut {
                fates.push((oi, uid, mpos, MineFate::Disarm));
                continue;
            }
            if !armed {
                continue;
            }
            let from = [mpos[0], mpos[1] + 0.15, mpos[2]];
            let burst_r = sc.mine_burst_radius.max(sc.mine_trigger_radius);
            let victims: Vec<usize> = (0..self.players.len())
                .filter(|&j| foe(j) && hittable(&self.players[j], tick))
                .filter(|&j| within(self.players[j].pos(&self.world), mpos, burst_r))
                .filter(|&j| self.los_clear(from, self.players[j].pos(&self.world)))
                .collect();
            if victims.iter().any(|&j| within(self.players[j].pos(&self.world), mpos, sc.mine_trigger_radius)) {
                fates.push((oi, uid, mpos, MineFate::Burst(victims)));
            }
        }
        for (oi, uid, mpos, fate) in fates {
            let oid = self.players[oi].id;
            if let Some(d) = self.players[oi].combat.loadout.discs_mut() {
                d.retain(|x| x.uid != uid);
            }
            let at = Point::new(mpos[0], mpos[1], mpos[2]);
            match fate {
                MineFate::Disarm => out_events.push((oid, CombatEvent::StarGone { kind: StarKind::Mine, pos: at })),
                MineFate::Burst(victims) => {
                    out_events.push((oid, CombatEvent::StarBurst { pos: at }));
                    for vj in victims {
                        let vpos = self.players[vj].pos(&self.world);
                        let v = &mut self.players[vj];
                        hits.push(PvpHit { attacker: oid, victim: vj, amount: sc.mine_damage, cause: Cause::Weapon(DamageKind::Mine), head: false });
                        out_events.push((oid, CombatEvent::Damage { target: v.id as u128, collider: v.vitals.hitbox, amount: sc.mine_damage, kind: DamageKind::Mine, point: at }));
                        if let Some(push) = crate::katana_moves::shock_push(mpos, vpos, sc.mine_burst_radius.max(sc.mine_trigger_radius), sc.mine_knock) {
                            v.ch.vel = Vector::new(push[0], push[1], push[2]);
                            if matches!(v.ch.state, crate::character::MoveState::Grounded) {
                                v.ch.state = crate::character::MoveState::Airborne;
                            }
                        }
                        v.melee.last_hit = Some((oid, tick));
                    }
                }
            }
        }
        self.events.extend(out_events);
    }
}
