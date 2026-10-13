//! Server-side PvP resolution: player hitboxes, lag-compensated hit tests, damage, death, respawn.
//!
//! Why a separate hitbox: combat queries the shared `QueryPipeline`, which only knows where colliders are
//! *now*. Each player therefore owns a parentless SENSOR capsule (`GROUP_HITBOX`, `user_data` = player id)
//! that is repositioned per attacker to where that attacker's screen showed the target
//! (`resolve_rewind` + `PoseHistory`). Combat's own queries stay untouched, a disc that ricochets back
//! through its thrower cannot self-hit (the attacker's hitbox is switched off for their own shots), and
//! in-flight discs keep flying in the attacker's rewound frame because the rewind is redone every tick.
//!
//! Tick protocol (see `tick::Server::step`):
//! 1. `tick_respawns`              start of tick
//! 2. per attacker: `rewind_hitboxes` -> `combat.update` -> `player_hits`
//! 3. `apply_hits`                 once, after every attacker has run (player order never decides a trade)
//! 4. `finish_tick`                record history, put hitboxes back at "now"

use shinobi_proto::intent::ViewState;

use crate::character::{Cfg, PhysicsWorld};
use crate::combat::{CombatEvent, DamageKind, GROUP_HITBOX, GROUP_MELEE, GROUP_PROJECTILE};
use crate::lagcomp::{resolve_rewind, rewound_segment, swept_hit, Body, RewindTime};
use crate::rapier3d::prelude::*;
use crate::tick::Player;

pub const MAX_HP: f32 = 100.0;
/// 3 s at 60 Hz.
pub const RESPAWN_TICKS: u32 = 180;
/// Spawn protection. Must stay >= `MAX_REWIND_TICKS` so a laggy, rewound shot cannot hit the corpse's old
/// position or the interpolated path across the respawn teleport.
pub const INVULN_TICKS: u32 = 90;
const _: () = assert!(INVULN_TICKS >= crate::lagcomp::MAX_REWIND_TICKS);

/// Everything the network snapshot / HUD read is a `pub` field here.
#[derive(Clone, Debug)]
pub struct Vitals {
    pub hp: f32,
    pub max_hp: f32,
    pub kills: u16,
    pub deaths: u16,
    /// Consecutive kills without dying since the last respawn; reset to 0 on death. Drives the BOUNTY system
    /// (docs/bounty.md, `MatchState::update_bounty`); not a network field by itself (the bounty id is).
    pub kill_streak: u16,
    /// `Some(t)`: dead, respawns at tick `t`.
    pub dead_until_tick: Option<u32>,
    /// Damage-immune while `tick < invuln_until_tick`.
    pub invuln_until_tick: u32,
    /// Tick of the last damage taken (or of the spawn): health regen waits `regen_delay_s` after it.
    pub last_damage_tick: u32,
    pub spawn: [f32; 3],
    pub hitbox: ColliderHandle,
}

impl Vitals {
    pub fn alive(&self) -> bool {
        self.dead_until_tick.is_none()
    }

    /// HUD: 0.0..=1.0.
    pub fn hp_frac(&self) -> f32 {
        (self.hp / self.max_hp).clamp(0.0, 1.0)
    }

    pub fn spawn_protected(&self, tick: u32) -> bool {
        tick < self.invuln_until_tick
    }
}

/// "No player" attacker id for environmental damage (obstacles).
pub const NO_ATTACKER: u16 = u16::MAX;

/// Obstacle id reserved for "the world itself": falling below the kill plane. Wire-compatible with the
/// existing `HazardSource::Obstacle` (a new variant would break exhaustive matches downstream); clients that
/// care can special-case this id ("fell").
pub const VOID_SOURCE_ID: u16 = u16::MAX;
pub const VOID: HazardSource = HazardSource::Obstacle(VOID_SOURCE_ID);
/// A wall-splat (`melee.rs`): an obstacle id too, so clients can special-case it ("splatted").
pub const SPLAT: HazardSource = HazardSource::Obstacle(crate::melee::SPLAT_SOURCE_ID);
/// Default kill plane: a player whose capsule centre drops below this dies and respawns.
pub const KILL_PLANE_Y: f32 = -20.0;

/// Non-weapon damage source (see `hazards.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HazardSource {
    Obstacle(u16),
    /// Light-wall segment, with the owning player's id (`id as u8`, as trails are stamped).
    Wall { id: u32, owner: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cause {
    Weapon(DamageKind),
    Hazard(HazardSource),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PvpEvent {
    /// `head`: a shuriken that struck head height (`HeadCfg`), for the headshot marker.
    /// `bonus`: the hit landed inside the victim's strike window (`block.rs`) and dealt bonus damage.
    Hit { tick: u32, attacker: u16, victim: u16, amount: f32, kind: DamageKind, head: bool, bonus: bool },
    Kill { tick: u32, killer: u16, victim: u16, kind: DamageKind, head: bool },
    /// Obstacle / light-wall contact on a player. `Wall` hits carry the owner in `source`.
    HazardHit { tick: u32, victim: u16, amount: f32, source: HazardSource },
    /// `killer`: wall owner when it is another player; `None` for obstacles and self-kills.
    HazardKill { tick: u32, victim: u16, source: HazardSource, killer: Option<u16> },
    /// A light-wall hurt a non-player (ENEMY group) target. `enemy` = its collider `user_data`.
    EnemyHazardHit { tick: u32, enemy: u128, amount: f32, source: HazardSource },
    Respawn { tick: u32, player: u16 },
}

/// One resolved player-vs-player hit, pending `apply_hits`.
#[derive(Clone, Copy, Debug)]
pub struct PvpHit {
    /// Player id, or `NO_ATTACKER`.
    pub attacker: u16,
    /// Index into the players slice.
    pub victim: usize,
    pub amount: f32,
    pub cause: Cause,
    /// A headshot (shuriken above `HeadCfg::min_height`).
    pub head: bool,
    /// The attacker was still cloaked when this hit left their hands / blade: a stealth kill
    /// (`combat.cloak.stealth_mult`, see `apply_hits`). Never set on a hazard hit.
    pub stealth: bool,
}

pub fn new_vitals(world: &mut PhysicsWorld, cfg: &Cfg, id: u16, spawn: [f32; 3], max_hp: f32) -> Vitals {
    let hitbox = world.colliders.insert(
        ColliderBuilder::capsule_y(cfg.half_height, cfg.radius)
            .translation(vector![spawn[0], spawn[1], spawn[2]])
            .sensor(true) // never blocks movement; combat queries opt in via GROUP_HITBOX
            .collision_groups(hitbox_groups())
            .user_data(id as u128)
            .build(),
    );
    Vitals {
        hp: max_hp,
        max_hp,
        kills: 0,
        deaths: 0,
        kill_streak: 0,
        dead_until_tick: None,
        invuln_until_tick: 0,
        last_damage_tick: 0,
        spawn,
        hitbox,
    }
}

fn hitbox_groups() -> InteractionGroups {
    InteractionGroups::new(GROUP_HITBOX, GROUP_PROJECTILE | GROUP_MELEE)
}

/// Rewind target for an attacker at `tick`. History for `tick` itself is recorded only at the end of the
/// step, so the newest usable sample is `tick - 1`. `None` on tick 0.
pub fn rewind_time(tick: u32, view: ViewState) -> Option<RewindTime> {
    let last = tick.checked_sub(1)?;
    let mut rt = resolve_rewind(tick, view);
    if rt.tick > last {
        rt = RewindTime { tick: last, alpha: 0.0 };
    }
    Some(rt)
}

fn hittable(p: &Player, tick: u32) -> bool {
    p.vitals.alive() && !p.vitals.spawn_protected(tick) && !p.ch.invulnerable()
}

fn hit_body(cfg: &Cfg) -> Body {
    // `lagcomp::Body.half_height` is the full half extent (capsule half-height + radius)
    Body { radius: cfg.radius, half_height: cfg.half_height + cfg.radius }
}

/// Put every hitbox where `attacker`'s screen showed it, hide the attacker's own and anyone who cannot be
/// hurt (dead, spawn-protected, roll i-frames), then rebuild the query structure.
pub fn rewind_hitboxes(world: &mut PhysicsWorld, players: &[Player], attacker: usize, tick: u32, cfg: &Cfg) {
    let rt = rewind_time(tick, players[attacker].queue.latest_view);
    for (j, p) in players.iter().enumerate() {
        let Some(col) = world.colliders.get_mut(p.vitals.hitbox) else { continue };
        if j == attacker || !hittable(p, tick) {
            col.set_collision_groups(InteractionGroups::none());
            continue;
        }
        col.set_collision_groups(hitbox_groups());
        let pos = rt.and_then(|rt| p.history.sample(rt.tick, rt.alpha)).unwrap_or_else(|| {
            let t = world.bodies[p.ch.body].translation();
            [t.x, t.y, t.z]
        });
        let Some(col) = world.colliders.get_mut(p.vitals.hitbox) else { continue };
        col.set_position(Isometry::translation(pos[0], pos[1], pos[2]));
    }
    let _ = cfg;
    world.queries.update(&world.colliders);
}

/// Headshot rule for shurikens: a hit whose impact point is at least `min_height` above the target's capsule
/// centre multiplies the damage by `mult`. The capsule spans centre +- 0.8 and the shooter's eye is centre + 0.6, so
/// the default 0.5 makes a level crosshair at head height a headshot, centre-mass aim a body shot. `mult` 1.0
/// disables the bonus.
#[derive(Clone, Copy, Debug)]
pub struct HeadCfg {
    pub min_height: f32,
    pub mult: f32,
}

impl Default for HeadCfg {
    fn default() -> Self {
        Self { min_height: 0.5, mult: 1.0 }
    }
}

/// Player hits produced by one attacker's combat events this tick:
/// * `Damage` events whose collider is a player hitbox (shuriken, slash box, ultimate), and
/// * a swept slash test (`swept_hit`) for targets that moved across the tick and slipped between the two
///   point-in-time box tests. Those hits have no raw `Damage` event, so one is pushed to `synthesized` (the
///   caller appends it to the attacker's events).
pub fn player_hits(
    world: &PhysicsWorld,
    players: &[Player],
    attacker: usize,
    events: &[CombatEvent],
    tick: u32,
    cfg: &Cfg,
    head: HeadCfg,
    synthesized: &mut Vec<CombatEvent>,
) -> Vec<PvpHit> {
    let atk = &players[attacker];
    let mut out: Vec<PvpHit> = Vec::new();
    let victim_of = |h: ColliderHandle| players.iter().position(|p| p.vitals.hitbox == h);

    for e in events {
        if let CombatEvent::Damage { collider, amount, kind, point, .. } = *e {
            if let Some(v) = victim_of(collider) {
                if v != attacker && !crate::tick::allied(atk.team, players[v].team) {
                    // `point` is the disc centre at impact; the hitbox is at its rewound position right now
                    let centre_y = world.colliders.get(collider).map_or(point.y, |c| c.translation().y);
                    let is_head = kind == DamageKind::Shuriken && point.y - centre_y >= head.min_height;
                    let amount = if is_head { amount * head.mult } else { amount };
                    out.push(PvpHit { attacker: atk.id, victim: v, amount, cause: Cause::Weapon(kind), head: is_head, stealth: false });
                }
            }
        }
    }

    let Some(rt) = rewind_time(tick, atk.queue.latest_view) else { return out };
    for e in events {
        let CombatEvent::Slash { dir, origin, reach, half_width, half_height, damage, .. } = *e else { continue };
        // swept volume: a cylinder of radius `half_width` whose centre runs origin+hw .. origin+reach-hw,
        // so its overall footprint matches the box (origin .. origin+reach).
        let lead = half_width.min(reach * 0.5);
        let (a0, a1) = (origin + dir * lead, origin + dir * (reach - lead).max(lead));
        let body = Body { radius: half_width, half_height };
        for (j, p) in players.iter().enumerate() {
            if j == attacker || crate::tick::allied(atk.team, p.team) || !hittable(p, tick) || out.iter().any(|h| h.victim == j && h.cause == Cause::Weapon(DamageKind::Slash)) {
                continue;
            }
            let Some((p0, p1)) = rewound_segment(&p.history, rt) else { continue };
            if swept_hit([a0.x, a0.y, a0.z], [a1.x, a1.y, a1.z], body, p0, p1, hit_body(cfg)).is_some() {
                out.push(PvpHit { attacker: atk.id, victim: j, amount: damage, cause: Cause::Weapon(DamageKind::Slash), head: false, stealth: false });
                // the box test missed this target (it moved across the tick), so no raw `Damage` exists yet: emit
                // it, so every `PvpEvent::Hit` has its combat-side twin (FX, hit markers, bookkeeping)
                synthesized.push(CombatEvent::Damage {
                    target: p.id as u128,
                    collider: p.vitals.hitbox,
                    amount: damage,
                    kind: DamageKind::Slash,
                    point: Point::new(p1[0], p1[1], p1[2]),
                });
            }
        }
    }
    let _ = world;
    out
}

/// Lethal `PvpHit`s for every living player below `plane_y` (fell out of the world).
pub fn void_hits(players: &[Player], world: &PhysicsWorld, plane_y: f32, tick: u32, melee: &crate::melee::MeleeCfg) -> Vec<PvpHit> {
    players
        .iter()
        .enumerate()
        .filter(|(_, p)| p.vitals.alive() && world.bodies[p.ch.body].translation().y < plane_y)
        .map(|(i, p)| {
            // ring-out: whoever hit this player a moment ago gets the kill
            let pusher = p.melee.last_hit.filter(|(_, t)| melee.ring_ticks > 0 && tick.wrapping_sub(*t) <= melee.ring_ticks).map(|(a, _)| a);
            PvpHit { attacker: pusher.unwrap_or(NO_ATTACKER), victim: i, amount: p.vitals.max_hp * 10.0, cause: Cause::Hazard(VOID), head: false, stealth: false }
        })
        .collect()
}

/// Apply all of this tick's hits at once. A player who dies this tick still lands hits they already threw
/// (trades are symmetric); only the first lethal hit on a victim is credited, and a victim dies at most once.
pub fn apply_hits(
    players: &mut [Player],
    world: &mut PhysicsWorld,
    hits: &[PvpHit],
    tick: u32,
    respawn_ticks: u32,
    block: &crate::block::BlockCfg,
    melee: &crate::melee::MeleeCfg,
    cloak: &crate::cloak::CloakCfg,
    // `(bounty's player id, damage multiplier)` while a BOUNTY is marked (docs/bounty.md); `None` or a
    // multiplier of 1 is a no-op. Applied the same place momentum/streak are: only against an open target,
    // same as the comment above says of those -- a shield is judged on the base cut, never on a bonus.
    bounty: Option<(u16, f32)>,
    out: &mut Vec<PvpEvent>,
) {
    let mut died: Vec<usize> = Vec::new();
    for h in hits {
        // the victim's shield and strike window decide what this hit is worth (weapons only, never hazards)
        let (mut amount, mut bonus) = (h.amount, false);
        let mut guard_seen = crate::block::Guard::None;
        let mut mom = 1.0f32;
        // arrows, tag blasts and star explosions are resolved against shield, parry and cut-down (`power_pass.rs`, `star_pass.rs`) before they get here
        let pre_resolved = matches!(h.cause, Cause::Weapon(DamageKind::Arrow | DamageKind::Tag));
        if let (Cause::Weapon(kind), false) = (h.cause, pre_resolved) {
            let apos = players.iter().find(|p| p.id == h.attacker).map(|p| p.pos(world));
            // momentum + kill streak: a fast, hot swordsman cuts harder (but only against an open target: a shield is
            // judged on the BASE cut, so a lunge or a streak never punches through it)
            if kind == DamageKind::Slash {
                if let Some(a) = players.iter().find(|p| p.id == h.attacker) {
                    let v = a.ch.vel;
                    let falling = if matches!(a.ch.state, crate::character::MoveState::Grounded) { 0.0 } else { (-v.y).max(0.0) * 0.5 };
                    let speed = (v.x * v.x + v.z * v.z).sqrt() + falling;
                    mom = melee.damage_mult(&a.melee, speed, tick);
                }
            }
            let vp = &players[h.victim];
            let vyaw = shinobi_proto::quant::dq_yaw(vp.yaw);
            let guard = apos.map_or(crate::block::Guard::None, |a| crate::block::guard_vs(&vp.blk, vyaw, vp.pos(world), a, tick, block));
            guard_seen = guard;
            // a star on a raised shield (block or parry) hurts nobody: it ricochets straight back at its thrower, now the
            // shield-holder's star (so it can hit the original thrower and credits the holder); the shield pays for it below
            if kind == DamageKind::Shuriken && matches!(guard, crate::block::Guard::Blocked | crate::block::Guard::Parried) {
                if let Some(a) = apos {
                    let v = players[h.victim].pos(world);
                    let back = Vector::new(a[0] - v[0], a[1] - v[1], a[2] - v[2]);
                    let dir = if back.norm() > 1e-3 { back.normalize() } else { Vector::new(0.0, 0.0, 1.0) };
                    let at = Point::new(v[0] + dir.x * 0.9, v[1] + 0.2 + dir.y * 0.9, v[2] + dir.z * 0.9);
                    if let Some(discs) = players[h.victim].combat.loadout.discs_mut() {
                        discs.push(crate::combat::shuriken::Disc {
                            pos: at,
                            dir,
                            bounces: 0,
                            age: 0.0,
                            speed_scale: 1.0,
                            homing: None,
                            mark: false,
                            kind: crate::combat::stars::StarKind::Standard,
                            travelled: 0.0,
                        });
                    }
                }
            }
            match guard {
                crate::block::Guard::Parried => {
                    players[h.victim].blk.mark_used();
                    let parried = amount;
                    amount = 0.0;
                    // a parried melee attacker is staggered and wide open to the counter
                    if kind == DamageKind::Slash {
                        if let Some(a) = players.iter_mut().find(|p| p.id == h.attacker) {
                            a.blk.stagger_until = tick + block.stagger_ticks;
                            a.blk.open_for(tick, block.counter_open_ticks.max(block.stagger_ticks));
                        }
                        // the parrier's next cut inside the window is a counter cut (katana_moves.rs)
                        if melee.counter_ticks > 0 && h.attacker != NO_ATTACKER {
                            players[h.victim].kmoves.counter = Some((h.attacker, tick + melee.counter_ticks, parried));
                        }
                    }
                }
                crate::block::Guard::Blocked => {
                    // base cut: no momentum, streak or window bonus; a deflected star does no damage at all
                    amount = if kind == DamageKind::Shuriken { 0.0 } else { h.amount * block.front_mult };
                    let v = &mut players[h.victim];
                    v.blk.mark_used();
                    // a blocked hit costs guard in proportion to its raw damage (a 20 hp cut = `hit_cost`, a 7 hp star a third)
                    v.blk.energy -= block.hit_cost * (h.amount / 20.0).min(2.5);
                    if v.blk.energy <= 0.0 {
                        v.blk.break_guard(tick, block);
                        // the break staggers the owner briefly; the mercy window then protects them from a follow-up combo
                        v.blk.stagger_until = tick + block.break_stagger_ticks;
                    }
                }
                crate::block::Guard::None => {
                    amount *= mom;
                    if let Some((id, mult)) = bounty {
                        if h.attacker == id && mult > 1.0 {
                            amount *= mult;
                        }
                    }
                    let (mercy, open, stunned, vhp, vmax) = {
                        let v = &players[h.victim];
                        (v.blk.mercy(tick), v.blk.open(tick), v.blk.staggered(tick), v.vitals.hp, v.vitals.max_hp)
                    };
                    if h.stealth && cloak.stealth_mult > 0.0 {
                        // stealth kill: a first strike landed while the attacker was still cloaked (cloak.rs) hits
                        // like an execute, same pattern as `melee.exec_frac` below
                        amount *= cloak.stealth_mult;
                        bonus = true;
                    } else if kind == DamageKind::Slash && melee.executes(vhp, vmax, stunned) {
                        // execute: a stunned player at low hp does not survive a blow
                        amount = amount.max(vhp);
                        bonus = true;
                    } else if mercy {
                        amount *= block.mercy_mult;
                    } else if open && block.bonus_mult > 1.0 {
                        amount *= block.bonus_mult;
                        bonus = true;
                    }
                    // a MARK star on the victim, thrown by this attacker: the katana cut lands harder, once
                    if kind == DamageKind::Slash && melee.mark_ticks > 0 && melee.mark_bonus > 0.0 {
                        let v = &mut players[h.victim];
                        if v.melee.marked(tick) && v.melee.marked_by == h.attacker {
                            amount *= 1.0 + melee.mark_bonus;
                            v.melee.marked_until = 0;
                            bonus = true;
                        }
                    }
                }
            }
        }
        let v = &mut players[h.victim];
        // the void kills through spawn protection and roll i-frames: nothing may fall forever
        let bypass = h.cause == Cause::Hazard(VOID);
        if !v.vitals.alive() || died.contains(&h.victim) || (!bypass && (v.vitals.spawn_protected(tick) || v.ch.invulnerable())) {
            continue;
        }
        v.vitals.hp -= amount;
        if amount > 0.0 {
            v.combat.on_hurt();
        }
        if amount > 0.0 {
            v.vitals.last_damage_tick = tick;
        }
        let victim_id = v.id;
        let dead = v.vitals.hp <= 0.0;
        if dead {
            v.vitals.hp = 0.0;
            died.push(h.victim);
        }
        // credit only another player; obstacles and self-kills score nobody
        let killer = players.iter().position(|p| p.id == h.attacker && p.id != victim_id);
        match h.cause {
            Cause::Weapon(kind) => {
                out.push(PvpEvent::Hit { tick, attacker: h.attacker, victim: victim_id, amount, kind, head: h.head, bonus });
                if dead {
                    out.push(PvpEvent::Kill { tick, killer: h.attacker, victim: victim_id, kind, head: h.head });
                }
            }
            Cause::Hazard(source) => {
                out.push(PvpEvent::HazardHit { tick, victim: victim_id, amount: h.amount, source });
                if dead {
                    out.push(PvpEvent::HazardKill {
                        tick,
                        victim: victim_id,
                        source,
                        killer: killer.map(|k| players[k].id),
                    });
                }
            }
        }
        // melee heighteners: remember who hurt whom (ring-out), launch a slashed victim (knockback / juggle / splat window)
        if let Cause::Weapon(kind) = h.cause {
            if amount > 0.0 && h.attacker != NO_ATTACKER {
                players[h.victim].melee.last_hit = Some((h.attacker, tick));
            }
            if kind == DamageKind::Slash && !dead && amount > 0.0 && guard_seen != crate::block::Guard::Blocked && guard_seen != crate::block::Guard::Parried {
                if let Some(a) = players.iter().find(|p| p.id == h.attacker).map(|p| p.pos(world)) {
                    // an air cut drives its victim into the ground (the landing is a shock-wave) instead of launching them
                    let slam = melee.slam_speed > 0.0 && players.iter().find(|p| p.id == h.attacker).map_or(false, |p| p.kmoves.slams(tick));
                    let vp = &mut players[h.victim];
                    let airborne = !matches!(vp.ch.state, crate::character::MoveState::Grounded);
                    let vel = [vp.ch.vel.x, vp.ch.vel.y, vp.ch.vel.z];
                    if slam {
                        vp.ch.vel = Vector::new(0.0, -melee.slam_speed, 0.0);
                        if matches!(vp.ch.state, crate::character::MoveState::Grounded) {
                            vp.ch.state = crate::character::MoveState::Airborne;
                        }
                        vp.kmoves.slammed(h.attacker, tick);
                    } else if let Some(nv) = melee.knock(vp.pos(world), a, vel, airborne) {
                        vp.ch.vel = Vector::new(nv[0], nv[1], nv[2]);
                        if matches!(vp.ch.state, crate::character::MoveState::Grounded) {
                            vp.ch.state = crate::character::MoveState::Airborne; // the controller keeps a grounded body stuck to the floor
                        }
                        vp.melee.knock_until = tick + melee.splat_ticks;
                        vp.melee.knock_speed = melee.knock_speed;
                        vp.melee.knock_by = h.attacker;
                    }
                }
            }
        }
        if dead {
            if let Some(k) = killer {
                players[k].vitals.kills = players[k].vitals.kills.saturating_add(1);
                players[k].vitals.kill_streak = players[k].vitals.kill_streak.saturating_add(1);
                // a killer who died this same frame (trade, or killed earlier in the hit list) gets no heal: it would
                // raise the hp of a corpse
                if matches!(h.cause, Cause::Weapon(DamageKind::Slash)) && !died.contains(&k) && players[k].vitals.alive() {
                    let heal = melee.on_kill(&mut players[k].melee, tick);
                    let a = &mut players[k].vitals;
                    a.hp = (a.hp + heal).min(a.max_hp);
                }
            }
        }
    }
    for v in died {
        let p = &mut players[v];
        p.vitals.deaths = p.vitals.deaths.saturating_add(1);
        p.vitals.kill_streak = 0;
        p.vitals.dead_until_tick = Some(tick + respawn_ticks);
        p.ch.vel = Vector::zeros();
        p.ch.clear_grapple();
        p.combat.reset_projectiles();
        p.blk.reset();
        p.kmoves.reset();
        p.melee.marked_until = 0;
        p.melee.knock_speed = 0.0; // a corpse splats nothing (the kill streak and last-hit survive: ring-outs after death are over)
        if let Some(col) = world.colliders.get_mut(p.vitals.hitbox) {
            col.set_collision_groups(InteractionGroups::none());
        }
    }
}

/// Start of tick: bring back everyone whose timer ran out. The teleport happens BEFORE the player's
/// `before` position is sampled, so anti-cheat sees no displacement (no snap-back, no strike).
pub fn tick_respawns(
    players: &mut [Player],
    world: &mut PhysicsWorld,
    spawns: crate::spawn::SpawnSet,
    spawn_counter: &mut u32,
    tick: u32,
    invuln_ticks: u32,
    out: &mut Vec<PvpEvent>,
) {
    for i in 0..players.len() {
        if players[i].vitals.dead_until_tick.map_or(true, |t| tick < t) {
            continue;
        }
        let s = pick_spawn_for(players, i, world, spawns, spawn_counter);
        revive(&mut players[i], world, s, tick, invuln_ticks);
        out.push(PvpEvent::Respawn { tick, player: players[i].id });
    }
}

/// Spawn point for `players[i]`: farthest from every *other* living player, unseen if possible; the player's
/// fixed home when the list is empty. Refreshes the query structure.
pub fn pick_spawn_for(
    players: &[Player],
    i: usize,
    world: &mut PhysicsWorld,
    spawns: crate::spawn::SpawnSet,
    spawn_counter: &mut u32,
) -> [f32; 3] {
    let enemies: Vec<[f32; 3]> = players
        .iter()
        .enumerate()
        .filter(|(j, q)| *j != i && q.vitals.alive() && !crate::tick::allied(players[i].team, q.team))
        .map(|(_, q)| q.pos(world))
        .collect();
    world.queries.update(&world.colliders);
    match crate::spawn::choose_among(world, spawns.pos, &spawns.allowed(players[i].team), &enemies, *spawn_counter) {
        Some(k) => {
            *spawn_counter = crate::spawn::push_history(*spawn_counter, k, true);
            spawns.pos[k]
        }
        None => players[i].vitals.spawn,
    }
}

/// Bring a player back at `s`: full hp, alive, spawn-protected, nothing in flight. Done before the tick's
/// `before` position is sampled (anti-cheat sees no displacement) and with the history reseeded so rewound
/// reads never see the old position.
pub fn revive(p: &mut Player, world: &mut PhysicsWorld, s: [f32; 3], tick: u32, invuln_ticks: u32) {
    world.bodies[p.ch.body].set_position(Isometry::translation(s[0], s[1], s[2]), true);
    crate::character::sync_body_colliders(world, p.ch.body);
    p.ch.vel = Vector::zeros();
    p.ch.clear_grapple();
    p.vitals.hp = p.vitals.max_hp;
    p.vitals.last_damage_tick = tick;
    p.vitals.dead_until_tick = None;
    p.vitals.invuln_until_tick = tick + invuln_ticks;
    p.combat.reset_projectiles();
    p.power.clear();
    p.history.record(tick.wrapping_sub(1), s);
}

/// End of tick: history gets the post-move pose (the tick a snapshot would be labelled with), and hitboxes
/// return to "now" so nothing else queries a rewound frame.
/// Health regeneration: a living, hurt player who has taken no damage for `delay_ticks` heals `per_tick` hp per tick
/// up to max (`per_tick` 0 = off).
pub fn regen(players: &mut [Player], tick: u32, delay_ticks: u32, per_tick: f32) {
    if per_tick <= 0.0 {
        return;
    }
    for p in players.iter_mut() {
        let v = &mut p.vitals;
        if v.alive() && v.hp < v.max_hp && tick.wrapping_sub(v.last_damage_tick) >= delay_ticks {
            v.hp = (v.hp + per_tick).min(v.max_hp);
        }
    }
}

pub fn finish_tick(players: &mut [Player], world: &mut PhysicsWorld, tick: u32) {
    for p in players.iter_mut() {
        let t = world.bodies[p.ch.body].translation();
        let pos = [t.x, t.y, t.z];
        p.history.record(tick, pos);
        let alive = p.vitals.alive();
        if let Some(col) = world.colliders.get_mut(p.vitals.hitbox) {
            col.set_position(Isometry::translation(pos[0], pos[1], pos[2]));
            col.set_collision_groups(if alive { hitbox_groups() } else { InteractionGroups::none() });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use shinobi_proto::intent::{Buttons, ClientPacket, PlayerIntent, ViewState};

    use super::*;
    use crate::balance::Balance;
    use crate::combat::CombatEvent;
    use crate::tick::Server;

    const A: u16 = 1;
    const B: u16 = 2;

    struct Sim {
        s: Server,
        pvp: Vec<PvpEvent>,
        combat: Vec<(u16, CombatEvent)>,
    }

    impl Sim {
        fn new(players: &[(u16, [f32; 3])]) -> Self {
            let mut s = Server::new(Cfg::default());
            s.add_static_box([0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
            for (id, at) in players {
                s.add_player(*id, *at);
            }
            Self { s, pvp: Vec::new(), combat: Vec::new() }
        }

        /// One server tick. `inputs`: (player, intent, rewind_ticks) where rewind_ticks is how far behind the
        /// server the client's screen was (the packet's view time).
        fn tick(&mut self, inputs: &[(u16, PlayerIntent, u32)]) {
            let t = self.s.tick;
            for (id, it, rewind) in inputs {
                let from = t.saturating_sub(*rewind);
                let pkt = ClientPacket {
                    ack_snapshot_tick: from,
                    view: ViewState { from_tick: from, alpha: 0 },
                    intents: vec![PlayerIntent { tick: t, ..*it }],
                    ..Default::default()
                };
                self.s.ingest(*id, &pkt.encode().unwrap());
            }
            assert_eq!(self.s.frame(Duration::from_nanos(16_666_667)).ticks, 1);
            self.pvp.extend(self.s.pvp_events.iter().copied());
            self.combat.extend(self.s.events.iter().copied());
        }

        fn player(&self, id: u16) -> &Player {
            self.s.players.iter().find(|p| p.id == id).unwrap()
        }

        fn hits_on(&self, victim: u16) -> usize {
            self.pvp.iter().filter(|e| matches!(e, PvpEvent::Hit { victim: v, .. } if *v == victim)).count()
        }
    }

    fn press(flag: u8) -> PlayerIntent {
        PlayerIntent { buttons: Buttons(flag), ..IDLE }
    }

    const IDLE: PlayerIntent = PlayerIntent {
        tick: 0,
        buttons: Buttons(0),
        move_x: 0,
        move_z: 0,
        aim_yaw: 0,
        aim_pitch: 2048,
        ultimate: false,
        build: false, build_wall: false, build_ramp: false,
        zip: false,
        block: false,
        mark: false,
        star: 0,
        draw: false,
        cloak: false,
    };

    #[test]
    fn a_swept_only_slash_hit_gets_its_raw_damage_event() {
        // B stands inside A's slash volume, but no box-test Damage event exists (as when B moved across the
        // tick): the sweep must create the hit AND its combat-side twin.
        let mut sim = Sim::new(&[(A, [0.0, 0.81, 0.0]), (B, [0.0, 0.81, -3.0])]);
        for _ in 0..3 {
            sim.tick(&[(A, IDLE, 0), (B, IDLE, 0)]);
        }
        let tick = sim.s.tick;
        let slash = CombatEvent::Slash {
            flip: true,
            dir: vector![0.0, 0.0, -1.0],
            lunge: false,
            hits: 0,
            origin: Point::new(0.0, 0.81, 0.0),
            reach: 5.0,
            half_width: 1.0,
            half_height: 1.0,
            damage: 60.0,
            kind: crate::combat::SlashKind::Light,
        };
        let mut synth = Vec::new();
        let hits = player_hits(&sim.s.world, &sim.s.players, 0, &[slash], tick, &Cfg::default(), HeadCfg::default(), &mut synth);
        assert_eq!(hits.len(), 1, "swept test finds B");
        assert_eq!((hits[0].attacker, hits[0].amount), (A, 60.0));
        let b = &sim.s.players[1];
        assert!(
            matches!(synth[..], [CombatEvent::Damage { collider, amount, kind: DamageKind::Slash, .. }] if collider == b.vitals.hitbox && amount == 60.0),
            "{synth:?}"
        );
        // control: a victim already hit by the box test (a Damage event is present) is not double counted
        let boxed = CombatEvent::Damage { target: B as u128, collider: b.vitals.hitbox, amount: 60.0, kind: DamageKind::Slash, point: Point::new(0.0, 0.81, -3.0) };
        let mut synth2 = Vec::new();
        let hits2 = player_hits(&sim.s.world, &sim.s.players, 0, &[boxed, slash], tick, &Cfg::default(), HeadCfg::default(), &mut synth2);
        assert_eq!(hits2.len(), 1);
        assert!(synth2.is_empty(), "no second Damage event for a box hit");
    }

    /// A small floor at x 10..20 only; everything else is void.
    fn island(plane: Option<f32>) -> Sim {
        let mut s = Server::new(Cfg::default());
        s.add_static_box([15.0, -0.5, 0.0], [5.0, 0.5, 10.0]);
        s.add_player(A, [15.0, 0.81, 0.0]);
        if let Some(y) = plane {
            s.kill_plane_y = y;
        }
        Sim { s, pvp: Vec::new(), combat: Vec::new() }
    }

    fn throw_into_void(sim: &mut Sim) {
        let body = sim.s.players[0].ch.body;
        sim.s.world.bodies[body].set_position(Isometry::translation(0.0, 5.0, 0.0), true);
        crate::character::sync_body_colliders(&mut sim.s.world, body);
        sim.s.players[0].ch.vel = Vector::zeros();
    }

    fn void_kills(sim: &Sim) -> usize {
        sim.pvp.iter().filter(|e| matches!(e, PvpEvent::HazardKill { source: VOID, killer: None, .. })).count()
    }

    #[test]
    fn falling_out_of_the_world_kills_and_respawns_on_the_floor() {
        let mut sim = island(None);
        for _ in 0..5 {
            sim.tick(&[(A, IDLE, 0)]);
        }
        assert_eq!(void_kills(&sim), 0, "standing on the floor is safe");
        throw_into_void(&mut sim);
        for _ in 0..150 {
            sim.tick(&[(A, IDLE, 0)]);
        }
        assert_eq!(void_kills(&sim), 1);
        let v = &sim.player(A).vitals;
        assert_eq!((v.deaths, v.kills), (1, 0), "a death, and no credit for anyone");
        assert!(!v.alive());
        // respawn after the timer (180 ticks after the fall), back on the island, alive and staying alive
        for _ in 0..140 {
            sim.tick(&[(A, IDLE, 0)]);
        }
        assert!(sim.pvp.iter().any(|e| matches!(e, PvpEvent::Respawn { player: A, .. })));
        let (p, v) = (sim.player(A).pos(&sim.s.world), &sim.player(A).vitals);
        assert!(v.alive() && (p[0] - 15.0).abs() < 1.0 && p[1] > 0.0, "respawned at the spawn point: {p:?}");
        for _ in 0..60 {
            sim.tick(&[(A, IDLE, 0)]);
        }
        assert_eq!(void_kills(&sim), 1, "the respawned player is not killed again");
    }

    #[test]
    fn the_void_kills_through_spawn_protection_and_a_lower_plane_is_a_real_control() {
        let mut sim = island(None);
        sim.tick(&[(A, IDLE, 0)]);
        sim.s.players[0].vitals.invuln_until_tick = sim.s.tick + 10_000; // protected for the whole test
        throw_into_void(&mut sim);
        for _ in 0..150 {
            sim.tick(&[(A, IDLE, 0)]);
        }
        assert_eq!(void_kills(&sim), 1, "protection does not stop the void");

        // control: with the plane far below, the same fall is not a death yet
        let mut sim = island(Some(-1000.0));
        sim.tick(&[(A, IDLE, 0)]);
        throw_into_void(&mut sim);
        for _ in 0..150 {
            sim.tick(&[(A, IDLE, 0)]);
        }
        assert_eq!(void_kills(&sim), 0);
        assert!(sim.player(A).pos(&sim.s.world)[1] < -20.0, "it really did fall past the default plane");
    }

    /// A shoots B (8 m, same floor) with `pitch`; returns the damage of the one Shuriken hit.
    fn shot_damage(mult: f32, pitch: u16) -> f32 {
        let mut sim = Sim::new(&[(A, [0.0, 0.81, 0.0]), (B, [0.0, 0.81, -8.0])]);
        sim.s.balance.combat.headshot_mult = mult;
        let fire = PlayerIntent { buttons: Buttons(Buttons::FIRE), aim_pitch: pitch, ..IDLE };
        sim.tick(&[(A, fire, 0), (B, IDLE, 0)]);
        for _ in 0..30 {
            sim.tick(&[(A, PlayerIntent { aim_pitch: pitch, ..IDLE }, 0), (B, IDLE, 0)]);
        }
        let hits: Vec<f32> = sim
            .pvp
            .iter()
            .filter_map(|e| if let PvpEvent::Hit { victim: B, kind: DamageKind::Shuriken, amount, .. } = e { Some(*amount) } else { None })
            .collect();
        assert_eq!(hits.len(), 1, "exactly one hit: {hits:?}");
        hits[0]
    }

    #[test]
    fn a_level_crosshair_is_a_headshot_and_aiming_low_is_a_body_shot() {
        let base = Balance::default().combat.shuriken.damage;
        // the shooter's eye is 0.6 above centre, so a level shot meets the target at head height
        assert_eq!(shot_damage(2.0, 2048), base * 2.0, "level = head");
        // 4.3 deg down from the eye hits the target's centre mass
        assert_eq!(shot_damage(2.0, 1950), base, "low = body");
        // control: with the bonus off, both are plain hits
        assert_eq!(shot_damage(1.0, 2048), base);
        assert_eq!(shot_damage(1.0, 1950), base);
    }

    /// `settle`: ticks the crosshair rests near the target before the throw (the lock needs `lock_secs` to ramp up).
    fn near_miss_hits(assist: bool, settle: u32) -> bool {
        // target 1.5 m off the crosshair at 20 m (~4.3 deg): a clean miss without assist
        let mut sim = Sim::new(&[(A, [0.0, 0.81, 0.0]), (B, [1.5, 0.81, -20.0])]);
        let mut b = sim.s.balance.clone();
        b.combat.shuriken.range = 0.0; // 20 m is past a star's 14 m range and its 20 m assist reach
        b.combat.shuriken.assist_range = 80.0;
        if !assist {
            b.combat.shuriken.assist_strength = 0.0;
        }
        sim.s.set_balance(b).unwrap();
        for _ in 0..settle {
            sim.tick(&[(A, IDLE, 0), (B, IDLE, 0)]);
        }
        sim.tick(&[(A, press(Buttons::FIRE), 0), (B, IDLE, 0)]);
        for _ in 0..60 {
            sim.tick(&[(A, IDLE, 0), (B, IDLE, 0)]);
        }
        sim.pvp.iter().any(|e| matches!(e, PvpEvent::Hit { victim: B, kind: DamageKind::Shuriken, .. }))
    }

    #[test]
    fn aim_assist_forgives_a_near_miss_and_can_be_turned_off() {
        // 30 ticks = 0.5 s on target: the lock is complete and the pull is full
        assert!(near_miss_hits(true, 30), "a locked assist bends the throw onto the target");
        assert!(!near_miss_hits(false, 30), "without assist the same throw misses");
        // the pull is weak while still acquiring: an instant throw at the same near-miss still misses
        assert!(!near_miss_hits(true, 0), "weak while acquiring");
    }

    /// A is a perfect tracker (crosshair always on B's current position, no lead) and throws at B strafing at
    /// ~`speed` across the line of fire at `dist` m. `lead` is `combat.shuriken.assist_lead`.
    fn strafing_throw_hits(dist: f32, lead: f32) -> bool {
        use shinobi_proto::quant::q_yaw;
        let mut sim = Sim::new(&[(A, [0.0, 0.81, 0.0]), (B, [-4.0, 0.81, -dist])]);
        let mut b = sim.s.balance.clone();
        b.combat.shuriken.speed = 70.0; // the demo's flat, fast stars
        b.combat.shuriken.range = 0.0; // up to 30 m in this test, past a star's 14 m range
        b.combat.shuriken.assist_range = 80.0;
        b.combat.shuriken.assist_lead = lead;
        b.combat.shuriken.homing_turn = 0.0; // this test is about the lead; homing has its own
        sim.s.set_balance(b).unwrap();
        let mut fired = false;
        for t in 0..140u32 {
            let bp = sim.player(B).pos(&sim.s.world);
            let yaw = (-bp[0]).atan2(-bp[2]);
            let fire = t == 50; // lock has formed while tracking
            let a = PlayerIntent { aim_yaw: q_yaw(yaw), aim_pitch: 2048, buttons: Buttons(if fire { Buttons::FIRE } else { 0 }), ..IDLE };
            fired |= fire;
            // B walks right (+x) for the first 70 ticks
            let bi = PlayerIntent { move_x: 127, ..IDLE };
            sim.tick(&[(A, a, 0), (B, if t < 70 { bi } else { IDLE }, 0)]);
        }
        assert!(fired);
        sim.pvp.iter().any(|e| matches!(e, PvpEvent::Hit { victim: B, kind: DamageKind::Shuriken, .. }))
    }

    #[test]
    fn a_locked_throw_leads_a_strafing_target() {
        for dist in [12.0f32, 20.0, 30.0] {
            assert!(strafing_throw_hits(dist, 1.0), "locked throw at {dist} m leads the mover");
            assert!(!strafing_throw_hits(dist, 0.0), "without lead the same throw trails a mover at {dist} m");
        }
    }

    #[test]
    fn shuriken_hits_a_player() {
        let mut sim = Sim::new(&[(A, [0.0, 0.81, 0.0]), (B, [0.0, 0.81, -8.0])]);
        sim.tick(&[(A, press(Buttons::FIRE), 0), (B, IDLE, 0)]);
        for _ in 0..40 {
            sim.tick(&[(A, IDLE, 0), (B, IDLE, 0)]);
        }
        assert_eq!(sim.player(B).vitals.hp, MAX_HP - 25.0);
        assert_eq!(sim.player(A).vitals.hp, MAX_HP);
        assert!(sim.pvp.iter().any(|e| matches!(e, PvpEvent::Hit { attacker: A, victim: B, kind: DamageKind::Shuriken, .. })));
    }

    #[test]
    fn own_shuriken_never_self_hits_even_when_it_ricochets_through_the_thrower() {
        // wall 2 m ahead sends the disc straight back through A and on to B standing behind A
        let mut sim = Sim::new(&[(A, [0.0, 0.81, 0.0]), (B, [0.0, 0.81, 6.0])]);
        sim.s.add_static_box([0.0, 1.0, -2.1], [5.0, 3.0, 0.1]);
        sim.tick(&[(A, press(Buttons::FIRE), 0), (B, IDLE, 0)]);
        for _ in 0..60 {
            sim.tick(&[(A, IDLE, 0), (B, IDLE, 0)]);
        }
        assert!(sim.combat.iter().any(|(id, e)| *id == A && matches!(e, CombatEvent::ShurikenBounce { .. })));
        assert_eq!(sim.hits_on(A), 0, "thrower was hit by own disc");
        assert_eq!(sim.player(A).vitals.hp, MAX_HP);
        assert_eq!(sim.hits_on(B), 1, "the ricochet did travel through A's position and reach B");
    }

    #[test]
    fn slice_kills_then_victim_respawns_protected_and_can_die_again() {
        // B at the edge of slice reach (lunge 3.8 m + blade 1.2 m), so A's lunge ends in front of B, not past it
        let mut sim = Sim::new(&[(A, [0.0, 0.81, 0.0]), (B, [0.0, 0.81, -4.8])]);
        sim.tick(&[(A, press(Buttons::SLICE), 0), (B, IDLE, 0)]);
        assert!(!sim.player(B).vitals.alive());
        assert_eq!((sim.player(A).vitals.kills, sim.player(B).vitals.deaths), (1, 1));
        assert!(sim.pvp.iter().any(|e| matches!(e, PvpEvent::Kill { killer: A, victim: B, .. })));

        // dead players cannot act
        for _ in 0..20 {
            sim.tick(&[(A, IDLE, 0), (B, press(Buttons::FIRE), 0)]);
        }
        assert!(!sim.combat.iter().any(|(id, e)| *id == B && matches!(e, CombatEvent::ShurikenThrown { .. })));

        // run to respawn
        while !sim.player(B).vitals.alive() {
            sim.tick(&[(A, IDLE, 0), (B, IDLE, 0)]);
            assert!(sim.s.tick < 400, "never respawned");
        }
        let v = &sim.player(B).vitals;
        assert_eq!(v.hp, MAX_HP);
        assert!(sim.pvp.iter().any(|e| matches!(e, PvpEvent::Respawn { player: B, .. })));
        let spawn = sim.player(B).pos(&sim.s.world);
        assert!((spawn[2] + 4.8).abs() < 0.3 && spawn[1] > 0.5, "teleported home: {spawn:?}");
        assert_eq!(sim.player(B).guard.strikes, 0, "respawn teleport must not look like a speed hack");

        // spawn protection: a point-blank shuriken (no lunge, so A stays put) does nothing...
        for _ in 0..40 {
            sim.tick(&[(A, IDLE, 0), (B, IDLE, 0)]);
        }
        let inv_until = sim.player(B).vitals.invuln_until_tick;
        assert!(sim.s.tick < inv_until);
        let (a, b) = (sim.player(A).pos(&sim.s.world), sim.player(B).pos(&sim.s.world));
        assert!(a[2] > b[2] && a[2] - b[2] < 2.0, "A must be in slice range in front of B: {a:?} {b:?}");
        for _ in 0..3 {
            sim.tick(&[(A, press(Buttons::FIRE), 0), (B, IDLE, 0)]);
        }
        assert_eq!(sim.hits_on(B), 1, "only the kill so far: protected B took nothing");
        assert_eq!(sim.player(B).vitals.hp, MAX_HP);
        // ...and after it expires the same slice kills again
        while sim.s.tick < inv_until + 40 {
            sim.tick(&[(A, IDLE, 0), (B, IDLE, 0)]);
        }
        // positive control for the shot above: the same point-blank shuriken now lands
        for _ in 0..3 {
            sim.tick(&[(A, press(Buttons::FIRE), 0), (B, IDLE, 0)]);
        }
        assert_eq!(sim.hits_on(B), 2);
        assert_eq!(sim.player(B).vitals.hp, MAX_HP - 25.0);
        while sim.s.tick % 60 != 0 {
            sim.tick(&[(A, IDLE, 0), (B, IDLE, 0)]);
        }
        sim.tick(&[(A, press(Buttons::SLICE), 0), (B, IDLE, 0)]);
        assert_eq!(sim.player(B).vitals.deaths, 2);
        assert_eq!(sim.player(A).vitals.kills, 2);
    }

    /// B strafes +X across A's line of fire at `b_z`. Returns the first tick whose recorded pose has x >= 0.
    fn crossing_tick(b_z: f32) -> u32 {
        let mut sim = Sim::new(&[(A, [0.0, 0.81, 0.0]), (B, [-3.0, 0.81, b_z])]);
        let run = PlayerIntent { move_x: 127, ..IDLE };
        for _ in 0..80 {
            sim.tick(&[(A, IDLE, 0), (B, run, 0)]);
        }
        (0..80).find(|t| sim.player(B).history.get(*t).map_or(false, |p| p[0] >= 0.0)).expect("B never crossed x = 0")
    }

    /// A acts with `button` at `fire_tick`; every packet carries a view `rewind` ticks behind the server.
    fn duel(button: u8, fire_tick: u32, rewind: u32, b_z: f32) -> Sim {
        let mut sim = Sim::new(&[(A, [0.0, 0.81, 0.0]), (B, [-3.0, 0.81, b_z])]);
        // this is about lag compensation: no aim assist bending the live shot onto the mover
        let mut b = sim.s.balance.clone();
        b.combat.shuriken.assist_strength = 0.0;
        b.combat.shuriken.assist_lead = 0.0;
        b.combat.shuriken.assist_cone_deg = 0.0; // no lock either: the katana magnet would bend the live slice onto the mover
        sim.s.set_balance(b).unwrap();
        let run = PlayerIntent { move_x: 127, ..IDLE };
        for t in 0..90 {
            let a = if t == fire_tick { press(button) } else { IDLE };
            sim.tick(&[(A, a, rewind), (B, run, 0)]);
        }
        sim
    }

    #[test]
    fn rewound_shuriken_hits_a_target_that_has_since_moved_away() {
        let t0 = crossing_tick(-6.0);
        // disc needs ~8 ticks to reach z = -6; aim so the *displayed* (12 ticks old) B crosses the line then
        let fire = t0 + 4;
        let rewound = duel(Buttons::FIRE, fire, 12, -6.0);
        assert_eq!(rewound.player(B).vitals.hp, MAX_HP - 25.0, "rewound shot must hit");
        // negative control: identical shot against the live positions misses (B is ~1.8 m away by then)
        let live = duel(Buttons::FIRE, fire, 0, -6.0);
        assert_eq!(live.player(B).vitals.hp, MAX_HP, "without lag compensation the same shot misses");
    }

    #[test]
    fn rewound_slice_hits_a_target_that_has_since_moved_away() {
        let t0 = crossing_tick(-3.0);
        // re-baselined for run 7.2 m/s (was 5.4, fire t0+36): fire t0+28 so the displayed B (24 ticks back) is at x ~ 0.5 (in the box); live B is ~3.4 m off (outside box + aim assist)
        let fire = t0 + 28;
        let rewound = duel(Buttons::SLICE, fire, 24, -3.0);
        assert!(!rewound.player(B).vitals.alive() || rewound.player(B).vitals.deaths > 0, "rewound slice must hit");
        let live = duel(Buttons::SLICE, fire, 0, -3.0);
        assert_eq!(live.player(B).vitals.deaths, 0, "without lag compensation the slice misses");
        assert_eq!(live.player(B).vitals.hp, MAX_HP);
    }
}

#[cfg(test)]
mod regen_tests {
    use super::*;

    fn hurt(hp: f32, last: u32) -> Vitals {
        Vitals { hp, max_hp: 100.0, kills: 0, deaths: 0, kill_streak: 0, dead_until_tick: None, invuln_until_tick: 0, last_damage_tick: last, spawn: [0.0; 3], hitbox: ColliderHandle::invalid() }
    }

    #[test]
    fn regen_waits_for_the_delay_then_heals_to_max_and_skips_the_dead() {
        let mut s = crate::tick::Server::new(Cfg::default());
        s.add_player(1, [0.0, 1.0, 0.0]);
        s.add_player(2, [5.0, 1.0, 0.0]);
        s.players[0].vitals = Vitals { hitbox: s.players[0].vitals.hitbox, ..hurt(40.0, 100) };
        s.players[1].vitals = Vitals { hitbox: s.players[1].vitals.hitbox, dead_until_tick: Some(500), ..hurt(0.0, 0) };
        regen(&mut s.players, 339, 240, 1.0 / 3.0);
        assert_eq!(s.players[0].vitals.hp, 40.0, "still inside the 4 s delay");
        regen(&mut s.players, 340, 240, 1.0 / 3.0);
        assert!(s.players[0].vitals.hp > 40.0, "delay over: healing");
        for t in 341..700 {
            regen(&mut s.players, t, 240, 1.0 / 3.0);
        }
        assert_eq!(s.players[0].vitals.hp, 100.0, "capped at max");
        assert_eq!(s.players[1].vitals.hp, 0.0, "the dead do not heal");
        regen(&mut s.players, 1000, 240, 0.0);
    }
}
