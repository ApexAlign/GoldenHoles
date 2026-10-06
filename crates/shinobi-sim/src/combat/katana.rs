use rapier3d::na::UnitQuaternion;
use rapier3d::prelude::*;

use super::{
    solid_groups, TARGETS,
    weapon::{FireCtx, Trigger, Weapon},
    CombatEvent, DamageKind, GROUP_MELEE,
};
use crate::character::Cfg;

/// Yaw offsets (degrees, tried left and right) a missed swing re-sweeps before giving up.
const ASSIST_STEPS_DEG: [f32; 3] = [8.0, 16.0, 24.0];

#[derive(Clone, Copy, Debug)]
pub struct KatanaCfg {
    pub damage: f32,
    pub blade_len: f32,
    pub half_width: f32,
    pub half_height: f32,
    pub cooldown: f32,
    /// Horizontal distance the character's dash covers; sizes the hit box.
    pub lunge_distance: f32,
    /// Strength (0..1, times the lock pull) of the hit sweep's bend toward the lock-on target.
    pub magnet: f32,
    /// Damage to each player-built piece inside the blade box.
    pub piece_damage: f32,
    /// Hook-strike (swing while hooked / just after the rope let go): damage multiplier, and blade reach added on top
    /// of the blade length (plus `STRIKE_REACH_PER_MPS` x the speed you arrive with).
    pub hook_damage_mult: f32,
    pub hook_reach: f32,
    /// Wall slash (swing during a wall-run): same two knobs.
    pub wall_damage_mult: f32,
    pub wall_reach: f32,
    /// Seconds after a swing within which the next swing continues the combo.
    pub combo_window: f32,
    /// Damage multiplier of the third swing of a combo (every swing before it must have connected).
    pub finisher_mult: f32,
    /// Extra blade length (m) of that finishing swing.
    pub finisher_reach: f32,
    /// Target-centred hit cone: a swing also hits any enemy whose centre lies within this half-angle (degrees, flat) of
    /// the aim and within reach + `cone_extra` of the eye, line of sight permitting (0 = off, box only).
    pub cone_deg: f32,
    /// Extra reach (m) of the cone beyond the blade box: generous at the start of a swing.
    pub cone_extra: f32,
}

/// Extra strike reach per m/s of speed at the moment of the swing (a 40 m/s reel reaches 6 m further).
pub const STRIKE_REACH_PER_MPS: f32 = 0.15;

/// A katana swing comes round after this fraction of the dash cooldown: a fast combo of cuts, of which only every
/// third or so carries the dash lunge (the dash has its own, longer cooldown). Derived from the character, like
/// the dash itself, so the client's prediction (`KatanaCfg::for_character`) and the server agree.
pub const SWING_CD_FRAC: f32 = 0.35;
/// Swings in one combo; the last is the finisher.
pub const COMBO_LEN: u32 = 3;

/// Distance `CharacterSystem`'s dash travels: speed = dash_speed * (0.4 + 0.6 f), f: 1 -> 0 over
/// `dash_time`, so the mean speed is 0.7 * dash_speed (30 * 0.18 * 0.7 = 3.8 m by default).
pub fn dash_distance(ch: &Cfg) -> f32 {
    ch.dash_speed * ch.dash_time * 0.7
}

impl KatanaCfg {
    pub fn for_character(ch: &Cfg) -> Self {
        Self {
            damage: 120.0,
            blade_len: 1.6,
            half_width: 1.0,
            half_height: 1.0,
            // Swings are quicker than the dash comes back: a combo of cuts, the lunge on the first.
            cooldown: (ch.dash_cd * SWING_CD_FRAC).max(0.25),
            lunge_distance: dash_distance(ch),
            magnet: 0.8,
            piece_damage: 60.0,
            hook_damage_mult: 1.5,
            hook_reach: 4.0,
            wall_damage_mult: 1.35,
            wall_reach: 3.0,
            combo_window: 1.0,
            finisher_mult: 1.0,
            finisher_reach: 0.0,
            cone_deg: 0.0,
            cone_extra: 0.0,
        }
    }
}

/// Enemies inside the swing's cone: the target-centred half of the hit test. The blade box is a hand-sized volume and a
/// moving target slips out of its corners; this is "is the enemy roughly where I swung, and near enough".
fn cone_hits(world: &crate::character::PhysicsWorld, eye: Vector<Real>, dir: Vector<Real>, reach: f32, cone_deg: f32, own: RigidBodyHandle, allies: &[ColliderHandle], lock: Option<ColliderHandle>) -> Vec<ColliderHandle> {
    let cos = cone_deg.to_radians().cos();
    let flat_dir = vector![dir.x, 0.0, dir.z];
    let fd = flat_dir.norm();
    if fd < 1e-4 {
        return Vec::new();
    }
    let flat_dir = flat_dir / fd;
    let mut out = Vec::new();
    for (h, c) in world.colliders.iter() {
        if !c.collision_groups().memberships.intersects(TARGETS) || c.parent() == Some(own) || allies.contains(&h) {
            continue;
        }
        let centre = *c.translation();
        let to = centre - eye;
        let flat = vector![to.x, 0.0, to.z];
        let dist = flat.norm();
        // a locked target is forgiven a wider angle (the player is plainly trying to hit it)
        let wide = if lock == Some(h) { (cos * 0.82).max(-1.0) } else { cos };
        if dist > reach || dist < 1e-4 || flat.dot(&flat_dir) / dist < wide || to.y.abs() > 1.6 {
            continue;
        }
        if crate::spawn::blocked(world, [eye.x, eye.y, eye.z], [centre.x, centre.y, centre.z]) {
            continue;
        }
        out.push(h);
    }
    out
}

/// The movement-relevant part of a katana swing, for client prediction and replay. Mirrors `Katana::fire`
/// minus everything that needs the world (wall truncation of the blade, hit boxes): advance the cooldown by
/// `dt`, and if `pressed` and ready, consume it and (when the dash is ready, or `strike` says a hook-strike / wall
/// slash launch applies) turn the tick's input into a dash / strike launch along `dir`. Returns `Some(lunged)` when a
/// swing happened.
///
/// Known divergence: `Katana::fire` cuts in place when a target is already inside blade reach (it must not dash
/// through them); that needs the enemy set, which the client does not have, so it is corrected by reconciliation.
/// `tests::predict_slice_matches_the_real_katana` keeps the two in step.
pub fn predict_slice(
    cooldown: f32,
    cd_left: &mut f32,
    dt: f32,
    pressed: bool,
    dash_ready: bool,
    strike: Option<crate::character::StrikeKind>,
    dir: Vector<Real>,
    input: &mut crate::character::PlayerInput,
) -> Option<bool> {
    *cd_left = (*cd_left - dt).max(0.0);
    if pressed && *cd_left <= 0.0 {
        let lunge = dash_ready || strike.is_some();
        if strike.is_some() {
            input.strike = true;
            input.move_dir = dir;
        } else if dash_ready {
            input.dash_pressed = true;
            input.move_dir = dir;
        }
        *cd_left = cooldown;
        Some(lunge)
    } else {
        None
    }
}

pub struct Katana {
    pub cfg: KatanaCfg,
    flip: bool,
    /// Consecutive swings that connected inside the combo window (0, 1, 2 -> the next one is the finisher).
    combo: u32,
    /// `now` of the last swing (the combo window runs from it).
    last_swing: f32,
}

impl Katana {
    pub fn new(cfg: KatanaCfg) -> Self {
        Self { cfg, flip: false, combo: 0, last_swing: f32::NEG_INFINITY }
    }
}

impl Weapon for Katana {
    fn name(&self) -> &'static str {
        "Cyber-Katana"
    }
    fn apply_balance(&mut self, b: &crate::balance::Balance, ch: &Cfg) {
        let mut k = KatanaCfg::for_character(ch);
        b.apply_katana(&mut k);
        self.cfg = k;
    }
    fn trigger(&self) -> Trigger {
        Trigger::Secondary
    }
    fn cooldown(&self) -> f32 {
        self.cfg.cooldown
    }

    fn reset(&mut self) {
        self.combo = 0;
        self.last_swing = f32::NEG_INFINITY;
    }

    fn fire(&mut self, ctx: &mut FireCtx) -> bool {
        let mut c = self.cfg;
        // combo: every swing that connected within the window brings the finisher closer; it hits harder and reaches further
        if ctx.now - self.last_swing > c.combo_window {
            self.combo = 0;
        }
        let finisher = self.combo + 1 >= COMBO_LEN;
        if finisher {
            c.damage *= c.finisher_mult;
            c.blade_len += c.finisher_reach;
        }
        let dir = ctx.aim.flat_forward();
        let origin = ctx.aim.origin;

        // Walls stop both lunge and blade: truncate the longest possible reach at the first solid.
        let solid = QueryFilter::default()
            .exclude_rigid_body(ctx.ch.body)
            .exclude_sensors()
            .groups(InteractionGroups::new(GROUP_MELEE, solid_groups()));
        let ray = Ray::new(Point::from(origin), dir);
        // hook-strike / wall slash: the launch carries the blade further and hits harder (docs/traversal_melee.md)
        let strike = ctx.ch.strike_kind();
        let (strike_dmg, strike_reach) = match strike {
            Some(crate::character::StrikeKind::Hook) => (c.hook_damage_mult, c.hook_reach),
            Some(crate::character::StrikeKind::Wall) => (c.wall_damage_mult, c.wall_reach),
            None => (1.0, 0.0),
        };
        let strike_reach = if strike.is_some() { strike_reach + STRIKE_REACH_PER_MPS * ctx.ch.vel.norm().min(60.0) } else { 0.0 };
        let max_reach = c.blade_len + if strike.is_some() { strike_reach } else { c.lunge_distance };
        let wall = ctx
            .world
            .queries
            .cast_ray(&ctx.world.bodies, &ctx.world.colliders, &ray, max_reach, true, solid);
        // the solid that truncates the blade may itself be a printed piece: it is struck by the swing
        let wall_piece = wall.and_then(|(h, _)| ctx.world.colliders.get(h)).and_then(|col| crate::blocks::piece_cell(col.user_data));
        let wall = wall.map(|(_, toi)| toi.max(0.3));
        let full = wall.unwrap_or(max_reach).min(max_reach);
        let short = c.blade_len.min(full);

        // Short-lived hit boxes: query-only, they exist for exactly this call.
        let not_ally = |h: ColliderHandle, _: &Collider| !ctx.allies.contains(&h);
        let filter = QueryFilter::default()
            .exclude_rigid_body(ctx.ch.body)
            .predicate(&not_ally)
            .groups(InteractionGroups::new(GROUP_MELEE, TARGETS));
        let boxhits_at = |dir: Vector<Real>, reach: f32| -> Vec<ColliderHandle> {
            let rot = UnitQuaternion::face_towards(&dir, &Vector::y());
            let shape = Cuboid::new(vector![c.half_width, c.half_height, reach * 0.5]);
            let pos = Isometry::from_parts((origin + dir * (reach * 0.5)).into(), rot);
            let mut hits = Vec::new();
            ctx.world.queries.intersections_with_shape(&ctx.world.bodies, &ctx.world.colliders, &pos, &shape, filter, |h| {
                hits.push(h);
                true
            });
            hits
        };
        // Melee aim assist: a swing that misses dead-ahead re-sweeps a few degrees to either side and takes the
        // first sweep that connects. Hit boxes only: the lunge keeps the aimed direction (client prediction).
        // magnetism: with a lock, sweep first along the aim bent toward the target (horizontally: the blade is flat)
        let magnet_dir = ctx.lock.map(|h| {
            let flat = vector![h.dir.x, 0.0, h.dir.z];
            let n = flat.norm();
            if n > 1e-4 { super::aim_assist::bend(dir, flat / n, c.magnet * h.pull) } else { dir }
        });
        let boxhits = |reach: f32| -> Vec<ColliderHandle> {
            if let Some(m) = magnet_dir {
                let h = boxhits_at(m, reach);
                if !h.is_empty() {
                    return h;
                }
            }
            let straight = boxhits_at(dir, reach);
            if !straight.is_empty() {
                return straight;
            }
            for deg in ASSIST_STEPS_DEG {
                for sign in [1.0f32, -1.0] {
                    let r = UnitQuaternion::from_axis_angle(&Vector::y_axis(), (deg * sign).to_radians());
                    let h = boxhits_at(r * dir, reach);
                    if !h.is_empty() {
                        return h;
                    }
                }
            }
            straight
        };

        // Lunge = the character's own dash, aimed along the slash. If the dash is still on cooldown the
        // slash still lands (blade reach only, no lunge) rather than eating the input. If someone is already
        // inside blade reach we cut in place: dashing would carry you through them and out the other side.
        let mut lunge = strike.is_some() || ctx.ch.dash_ready();
        let mut reach = if lunge { full } else { short };
        let mut hits = boxhits(reach);
        if lunge {
            let near = boxhits(short);
            if !near.is_empty() {
                lunge = false;
                reach = short;
                hits = near;
            }
        }
        if lunge {
            if strike.is_some() {
                ctx.input.strike = true;
            } else {
                ctx.input.dash_pressed = true;
            }
            ctx.input.move_dir = dir;
        }

        // target-centred cone: anything roughly where the swing points and near enough is hit, whatever the box did
        let (mut ev_reach, mut ev_half) = (reach, c.half_width);
        if c.cone_deg > 0.0 {
            ev_reach = reach + c.cone_extra;
            for h in cone_hits(ctx.world, origin, dir, ev_reach, c.cone_deg, ctx.ch.body, ctx.allies, ctx.lock.map(|l| l.collider)) {
                if !hits.contains(&h) {
                    hits.push(h);
                }
            }
            // the lag-compensated re-test in pvp.rs sweeps a cylinder of this half-width: match the cone at full reach
            ev_half = ev_half.max((ev_reach * c.cone_deg.to_radians().tan()).min(2.2));
        }

        // player-built pieces in the blade box take damage too (the server decides if they de-rez)
        {
            let shape = Cuboid::new(vector![c.half_width, c.half_height, reach * 0.5]);
            let pos = Isometry::from_parts((origin + dir * (reach * 0.5)).into(), UnitQuaternion::face_towards(&dir, &Vector::y()));
            let piece_filter = QueryFilter::default().groups(InteractionGroups::new(GROUP_MELEE, super::GROUP_WORLD));
            let mut cells: Vec<[i32; 3]> = wall_piece.into_iter().collect();
            ctx.world.queries.intersections_with_shape(&ctx.world.bodies, &ctx.world.colliders, &pos, &shape, piece_filter, |h| {
                if let Some(cell) = ctx.world.colliders.get(h).and_then(|col| crate::blocks::piece_cell(col.user_data)) {
                    cells.push(cell);
                }
                true
            });
            cells.sort();
            cells.dedup();
            for cell in cells {
                ctx.events.push(CombatEvent::PieceHit { cell, amount: c.piece_damage, kind: DamageKind::Slash });
            }
        }

        self.last_swing = ctx.now;
        self.combo = if hits.is_empty() || finisher { 0 } else { self.combo + 1 };

        if hits.is_empty() {
            ctx.events.push(CombatEvent::SlashMiss { origin: Point::from(origin), dir });
        }
        for h in &hits {
            let Some(col) = ctx.world.colliders.get(*h) else { continue };
            ctx.events.push(CombatEvent::SlashHit { target: col.user_data, point: Point::from(*col.translation()), killed: false });
            ctx.events.push(CombatEvent::Damage {
                target: col.user_data,
                collider: *h,
                amount: c.damage * strike_dmg,
                kind: DamageKind::Slash,
                point: Point::from(*col.translation()),
            });
        }

        self.flip = !self.flip;
        ctx.events.push(CombatEvent::Slash {
            flip: self.flip,
            dir,
            lunge,
            hits: hits.len() as u32,
            origin: Point::from(origin),
            reach: ev_reach,
            half_width: ev_half,
            half_height: c.half_height,
            damage: c.damage * strike_dmg,
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::{CharacterSystem, PhysicsWorld, PlayerInput};
    use crate::combat::{Aim, CombatInput, CombatSystem};

    const DT: f32 = 1.0 / 60.0;

    /// With no targets around, `predict_slice` must decide exactly what the real `Katana::fire` does: same
    /// input mutation, same cooldown timeline, same lunge/no-lunge, including while the dash is on cooldown.
    #[test]
    fn predict_slice_matches_the_real_katana() {
        let cfg = Cfg::default();
        let mut w = PhysicsWorld {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            queries: QueryPipeline::new(),
        };
        w.colliders.insert(
            ColliderBuilder::cuboid(100.0, 0.5, 100.0)
                .translation(vector![0.0, -0.5, 0.0])
                .collision_groups(InteractionGroups::new(crate::combat::GROUP_WORLD, Group::ALL)) // not a target
                .build(),
        );
        let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![0.0, 0.81, 0.0]).build());
        w.colliders.insert_with_parent(
            ColliderBuilder::capsule_y(cfg.half_height, cfg.radius)
                .collision_groups(InteractionGroups::new(crate::combat::GROUP_PLAYER, Group::ALL))
                .build(),
            body,
            &mut w.bodies,
        );
        let mut ch = CharacterSystem::new(body, cfg);
        let mut combat = CombatSystem::ninja(&cfg);
        let k = KatanaCfg::for_character(&cfg);
        let mut cd = 0.0f32;
        let mut now = 0.0;
        let mut swings = (0, 0);
        for t in 0..2000u32 {
            // slice taps at irregular intervals, dash taps that sometimes steal the dash cooldown first
            let slice = t % 25 == 3 || t % 91 == 50;
            let dash = t % 100 == 76 || t % 53 == 20;
            let yaw = t as f32 * 0.05;
            w.queries.update(&w.colliders);

            let aim = Aim { origin: vector![0.0, 0.81, 0.0], yaw, pitch: 0.0 };
            let mut real = PlayerInput { dash_pressed: dash, ..Default::default() };
            let mut ev = Vec::new();
            combat.update(DT, now, &CombatInput::new(false, slice), &aim, &w, &ch, &mut real, &mut ev);
            let real_slash = ev.iter().find_map(|e| match e {
                CombatEvent::Slash { lunge, .. } => Some(*lunge),
                _ => None,
            });

            let mut pred = PlayerInput { dash_pressed: dash, ..Default::default() };
            let got = predict_slice(k.cooldown, &mut cd, DT, slice, ch.dash_ready(), ch.strike_kind(), aim.flat_forward(), &mut pred);

            assert_eq!(got, real_slash, "tick {t}: swing/lunge decision");
            assert_eq!((pred.dash_pressed, pred.move_dir), (real.dash_pressed, real.move_dir), "tick {t}: input");
            assert!((cd - combat.slice_cooldown_left()).abs() < 1e-6, "tick {t}: cooldown {cd} vs {}", combat.slice_cooldown_left());
            if let Some(l) = got {
                swings.0 += 1;
                swings.1 += l as u32;
            }
            ch.set_slice_cooldown(cd);
            crate::movement::step_player(&mut ch, &real, &mut w, now, DT);
            now += DT;
        }
        assert!(swings.0 >= 10 && swings.1 >= 5 && swings.1 < swings.0, "covers lunges and cooldown-blocked swings: {swings:?}");
    }

    /// Drives the demo katana against a standing target: returns `(time, damage)` of every hit while the slice button
    /// is held down for `secs`; the target is out of reach during `away` (a whiff).
    fn hold_slice(secs: f32, away: Option<(f32, f32)>) -> Vec<(f32, f32)> {
        let cfg = Cfg::default();
        let mut w = PhysicsWorld {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            queries: QueryPipeline::new(),
        };
        let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![0.0, 0.81, 0.0]).build());
        let target = |z: f32| {
            ColliderBuilder::cuboid(0.4, 0.9, 0.4)
                .translation(vector![0.0, 0.81, z])
                .collision_groups(InteractionGroups::new(crate::combat::GROUP_ENEMY, Group::ALL))
                .user_data(7)
                .build()
        };
        let tgt = w.colliders.insert(target(-1.6));
        let ch = CharacterSystem::new(body, cfg);
        let mut combat = CombatSystem::ninja(&cfg);
        combat.apply_balance(&crate::balance::Balance::demo(), &cfg);
        let aim = Aim { origin: vector![0.0, 0.81, 0.0], yaw: 0.0, pitch: 0.0 };
        let mut hits = Vec::new();
        let (mut tgt, mut gone) = (tgt, false);
        let mut now = 0.0f32;
        while now < secs {
            let want_gone = away.map_or(false, |(a, b)| now >= a && now < b);
            if want_gone && !gone {
                w.colliders.remove(tgt, &mut w.islands, &mut w.bodies, false);
                gone = true;
            } else if !want_gone && gone {
                tgt = w.colliders.insert(target(-1.6));
                gone = false;
            }
            w.queries.update(&w.colliders);
            let mut input = PlayerInput::default();
            let mut ev = Vec::new();
            combat.update(DT, now, &CombatInput::new(false, true), &aim, &w, &ch, &mut input, &mut ev);
            for e in &ev {
                if let CombatEvent::Damage { amount, kind: DamageKind::Slash, .. } = e {
                    hits.push((now, *amount));
                }
            }
            now += DT;
        }
        hits
    }

    /// The demo combo: 20, 20, then a 30 finisher, then it starts over; swings 0.42 s apart. A whiff resets it.
    #[test]
    fn the_third_connected_cut_is_the_finisher_and_a_whiff_resets_the_combo() {
        let hits = hold_slice(2.4, None);
        let dmg: Vec<f32> = hits.iter().map(|h| h.1).collect();
        assert!(dmg.len() >= 5, "a held slice button keeps cutting: {hits:?}");
        assert_eq!(dmg[..5], [20.0, 20.0, 30.0, 20.0, 20.0], "combo: two cuts, a 30 hp finisher, then it starts over");
        let gaps: Vec<f32> = hits.windows(2).map(|w| w[1].0 - w[0].0).collect();
        assert!(gaps.iter().all(|g| (0.38..0.47).contains(g)), "swings come round every ~0.42 s: {gaps:?}");
        // five cuts = 20+20+30+20+20 = 110 >= 100 hp: a kill ~1.7 s after the first cut
        let ttk = hits[4].0 - hits[0].0;
        eprintln!("katana-only kill: 5 cuts in {ttk:.2} s");
        assert!((1.4..2.0).contains(&ttk), "katana-only time to kill {ttk}");

        // the target steps out of reach for one swing after the second cut (a whiff) and comes back: the combo starts over
        let hits = hold_slice(2.4, Some((0.6, 1.1)));
        let dmg: Vec<f32> = hits.iter().map(|h| h.1).collect();
        assert_eq!(dmg[..3], [20.0, 20.0, 20.0], "the whiff took the finisher away: {hits:?}");
    }
}
