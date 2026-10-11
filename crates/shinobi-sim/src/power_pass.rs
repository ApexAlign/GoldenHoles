//! Server-side world half of the power weapons (docs/powerups.md). `power.rs` holds what a player carries; this file flies the
//! arrows, sticks and counts down the fuda tags and resolves their hits, once per tick after every player has moved
//! (like `star_pass.rs`):
//!
//! * ARROWS fly with a little gravity and are swept against solids and living foes. A raised shield blocks (damage cut to
//!   `front_mult`, a big bite out of the guard), a fresh one PARRIES (the arrow turns around toward the shooter), a katana
//!   blade crossing the arrow cuts it out of the air.
//! * TAGS fly, stick to the first surface or foe, arm, and go off after the fuse or on the owner's press: a radial blast with
//!   falloff damage and a strong, wall-splat / ring-out compatible launch (`melee` bookkeeping). The owner takes
//!   `self_mult` of it, teammates nothing. A katana crossing a tag cuts it off (no blast).

use rapier3d::prelude::*;
use shinobi_proto::quant::dq_yaw;
use shinobi_proto::TICK_DT;

use crate::combat::{solid_groups, CombatEvent, DamageKind, GROUP_PROJECTILE};
use crate::power::*;
use crate::pvp::{Cause, PvpHit};
use crate::tick::{allied, Player, Server};

/// A yumi arrow in flight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arrow {
    pub owner: u16,
    pub uid: u16,
    pub pos: V3,
    pub vel: V3,
    /// Where it left the bow / the parrying shield (guard arcs are judged from here).
    pub origin: V3,
    pub age: f32,
    /// Turned around by a parry: the original shooter is its target now.
    pub reflected: bool,
}

/// A fuda tag: flying, then stuck to a surface or to a player.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tag {
    pub owner: u16,
    pub uid: u16,
    pub pos: V3,
    pub vel: V3,
    /// Surface normal (glyph orientation) once stuck.
    pub normal: V3,
    pub stuck: bool,
    /// Seconds in flight, then seconds since sticking.
    pub age: f32,
    /// Stuck to this player, at this offset from their centre.
    pub on: Option<(u16, V3)>,
    armed_sent: bool,
}

impl Tag {
    /// A tag already stuck at `pos` on a surface facing `normal` (tests, scripted scenes).
    pub fn stuck_at(owner: u16, uid: u16, pos: V3, normal: V3) -> Self {
        Self { owner, uid, pos, vel: [0.0; 3], normal, stuck: true, age: 0.0, on: None, armed_sent: false }
    }

    pub fn armed(&self, cfg: &TagCfg) -> bool {
        self.stuck && self.age >= cfg.arm_s
    }

    /// Fuse left, 0..=1 (1 = just stuck).
    pub fn fuse_left(&self, cfg: &TagCfg) -> f32 {
        if self.stuck {
            (1.0 - self.age / cfg.fuse_s.max(1e-3)).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }
}

/// A launch decided by a player's `PowerState` in the player loop, carried out in the pass (it needs the whole world).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Launch {
    pub owner: u16,
    pub ev: PowerEvent,
    pub eye: V3,
    pub dir: V3,
}

/// `CombatEvent::PowerFx::what` / `NetEvent::PowerFx::what`.
pub const FX_PICKUP_YUMI: u8 = 1;
pub const FX_PICKUP_TAG: u8 = 2;
pub const FX_DRAW: u8 = 3;
pub const FX_LOOSE: u8 = 4;
pub const FX_ARROW_HIT: u8 = 5;
pub const FX_ARROW_BLOCKED: u8 = 6;
pub const FX_ARROW_PARRIED: u8 = 7;
pub const FX_ARROW_CUT: u8 = 8;
pub const FX_ARROW_WALL: u8 = 9;
pub const FX_TAG_THROW: u8 = 10;
pub const FX_TAG_STUCK: u8 = 11;
pub const FX_TAG_ARMED: u8 = 12;
pub const FX_TAG_BLAST: u8 = 13;
pub const FX_TAG_CUT: u8 = 14;
pub const FX_DROP: u8 = 15;
pub const FX_ARROW_HEAD: u8 = 16;
pub const FX_EXPIRED: u8 = 17;

pub(crate) fn pt(v: V3) -> Point<Real> {
    Point::new(v[0], v[1], v[2])
}

pub(crate) fn hittable(p: &Player, tick: u32) -> bool {
    p.vitals.alive() && !p.vitals.spawn_protected(tick) && !p.ch.invulnerable()
}

/// First solid in the way of the segment `a..b`: (distance along it, surface normal).
fn solid_hit(s: &Server, a: V3, b: V3) -> Option<(f32, V3)> {
    let d = sub(b, a);
    let l = len(d);
    if l < 1e-6 {
        return None;
    }
    let ray = Ray::new(pt(a), vector![d[0] / l, d[1] / l, d[2] / l]);
    let pass = |h: ColliderHandle, c: &Collider| crate::combat::passes_projectile(h, c);
    let filter = QueryFilter::default().predicate(&pass).groups(InteractionGroups::new(GROUP_PROJECTILE, solid_groups()));
    s.world.queries.cast_ray_and_get_normal(&s.world.bodies, &s.world.colliders, &ray, l, true, filter).map(|(_, h)| (h.time_of_impact, [h.normal.x, h.normal.y, h.normal.z]))
}

/// One katana swing this tick: who, and the blade's centre line.
struct Swing {
    owner: u16,
    a: V3,
    b: V3,
    half_width: f32,
}

impl Server {
    /// Live tags of `owner`.
    pub fn live_tags(&self, owner: u16) -> u32 {
        self.tags.iter().filter(|t| t.owner == owner).count() as u32
    }

    /// Armed, live tags of `owner` (what a press with empty hands sets off).
    pub fn armed_tags(&self, owner: u16) -> u32 {
        let c = self.balance.combat.power.tag;
        self.tags.iter().filter(|t| t.owner == owner && t.armed(&c)).count() as u32
    }

    fn next_uid(&mut self) -> u16 {
        self.power_uid = self.power_uid.wrapping_add(1);
        self.power_uid
    }

    fn player_index(&self, id: u16) -> Option<usize> {
        self.players.iter().position(|p| p.id == id)
    }

    /// The owner's dead weapon lies where it died, for anyone with empty hands (`pickups.rs` one-shot loot).
    pub(crate) fn drop_power_weapon(&mut self, i: usize, tick: u32) {
        let (kind, ammo, left) = {
            let p = &self.players[i];
            (p.power.kind, p.power.ammo, p.power.left)
        };
        if kind == PowerKind::None {
            return;
        }
        let pos = self.players[i].pos(&self.world);
        let pcfg = self.balance.combat.power;
        self.drop_seq = self.drop_seq.wrapping_add(1);
        // a spent tag bag is nothing to pick up
        if ammo > 0 {
            self.pickups.drop_loot(
                crate::pickups::PickupPoint {
                    id: POWER_DROP_ID_BASE + self.drop_seq,
                    pos: [pos[0], pos[1] - 0.2, pos[2]],
                    kind: crate::combat::powerup::PowerupKind::GridBeam,
                    respawn_ticks: 0,
                    charge_secs: left.max(5.0),
                    weapon: kind,
                    ammo,
                },
                tick.wrapping_add((pcfg.drop_life_s(kind) * crate::match_state::TICK_HZ as f32) as u32),
            );
            let id = self.players[i].id;
            self.events.push((id, CombatEvent::PowerFx { what: FX_DROP, pos: pt(pos) }));
        }
        self.players[i].power.clear();
    }

    /// Arrows, tags, blasts for this tick. `ev_start`: first index of this tick's combat events.
    pub(crate) fn power_pass(&mut self, tick: u32, ev_start: usize, hits: &mut Vec<PvpHit>) {
        let pcfg = self.balance.combat.power;
        let (yc, tc) = (pcfg.yumi, pcfg.tag);
        let bcfg = self.balance.guard.cfg();
        let mcfg = self.balance.melee.cfg();
        let dt = TICK_DT;
        let mut fx: Vec<(u16, CombatEvent)> = Vec::new();
        let swings: Vec<Swing> = self.events[ev_start.min(self.events.len())..]
            .iter()
            .filter_map(|(id, e)| match *e {
                CombatEvent::Slash { origin, dir, reach, half_width, .. } => {
                    let a = [origin.x, origin.y, origin.z];
                    Some(Swing { owner: *id, a, b: add(a, scale([dir.x, dir.y, dir.z], reach)), half_width })
                }
                _ => None,
            })
            .collect();

        // ---- launches decided in the player loop ----
        let launches = std::mem::take(&mut self.power_launch);
        let mut detonate: Vec<u16> = Vec::new();
        for l in launches {
            match l.ev {
                PowerEvent::Loose => {
                    let uid = self.next_uid();
                    let pos = add(l.eye, scale(l.dir, 0.6));
                    self.arrows.push(Arrow { owner: l.owner, uid, pos, vel: scale(l.dir, yc.speed), origin: l.eye, age: 0.0, reflected: false });
                    fx.push((l.owner, CombatEvent::PowerFx { what: FX_LOOSE, pos: pt(l.eye) }));
                }
                PowerEvent::TagThrow => {
                    if self.live_tags(l.owner) >= tc.max_live {
                        continue;
                    }
                    let uid = self.next_uid();
                    let pos = add(l.eye, scale(l.dir, 0.6));
                    self.tags.push(Tag { owner: l.owner, uid, pos, vel: scale(l.dir, tc.throw_speed), normal: [0.0, 1.0, 0.0], stuck: false, age: 0.0, on: None, armed_sent: false });
                    fx.push((l.owner, CombatEvent::PowerFx { what: FX_TAG_THROW, pos: pt(l.eye) }));
                }
                PowerEvent::TagDetonate => detonate.push(l.owner),
                PowerEvent::DrawStart => fx.push((l.owner, CombatEvent::PowerFx { what: FX_DRAW, pos: pt(l.eye) })),
                _ => {}
            }
        }

        // ---- tags ----
        let mut blasts: Vec<Tag> = Vec::new();
        let mut i = 0;
        while i < self.tags.len() {
            let mut t = self.tags[i];
            let owner_ok = self.player_index(t.owner).map_or(false, |o| self.players[o].vitals.alive());
            if !owner_ok {
                self.tags.remove(i);
                continue;
            }
            let oteam = self.players[self.player_index(t.owner).unwrap()].team;
            // a blade from anyone but the owner cuts it off, flying or stuck
            let cut = swings.iter().any(|s| s.owner != t.owner && seg_seg(s.a, s.b, t.pos, t.pos).0 <= s.half_width + tc.cut_pad);
            if cut {
                fx.push((t.owner, CombatEvent::PowerFx { what: FX_TAG_CUT, pos: pt(t.pos) }));
                self.tags.remove(i);
                continue;
            }
            if !t.stuck {
                let mut vel = t.vel;
                vel[1] -= tc.gravity * dt;
                let a = t.pos;
                let b = add(a, scale(vel, dt));
                t.age += dt;
                // nearest of: a foe's body, a solid
                let mut best: Option<(f32, Option<usize>, V3)> = None; // (distance, victim index, normal)
                let seglen = len(sub(b, a));
                if let Some((d, n)) = solid_hit(self, a, b) {
                    best = Some((d, None, n));
                }
                for (j, q) in self.players.iter().enumerate() {
                    if q.id == t.owner || !hittable(q, tick) {
                        continue;
                    }
                    let c = q.pos(&self.world);
                    let hh = self.cfg().half_height;
                    let (dist, s) = seg_seg(a, b, [c[0], c[1] - hh, c[2]], [c[0], c[1] + hh, c[2]]);
                    if dist <= self.cfg().radius + tc.stick_radius && best.map_or(true, |(bd, _, _)| s * seglen < bd) {
                        best = Some((s * seglen, Some(j), [0.0, 1.0, 0.0]));
                    }
                }
                match best {
                    Some((d, victim, n)) => {
                        let hit = add(a, scale(norm(sub(b, a)), d));
                        t.stuck = true;
                        t.age = 0.0;
                        t.vel = [0.0; 3];
                        match victim {
                            Some(j) => {
                                let c = self.players[j].pos(&self.world);
                                // pinned on the torso, on the side it came from
                                let off = sub(hit, c);
                                let off = [off[0], off[1].clamp(-0.5, 0.5), off[2]];
                                t.on = Some((self.players[j].id, off));
                                t.pos = add(c, off);
                                t.normal = norm(scale(sub(b, a), -1.0));
                            }
                            None => {
                                t.pos = add(hit, scale(n, 0.03));
                                t.normal = n;
                            }
                        }
                        fx.push((t.owner, CombatEvent::PowerFx { what: FX_TAG_STUCK, pos: pt(t.pos) }));
                    }
                    None => {
                        t.pos = b;
                        t.vel = vel;
                        if t.age > tc.flight_s || b[1] < self.kill_plane_y {
                            fx.push((t.owner, CombatEvent::PowerFx { what: FX_TAG_CUT, pos: pt(t.pos) }));
                            self.tags.remove(i);
                            continue;
                        }
                    }
                }
            } else {
                t.age += dt;
                if let Some((vid, off)) = t.on {
                    match self.player_index(vid).filter(|&j| self.players[j].vitals.alive()) {
                        Some(j) => t.pos = add(self.players[j].pos(&self.world), off),
                        None => {
                            self.tags.remove(i);
                            continue;
                        }
                    }
                }
                if t.armed(&tc) && !t.armed_sent {
                    t.armed_sent = true;
                    fx.push((t.owner, CombatEvent::PowerFx { what: FX_TAG_ARMED, pos: pt(t.pos) }));
                }
                let by_hand = t.armed(&tc) && detonate.contains(&t.owner);
                if t.age >= tc.fuse_s || by_hand {
                    blasts.push(t);
                    self.tags.remove(i);
                    continue;
                }
            }
            let _ = oteam;
            self.tags[i] = t;
            i += 1;
        }
        for t in blasts {
            self.blast(&t, tick, &tc, &mcfg, hits, &mut fx);
        }

        // ---- arrows ----
        let hh = self.cfg().half_height;
        let body_r = self.cfg().radius;
        let mut i = 0;
        while i < self.arrows.len() {
            let mut a = self.arrows[i];
            let Some(oi) = self.player_index(a.owner) else {
                // the shooter left: the arrow still flies as a neutral shot (nobody allied to it), credited to nobody
                self.arrows.remove(i);
                continue;
            };
            let oteam = self.players[oi].team;
            let from = a.pos;
            a.vel[1] -= yc.gravity * dt;
            let to = add(from, scale(a.vel, dt));
            a.age += dt;
            let seglen = len(sub(to, from));
            // cut out of the air by a foe's blade
            let cut = swings.iter().any(|s| {
                let foe = s.owner != a.owner && self.player_index(s.owner).map_or(false, |k| !allied(oteam, self.players[k].team));
                foe && seg_seg(s.a, s.b, from, to).0 <= s.half_width + yc.cut_pad
            });
            if cut {
                fx.push((a.owner, CombatEvent::PowerFx { what: FX_ARROW_CUT, pos: pt(from) }));
                self.arrows.remove(i);
                continue;
            }
            let wall = solid_hit(self, from, to);
            let mut victim: Option<(f32, usize, V3)> = None; // (distance along the step, index, hit point)
            for (j, q) in self.players.iter().enumerate() {
                if q.id == a.owner || allied(oteam, q.team) || !hittable(q, tick) {
                    continue;
                }
                let c = q.pos(&self.world);
                let (dist, s) = seg_seg(from, to, [c[0], c[1] - hh, c[2]], [c[0], c[1] + hh, c[2]]);
                if dist <= body_r + yc.radius && victim.map_or(true, |(vd, _, _)| s * seglen < vd) {
                    victim = Some((s * seglen, j, add(from, scale(sub(to, from), s))));
                }
            }
            let wall_first = match (wall, victim) {
                (Some((wd, _)), Some((vd, _, _))) => wd < vd,
                (Some(_), None) => true,
                _ => false,
            };
            if wall_first {
                let (wd, _) = wall.unwrap();
                let p = add(from, scale(norm(sub(to, from)), wd));
                fx.push((a.owner, CombatEvent::PowerFx { what: FX_ARROW_WALL, pos: pt(p) }));
                // an arrow into a destructible box chips it (about 0.6 x an arrow's damage after `blast_mult`)
                if !self.breaks.is_empty() {
                    let chip = yc.damage * 0.12 * self.breaks.cfg.chip_mult;
                    self.breaks.blast(&mut self.world, p, 0.8, |_| chip, tick, &mut self.break_events);
                }
                self.arrows.remove(i);
                continue;
            }
            if let Some((_, j, hitp)) = victim {
                let q = &self.players[j];
                let vpos = q.pos(&self.world);
                let guard = crate::block::guard_vs(&q.blk, dq_yaw(q.yaw), vpos, a.origin, tick, &bcfg);
                let vid = q.id;
                match guard {
                    crate::block::Guard::Parried => {
                        // the arrow turns around and goes for whoever loosed it
                        let shooter = Some(oi).map(|k| {
                            let c = self.players[k].pos(&self.world);
                            [c[0], c[1] + 0.4, c[2]]
                        });
                        let dir = match shooter {
                            Some(s) => norm(sub(s, hitp)),
                            None => norm(scale(a.vel, -1.0)),
                        };
                        let speed = len(a.vel);
                        a.owner = vid;
                        a.vel = scale(dir, speed);
                        a.pos = add(hitp, scale(dir, 0.7));
                        a.origin = hitp;
                        a.age = 0.0;
                        a.reflected = true;
                        fx.push((vid, CombatEvent::PowerFx { what: FX_ARROW_PARRIED, pos: pt(hitp) }));
                        self.arrows[i] = a;
                        i += 1;
                        continue;
                    }
                    crate::block::Guard::Blocked => {
                        let v = &mut self.players[j];
                        v.blk.energy -= yc.block_drain;
                        if v.blk.energy <= 0.0 {
                            v.blk.break_guard(tick, &bcfg);
                            v.blk.stagger_until = tick + bcfg.break_stagger_ticks;
                        }
                        hits.push(PvpHit { attacker: a.owner, victim: j, amount: yc.damage * bcfg.front_mult, cause: Cause::Weapon(DamageKind::Arrow), head: false });
                        fx.push((a.owner, CombatEvent::PowerFx { what: FX_ARROW_BLOCKED, pos: pt(hitp) }));
                    }
                    crate::block::Guard::None => {
                        let head = hitp[1] >= vpos[1] + self.balance.combat.head_min_height;
                        let amount = yc.damage * if head { yc.head_mult } else { 1.0 };
                        hits.push(PvpHit { attacker: a.owner, victim: j, amount, cause: Cause::Weapon(DamageKind::Arrow), head });
                        fx.push((a.owner, CombatEvent::PowerFx { what: if head { FX_ARROW_HEAD } else { FX_ARROW_HIT }, pos: pt(hitp) }));
                    }
                }
                self.arrows.remove(i);
                continue;
            }
            a.pos = to;
            if a.age * yc.speed > yc.range || to[1] < self.kill_plane_y {
                self.arrows.remove(i);
                continue;
            }
            self.arrows[i] = a;
            i += 1;
        }

        // ---- used-up tag bags ----
        for k in 0..self.players.len() {
            let id = self.players[k].id;
            let live = self.live_tags(id);
            if self.players[k].power.spent_out(live) {
                fx.push((id, CombatEvent::PowerFx { what: FX_EXPIRED, pos: pt(self.players[k].pos(&self.world)) }));
            }
        }
        self.events.extend(fx);
    }

    /// A tag goes off: damage with falloff, launch, wall-splat and ring-out bookkeeping.
    fn blast(&mut self, t: &Tag, tick: u32, tc: &TagCfg, mcfg: &crate::melee::MeleeCfg, hits: &mut Vec<PvpHit>, fx: &mut Vec<(u16, CombatEvent)>) {
        let centre = t.pos;
        // the occlusion test starts a little off the surface the tag is on
        let from = add(centre, scale(t.normal, 0.3));
        let oi = self.player_index(t.owner);
        let oteam = oi.map(|o| self.players[o].team);
        let owner_pos = oi.map(|o| self.players[o].pos(&self.world)).unwrap_or(centre);
        fx.push((t.owner, CombatEvent::PowerFx { what: FX_TAG_BLAST, pos: pt(centre) }));
        // the blast shakes destructible level boxes too (pillars, towers, bamboo)
        if !self.breaks.is_empty() {
            self.breaks.blast(&mut self.world, centre, tc.radius, |d| blast_damage(tc, d), tick, &mut self.break_events);
        }
        for j in 0..self.players.len() {
            let (qid, qteam, qpos) = {
                let q = &self.players[j];
                (q.id, q.team, q.pos(&self.world))
            };
            let me = qid == t.owner;
            if !hittable(&self.players[j], tick) || (!me && oteam.map_or(false, |ot| allied(ot, qteam))) {
                continue;
            }
            let d = len(sub(qpos, centre));
            if d > tc.radius {
                continue;
            }
            let on_me = t.on.map_or(false, |(v, _)| v == qid);
            if !on_me && !self.los_clear(from, qpos) {
                continue;
            }
            let k = if me { tc.self_mult } else { 1.0 };
            let amount = blast_damage(tc, d) * k;
            if amount > 0.0 {
                hits.push(PvpHit { attacker: t.owner, victim: j, amount, cause: Cause::Weapon(DamageKind::Tag), head: false });
            }
            let push = blast_push(tc, centre, owner_pos, qpos);
            let v = &mut self.players[j];
            v.ch.vel = rapier3d::prelude::Vector::new(push[0] * k, push[1], push[2] * k);
            if matches!(v.ch.state, crate::character::MoveState::Grounded) {
                v.ch.state = crate::character::MoveState::Airborne;
            }
            if !me {
                v.melee.last_hit = Some((t.owner, tick));
            }
            // wall-splat window: slammed into a wall within it, the victim takes the splat (melee.rs)
            v.melee.knock_until = tick + mcfg.splat_ticks;
            v.melee.knock_speed = tc.knock * k;
            v.melee.knock_by = t.owner;
        }
    }
}

/// Floor drops of power weapons use ids from here (enemy loot uses `DROP_ID_BASE`).
pub const POWER_DROP_ID_BASE: u32 = 2_000_000;
