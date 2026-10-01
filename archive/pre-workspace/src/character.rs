use rapier3d::control::{CharacterAutostep, CharacterLength, KinematicCharacterController};
use rapier3d::prelude::*;

pub const TRAIL_CAP: usize = 128;
pub const TRAIL_TAG: u128 = 0x7241_11; // user_data on trail colliders
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

#[derive(Clone, Copy)]
struct Evasion {
    roll: bool,
    left: f32,
    total: f32,
    dir: Vector<Real>,
}

pub struct PhysicsWorld {
    pub bodies: RigidBodySet,
    pub colliders: ColliderSet,
    pub islands: IslandManager,
    pub queries: QueryPipeline,
}

// ---------- Trail ring buffer (render reads via as_slices) ----------
#[derive(Clone, Copy, Default)]
pub struct TrailPoint {
    pub pos: Vector<Real>,
    pub born: f32,
}

pub struct TrailBuffer {
    pts: [TrailPoint; TRAIL_CAP],
    head: usize,
    len: usize,
}

impl TrailBuffer {
    fn new() -> Self {
        Self { pts: [TrailPoint::default(); TRAIL_CAP], head: 0, len: 0 }
    }

    fn oldest(&self) -> usize {
        (self.head + TRAIL_CAP - self.len) % TRAIL_CAP
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn last(&self) -> Option<TrailPoint> {
        (self.len > 0).then(|| self.pts[(self.head + TRAIL_CAP - 1) % TRAIL_CAP])
    }

    /// Returns slot written.
    fn push(&mut self, p: TrailPoint) -> usize {
        let slot = self.head;
        self.pts[slot] = p;
        self.head = (self.head + 1) % TRAIL_CAP;
        self.len = (self.len + 1).min(TRAIL_CAP);
        slot
    }

    fn pop_expired(&mut self, now: f32, life: f32) {
        while self.len > 0 && now - self.pts[self.oldest()].born > life {
            self.len -= 1;
        }
    }

    /// Oldest to newest, two slices (ring wrap). Draw quad strip across both.
    pub fn as_slices(&self) -> (&[TrailPoint], &[TrailPoint]) {
        let s = self.oldest();
        if s + self.len <= TRAIL_CAP {
            (&self.pts[s..s + self.len], &[])
        } else {
            (&self.pts[s..], &self.pts[..s + self.len - TRAIL_CAP])
        }
    }
}

// ---------- Character ----------
pub struct CharacterSystem {
    pub cfg: Cfg,
    pub body: RigidBodyHandle,
    pub kcc: KinematicCharacterController,
    pub vel: Vector<Real>,
    pub state: MoveState,
    pub trail: TrailBuffer,
    seg_colliders: [Option<(ColliderHandle, f32)>; TRAIL_CAP], // by newer-point slot
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
            offset: CharacterLength::Absolute(0.01),
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
            trail: TrailBuffer::new(),
            seg_colliders: [None; TRAIL_CAP],
            evasion: None,
            facing: -Vector::z(),
            was_grounded: false,
            double_used: false,
            double_jumped: false,
            coyote: 0.0,
            jump_buf: 0.0,
            dash_cd: 0.0,
            wall_cd: 0.0,
        }
    }

    pub fn invulnerable(&self) -> bool {
        self.evasion.map_or(false, |e| e.roll)
    }

    /// True when a dash (and therefore a katana lunge) can start this tick.
    pub fn dash_ready(&self) -> bool {
        self.evasion.is_none() && self.dash_cd <= 0.0
    }

    /// Wall normal (points from wall toward player) while wall-running.
    pub fn wall_run_normal(&self) -> Option<Vector<Real>> {
        match self.state {
            MoveState::WallRun { normal, .. } => Some(normal),
            _ => None,
        }
    }

    fn filter(&self) -> QueryFilter<'static> {
        // trails are sensors, so excluding sensors ignores them
        QueryFilter::default().exclude_rigid_body(self.body).exclude_sensors()
    }

    fn cast(
        &self,
        w: &PhysicsWorld,
        o: Vector<Real>,
        d: Vector<Real>,
        max: f32,
    ) -> Option<(f32, Vector<Real>)> {
        let ray = Ray::new(Point::from(o), d);
        w.queries
            .cast_ray_and_get_normal(&w.bodies, &w.colliders, &ray, max, true, self.filter())
            .map(|(_, hit)| (hit.time_of_impact, hit.normal))
    }

    pub fn update(&mut self, dt: f32, input: &PlayerInput, w: &mut PhysicsWorld, now: f32) {
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
            let hit = self.cast(w, pos, -normal, cfg.radius + cfg.wall_probe * 1.5);
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
            } else if !input.jump_held || time_left - dt <= 0.0 || hit.is_none() {
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
            // instant horizontal accel
            let target = dir_in * cfg.run_speed;
            let h = flat(self.vel);
            let delta = target - h;
            let max = if grounded_state { f32::INFINITY } else { cfg.air_accel * dt };
            let step = if delta.norm() <= max { delta } else { delta.normalize() * max };
            let nh = h + step;
            self.vel.x = nh.x;
            self.vel.z = nh.z;

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
            // gravity
            if grounded_state && !jumped {
                self.vel.y = cfg.ground_stick;
            } else {
                self.vel.y = (self.vel.y - cfg.gravity * dt).max(-cfg.terminal);
            }

            // wall-run entry: airborne + jump held + lateral rays
            if !grounded_state
                && input.jump_held
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
        if snappable {
            if let Some(gap) = self.ground_gap(w, new_pos) {
                new_pos.y -= gap;
                grounded = true;
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
        let feet = new_pos - up * (cfg.half_height + cfg.radius);
        self.update_trail(w, feet, now);
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

    /// Vertical gap between capsule and ground below, if snappable.
    fn ground_gap(&self, w: &PhysicsWorld, center: Vector<Real>) -> Option<f32> {
        let cfg = self.cfg;
        let origin = center - Vector::y() * cfg.half_height; // lower sphere center
        let max = cfg.radius * std::f32::consts::SQRT_2 + cfg.snap_dist;
        let (toi, n) = self.cast(w, origin, -Vector::y(), max)?;
        if n.y < cfg.slope_limit.cos() {
            return None; // too steep
        }
        let gap = toi - cfg.radius / n.y; // exact for planar slope
        (gap > 1e-4 && gap < cfg.snap_dist).then_some(gap)
    }

    fn update_trail(&mut self, w: &mut PhysicsWorld, feet: Vector<Real>, now: f32) {
        let cfg = self.cfg;
        for s in self.seg_colliders.iter_mut() {
            if let Some((h, born)) = *s {
                if now - born > cfg.trail_life {
                    w.colliders.remove(h, &mut w.islands, &mut w.bodies, false);
                    *s = None;
                }
            }
        }
        self.trail.pop_expired(now, cfg.trail_life);

        let prev = self.trail.last();
        if prev.map_or(true, |p| flat(feet - p.pos).norm() >= cfg.trail_spacing) {
            let slot = self.trail.push(TrailPoint { pos: feet, born: now });
            // slot reused: old segment dies with evicted point
            if let Some((h, _)) = self.seg_colliders[slot].take() {
                w.colliders.remove(h, &mut w.islands, &mut w.bodies, false);
            }
            if let Some(p) = prev {
                let d = feet - p.pos;
                let len = flat(d).norm();
                if len > 1e-3 {
                    let mid = (feet + p.pos) * 0.5 + Vector::y() * cfg.trail_height * 0.5;
                    let yaw = (-d.z).atan2(d.x);
                    let col = ColliderBuilder::cuboid(
                        len * 0.5 + 0.02,
                        cfg.trail_height * 0.5,
                        cfg.trail_thick * 0.5,
                    )
                    .position(Isometry::new(mid, vector![0.0, yaw, 0.0]))
                    .sensor(true)
                    .collision_groups(InteractionGroups::new(GROUP_TRAIL, GROUP_ENEMY))
                    .active_events(ActiveEvents::COLLISION_EVENTS)
                    // default types skip kinematic-vs-fixed: kinematic enemies would never trigger
                    .active_collision_types(
                        ActiveCollisionTypes::default() | ActiveCollisionTypes::KINEMATIC_FIXED,
                    )
                    .user_data(TRAIL_TAG)
                    .build();
                    self.seg_colliders[slot] = Some((w.colliders.insert(col), now));
                }
            }
        }
    }
}

/// Enemy system: on CollisionEvent::Started(a, b, _), if this is true for either, kill the other.
pub fn is_trail(colliders: &ColliderSet, h: ColliderHandle) -> bool {
    colliders.get(h).map_or(false, |c| c.user_data == TRAIL_TAG)
}
