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
    if cone_deg <= 0.0 {
        return None;
    }
    let min_cos = cone_deg.to_radians().cos();
    let mut best: Option<Target> = None;
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
        if cos < min_cos || best.map_or(false, |b| cos <= b.cos) {
            continue;
        }
        if crate::spawn::blocked(world, [eye.x, eye.y, eye.z], [centre.x, centre.y, centre.z]) {
            continue;
        }
        best = Some(Target { collider: h, id: c.user_data, centre, dir, dist, cos });
    }
    best
}

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
    break_left: f32,
}

impl Default for LockState {
    fn default() -> Self {
        Self { phase: LockPhase::Search, target: None, target_id: 0, progress: 0.0, pos: [0.0; 3], dir: Vector::z(), break_left: 0.0 }
    }
}

impl LockState {
    /// Advance by `dt` given this tick's `best_target`. Returns the edge that happened, if any.
    pub fn update(&mut self, dt: f32, cfg: &LockCfg, found: Option<&Target>) -> Option<LockEdge> {
        match found {
            Some(t) => {
                let same = self.target == Some(t.collider) && matches!(self.phase, LockPhase::Acquire | LockPhase::Locked);
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
        let wider = enemy(&mut w, vector![1.2, 1.0, -10.0], 2); // ~6.8 deg off, outside a 5 deg cone
        w.colliders.insert(ColliderBuilder::cuboid(0.6, 3.0, 0.1).translation(vector![-0.75, 1.0, -5.0]).collision_groups(InteractionGroups::new(GROUP_WORLD, Group::ALL)).build());
        let walled = enemy(&mut w, vector![-3.0, 1.0, -20.0], 3); // behind that wall
        w.queries.update(&w.colliders);

        let t = best_target(&w, EYE, FWD, 5.0, 40.0, &[], body).expect("an enemy in the cone");
        assert_eq!((t.collider, t.id), (near_axis, 1));
        assert!(best_target(&w, EYE, FWD, 5.0, 40.0, &[near_axis], body).is_none(), "an ally (ignored) is never selected; the next is outside the cone");
        let wide = best_target(&w, EYE, FWD, 9.0, 40.0, &[near_axis], body).expect("wider cone");
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
