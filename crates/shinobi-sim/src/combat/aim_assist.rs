//! The one aim-assist brain (docs/lock-on-and-hud.md): which enemy is under the crosshair cone, and the lock
//! state machine that turns "a target stays in the cone" into a pull that grows with time.
//!
//! `best_target` is used by the shuriken throw, the katana's hit sweep, the server and the client predictor, so the
//! triangle the player sees is exactly the target the assist bends toward. Everything here is a pure function of
//! (world, eye, aim, dt): the same inputs give the same lock on server and client.
//!
//! Enemies only: allies, the owner's own hitbox and anything hidden from the attacker's query (dead, spawn-
//! protected: their hitbox has no groups, see `pvp::rewind_hitboxes`) are never selected; a wall between eye and
//! target (`spawn::blocked`) breaks the lock.

use rapier3d::prelude::*;

use super::TARGETS;
use crate::character::PhysicsWorld;

/// An enemy inside the assist cone with a clear line of sight.
#[derive(Clone, Copy, Debug)]
pub struct Target {
    pub collider: ColliderHandle,
    /// Collider `user_data`: the player id (or an NPC's id) the hitbox belongs to.
    pub id: u128,
    /// Aim point (hitbox centre, chest height).
    pub centre: Point<Real>,
    /// Unit vector from the eye to `centre`.
    pub dir: Vector<Real>,
    pub dist: f32,
    /// Cosine of the angle off the crosshair (1 = dead centre).
    pub cos: f32,
}

/// Narrowest the cone is allowed to get, in metres at the target: close targets are covered by a wider angle so a
/// strafing enemy at 4-8 m does not slip out of a 6 degree cone every half second.
pub const CONE_MIN_WIDTH: f32 = 1.5;
/// A target we already hold stays selected out to this multiple of the cone (hysteresis against jitter and movers).
pub const HOLD_CONE_MULT: f32 = 1.8;

/// Closest-to-the-crosshair enemy within `cone_deg` and `range`, with line of sight from `eye`. `ignore` = own
/// hitbox and teammates; `own_body` = the owner's body (parent of its colliders). Deterministic: ties keep the
/// first collider in the set's iteration order.
pub fn best_target(
    world: &PhysicsWorld,
    eye: Vector<Real>,
    fwd: Vector<Real>,
    cone_deg: f32,
    range: f32,
    ignore: &[ColliderHandle],
    own_body: RigidBodyHandle,
) -> Option<Target> {
    best_target_held(world, eye, fwd, cone_deg, range, ignore, own_body, None)
}

/// `best_target` with hysteresis: the `held` collider (the current lock) stays selected inside `HOLD_CONE_MULT` x
/// the cone, unless another enemy is within half as far off the crosshair as it is.
pub fn best_target_held(
    world: &PhysicsWorld,
    eye: Vector<Real>,
    fwd: Vector<Real>,
    cone_deg: f32,
    range: f32,
    ignore: &[ColliderHandle],
    own_body: RigidBodyHandle,
    held: Option<ColliderHandle>,
) -> Option<Target> {
    if cone_deg <= 0.0 {
        return None;
    }
    let cone = cone_deg.to_radians();
    let mut best: Option<Target> = None;
    let mut keep: Option<Target> = None;
    for (h, c) in world.colliders.iter() {
        if !c.collision_groups().memberships.intersects(TARGETS) || c.parent() == Some(own_body) || ignore.contains(&h) {
            continue;
        }
        let centre = Point::from(c.position().translation.vector);
        let to = centre.coords - eye;
        let dist = to.norm();
        if !(0.5..=range).contains(&dist) {
            continue;
        }
        let dir = to / dist;
        let cos = dir.dot(&fwd);
        let wide = cone.max((CONE_MIN_WIDTH / dist).atan());
        let is_held = held == Some(h);
        let reach = if is_held { wide * HOLD_CONE_MULT } else { wide };
        if cos < reach.min(std::f32::consts::FRAC_PI_2).cos() {
            continue;
        }
        if !is_held && best.map_or(false, |b| cos <= b.cos) {
            continue;
        }
        if crate::spawn::blocked(world, [eye.x, eye.y, eye.z], [centre.x, centre.y, centre.z]) {
            continue;
        }
        let t = Target { collider: h, id: c.user_data, centre, dir, dist, cos };
        if is_held {
            keep = Some(t);
        }
        if cos > best.map_or(f32::MIN, |b| b.cos) && (!is_held || cos >= wide.cos()) {
            best = Some(t); // a held target only competes as a fresh pick while it is inside the plain cone
        }
    }
    match (keep, best) {
        (Some(k), Some(b)) if b.collider != k.collider => {
            // switch only when the newcomer is much closer to the crosshair than the held target
            if b.cos.clamp(-1.0, 1.0).acos() < 0.5 * k.cos.clamp(-1.0, 1.0).acos() { Some(b) } else { Some(k) }
        }
        (Some(k), _) => Some(k),
        (None, b) => b,
    }
}

/// Fastest target speed the lead trusts (m/s): wall-runs and dashes top out below this.
const MAX_LEAD_SPEED: f32 = 20.0;

/// Lock tunables (Balance `combat.lock_*`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LockCfg {
    /// Seconds a target must stay in the cone to reach full lock.
    pub lock_secs: f32,
    /// Fraction of the full pull while acquiring (progress 0); rises linearly to 1.0 at lock.
    pub pull_min: f32,
    /// Seconds the broken lock lingers (visual scatter) before the state returns to Search.
    pub break_secs: f32,
}

impl Default for LockCfg {
    fn default() -> Self {
        Self { lock_secs: 0.35, pull_min: 0.25, break_secs: 0.25 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockPhase {
    /// No target in the cone.
    Search,
    /// A target is in the cone; dots converge as `progress` rises.
    Acquire,
    /// Fully locked (progress 1).
    Locked,
    /// Target just lost: scatter and fade for `break_secs`.
    Break,
}

/// State transitions, for audio / visuals to trigger on edges instead of polling `progress`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockEdge {
    /// A (new) target entered the cone.
    Acquired,
    /// Progress reached 1.
    Locked,
    /// The target left the cone / lost line of sight / was switched away.
    Broken,
}

#[derive(Clone, Copy, Debug)]
pub struct LockState {
    pub phase: LockPhase,
    pub target: Option<ColliderHandle>,
    pub target_id: u128,
    /// 0..=1 while acquiring/locked; decays to 0 during Break.
    pub progress: f32,
    /// Last known aim point and eye->target unit vector (valid while a target is held or breaking).
    pub pos: [f32; 3],
    pub dir: Vector<Real>,
    /// Smoothed target velocity (m/s), measured from its aim point between ticks (0 on a fresh lock).
    pub vel: Vector<Real>,
    break_left: f32,
}

impl Default for LockState {
    fn default() -> Self {
        Self { phase: LockPhase::Search, target: None, target_id: 0, progress: 0.0, pos: [0.0; 3], dir: Vector::z(), vel: Vector::zeros(), break_left: 0.0 }
    }
}

impl LockState {
    /// Advance by `dt` given this tick's `best_target`. Returns the edge that happened, if any.
    pub fn update(&mut self, dt: f32, cfg: &LockCfg, found: Option<&Target>) -> Option<LockEdge> {
        match found {
            Some(t) => {
                let same = self.target == Some(t.collider) && matches!(self.phase, LockPhase::Acquire | LockPhase::Locked);
                if same && dt > 0.0 {
                    let moved = (t.centre.coords - Vector::new(self.pos[0], self.pos[1], self.pos[2])) / dt;
                    // clamp (a rewind or respawn jump is not motion) and smooth (tick-to-tick positions are quantised)
                    let moved = if moved.norm() > MAX_LEAD_SPEED { moved * (MAX_LEAD_SPEED / moved.norm()) } else { moved };
                    self.vel = self.vel * 0.6 + moved * 0.4;
                } else {
                    self.vel = Vector::zeros();
                }
                self.pos = [t.centre.x, t.centre.y, t.centre.z];
                self.dir = t.dir;
                if !same {
                    self.target = Some(t.collider);
                    self.target_id = t.id;
                    self.progress = 0.0;
                    self.phase = LockPhase::Acquire;
                    self.break_left = 0.0;
                    return Some(LockEdge::Acquired);
                }
                let before = self.progress;
                self.progress = (self.progress + dt / cfg.lock_secs.max(1e-3)).min(1.0);
                if self.progress >= 1.0 {
                    self.phase = LockPhase::Locked;
                    if before < 1.0 {
                        return Some(LockEdge::Locked);
                    }
                }
                None
            }
            None => match self.phase {
                LockPhase::Acquire | LockPhase::Locked => {
                    self.phase = LockPhase::Break;
                    self.break_left = cfg.break_secs;
                    Some(LockEdge::Broken)
                }
                LockPhase::Break => {
                    self.break_left -= dt;
                    self.progress = (self.progress - dt / cfg.break_secs.max(1e-3)).max(0.0);
                    if self.break_left <= 0.0 {
                        *self = LockState::default();
                    }
                    None
                }
                LockPhase::Search => None,
            },
        }
    }

    /// Fraction (0..=1) of the full assist pull to apply right now: none while searching or breaking, weak while
    /// acquiring, full at lock.
    pub fn pull(&self, cfg: &LockCfg) -> f32 {
        match self.phase {
            LockPhase::Acquire | LockPhase::Locked => cfg.pull_min + (1.0 - cfg.pull_min) * self.progress,
            _ => 0.0,
        }
    }

    pub fn holding(&self) -> bool {
        matches!(self.phase, LockPhase::Acquire | LockPhase::Locked)
    }
}

/// What a weapon gets to know about the current lock when it fires.
#[derive(Clone, Copy, Debug)]
pub struct LockHint {
    /// Unit eye->target vector.
    pub dir: Vector<Real>,
    /// 0..=1 pull fraction (`LockState::pull`).
    pub pull: f32,
    pub collider: ColliderHandle,
    /// Target aim point and its smoothed velocity (m/s), for leading a moving target.
    pub centre: Point<Real>,
    pub vel: Vector<Real>,
}

pub use crate::balance::AssistBalance as AssistCfg;

fn wrap(a: f32) -> f32 {
    (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

/// (yaw, pitch) of the unit-ish vector `v`, same convention as `CombatAim` (yaw 0 faces -Z, + turns toward -X).
pub fn angles_of(v: Vector<Real>) -> (f32, f32) {
    let len = v.norm().max(1e-6);
    (libm::atan2f(-v.x, -v.z), libm::asinf((v.y / len).clamp(-1.0, 1.0)))
}

/// What the look assist needs from the current lock for one tick.
#[derive(Clone, Copy, Debug)]
pub struct AssistInput {
    /// Eye position and the locked target's aim point (the lock's `pos`).
    pub eye: Vector<Real>,
    pub target: Vector<Real>,
    pub target_id: u128,
    /// `LockState::pull`, 0..=1.
    pub pull: f32,
    /// Assist cone half-angle, degrees (friction fades to nothing at its edge).
    pub cone_deg: f32,
    pub touch: bool,
    pub dt: f32,
}

/// Look assist: console-shooter friction (the crosshair slows near a target) and magnetism (it follows the target's
/// lateral motion while you are on it). Pure and deterministic: the result is what the client sends in its intent,
/// so the server needs nothing extra. Keeps the previous target angles to measure the target's angular velocity.
#[derive(Clone, Copy, Debug, Default)]
pub struct AimAssist {
    prev: Option<(u128, f32, f32)>,
}

impl AimAssist {
    /// `before` = aim at the start of the tick, `after` = aim with this tick's raw look applied (yaw, pitch rad).
    /// Returns the assisted aim. `input` is `None` when nothing is locked.
    pub fn apply(&mut self, cfg: &AssistCfg, input: Option<&AssistInput>, before: (f32, f32), after: (f32, f32)) -> (f32, f32) {
        let Some(i) = input else {
            self.prev = None;
            return after;
        };
        let (ty, tp) = angles_of(i.target - i.eye);
        let omega = match self.prev {
            Some((id, py, pp)) if id == i.target_id && i.dt > 0.0 => (wrap(ty - py) / i.dt, (tp - pp) / i.dt),
            _ => (0.0, 0.0),
        };
        self.prev = Some((i.target_id, ty, tp));

        let cone = i.cone_deg.to_radians().max(1e-3);
        let (dy, dp) = (wrap(ty - after.0), tp - after.1);
        let off = (dy * after.1.cos()).hypot(dp);
        let fall = (1.0 - off / cone).clamp(0.0, 1.0);
        let k = i.pull * fall * if i.touch { cfg.touch_mult } else { 1.0 };
        if k <= 0.0 {
            return after;
        }
        // friction: only the player's own look is slowed
        let slow = 1.0 - (cfg.friction * k).min(0.9);
        let (mut yaw, mut pitch) = (before.0 + wrap(after.0 - before.0) * slow, before.1 + (after.1 - before.1) * slow);
        // magnetism: follow the target's own angular motion, then drift toward its centre
        let m = (cfg.magnet * k).min(1.0);
        yaw += omega.0 * i.dt * m;
        pitch += omega.1 * i.dt * m;
        let c = (cfg.centre * k * i.dt).min(1.0);
        yaw += wrap(ty - yaw) * c;
        pitch += (tp - pitch) * c;
        (yaw.rem_euclid(std::f32::consts::TAU), pitch.clamp(-89f32.to_radians(), 89f32.to_radians()))
    }
}

/// `fwd` bent toward `to` by `strength` (0 = unchanged, 1 = onto the target), renormalised.
pub fn bend(fwd: Vector<Real>, to: Vector<Real>, strength: f32) -> Vector<Real> {
    let v = fwd * (1.0 - strength) + to * strength;
    let n = v.norm();
    if n > 1e-4 {
        v / n
    } else {
        fwd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::GROUP_ENEMY;
    use crate::combat::GROUP_WORLD;

    fn world() -> PhysicsWorld {
        PhysicsWorld {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            queries: QueryPipeline::new(),
        }
    }

    fn enemy(w: &mut PhysicsWorld, pos: Vector<Real>, id: u128) -> ColliderHandle {
        w.colliders.insert(ColliderBuilder::cuboid(0.4, 0.9, 0.4).translation(pos).collision_groups(InteractionGroups::new(GROUP_ENEMY, Group::ALL)).user_data(id).build())
    }

    fn own_body(w: &mut PhysicsWorld) -> RigidBodyHandle {
        w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![0.0, 1.0, 0.0]).build())
    }

    const EYE: Vector<Real> = Vector::new(0.0, 1.0, 0.0);
    const FWD: Vector<Real> = Vector::new(0.0, 0.0, -1.0);

    #[test]
    fn picks_the_enemy_nearest_the_crosshair_and_never_allies_self_or_walled_targets() {
        let mut w = world();
        let body = own_body(&mut w);
        let near_axis = enemy(&mut w, vector![0.5, 1.0, -10.0], 1); // ~2.9 deg off
        let wider = enemy(&mut w, vector![1.6, 1.0, -10.0], 2); // ~9.1 deg off, outside a 5 deg cone (and the 8.5 deg min-width cone at 10 m)
        w.colliders.insert(ColliderBuilder::cuboid(0.6, 3.0, 0.1).translation(vector![-0.75, 1.0, -5.0]).collision_groups(InteractionGroups::new(GROUP_WORLD, Group::ALL)).build());
        let walled = enemy(&mut w, vector![-3.0, 1.0, -20.0], 3); // behind that wall
        w.queries.update(&w.colliders);

        let t = best_target(&w, EYE, FWD, 5.0, 40.0, &[], body).expect("an enemy in the cone");
        assert_eq!((t.collider, t.id), (near_axis, 1));
        assert!(best_target(&w, EYE, FWD, 5.0, 40.0, &[near_axis], body).is_none(), "an ally (ignored) is never selected; the next is outside the cone");
        let wide = best_target(&w, EYE, FWD, 10.0, 40.0, &[near_axis], body).expect("wider cone");
        assert_eq!(wide.collider, wider);
        let towards_wall = vector![-3.0, 0.0, -20.0].normalize();
        assert!(best_target(&w, EYE, towards_wall, 5.0, 40.0, &[near_axis, wider], body).is_none(), "line of sight to {walled:?} is walled off");
        assert!(best_target(&w, EYE, FWD, 5.0, 8.0, &[], body).is_none(), "range limit");
        // the owner's own hitbox (in `ignore`) is never a target
        let me = enemy(&mut w, vector![0.0, 1.0, -3.0], 9);
        w.queries.update(&w.colliders);
        let t = best_target(&w, EYE, FWD, 5.0, 40.0, &[me], body).unwrap();
        assert_ne!(t.id, 9);
    }

    #[test]
    fn selection_is_deterministic() {
        let mut w = world();
        let body = own_body(&mut w);
        for i in 0..6 {
            enemy(&mut w, vector![(i as f32 - 2.5) * 0.4, 1.0, -12.0 - i as f32], i as u128);
        }
        w.queries.update(&w.colliders);
        let a = best_target(&w, EYE, FWD, 8.0, 60.0, &[], body).map(|t| (t.collider, t.cos));
        for _ in 0..5 {
            assert_eq!(best_target(&w, EYE, FWD, 8.0, 60.0, &[], body).map(|t| (t.collider, t.cos)), a);
        }
    }

    #[test]
    fn lock_ramps_snaps_breaks_and_pulls_harder_as_it_builds() {
        let cfg = LockCfg { lock_secs: 0.5, pull_min: 0.2, break_secs: 0.25 };
        let mut w = world();
        let body = own_body(&mut w);
        let h = enemy(&mut w, vector![0.0, 1.0, -10.0], 1);
        w.queries.update(&w.colliders);
        let t = best_target(&w, EYE, FWD, 5.0, 40.0, &[], body).unwrap();
        assert_eq!(t.collider, h);

        let mut l = LockState::default();
        assert_eq!(l.pull(&cfg), 0.0, "search: no pull");
        assert_eq!(l.update(0.1, &cfg, Some(&t)), Some(LockEdge::Acquired));
        assert_eq!((l.phase, l.progress), (LockPhase::Acquire, 0.0));
        assert!((l.pull(&cfg) - 0.2).abs() < 1e-6, "weak at progress 0");
        let mut edges = Vec::new();
        for _ in 0..10 {
            edges.extend(l.update(0.1, &cfg, Some(&t)));
        }
        assert_eq!(edges, vec![LockEdge::Locked], "exactly one Locked edge");
        assert_eq!((l.phase, l.progress), (LockPhase::Locked, 1.0));
        assert!((l.pull(&cfg) - 1.0).abs() < 1e-6, "full at lock");
        // losing the target breaks the lock, fades, and returns to search
        assert_eq!(l.update(0.1, &cfg, None), Some(LockEdge::Broken));
        assert_eq!(l.phase, LockPhase::Break);
        assert_eq!(l.pull(&cfg), 0.0, "no pull while breaking");
        assert!(l.progress < 1.0 || l.update(0.1, &cfg, None).is_none());
        for _ in 0..5 {
            l.update(0.1, &cfg, None);
        }
        assert_eq!(l.phase, LockPhase::Search);
        assert!(l.target.is_none());
    }
}
