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
}

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
            half_width: 1.3,
            half_height: 1.0,
            // Matching the dash cooldown keeps every slice paired with a lunge.
            cooldown: ch.dash_cd,
            lunge_distance: dash_distance(ch),
        }
    }
}

/// The movement-relevant part of a katana swing, for client prediction and replay. Mirrors `Katana::fire`
/// minus everything that needs the world (wall truncation of the blade, hit boxes): advance the cooldown by
/// `dt`, and if `pressed` and ready, consume it and (when the dash is ready) turn the tick's input into a dash
/// along `dir`. Returns `Some(lunged)` when a swing happened.
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
    dir: Vector<Real>,
    input: &mut crate::character::PlayerInput,
) -> Option<bool> {
    *cd_left = (*cd_left - dt).max(0.0);
    if pressed && *cd_left <= 0.0 {
        if dash_ready {
            input.dash_pressed = true;
            input.move_dir = dir;
        }
        *cd_left = cooldown;
        Some(dash_ready)
    } else {
        None
    }
}

pub struct Katana {
    pub cfg: KatanaCfg,
    flip: bool,
}

impl Katana {
    pub fn new(cfg: KatanaCfg) -> Self {
        Self { cfg, flip: false }
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

    fn fire(&mut self, ctx: &mut FireCtx) -> bool {
        let c = self.cfg;
        let dir = ctx.aim.flat_forward();
        let origin = ctx.aim.origin;

        // Walls stop both lunge and blade: truncate the longest possible reach at the first solid.
        let solid = QueryFilter::default()
            .exclude_rigid_body(ctx.ch.body)
            .exclude_sensors()
            .groups(InteractionGroups::new(GROUP_MELEE, solid_groups()));
        let ray = Ray::new(Point::from(origin), dir);
        let max_reach = c.blade_len + c.lunge_distance;
        let wall = ctx
            .world
            .queries
            .cast_ray(&ctx.world.bodies, &ctx.world.colliders, &ray, max_reach, true, solid)
            .map(|(_, toi)| toi.max(0.3));
        let full = wall.unwrap_or(max_reach).min(max_reach);
        let short = c.blade_len.min(full);

        // Short-lived hit boxes: query-only, they exist for exactly this call.
        let filter = QueryFilter::default()
            .exclude_rigid_body(ctx.ch.body)
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
        let boxhits = |reach: f32| -> Vec<ColliderHandle> {
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
        let mut lunge = ctx.ch.dash_ready();
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
            ctx.input.dash_pressed = true;
            ctx.input.move_dir = dir;
        }

        for h in &hits {
            let Some(col) = ctx.world.colliders.get(*h) else { continue };
            ctx.events.push(CombatEvent::Damage {
                target: col.user_data,
                collider: *h,
                amount: c.damage,
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
            reach,
            half_width: c.half_width,
            half_height: c.half_height,
            damage: c.damage,
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
        for t in 0..600u32 {
            // slice taps at irregular intervals, dash taps that sometimes steal the dash cooldown first
            let slice = t % 25 == 3 || t % 91 == 50;
            let dash = t % 53 == 20;
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
            let got = predict_slice(k.cooldown, &mut cd, DT, slice, ch.dash_ready(), aim.flat_forward(), &mut pred);

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
}
