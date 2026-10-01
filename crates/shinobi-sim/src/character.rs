use rapier3d::control::{CharacterAutostep, CharacterLength, KinematicCharacterController};
use rapier3d::prelude::*;

const MAX_SINK: f32 = 0.15; // deepest floor penetration the depenetration step will undo
pub const TRAIL_CAP: usize = 128;
pub const GROUP_ENEMY: Group = Group::GROUP_2;
pub const GROUP_TRAIL: Group = Group::GROUP_3;

#[derive(Clone, Copy)]
pub struct Cfg {
    pub half_height: f32,
    pub radius: f32,
    pub run_speed: f32,
    pub air_accel: f32,
    pub gravity: f32,
    pub terminal: f32,
    pub ground_stick: f32,
    pub jump_speed: f32,
    pub double_jump_speed: f32,
    pub peak_window: f32,
    pub coyote: f32,
    pub jump_buffer: f32,
    pub dash_speed: f32,
    pub dash_time: f32,
    pub dash_cd: f32,
    pub roll_speed: f32,
    pub roll_time: f32,
    pub wall_probe: f32,
    pub wall_min_speed: f32,
    pub wall_speed: f32,
    pub wall_stick: f32,
    pub wall_max_time: f32,
    pub wall_jump_push: f32,
    pub snap_dist: f32,
    pub slope_limit: f32,
    pub trail_spacing: f32,
    pub trail_life: f32,
    pub trail_height: f32,
    pub trail_thick: f32,
    pub slide_start_speed: f32, // min ground speed to begin a slide
    pub slide_boost: f32,       // speed at slide start
    pub slide_friction: f32,    // m/s^2 decay while sliding
    pub slide_min_speed: f32,   // slide ends below this
    pub slide_max_time: f32,
    pub slide_cd: f32,
    /// KCC skin. Raising it hides the sink glitch below but floats the capsule and moves the spawn-height
    /// every other system assumes, so `depenetrate` fixes the glitch instead.
    pub kcc_offset: f32,
    /// Eye (camera and aim origin) height above the capsule centre. Single source: the client camera, the
    /// predicted combat aim and the server's combat aim all read it, so shots leave where the crosshair is.
    /// Cosmetic/aim only; it never affects movement.
    pub eye_above_center: f32,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            half_height: 0.5,
            radius: 0.3,
            run_speed: 9.0,
            air_accel: 70.0,
            gravity: 28.0,
            terminal: 40.0,
            ground_stick: -2.0,
            jump_speed: 9.5,
            double_jump_speed: 8.5,
            peak_window: 2.5,
            coyote: 0.1,
            jump_buffer: 0.12,
            dash_speed: 30.0,
            dash_time: 0.18,
            dash_cd: 0.6,
            roll_speed: 14.0,
            roll_time: 0.4,
            wall_probe: 0.35,
            wall_min_speed: 4.0,
            wall_speed: 11.0,
            wall_stick: 1.5,
            wall_max_time: 1.5,
            wall_jump_push: 9.0,
            snap_dist: 0.25,
            slope_limit: 45f32.to_radians(),
            trail_spacing: 0.4,
            trail_life: 4.0,
            trail_height: 1.6,
            trail_thick: 0.15,
            slide_start_speed: 6.0,
            slide_boost: 14.0,
            slide_friction: 12.0,
            slide_min_speed: 5.0,
            slide_max_time: 1.2,
            slide_cd: 0.5,
            kcc_offset: 0.01,
            eye_above_center: 0.6,
        }
    }
}

#[derive(Default, Clone, Copy)]
pub struct PlayerInput {
    pub move_dir: Vector<Real>, // world space, y ignored, len <= 1
    pub jump_pressed: bool,
    pub jump_held: bool,
    pub dash_pressed: bool,
    pub roll_pressed: bool,
    /// Held: explicit wall grab. Entering/sustaining a wall-run needs `jump_held || wall_cling`.
    pub wall_cling: bool,
    /// Held: slide (ground only, needs `slide_start_speed`). Keeps the capsule; speed boost + friction decay.
    pub slide: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MoveState {
    Grounded,
    Airborne,
    WallRun {
        normal: Vector<Real>,
        tangent: Vector<Real>,
        time_left: f32,
    },
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Evasion {
    pub roll: bool,
    pub left: f32,
    pub total: f32,
    pub dir: Vector<Real>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SlideState {
    pub dir: Vector<Real>,
    pub speed: f32,
    pub time: f32,
}

/// Everything `update` reads besides the world and the input. Exact (f32) copy: restoring one and replaying the
/// same inputs reproduces the original run bit-for-bit. See `own_state::OwnState` for the compact wire form.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CharacterSnapshot {
    pub pos: Vector<Real>,
    pub vel: Vector<Real>,
    pub state: MoveState,
    pub evasion: Option<Evasion>,
    pub slide: Option<SlideState>,
    pub facing: Vector<Real>,
    pub was_grounded: bool,
    pub double_used: bool,
    pub coyote: f32,
    pub jump_buf: f32,
    pub dash_cd: f32,
    pub wall_cd: f32,
    pub slide_cd: f32,
    pub slice_cd: f32,
}

pub struct PhysicsWorld {
    pub bodies: RigidBodySet,
    pub colliders: ColliderSet,
    pub islands: IslandManager,
    pub queries: QueryPipeline,
}

// ---------- Trail point (legacy render type) ----------
/// A position sample with a birth time. The character no longer records or collides trails (the game is a shooter;
/// Tron is visual style only); the type stays only because the client's ribbon renderer and the game shell still
/// build on it, and goes with them.
#[derive(Clone, Copy, Default)]
pub struct TrailPoint {
    pub pos: Vector<Real>,
    pub born: f32,
}

// ---------- Character ----------
pub struct CharacterSystem {
    pub cfg: Cfg,
    pub body: RigidBodyHandle,
    pub kcc: KinematicCharacterController,
    pub vel: Vector<Real>,
    pub state: MoveState,
    evasion: Option<Evasion>,
    facing: Vector<Real>,
    was_grounded: bool,
    double_used: bool,
    /// Edge flag: true only for the update() tick in which a double jump fired.
    pub double_jumped: bool,
    coyote: f32,
    jump_buf: f32,
    dash_cd: f32,
    wall_cd: f32,
    slide: Option<SlideState>,
    slide_cd: f32,
    /// Katana cooldown mirror (seconds), written by whoever runs combat after each combat tick.
    slice_cd: f32,
}

/// Parented colliders only follow their body during `PhysicsPipeline::step`, which this sim never runs, so any
/// `set_position` on a body must be followed by this or queries (other players' KCC casts, wall rays, hitscan)
/// keep seeing the capsule at its spawn point. Same math as `propagate_modified_body_positions_to_colliders`,
/// without its ever-growing modified-body list.
pub fn sync_body_colliders(w: &mut PhysicsWorld, body: RigidBodyHandle) {
    let pos = *w.bodies[body].position();
    for &h in w.bodies[body].colliders() {
        if let Some(c) = w.colliders.get_mut(h) {
            if let Some(rel) = c.position_wrt_parent().copied() {
                c.set_position(pos * rel);
            }
        }
    }
}

fn flat(v: Vector<Real>) -> Vector<Real> {
    vector![v.x, 0.0, v.z]
}

fn flat_norm(v: Vector<Real>) -> Vector<Real> {
    let f = flat(v);
    let n = f.norm();
    if n > 1e-5 { f / n } else { Vector::zeros() }
}

impl CharacterSystem {
    pub fn new(body: RigidBodyHandle, cfg: Cfg) -> Self {
        let kcc = KinematicCharacterController {
            up: Vector::y_axis(), // Y-up
            offset: CharacterLength::Absolute(cfg.kcc_offset),
            slide: true,
            autostep: Some(CharacterAutostep {
                max_height: CharacterLength::Absolute(0.35),
                min_width: CharacterLength::Absolute(0.2),
                include_dynamic_bodies: false,
            }),
            max_slope_climb_angle: cfg.slope_limit, // 45 deg
            min_slope_slide_angle: cfg.slope_limit,
            snap_to_ground: None, // manual raycast snap below
            ..KinematicCharacterController::default()
        };
        Self {
            cfg,
            body,
            kcc,
            vel: Vector::zeros(),
            state: MoveState::Airborne,
            evasion: None,
            facing: -Vector::z(),
            was_grounded: false,
            double_used: false,
            double_jumped: false,
            coyote: 0.0,
            jump_buf: 0.0,
            dash_cd: 0.0,
            wall_cd: 0.0,
            slide: None,
            slide_cd: 0.0,
            slice_cd: 0.0,
        }
    }

    pub fn slice_cooldown(&self) -> f32 {
        self.slice_cd
    }

    pub fn set_slice_cooldown(&mut self, v: f32) {
        self.slice_cd = v;
    }

    pub fn sliding(&self) -> bool {
        self.slide.is_some()
    }

    pub fn invulnerable(&self) -> bool {
        self.evasion.map_or(false, |e| e.roll)
    }

    /// `Some(is_roll)` while a dash/roll is in progress (for snapshots: `MoveKind::Evading`).
    pub fn evading(&self) -> Option<bool> {
        self.evasion.map(|e| e.roll)
    }

    /// True when a dash (and therefore a katana lunge) can start this tick.
    pub fn dash_ready(&self) -> bool {
        self.evasion.is_none() && self.dash_cd <= 0.0
    }

    pub fn snapshot(&self, w: &PhysicsWorld) -> CharacterSnapshot {
        CharacterSnapshot {
            pos: *w.bodies[self.body].translation(),
            vel: self.vel,
            state: self.state,
            evasion: self.evasion,
            slide: self.slide,
            facing: self.facing,
            was_grounded: self.was_grounded,
            double_used: self.double_used,
            coyote: self.coyote,
            jump_buf: self.jump_buf,
            dash_cd: self.dash_cd,
            wall_cd: self.wall_cd,
            slide_cd: self.slide_cd,
            slice_cd: self.slice_cd,
        }
    }

    /// Put the body and all controller state back to `s`.
    pub fn restore(&mut self, w: &mut PhysicsWorld, s: &CharacterSnapshot) {
        w.bodies[self.body].set_position(Isometry::translation(s.pos.x, s.pos.y, s.pos.z), true);
        sync_body_colliders(w, self.body);
        self.vel = s.vel;
        self.state = s.state;
        self.evasion = s.evasion;
        self.slide = s.slide;
        self.facing = s.facing;
        self.was_grounded = s.was_grounded;
        self.double_used = s.double_used;
        self.coyote = s.coyote;
        self.jump_buf = s.jump_buf;
        self.dash_cd = s.dash_cd;
        self.wall_cd = s.wall_cd;
        self.slide_cd = s.slide_cd;
        self.slice_cd = s.slice_cd;
        self.double_jumped = false;
    }

    /// Restore from the exact server blob (`own.pos` is the authoritative position).
    pub fn restore_own(&mut self, w: &mut PhysicsWorld, own: &crate::own_state::OwnState) {
        let s = own.to_snapshot(vector![own.pos[0], own.pos[1], own.pos[2]], &self.cfg);
        self.restore(w, &s);
    }

    /// Wall normal (points from wall toward player) while wall-running.
    pub fn wall_run_normal(&self) -> Option<Vector<Real>> {
        match self.state {
            MoveState::WallRun { normal, .. } => Some(normal),
            _ => None,
        }
    }

    fn filter(&self) -> QueryFilter<'static> {
        // sensors (hit boxes, pickups) never block movement
        QueryFilter::default().exclude_rigid_body(self.body).exclude_sensors()
    }

    fn cast(
        &self,
        w: &PhysicsWorld,
        o: Vector<Real>,
        d: Vector<Real>,
        max: f32,
    ) -> Option<(f32, Vector<Real>)> {
        // A zero / NaN direction makes parry3d index out of bounds (panic). Never let a bad ray reach it.
        if !(o.iter().all(|v| v.is_finite()) && d.iter().all(|v| v.is_finite()) && d.norm_squared() > 1e-12) {
            return None;
        }
        let ray = Ray::new(Point::from(o), d);
        w.queries
            .cast_ray_and_get_normal(&w.bodies, &w.colliders, &ray, max, true, self.filter())
            .map(|(_, hit)| (hit.time_of_impact, hit.normal))
    }

    /// One movement tick. `_now` (sim time) is unused since the trail ribbon was dropped; the parameter stays so the
    /// many callers (server, predictor, tests) do not change in the same commit.
    pub fn update(&mut self, dt: f32, input: &PlayerInput, w: &mut PhysicsWorld, _now: f32) {
        let cfg = self.cfg;
        let up = Vector::y();
        let pos = *w.bodies[self.body].translation();

        self.double_jumped = false;
        // timers + input buffer
        self.coyote = (self.coyote - dt).max(0.0);
        self.jump_buf = if input.jump_pressed {
            cfg.jump_buffer
        } else {
            (self.jump_buf - dt).max(0.0)
        };
        self.dash_cd = (self.dash_cd - dt).max(0.0);
        self.wall_cd = (self.wall_cd - dt).max(0.0);
        self.slide_cd = (self.slide_cd - dt).max(0.0);
        let dir_in = flat_norm(input.move_dir);
        if dir_in.norm_squared() > 0.0 {
            self.facing = dir_in;
        }
        let grounded_state = self.state == MoveState::Grounded;
        let mut jumped = false;

        // ---- evasion start (dash any time, roll on ground) ----
        if self.evasion.is_none() {
            let d = if dir_in.norm_squared() > 0.0 { dir_in } else { self.facing };
            if input.roll_pressed && grounded_state {
                self.evasion = Some(Evasion {
                    roll: true,
                    left: cfg.roll_time,
                    total: cfg.roll_time,
                    dir: d,
                });
            } else if input.dash_pressed && self.dash_cd <= 0.0 {
                self.evasion = Some(Evasion {
                    roll: false,
                    left: cfg.dash_time,
                    total: cfg.dash_time,
                    dir: d,
                });
                self.dash_cd = cfg.dash_cd;
                if let MoveState::WallRun { .. } = self.state {
                    self.state = MoveState::Airborne;
                }
            }
        }

        // ---- velocity by state ----
        if let MoveState::WallRun { normal, tangent, time_left } = self.state {
            // re-probe wall toward its normal
            // A solid ray that starts inside a collider (thin Tron wall) reports a zero normal: that is "wall lost",
            // not a wall to run on (it would poison `normal` and every later probe).
            let hit = self
                .cast(w, pos, -normal, cfg.radius + cfg.wall_probe * 1.5)
                .filter(|(_, n)| flat_norm(*n).norm_squared() > 0.0);
            if self.jump_buf > 0.0 {
                // wall jump
                self.vel = normal * cfg.wall_jump_push
                    + tangent * cfg.wall_speed * 0.6
                    + up * cfg.jump_speed;
                self.jump_buf = 0.0;
                self.wall_cd = 0.3;
                self.double_used = false;
                self.state = MoveState::Airborne;
                jumped = true;
            } else if !(input.jump_held || input.wall_cling) || time_left - dt <= 0.0 || hit.is_none() {
                self.wall_cd = 0.15;
                self.state = MoveState::Airborne;
            } else if let Some((_, n)) = hit {
                let mut t = flat_norm(up.cross(&n));
                if t.dot(&tangent) < 0.0 {
                    t = -t;
                }
                self.vel = t * cfg.wall_speed - flat_norm(n) * cfg.wall_stick; // gravity OFF: vy = 0
                self.state = MoveState::WallRun {
                    normal: flat_norm(n),
                    tangent: t,
                    time_left: time_left - dt,
                };
            }
        }
        if !matches!(self.state, MoveState::WallRun { .. }) {
            // ---- slide: boost, friction decay, ends on release / low speed / leaving the ground ----
            if let Some(mut sl) = self.slide {
                sl.speed -= cfg.slide_friction * dt;
                sl.time += dt;
                if !input.slide
                    || !grounded_state
                    || sl.speed < cfg.slide_min_speed
                    || sl.time > cfg.slide_max_time
                    || self.evasion.is_some()
                {
                    self.slide = None;
                    self.slide_cd = cfg.slide_cd;
                } else {
                    self.slide = Some(sl);
                }
            } else if input.slide
                && grounded_state
                && self.slide_cd <= 0.0
                && self.evasion.is_none()
                && flat(self.vel).norm() >= cfg.slide_start_speed
            {
                self.slide = Some(SlideState { dir: flat_norm(self.vel), speed: cfg.slide_boost, time: 0.0 });
            }

            if let Some(sl) = self.slide {
                self.vel.x = sl.dir.x * sl.speed;
                self.vel.z = sl.dir.z * sl.speed;
            } else {
                // instant horizontal accel
                let target = dir_in * cfg.run_speed;
                let h = flat(self.vel);
                let delta = target - h;
                let max = if grounded_state { f32::INFINITY } else { cfg.air_accel * dt };
                let step = if delta.norm() <= max { delta } else { delta.normalize() * max };
                let nh = h + step;
                self.vel.x = nh.x;
                self.vel.z = nh.z;
            }

            // jumps: ground/coyote, then double jump at apex
            if self.jump_buf > 0.0 {
                if self.coyote > 0.0 {
                    self.vel.y = cfg.jump_speed;
                    self.coyote = 0.0;
                    self.jump_buf = 0.0;
                    jumped = true;
                } else if !grounded_state
                    && !self.double_used
                    && self.vel.y.abs() <= cfg.peak_window
                {
                    self.vel.y = cfg.double_jump_speed;
                    self.double_used = true;
                    self.double_jumped = true;
                    self.jump_buf = 0.0;
                    jumped = true;
                }
            }
            if jumped && self.slide.take().is_some() {
                self.slide_cd = cfg.slide_cd; // jump out of a slide keeps its speed into the air
            }
            // gravity
            if grounded_state && !jumped {
                self.vel.y = cfg.ground_stick;
            } else {
                self.vel.y = (self.vel.y - cfg.gravity * dt).max(-cfg.terminal);
            }

            // wall-run entry: airborne + jump held + lateral rays
            if !grounded_state
                && (input.jump_held || input.wall_cling)
                && self.wall_cd <= 0.0
                && self.evasion.is_none()
                && flat(self.vel).norm() >= cfg.wall_min_speed
            {
                let fwd = if dir_in.norm_squared() > 0.0 { dir_in } else { flat_norm(self.vel) };
                if let Some((n, t)) = self.probe_wall(w, pos, fwd) {
                    self.state = MoveState::WallRun {
                        normal: n,
                        tangent: t,
                        time_left: cfg.wall_max_time,
                    };
                    self.vel = t * cfg.wall_speed - n * cfg.wall_stick; // snap to horizontal run
                }
            }
        }

        // ---- evasion override ----
        if let Some(mut e) = self.evasion {
            let f = e.left / e.total;
            let spd = if e.roll { cfg.roll_speed } else { cfg.dash_speed * (0.4 + 0.6 * f) };
            self.vel.x = e.dir.x * spd;
            self.vel.z = e.dir.z * spd;
            if !e.roll {
                self.vel.y = 0.0; // air dash ignores gravity
            }
            e.left -= dt;
            if e.left <= 0.0 {
                self.evasion = None;
                self.vel.x = e.dir.x * cfg.run_speed;
                self.vel.z = e.dir.z * cfg.run_speed;
            } else {
                self.evasion = Some(e);
            }
        }

        // ---- KCC ----
        let shape = Capsule::new_y(cfg.half_height, cfg.radius);
        let desired = self.vel * dt;
        let mv = self.kcc.move_shape(
            dt,
            &w.bodies,
            &w.colliders,
            &w.queries,
            &shape,
            &Isometry::translation(pos.x, pos.y, pos.z),
            desired,
            self.filter(),
            |_| {},
        );
        let mut new_pos = pos + mv.translation;
        if desired.y > 0.0 && mv.translation.y < desired.y * 0.5 {
            self.vel.y = self.vel.y.min(0.0); // ceiling
        }
        let mut grounded = mv.grounded && !jumped && self.vel.y <= 0.0;

        // ---- downward-ray ground snap (<0.25 m, slope-limited) ----
        let running = flat(self.vel).norm() > 0.1;
        let snappable = !grounded
            && self.was_grounded
            && !jumped
            && self.vel.y <= 0.0
            && running
            && !matches!(self.state, MoveState::WallRun { .. })
            && self.evasion.map_or(true, |e| e.roll);
        // depenetrate: the KCC can start a tick inside its skin and tunnel into the floor at slide/dash speed;
        // lift back to contact (never below the skin) so the sink cannot accumulate
        if !jumped && self.vel.y <= 0.0 && !matches!(self.state, MoveState::WallRun { .. }) {
            if let Some(gap) = self.ground_gap(w, new_pos) {
                if gap < -0.002 {
                    new_pos.y -= gap;
                    grounded = true;
                }
            }
        }
        if snappable && !grounded {
            if let Some(gap) = self.ground_gap(w, new_pos) {
                if gap > 1e-4 {
                    new_pos.y -= gap;
                    grounded = true;
                }
            }
        }

        // ---- state transitions ----
        if grounded {
            self.state = MoveState::Grounded;
            self.coyote = cfg.coyote;
            self.double_used = false;
            self.vel.y = cfg.ground_stick;
        } else if self.state == MoveState::Grounded {
            self.state = MoveState::Airborne;
        }
        self.was_grounded = grounded;

        w.bodies[self.body].set_next_kinematic_translation(new_pos);
    }

    /// Three flat rays per side; all must hit a near-vertical, coplanar wall.
    fn probe_wall(
        &self,
        w: &PhysicsWorld,
        pos: Vector<Real>,
        fwd: Vector<Real>,
    ) -> Option<(Vector<Real>, Vector<Real>)> {
        let cfg = self.cfg;
        let up = Vector::y();
        let right = flat_norm(fwd.cross(&up));
        if right.norm_squared() == 0.0 {
            return None;
        }
        let reach = cfg.radius + cfg.wall_probe;
        let mut best: Option<(f32, Vector<Real>)> = None;
        for side in [right, -right] {
            let mut n0: Option<Vector<Real>> = None;
            let mut d0 = 0.0;
            let mut ok = true;
            for dy in [-cfg.half_height, 0.0, cfg.half_height] {
                match self.cast(w, pos + up * dy, side, reach) {
                    Some((toi, n)) if n.y.abs() < 0.15 && n.dot(&side) < -0.5 => match n0 {
                        None => {
                            n0 = Some(n);
                            d0 = toi;
                        }
                        Some(a) if a.dot(&n) > 0.95 => {}
                        _ => {
                            ok = false;
                            break;
                        }
                    },
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                if let Some(n) = n0 {
                    if best.map_or(true, |(d, _)| d0 < d) {
                        best = Some((d0, n));
                    }
                }
            }
        }
        let (_, n) = best?;
        let n = flat_norm(n);
        let mut t = flat_norm(up.cross(&n));
        let d = t.dot(&flat_norm(fwd));
        if d.abs() < 0.3 {
            return None; // head-on, no run
        }
        if d < 0.0 {
            t = -t;
        }
        Some((n, t))
    }

    /// Signed vertical gap between the capsule's lowest point and the ground below (negative = sunk in),
    /// for ground within `snap_dist` below or up to `MAX_SINK` above. `None`: nothing walkable there.
    fn ground_gap(&self, w: &PhysicsWorld, center: Vector<Real>) -> Option<f32> {
        let cfg = self.cfg;
        let origin = center - Vector::y() * cfg.half_height; // lower sphere center
        let max = cfg.radius * std::f32::consts::SQRT_2 + cfg.snap_dist;
        let (toi, n) = self.cast(w, origin, -Vector::y(), max)?;
        if n.y < libm::cosf(cfg.slope_limit) {
            return None; // too steep
        }
        let gap = toi - cfg.radius / n.y; // exact for planar slope
        (gap > -MAX_SINK && gap < cfg.snap_dist).then_some(gap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn world() -> PhysicsWorld {
        PhysicsWorld {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            queries: QueryPipeline::new(),
        }
    }

    fn boxc(w: &mut PhysicsWorld, c: [f32; 3], h: [f32; 3]) {
        w.colliders.insert(ColliderBuilder::cuboid(h[0], h[1], h[2]).translation(vector![c[0], c[1], c[2]]).build());
    }

    fn player(w: &mut PhysicsWorld, at: [f32; 3]) -> CharacterSystem {
        let cfg = Cfg::default();
        let body = w.bodies.insert(
            RigidBodyBuilder::kinematic_position_based().translation(vector![at[0], at[1], at[2]]).build(),
        );
        w.colliders.insert_with_parent(ColliderBuilder::capsule_y(cfg.half_height, cfg.radius).build(), body, &mut w.bodies);
        CharacterSystem::new(body, cfg)
    }

    /// Same per-tick sequence the server uses: refresh queries, update, commit kinematic target.
    fn tick(ch: &mut CharacterSystem, w: &mut PhysicsWorld, input: &PlayerInput, now: &mut f32) {
        w.queries.update(&w.colliders);
        ch.update(DT, input, w, *now);
        let next = *w.bodies[ch.body].next_position();
        w.bodies[ch.body].set_position(next, true);
        *now += DT;
    }

    /// Airborne at (0,5,0) running -Z at 9 m/s beside a wall whose face is at x = 0.5.
    fn wall_scene() -> (PhysicsWorld, CharacterSystem) {
        let mut w = world();
        boxc(&mut w, [1.5, 5.0, -20.0], [1.0, 10.0, 40.0]); // wall
        boxc(&mut w, [0.0, -50.0, 0.0], [100.0, 0.5, 100.0]); // far floor, never reached
        let mut ch = player(&mut w, [0.0, 5.0, 0.0]);
        ch.vel = vector![0.0, 0.0, -9.0];
        (w, ch)
    }

    fn y(ch: &CharacterSystem, w: &PhysicsWorld) -> f32 {
        w.bodies[ch.body].translation().y
    }

    #[test]
    fn cling_alone_enters_and_sustains_wall_run() {
        let (mut w, mut ch) = wall_scene();
        let input = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], wall_cling: true, ..Default::default() };
        let mut now = 0.0;
        for i in 0..60 {
            tick(&mut ch, &mut w, &input, &mut now);
            assert!(matches!(ch.state, MoveState::WallRun { .. }), "tick {i}: {:?}", ch.state);
        }
        assert!((y(&ch, &w) - 5.0).abs() < 0.01, "gravity must be off, y = {}", y(&ch, &w));
        assert!(w.bodies[ch.body].translation().z < -8.0, "ran along the wall");
        // releasing cling (and not holding jump) drops out of the run on the next tick
        tick(&mut ch, &mut w, &PlayerInput { move_dir: input.move_dir, ..Default::default() }, &mut now);
        assert_eq!(ch.state, MoveState::Airborne);
    }

    #[test]
    fn no_jump_no_cling_means_no_wall_run() {
        let (mut w, mut ch) = wall_scene();
        let input = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], ..Default::default() };
        let mut now = 0.0;
        for _ in 0..30 {
            tick(&mut ch, &mut w, &input, &mut now);
            assert_eq!(ch.state, MoveState::Airborne);
        }
        assert!(y(&ch, &w) < 4.0, "fell under gravity, y = {}", y(&ch, &w));
    }

    #[test]
    fn jump_held_still_wall_runs() {
        let (mut w, mut ch) = wall_scene();
        let input = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], jump_held: true, ..Default::default() };
        let mut now = 0.0;
        for _ in 0..10 {
            tick(&mut ch, &mut w, &input, &mut now);
        }
        assert!(matches!(ch.state, MoveState::WallRun { .. }));
    }

    // ---------- movement feel: numbers pinned so tuning changes are deliberate ----------

    fn floor(w: &mut PhysicsWorld) {
        boxc(w, [0.0, -0.5, 0.0], [200.0, 0.5, 200.0]);
    }

    fn player_cfg(w: &mut PhysicsWorld, at: [f32; 3], cfg: Cfg) -> CharacterSystem {
        let body = w.bodies.insert(
            RigidBodyBuilder::kinematic_position_based().translation(vector![at[0], at[1], at[2]]).build(),
        );
        w.colliders.insert_with_parent(ColliderBuilder::capsule_y(cfg.half_height, cfg.radius).build(), body, &mut w.bodies);
        CharacterSystem::new(body, cfg)
    }

    fn settle(ch: &mut CharacterSystem, w: &mut PhysicsWorld, now: &mut f32) {
        for _ in 0..10 {
            tick(ch, w, &PlayerInput::default(), now);
        }
        assert_eq!(ch.state, MoveState::Grounded);
    }

    #[test]
    fn acceleration_is_instant_on_the_ground() {
        let mut w = world();
        floor(&mut w);
        let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
        let mut now = 0.0;
        settle(&mut ch, &mut w, &mut now);
        tick(&mut ch, &mut w, &PlayerInput { move_dir: vector![0.0, 0.0, -1.0], ..Default::default() }, &mut now);
        assert!((ch.vel.z + ch.cfg.run_speed).abs() < 1e-4, "full speed on the first tick, vel.z = {}", ch.vel.z);
        tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        assert!(flat(ch.vel).norm() < 1e-4, "and a dead stop on release");
    }

    /// Jump, optionally double-jump at the apex; returns peak feet height above the start.
    fn jump_peak(double: bool) -> (f32, f32) {
        let mut w = world();
        floor(&mut w);
        let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
        let mut now = 0.0;
        settle(&mut ch, &mut w, &mut now);
        let y0 = w.bodies[ch.body].translation().y;
        let (mut peak, mut t_apex, mut prev_vy) = (0.0f32, 0.0f32, 0.0f32);
        let mut pressed = false;
        for i in 0..240 {
            let press = (i == 0) || (double && !pressed && ch.vel.y.abs() < 1.0 && ch.vel.y < prev_vy);
            pressed |= press && i > 0;
            prev_vy = ch.vel.y;
            let input = PlayerInput { jump_pressed: press, jump_held: press, ..Default::default() };
            tick(&mut ch, &mut w, &input, &mut now);
            let h = w.bodies[ch.body].translation().y - y0;
            if h > peak {
                peak = h;
                t_apex = i as f32 * DT;
            }
        }
        (peak, t_apex)
    }

    #[test]
    fn jump_height_and_double_jump_gain() {
        let (h1, t1) = jump_peak(false);
        assert!((1.4..1.75).contains(&h1), "single jump peak {h1} m");
        assert!((0.28..0.42).contains(&t1), "time to apex {t1} s");
        let (h2, _) = jump_peak(true);
        assert!(h2 > h1 + 1.0, "double jump at the apex must add real height: {h1} -> {h2}");
    }

    #[test]
    fn coyote_time_allows_a_late_first_jump_but_not_a_late_one() {
        for (frames_after_edge, expect_ground_jump) in [(4, true), (12, false)] {
            let mut w = world();
            boxc(&mut w, [0.0, -0.5, 0.0], [2.0, 0.5, 200.0]); // narrow platform, edge at x = 2
            let mut ch = player(&mut w, [1.0, 0.81, 0.0]);
            let mut now = 0.0;
            settle(&mut ch, &mut w, &mut now);
            let walk = PlayerInput { move_dir: vector![1.0, 0.0, 0.0], ..Default::default() };
            while ch.state == MoveState::Grounded || w.bodies[ch.body].translation().x < 2.0 {
                tick(&mut ch, &mut w, &walk, &mut now);
            }
            for _ in 0..frames_after_edge {
                tick(&mut ch, &mut w, &walk, &mut now);
            }
            let vy_before = ch.vel.y;
            tick(&mut ch, &mut w, &PlayerInput { jump_pressed: true, jump_held: true, ..walk }, &mut now);
            let ground_jump = !ch.double_used && ch.vel.y > 0.0;
            assert_eq!(ground_jump, expect_ground_jump, "{frames_after_edge} frames after the edge (vy {vy_before})");
        }
    }

    #[test]
    fn jump_buffer_fires_on_landing() {
        let mut w = world();
        floor(&mut w);
        let mut ch = player(&mut w, [0.0, 1.0, 0.0]);
        let mut now = 0.0;
        // fall until the feet are ~4 ticks from the floor, press jump early, expect a jump at touchdown
        let mut pressed = false;
        let mut jumped = false;
        for _ in 0..90 {
            let near = w.bodies[ch.body].translation().y < 0.81 + 0.15 && ch.vel.y < 0.0;
            let input = PlayerInput { jump_pressed: near && !pressed, jump_held: near && !pressed, ..Default::default() };
            pressed |= near;
            tick(&mut ch, &mut w, &input, &mut now);
            jumped |= ch.vel.y > 5.0;
        }
        assert!(pressed && jumped, "buffered press must launch on landing (pressed {pressed})");
    }

    /// Flat run-in at x < 0, then a planar slope dropping away toward +X at `deg`.
    fn slope_scene(w: &mut PhysicsWorld, deg: f32) {
        boxc(w, [-20.0, -0.5, 0.0], [20.0, 0.5, 20.0]);
        let a = deg.to_radians();
        let (s, c) = a.sin_cos();
        let center = vector![-0.5 * s + 20.0 * c, -0.5 * c - 20.0 * s, 0.0];
        w.colliders.insert(
            ColliderBuilder::cuboid(20.0, 0.5, 20.0)
                .position(Isometry::new(center, vector![0.0, 0.0, -a]))
                .build(),
        );
    }

    fn airborne_ticks_running_down(snap_dist: f32) -> u32 {
        let mut w = world();
        slope_scene(&mut w, 30.0);
        let cfg = Cfg { snap_dist, ..Cfg::default() };
        let mut ch = player_cfg(&mut w, [-8.0, 0.81, 0.0], cfg);
        let mut now = 0.0;
        settle(&mut ch, &mut w, &mut now);
        let run = PlayerInput { move_dir: vector![1.0, 0.0, 0.0], ..Default::default() };
        let mut air = 0;
        for i in 0..150 {
            tick(&mut ch, &mut w, &run, &mut now);
            if i > 60 && ch.state != MoveState::Grounded {
                air += 1; // well past the lip: every tick from here is on the slope
            }
        }
        assert!(w.bodies[ch.body].translation().x > 10.0 && w.bodies[ch.body].translation().y < -4.0, "actually ran down it");
        air
    }

    #[test]
    fn ground_snap_removes_slope_stutter() {
        assert_eq!(airborne_ticks_running_down(0.25), 0, "snapped: grounded on every slope tick");
        assert!(airborne_ticks_running_down(0.0) > 10, "control: without the snap the same run chatters");
    }

    #[test]
    fn autostep_climbs_030_but_not_050() {
        for (h, climbs) in [(0.30, true), (0.50, false)] {
            let mut w = world();
            floor(&mut w);
            boxc(&mut w, [3.0, h / 2.0, 0.0], [1.0, h / 2.0, 5.0]); // step, top at h, face at x = 2
            let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
            let mut now = 0.0;
            settle(&mut ch, &mut w, &mut now);
            let run = PlayerInput { move_dir: vector![1.0, 0.0, 0.0], ..Default::default() };
            for _ in 0..60 {
                tick(&mut ch, &mut w, &run, &mut now);
            }
            let x = w.bodies[ch.body].translation().x;
            assert_eq!(x > 3.0, climbs, "step {h} m: x = {x}");
        }
    }

    // ---------- slide ----------

    fn slide_scene() -> (PhysicsWorld, CharacterSystem, f32) {
        let mut w = world();
        floor(&mut w);
        let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
        let mut now = 0.0;
        settle(&mut ch, &mut w, &mut now);
        // get up to running speed
        let run = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], ..Default::default() };
        tick(&mut ch, &mut w, &run, &mut now);
        (w, ch, now)
    }

    #[test]
    fn slide_boosts_then_decays_and_ends_on_low_speed() {
        let (mut w, mut ch, mut now) = slide_scene();
        let input = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], slide: true, ..Default::default() };
        tick(&mut ch, &mut w, &input, &mut now);
        assert!(ch.slide.is_some());
        assert!(flat(ch.vel).norm() > ch.cfg.run_speed + 3.0, "boost over run speed: {}", flat(ch.vel).norm());
        let mut ticks = 1;
        while ch.slide.is_some() && ticks < 120 {
            tick(&mut ch, &mut w, &input, &mut now);
            ticks += 1;
        }
        let expect = ((ch.cfg.slide_boost - ch.cfg.slide_min_speed) / ch.cfg.slide_friction / DT) as i32;
        assert!((ticks as i32 - expect).abs() <= 3, "slid {ticks} ticks, expected ~{expect}");
        assert!(ch.slide_cd > 0.0, "cooldown armed");
    }

    #[test]
    fn slide_ends_on_release_and_respects_cooldown() {
        let (mut w, mut ch, mut now) = slide_scene();
        let hold = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], slide: true, ..Default::default() };
        for _ in 0..5 {
            tick(&mut ch, &mut w, &hold, &mut now);
        }
        assert!(ch.slide.is_some());
        let release = PlayerInput { move_dir: hold.move_dir, ..Default::default() };
        tick(&mut ch, &mut w, &release, &mut now);
        assert!(ch.slide.is_none());
        tick(&mut ch, &mut w, &hold, &mut now);
        assert!(ch.slide.is_none(), "cooldown blocks an immediate restart");
        for _ in 0..40 {
            tick(&mut ch, &mut w, &release, &mut now); // cd 0.5 s = 30 ticks
        }
        tick(&mut ch, &mut w, &hold, &mut now);
        assert!(ch.slide.is_some(), "restarts after the cooldown");
    }

    #[test]
    fn slide_needs_speed_and_ground_and_jump_cancels_it() {
        let mut w = world();
        floor(&mut w);
        let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
        let mut now = 0.0;
        settle(&mut ch, &mut w, &mut now);
        tick(&mut ch, &mut w, &PlayerInput { slide: true, ..Default::default() }, &mut now);
        assert!(ch.slide.is_none(), "standing still cannot slide");

        let (mut w, mut ch, mut now) = slide_scene();
        let hold = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], slide: true, ..Default::default() };
        tick(&mut ch, &mut w, &hold, &mut now);
        tick(&mut ch, &mut w, &PlayerInput { jump_pressed: true, jump_held: true, ..hold }, &mut now);
        assert!(ch.slide.is_none(), "jump leaves the slide");
        assert!(ch.vel.y > 0.0 && flat(ch.vel).norm() > ch.cfg.run_speed, "and keeps the slide speed into the air");
    }

    // ---------- snapshot / restore ----------

    #[test]
    fn restore_then_replay_is_bit_identical() {
        let (mut w, mut ch, mut now) = slide_scene();
        let inputs: Vec<PlayerInput> = (0..120)
            .map(|i| PlayerInput {
                move_dir: vector![(i as f32 * 0.1).sin(), 0.0, -1.0],
                jump_pressed: i % 40 == 5 || i % 40 == 25,
                jump_held: i % 40 >= 5 && i % 40 < 30,
                dash_pressed: i == 60,
                slide: (70..90).contains(&i),
                ..Default::default()
            })
            .collect();
        for i in &inputs[..40] {
            tick(&mut ch, &mut w, i, &mut now);
        }
        let snap = ch.snapshot(&w);
        let now0 = now;
        let mut reference = vec![];
        for i in &inputs[40..] {
            tick(&mut ch, &mut w, i, &mut now);
            reference.push(*w.bodies[ch.body].translation());
        }
        ch.restore(&mut w, &snap);
        let mut now = now0;
        for (k, i) in inputs[40..].iter().enumerate() {
            tick(&mut ch, &mut w, i, &mut now);
            assert_eq!(w.bodies[ch.body].translation().map(f32::to_bits), reference[k].map(f32::to_bits), "tick {k}");
        }
    }
}
