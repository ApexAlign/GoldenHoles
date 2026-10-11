//! Server-side world effects of the star variants (docs/stars.md). The launcher (`combat/shuriken.rs`) flies, lands and
//! ages the objects; everything that touches other players happens here, once per tick after every player has moved:
//!
//! * FLASH bursts white out (and daze) everyone facing them (`flash_strength`), no line of sight needed; the thrower only
//!   when looking at it from close by.
//! * BARRELS and CANISTERS go off when their fuse runs out, when a barrel strikes a player, or EARLY when anything touches
//!   them: a katana cut, a flying star, an arrow, a tag blast or another explosion. The blast hurts everyone in its radius
//!   with a linear falloff (the thrower too, unless a raised shield covers it), launches them, shakes destructible boxes
//!   and sets off neighbouring explosives (chain reactions). Credit goes to whoever set it off.

use rapier3d::prelude::*;
use shinobi_proto::quant::{dq_pitch, dq_yaw};

use crate::combat::stars::{flash_strength, view_dir, StarKind};
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

fn dist3(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// A barrel or canister somewhere in the world (flying, resting or stuck).
struct Exp {
    owner: usize,
    uid: u16,
    pos: [f32; 3],
    kind: StarKind,
    /// The player it is stuck to (a canister): not shielded from the blast by geometry.
    on: Option<usize>,
    gone: bool,
}

impl Server {
    /// Clear line (no solid geometry) between two points.
    pub(crate) fn los_clear(&self, a: [f32; 3], b: [f32; 3]) -> bool {
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

    /// Flash bursts and explosions for this tick. `ev_start` = index in `self.events` where this tick's combat events
    /// begin; the resulting hits are appended to `hits` (settled by `apply_hits` with everyone else's).
    pub(crate) fn star_pass(&mut self, tick: u32, ev_start: usize, hits: &mut Vec<PvpHit>) {
        let sc = self.balance.combat.star;
        let new_events: Vec<(u16, CombatEvent)> = self.events[ev_start.min(self.events.len())..].to_vec();
        let idx_of = |players: &[Player], id: u16| players.iter().position(|p| p.id == id);
        let mut out_events: Vec<(u16, CombatEvent)> = Vec::new();

        // ---- flash: white-out and daze everyone who faces the burst (no line of sight needed) ----
        for (owner, e) in &new_events {
            let CombatEvent::StarFlash { pos } = *e else { continue };
            let burst = [pos.x, pos.y, pos.z];
            let Some(oi) = idx_of(&self.players, *owner) else { continue };
            let oteam = self.players[oi].team;
            for qi in 0..self.players.len() {
                let q = &self.players[qi];
                if (qi != oi && allied(oteam, q.team)) || !q.vitals.alive() {
                    continue;
                }
                let at = q.pos(&self.world);
                let eye = [at[0], at[1] + q.ch.eye_above(), at[2]];
                // the thrower is only caught looking at it from close by
                if qi == oi && dist3(eye, burst) > sc.flash_self_radius {
                    continue;
                }
                let s = flash_strength(&sc, eye, view_dir(dq_yaw(q.yaw), dq_pitch(q.pitch)), burst);
                if s <= 0.0 {
                    continue;
                }
                let ticks = ((sc.flash_secs * (0.4 + 0.6 * s) * TICK_HZ as f32) as u32).min(u16::MAX as u32);
                let until = tick + ticks;
                let q = &mut self.players[qi];
                q.blind_until = q.blind_until.max(until);
                out_events.push((*owner, CombatEvent::StarBlind { victim: q.id, strength_q: (s * 255.0).round() as u8, ticks: ticks as u16 }));
            }
        }

        // ---- canisters stuck to a player ride along with them ----
        for pi in 0..self.players.len() {
            let mut moves: Vec<(u16, Option<Vector<Real>>, Option<Point<Real>>)> = Vec::new();
            for d in self.players[pi].combat.loadout.discs() {
                let Some(h) = d.attach else { continue };
                let host = self.players.iter().find(|q| q.vitals.hitbox == h);
                match host.filter(|q| q.vitals.alive()) {
                    Some(q) => {
                        let hp = q.pos(&self.world);
                        let hp = Point::new(hp[0], hp[1], hp[2]);
                        let off = d.attach_off.unwrap_or(d.pos - hp);
                        moves.push((d.uid, Some(off), Some(hp + off)));
                    }
                    None => moves.push((d.uid, None, None)), // the host died / left: it drops where it is
                }
            }
            if let Some(discs) = self.players[pi].combat.loadout.discs_mut() {
                for (uid, off, to) in moves {
                    if let Some(d) = discs.iter_mut().find(|d| d.uid == uid) {
                        match (off, to) {
                            (Some(o), Some(p)) => {
                                d.attach_off = Some(o);
                                d.pos = p;
                            }
                            _ => {
                                d.attach = None;
                                d.attach_off = None;
                            }
                        }
                    }
                }
            }
        }

        // ---- explosives ----
        let mut exps: Vec<Exp> = Vec::new();
        // standard stars flying this tick: (previous position, position)
        let mut flying: Vec<([f32; 3], [f32; 3])> = Vec::new();
        let speed = self.balance.combat.shuriken.speed;
        for (pi, p) in self.players.iter().enumerate() {
            for d in p.combat.loadout.discs() {
                let pos = [d.pos.x, d.pos.y, d.pos.z];
                if d.kind.explosive() {
                    let on = d.attach.and_then(|h| self.players.iter().position(|q| q.vitals.hitbox == h));
                    exps.push(Exp { owner: pi, uid: d.uid, pos, kind: d.kind, on, gone: false });
                } else if d.kind == StarKind::Standard && !d.stuck {
                    let back = d.dir * (speed * d.speed_scale / TICK_HZ as f32);
                    flying.push(([pos[0] - back.x, pos[1] - back.y, pos[2] - back.z], pos));
                }
            }
        }
        let blasts_in = std::mem::take(&mut self.pending_blasts);
        if exps.is_empty() {
            self.events.extend(out_events);
            return;
        }
        let tag_r = self.balance.combat.power.tag.radius;
        let hit_r = sc.barrel_hit_radius;
        // (explosive index, id of whoever set it off)
        let mut queue: Vec<(usize, u16)> = Vec::new();
        for (xi, x) in exps.iter().enumerate() {
            let me = self.players[x.owner].id;
            // fuse out
            let own = self.players[x.owner].combat.loadout.discs().iter().find(|d| d.uid == x.uid).copied();
            if own.map_or(false, |d| d.fuse_done(&sc)) {
                queue.push((xi, me));
                continue;
            }
            // a flying star passes through it
            if flying.iter().any(|(a, b)| seg_dist(*a, *b, x.pos) <= hit_r) {
                // credit the nearest flying star's owner: the shooter (falls back to the barrel's owner)
                let shooter = self
                    .players
                    .iter()
                    .filter(|p| p.combat.loadout.discs().iter().any(|d| {
                        d.kind == StarKind::Standard && !d.stuck && {
                            let pos = [d.pos.x, d.pos.y, d.pos.z];
                            let back = d.dir * (speed * d.speed_scale / TICK_HZ as f32);
                            seg_dist([pos[0] - back.x, pos[1] - back.y, pos[2] - back.z], pos, x.pos) <= hit_r
                        }
                    }))
                    .map(|p| p.id)
                    .next()
                    .unwrap_or(me);
                queue.push((xi, shooter));
                continue;
            }
            // a katana cut
            let cut = new_events.iter().find_map(|(id, e)| match *e {
                CombatEvent::Slash { origin, dir, reach, half_width, .. } => {
                    let a = [origin.x, origin.y, origin.z];
                    (seg_dist(a, [a[0] + dir.x * reach, a[1] + dir.y * reach, a[2] + dir.z * reach], x.pos) <= half_width + hit_r * 0.5).then_some(*id)
                }
                _ => None,
            });
            if let Some(by) = cut {
                queue.push((xi, by));
                continue;
            }
            // an arrow
            let arrow = self.arrows.iter().find(|a| seg_dist([a.pos[0] - a.vel[0] / TICK_HZ as f32, a.pos[1] - a.vel[1] / TICK_HZ as f32, a.pos[2] - a.vel[2] / TICK_HZ as f32], [a.pos[0] + a.vel[0] / TICK_HZ as f32, a.pos[1] + a.vel[1] / TICK_HZ as f32, a.pos[2] + a.vel[2] / TICK_HZ as f32], x.pos) <= hit_r);
            if let Some(a) = arrow {
                queue.push((xi, a.owner));
                continue;
            }
            // a tag blast
            if let Some((by, _)) = blasts_in.iter().find(|(_, c)| dist3(*c, x.pos) <= tag_r) {
                queue.push((xi, *by));
            }
        }

        // detonate, chaining through neighbours
        let bcfg = self.balance.guard.cfg();
        let mcfg = self.balance.melee.cfg();
        let mut qi = 0;
        while qi < queue.len() {
            let (xi, by) = queue[qi];
            qi += 1;
            if exps[xi].gone {
                continue;
            }
            exps[xi].gone = true;
            let (owner, uid, centre, kind, on) = (exps[xi].owner, exps[xi].uid, exps[xi].pos, exps[xi].kind, exps[xi].on);
            let oid = self.players[owner].id;
            if let Some(d) = self.players[owner].combat.loadout.discs_mut() {
                d.retain(|x| x.uid != uid);
            }
            let Some(blast) = sc.blast(kind) else { continue };
            let at = Point::new(centre[0], centre[1], centre[2]);
            out_events.push((oid, CombatEvent::StarBurst { pos: at, kind }));
            // neighbouring explosives go off with it
            for (xj, y) in exps.iter().enumerate() {
                if !y.gone && dist3(y.pos, centre) <= blast.radius {
                    queue.push((xj, by));
                }
            }
            // destructible boxes (`blast_mult` applies inside)
            if !self.breaks.is_empty() {
                let (r, bl) = (blast.radius, blast);
                self.breaks.blast(&mut self.world, centre, r, |d| bl.damage * bl.falloff(d), tick, &mut self.break_events);
            }
            let ai = idx_of(&self.players, by);
            let ateam = ai.map(|a| self.players[a].team);
            let from = [centre[0], centre[1] + 0.3, centre[2]];
            for vj in 0..self.players.len() {
                if !hittable(&self.players[vj], tick) {
                    continue;
                }
                // the attacker's own side is spared (the attacker itself is not: too close hurts yourself)
                if Some(vj) != ai && ateam.map_or(false, |t| allied(t, self.players[vj].team)) {
                    continue;
                }
                let vpos = self.players[vj].pos(&self.world);
                let d = dist3(vpos, centre);
                if d > blast.radius || (on != Some(vj) && !self.los_clear(from, vpos)) {
                    continue;
                }
                let f = blast.falloff(d);
                let raw = blast.damage * f;
                let (mut amount, mut knock) = (raw, f);
                // a raised shield facing the blast takes it as a blocked hit (reduced damage, guard spent)
                let vyaw = dq_yaw(self.players[vj].yaw);
                let guard = crate::block::guard_vs(&self.players[vj].blk, vyaw, vpos, centre, tick, &bcfg);
                if matches!(guard, crate::block::Guard::Blocked | crate::block::Guard::Parried) {
                    amount = raw * bcfg.front_mult;
                    knock *= 0.35;
                    let v = &mut self.players[vj];
                    v.blk.energy -= bcfg.hit_cost * (raw / 20.0).min(2.5);
                    if v.blk.energy <= 0.0 {
                        v.blk.break_guard(tick, &bcfg);
                        v.blk.stagger_until = tick + bcfg.break_stagger_ticks;
                    }
                }
                let v = &mut self.players[vj];
                hits.push(PvpHit { attacker: by, victim: vj, amount, cause: Cause::Weapon(DamageKind::Explosive), head: false });
                out_events.push((oid, CombatEvent::Damage { target: v.id as u128, collider: v.vitals.hitbox, amount, kind: DamageKind::Explosive, point: at }));
                // radial shove plus a launch, both scaled by the same falloff
                let away = Vector::new(vpos[0] - centre[0], vpos[1] + 0.3 - centre[1], vpos[2] - centre[2]);
                let dir = if away.norm() > 0.2 { away.normalize() } else { Vector::y() };
                let vel = dir * (blast.knock * knock) + Vector::y() * (blast.launch * knock);
                v.ch.vel = vel;
                if matches!(v.ch.state, crate::character::MoveState::Grounded) {
                    v.ch.state = crate::character::MoveState::Airborne;
                }
                if v.id != by {
                    v.melee.last_hit = Some((by, tick));
                }
                v.melee.knock_until = tick + mcfg.splat_ticks;
                v.melee.knock_speed = blast.knock * knock;
                v.melee.knock_by = by;
            }
        }
        self.events.extend(out_events);
    }
}
