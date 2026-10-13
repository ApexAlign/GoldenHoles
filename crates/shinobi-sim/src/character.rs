use rapier3d::control::{CharacterAutostep, CharacterLength, KinematicCharacterController};
use rapier3d::prelude::*;

const MAX_SINK: f32 = 0.15; // deepest floor penetration the depenetration step will undo
pub const TRAIL_CAP: usize = 128;
pub const GROUP_ENEMY: Group = Group::GROUP_2;
pub const GROUP_TRAIL: Group = Group::GROUP_3;

/// How far a wedged capsule is stepped out of a wall face before the controller retries (see `CharacterSystem::update`).
const WEDGE_NUDGE: f32 = 0.03;

#[derive(Clone, Copy)]
pub struct Cfg {
    pub half_height: f32,
    pub radius: f32,
    pub run_speed: f32,
    /// Ground acceleration toward the stick (m/s^2): ~0.10 s to full run speed.
    pub ground_accel: f32,
    /// Ground braking when the stick is released or reversed (m/s^2): a ~0.25 m stop from a full run, no skating.
    pub ground_decel: f32,
    pub air_accel: f32,
    pub gravity: f32,
    pub terminal: f32,
    pub ground_stick: f32,
    pub jump_speed: f32,
    pub double_jump_speed: f32,
    pub peak_window: f32,
    /// Gravity multiplier while rising with jump held (hold-to-extend: a held jump goes higher and floats).
    pub rise_hold_mult: f32,
    /// Gravity multiplier while rising with jump released (a tap is a short hop).
    pub rise_cut_mult: f32,
    /// Gravity multiplier while falling with jump held (a gentle glide-down; release to drop at full gravity).
    pub fall_glide_mult: f32,
    /// Horizontal speed (m/s) a ground jump adds along the current heading (forward carry: jumps cover ground).
    pub jump_carry: f32,
    /// A double jump redirects the horizontal momentum to the stick and adds this much speed.
    pub double_jump_carry: f32,
    /// Jump carries never push horizontal speed past this (a slide-jump may already be faster and keeps its speed).
    pub air_speed_cap: f32,
    /// Airborne momentum decay (m/s^2) toward run speed (when faster) or toward rest (no stick): jumps keep their carry.
    pub air_drag: f32,
    /// Fraction of the wall-run speed kept along the wall when jumping off it.
    pub wall_jump_keep: f32,
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
    /// Highest lip / stair riser (m) the character walks over without jumping (KCC autostep).
    pub step_height: f32,
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
    /// How far the eye (camera AND shot origin) drops while sliding. The client camera reads this same value, so the
    /// crosshair ray and the shuriken's flight line stay together during a slide.
    pub slide_eye_drop: f32,
    /// Wrist grapple (see docs/grapple.md).
    pub grapple_range: f32,
    pub grapple_min_len: f32,
    pub grapple_speed: f32,
    pub grapple_accel: f32,
    pub grapple_cd: f32,
    pub grapple_arrive: f32,
    pub grapple_max_time: f32,
    pub grapple_jump_boost: f32,
    /// Upward kick when the rope hooks while grounded, so the reel lifts the player off the floor instead of dragging.
    pub grapple_lift: f32,
    /// Fraction of `air_accel` the stick gets while on the rope (adds speed along the stick, never brakes).
    pub grapple_steer: f32,
    /// Cooldown after a miss (keeps E-spam from firing a ray and an event every tick).
    pub grapple_miss_cd: f32,
    /// Speed ceiling while on the rope (and so on release): below a dash (30 m/s), so the rope never outruns the
    /// fastest legal mover the anti-cheat and the interpolation are sized for, and swings stay readable.
    pub grapple_max_speed: f32,
    /// Reel pull ramps from 35 % to 100 % over this many seconds after hooking.
    pub grapple_ease_in: f32,
    /// Closing speed allowed right at the release radius; it widens linearly to `grapple_speed` over `grapple_soft_dist`.
    pub grapple_arrive_speed: f32,
    pub grapple_soft_dist: f32,
    /// Braking (m/s^2) applied when closing faster than the soft-arrival cap.
    pub grapple_brake: f32,
    /// Reel speed grows with the distance to the anchor (`grapple_speed_per_m` x metres) up to this, so a hook from
    /// across the map is a ~2 s trip instead of a 20 s crawl. Never below `grapple_speed`.
    pub grapple_far_speed: f32,
    pub grapple_speed_per_m: f32,
    /// A katana press while hooked, or within this many seconds after the rope let go, is a hook-strike.
    pub hookstrike_window: f32,
    /// Hook-strike launch: horizontal speed floor along the aim (momentum above it is kept) and vertical speed floor.
    pub hookstrike_speed: f32,
    pub hookstrike_lift: f32,
    /// Wall slash (katana during a wall-run): launch speed along the aim, away from the wall, and its upward kick.
    pub wallslash_speed: f32,
    pub wallslash_lift: f32,
    /// Wall lost mid-run (a corner, a gap): coast along the old tangent while burning the remaining run time this many
    /// times faster than normal. 0 = drop off at once.
    pub wall_coast_burn: f32,
    /// Horizontal speed over `grapple_max_speed` bleeds off at this rate per second (fraction of the excess): a rope
    /// release at 60 m/s is a fast flight that settles in a second or two, not a permanent rocket.
    pub overspeed_bleed: f32,
    /// A reel that drags a grounded player into a ledge or wall face it cannot step (the capsule barely moves while the
    /// rope pulls) pops them up at this vertical speed so the rope carries them over instead of grinding along the face.
    pub grapple_hop: f32,
    /// Vertical wall-runs (up / down the face): climb/descent speed, how fast the run velocity turns toward the stick
    /// (m/s^2), the closing speed needed to catch a face head-on, the climb speed that entry starts with, and the push
    /// over the roof edge when a climb runs out of wall.
    pub wall_climb_speed: f32,
    pub wall_steer: f32,
    pub wall_front_min_speed: f32,
    pub wall_up_enter: f32,
    pub wall_topout_push: f32,
    /// Wall clash (two players wall-running the same face and meeting; see `tick.rs`): how close they must be, how long
    /// the strike window lasts (s), the winner's damage multiplier on top of the wall-slash bonus, the knock-off speed
    /// the loser (or both, on a draw) gets, and the cooldown before the same pair can clash again.
    pub clash_range: f32,
    pub clash_window: f32,
    pub clash_mult: f32,
    pub clash_knock: f32,
    pub clash_cd: f32,
    /// Soft arrival: when the rope ends by arrival (inside `grapple_arrive` of the anchor) or by the hook key, only this
    /// fraction of the speed is kept (1.0 = the old launch), unless the player holds jump or presses dash (then the
    /// momentum is theirs: a deliberate launch). `cancel_keep` is the same for the hook key (a brake, not a release).
    pub arrive_speed_keep: f32,
    pub cancel_keep: f32,
    /// Ledge assist: after a reel ends within `mantle_window` s (or while on the rope), a ledge /
    /// deck / ramp edge up to `mantle_height` m above the feet and within reach ahead lifts the player onto it
    /// (0 height = off), with `mantle_carry` m/s of forward carry.
    pub mantle_height: f32,
    pub mantle_window: f32,
    pub mantle_carry: f32,
    /// Hook yank (hook key again on a player rope): the target is pulled toward you at this speed (+ a hop), then the
    /// rope is cut; cooldown before the next yank.
    pub yank_speed: f32,
    pub yank_hop: f32,
    pub yank_cd: f32,
    /// Seconds the yanked player is staggered (cannot act, wide open to a cut).
    pub yank_stagger: f32,
    /// Pull speed grows with the gap (m/s per metre) so a close target is not flung past you; floor 30 % of `yank_speed`.
    pub yank_gain: f32,
    /// Anchor assist: a hook press whose aim ray finds nothing snaps to the best hookable point within this many degrees
    /// of the aim (0 = off). Rim / edge points score `grapple_assist_edge` degrees better, tops half of that.
    pub grapple_assist_deg: f32,
    pub grapple_assist_edge: f32,
    /// Enemy hook: a hook that lands on a player pulls THEM toward you (m/s cap; 0 = off, the old behaviour where the
    /// rope reels you to them). The rope lets go within `grapple_pull_end` m; the victim is staggered `grapple_stagger` s
    /// (refreshed every pulling tick, so it outlasts the pull by that long). A raised shield in front stops the pull,
    /// costs `grapple_shield_drain` of the guard, and the rope drops. Cooldown after a player rope: `grapple_player_cd`.
    pub grapple_pull_max: f32,
    pub grapple_pull_end: f32,
    pub grapple_stagger: f32,
    pub grapple_player_cd: f32,
    pub grapple_shield_drain: f32,
    /// Slingshot: a launching release (jump, dash, anchor broke) adds this fraction of the current speed along the
    /// velocity (capped at `grapple_sling_cap` m/s; only above 6 m/s).
    pub grapple_sling: f32,
    pub grapple_sling_cap: f32,
}

impl Cfg {
    /// Apex height (m) of a ground jump: `hold` = jump held through the rise, else a tap (short hop).
    pub fn jump_apex(&self, hold: bool) -> f32 {
        let g = self.gravity * if hold { self.rise_hold_mult } else { self.rise_cut_mult };
        self.jump_speed * self.jump_speed / (2.0 * g)
    }

    /// Extra height (m) a double jump adds at the apex, jump held / tapped.
    pub fn double_jump_gain(&self, hold: bool) -> f32 {
        let g = self.gravity * if hold { self.rise_hold_mult } else { self.rise_cut_mult };
        self.double_jump_speed * self.double_jump_speed / (2.0 * g)
    }

    /// Reel speed toward an anchor `d` m away: `grapple_speed_per_m` x d, clamped to `grapple_speed..=grapple_far_speed`.
    pub fn grapple_reel_speed(&self, d: f32) -> f32 {
        (d * self.grapple_speed_per_m).clamp(self.grapple_speed, self.grapple_far_speed.max(self.grapple_speed))
    }

    /// Highest ledge (m above the feet) a held single + double jump reaches, with `margin` m to spare (bot planning).
    pub fn max_hop_rise(&self, margin: f32) -> f32 {
        self.jump_apex(true) + self.double_jump_gain(true) - margin
    }
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            half_height: 0.5,
            radius: 0.3,
            run_speed: 9.5,
            ground_accel: 72.0,
            ground_decel: 100.0,
            air_accel: 28.0,
            gravity: 24.0,
            terminal: 36.0,
            ground_stick: -2.0,
            jump_speed: 11.0,
            double_jump_speed: 8.4,
            peak_window: 3.0,
            rise_hold_mult: 0.75,
            rise_cut_mult: 1.25,
            fall_glide_mult: 0.6,
            jump_carry: 3.5,
            double_jump_carry: 2.5,
            air_speed_cap: 11.5,
            air_drag: 2.0,
            wall_jump_keep: 1.15,
            coyote: 0.14,
            jump_buffer: 0.15,
            dash_speed: 19.0,
            dash_time: 0.3,
            dash_cd: 1.2,
            roll_speed: 11.5,
            roll_time: 0.35,
            wall_probe: 0.35,
            wall_min_speed: 4.2,
            wall_speed: 11.0,
            wall_stick: 1.5,
            wall_max_time: 4.0,
            wall_jump_push: 10.0,
            snap_dist: 0.25,
            slope_limit: 45f32.to_radians(),
            step_height: 0.35, // a knob (KCC autostep + bot nav). Tried 0.38-0.55: each value broke a different parity/grapple/e2e test (chaotic, not monotonic): grapple_ffa edge clip, e2e_prediction drift, guard_duel foot-riding, full_loop_e2e builds. Maps are designed around 0.35.
            trail_spacing: 0.4,
            trail_life: 4.0,
            trail_height: 1.6,
            trail_thick: 0.15,
            slide_start_speed: 5.8,
            slide_boost: 12.5,
            slide_friction: 12.0,
            slide_min_speed: 3.8,
            slide_max_time: 1.2,
            slide_cd: 0.5,
            kcc_offset: 0.01,
            eye_above_center: 0.6,
            slide_eye_drop: 0.35,
            grapple_range: 1000.0,
            grapple_min_len: 2.5,
            grapple_speed: 18.0,
            grapple_accel: 80.0,
            grapple_cd: 0.4,
            grapple_arrive: 1.3,
            grapple_max_time: 10.0,
            grapple_jump_boost: 5.0,
            grapple_lift: 3.0,
            grapple_steer: 1.0,
            grapple_miss_cd: 0.15,
            grapple_max_speed: 20.0,
            grapple_ease_in: 0.25,
            grapple_arrive_speed: 6.0,
            grapple_soft_dist: 6.0,
            grapple_brake: 70.0,
            grapple_far_speed: 140.0,
            grapple_speed_per_m: 1.75,
            hookstrike_window: 0.35,
            hookstrike_speed: 24.0,
            hookstrike_lift: 3.0,
            wallslash_speed: 13.0,
            wallslash_lift: 4.5,
            wall_coast_burn: 5.0,
            overspeed_bleed: 0.8,
            grapple_hop: 4.5,
            wall_climb_speed: 9.5,
            wall_steer: 45.0,
            wall_front_min_speed: 2.5,
            wall_up_enter: 6.0,
            wall_topout_push: 3.5,
            clash_range: 2.2,
            clash_window: 0.6,
            clash_mult: 1.5,
            clash_knock: 11.0,
            clash_cd: 2.5,
            arrive_speed_keep: 0.25,
            cancel_keep: 0.2,
            mantle_height: 6.5,
            mantle_window: 0.45,
            mantle_carry: 3.2,
            yank_speed: 46.0,
            yank_hop: 6.0,
            yank_cd: 1.6,
            yank_stagger: 0.7,
            yank_gain: 2.0,
            grapple_assist_deg: 6.0,
            grapple_assist_edge: 2.5,
            grapple_pull_max: 45.0,
            grapple_pull_end: 2.0,
            grapple_stagger: 0.4,
            grapple_player_cd: 3.0,
            grapple_shield_drain: 0.4,
            grapple_sling: 0.25,
            grapple_sling_cap: 8.0,
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
    /// One-tick edge: fire (or, while attached, release) the wrist grapple along this view direction.
    pub grapple_aim: Option<Vector<Real>>,
    /// One-tick edge set by the katana (and mirrored by `predict_slice`) when a swing starts while `strike_kind()` is
    /// some: launch along `move_dir` (flat aim) instead of the plain dash. See docs/traversal_melee.md.
    pub strike: bool,
    /// Fraction of the run speed taken away (0 = none): a drawn yumi walks at `1 - walk_slow` (`power.rs`).
    pub walk_slow: f32,
    /// Fraction of the run speed added on top (0 = none): the flow streak's move-speed tier (`combat::flow`).
    pub speed_boost: f32,
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

/// What a katana press would be right now besides a plain slash: a hook-strike (rope attached, or just let go) or a
/// wall slash (wall-run). Both launch the character and give the blade extra reach and damage (`KatanaCfg`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StrikeKind {
    Hook,
    Wall,
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

/// Wrist-grapple rope. `len` only ever shortens; `time` is the attached time in seconds.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GrappleState {
    pub anchor: Vector<Real>,
    pub len: f32,
    pub time: f32,
}

/// One-tick edge describing what the grapple did this `update` (feeds `CombatEvent` / rope visuals).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum GrappleEvent {
    #[default]
    None,
    Attached { origin: Vector<Real>, anchor: Vector<Real> },
    Released,
    /// Hook key again on a player rope: the rope is cut and the server pulls whoever is at `anchor` toward `origin`.
    Yank { origin: Vector<Real>, anchor: Vector<Real> },
    /// Fired but nothing hookable in range (sky, too far, too close, a player / moving block): 'no target' feedback.
    Miss { origin: Vector<Real>, dir: Vector<Real> },
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
    pub grapple: Option<GrappleState>,
    pub grapple_cd: f32,
}

pub struct PhysicsWorld {
    pub bodies: RigidBodySet,
    pub colliders: ColliderSet,
    pub islands: IslandManager,
    pub queries: QueryPipeline,
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
    pub grapple: Option<GrappleState>,
    grapple_cd: f32,
    /// What the rope is on: `Some(true)` a player, `Some(false)` geometry, `None` unknown (after a reconcile restore;
    /// re-derived on the next tick). Not on the wire.
    rope_player: Option<bool>,
    /// Edge: what the grapple did in the last `update`.
    pub grapple_event: GrappleEvent,
    /// Seconds since the rope last let go / since the last wall jump (large = long ago). Katana air-cut bonuses read them
    /// (`combat::katana`). Not on the wire: the server runs them continuously, a client restore leaves them as they are.
    pub hook_age: f32,
    pub walljump_age: f32,
    /// Seconds since the last dash (not a ground roll) started (large = long ago). The flow streak (`combat::flow`)
    /// reads this the same way the air-cut bonuses read `hook_age` / `walljump_age`. Not on the wire.
    pub dash_age: f32,
    /// Seconds since the last fresh wall-run entry (large = long ago; continuing an existing wall-run does not reset
    /// it). Same purpose as `dash_age`. Not on the wire.
    pub wallrun_age: f32,
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
                max_height: CharacterLength::Absolute(cfg.step_height),
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
            grapple: None,
            grapple_cd: 0.0,
            rope_player: None,
            grapple_event: GrappleEvent::None,
            hook_age: 99.0,
            walljump_age: 99.0,
            dash_age: 99.0,
            wallrun_age: 99.0,
        }
    }

    pub fn slice_cooldown(&self) -> f32 {
        self.slice_cd
    }

    pub fn set_slice_cooldown(&mut self, v: f32) {
        self.slice_cd = v;
    }

    /// Eye height above the capsule centre right now: `eye_above_center`, lowered by `slide_eye_drop` in a slide.
    /// Shots, slashes and grapple aim start here, so they leave from where the camera is.
    pub fn eye_above(&self) -> f32 {
        self.cfg.eye_above_center - if self.slide.is_some() { self.cfg.slide_eye_drop } else { 0.0 }
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

    /// Server-side knock (wall clash): throw the character off the wall along `dir` (flat) at `speed` with a small hop,
    /// cancelling the rope and the wall-run. Reconciled to the client like any server push.
    pub fn knock_off(&mut self, dir: Vector<Real>, speed: f32) {
        let d = flat_norm(dir);
        self.vel = Vector::new(d.x * speed, 4.0, d.z * speed);
        if matches!(self.state, MoveState::WallRun { .. }) {
            self.state = MoveState::Airborne;
        }
        self.wall_cd = 0.6;
        if self.grapple.is_some() {
            self.grapple_release();
        }
    }

    /// Enemy hook victim side (server only, every tick of the pull): dragged toward `dir` at `speed`, airborne, wall-run and
    /// own rope dropped. The next tick's input still acts on top (air control), the pull re-asserts each tick.
    pub fn hook_pull(&mut self, dir: Vector<Real>, speed: f32) {
        if dir.norm_squared() < 1e-8 {
            return;
        }
        let d = dir.normalize();
        self.vel = Vector::new(d.x * speed, d.y * speed * 0.5 + 1.5, d.z * speed);
        self.state = MoveState::Airborne;
        self.slide = None;
        self.coyote = 0.0;
        self.wall_cd = 0.3;
        if self.grapple.is_some() {
            self.grapple_release();
        }
    }

    /// Enemy hook: the hooker's rope was stopped by a shield; drop it with the player-rope cooldown.
    pub fn hook_blocked(&mut self) {
        if self.grapple.is_some() {
            self.grapple_release();
        }
    }

    /// Hook yank victim side (server only): pulled toward `to` at `speed` (+ `hop`), ropes and wall-runs dropped.
    pub fn yank_pull(&mut self, dir: Vector<Real>, speed: f32, hop: f32) {
        let d = if dir.norm_squared() > 1e-8 { dir.normalize() } else { return };
        self.vel = Vector::new(d.x * speed, d.y * speed * 0.5 + hop, d.z * speed);
        if matches!(self.state, MoveState::WallRun { .. }) {
            self.state = MoveState::Airborne;
        }
        self.slide = None;
        self.state = MoveState::Airborne; // the hop lifts them off the floor (ground friction would eat the pull)
        self.coyote = 0.0;
        self.wall_cd = 0.4;
        if self.grapple.is_some() {
            self.grapple_release();
        }
    }

    /// The wall normal while wall-running (flat, out of the wall), else `None`.
    pub fn wall_normal(&self) -> Option<Vector<Real>> {
        match self.state {
            MoveState::WallRun { normal, .. } => Some(normal),
            _ => None,
        }
    }

    /// Katana press right now = hook-strike / wall slash? (Evaluated on the pre-step state, same on server and client.)
    pub fn strike_kind(&self) -> Option<StrikeKind> {
        if matches!(self.state, MoveState::WallRun { .. }) {
            Some(StrikeKind::Wall)
        } else if self.grapple.is_some() || self.released_within(self.cfg.hookstrike_window) {
            Some(StrikeKind::Hook)
        } else {
            None
        }
    }

    /// The rope let go (a clean release, or a player rope's longer cooldown) within the last `window` seconds.
    fn released_within(&self, window: f32) -> bool {
        let cfg = &self.cfg;
        let hit = |x: f32| self.grapple_cd > (x - window).max(0.0) && self.grapple_cd <= x;
        hit(cfg.grapple_cd) || (cfg.grapple_pull_max > 0.0 && hit(cfg.grapple_player_cd))
    }

    /// A rope that reels us (on, and not an enemy hook).
    fn reeling(&self) -> bool {
        self.grapple.is_some() && !self.pulling_enemy()
    }

    /// The rope is on a player and `grapple_pull_max` is on: the hook pulls THEM, it does not reel you.
    pub fn pulling_enemy(&self) -> bool {
        self.grapple.is_some() && self.cfg.grapple_pull_max > 0.0 && self.rope_player == Some(true)
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
            grapple: self.grapple,
            grapple_cd: self.grapple_cd,
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
        self.grapple = s.grapple;
        self.grapple_cd = s.grapple_cd;
        self.rope_player = None;
        self.grapple_event = GrappleEvent::None;
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
        self.grapple_event = GrappleEvent::None;
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
        self.grapple_cd = (self.grapple_cd - dt).max(0.0);
        self.hook_age = (self.hook_age + dt).min(99.0);
        self.walljump_age = (self.walljump_age + dt).min(99.0);
        self.dash_age = (self.dash_age + dt).min(99.0);
        self.wallrun_age = (self.wallrun_age + dt).min(99.0);
        let dir_in = flat_norm(input.move_dir);
        if dir_in.norm_squared() > 0.0 {
            self.facing = dir_in;
        }
        let grounded_state = self.state == MoveState::Grounded;
        let mut jumped = false;

        // ---- wrist grapple: fire / release ----
        if self.grapple.is_some() {
            self.grapple_release_check(w, pos, input, &mut jumped);
        } else if let Some(aim) = input.grapple_aim {
            self.grapple_fire(w, pos, aim, &mut jumped);
        }

        // ---- katana strike: hook-strike / wall slash launch along the aim (set by Katana::fire / predict_slice) ----
        if input.strike {
            self.strike_launch(dir_in, &mut jumped);
        }

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
                self.dash_age = 0.0; // a dash (not a ground roll) is a flow move (`combat::flow`)
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
            // the run is a 2D velocity in the wall's plane: `a` along the horizontal tangent, `b` straight up (down < 0).
            // Both live in `vel` (on the wire), so a vertical run needs no extra state.
            let (a, b) = (self.vel.dot(&tangent), self.vel.y);
            if self.jump_buf > 0.0 {
                // wall jump: kick off the wall, keep the sideways run, and add the climb you had
                self.vel = normal * cfg.wall_jump_push
                    + tangent * a.clamp(-cfg.wall_speed, cfg.wall_speed) * cfg.wall_jump_keep
                    + up * (cfg.jump_speed + b.max(0.0) * 0.5);
                self.jump_buf = 0.0;
                self.wall_cd = 0.3;
                self.walljump_age = 0.0;
                self.double_used = false;
                self.state = MoveState::Airborne;
                jumped = true;
            } else if !(input.jump_held || input.wall_cling) || time_left - dt <= 0.0 {
                self.wall_cd = 0.15;
                self.state = MoveState::Airborne;
            } else {
                // steer in the plane: the stick along the wall runs sideways, pressing INTO the wall climbs, pressing AWAY
                // runs down; no stick keeps the current run (sticky: a wall-run never slides off by itself)
                let (s_h, s_v) = (dir_in.dot(&tangent), -dir_in.dot(&normal));
                let (mut ta, mut tb) = (a, b);
                if dir_in.norm_squared() > 0.0 && s_h.abs() + s_v.abs() > 0.2 {
                    let m = (s_h * s_h + s_v * s_v).sqrt();
                    ta = s_h / m * cfg.wall_speed;
                    tb = s_v / m * cfg.wall_climb_speed;
                }
                let (da, db) = (ta - a, tb - b);
                let dn = (da * da + db * db).sqrt();
                let step = cfg.wall_steer * dt;
                let (mut a2, mut b2) = if dn <= step { (ta, tb) } else { (a + da / dn * step, b + db / dn * step) };
                let cap = cfg.wall_speed.max(cfg.wall_climb_speed);
                let m2 = (a2 * a2 + b2 * b2).sqrt();
                if m2 > cap {
                    a2 *= cap / m2;
                    b2 *= cap / m2;
                }
                match hit {
                    Some((_, n)) => {
                        let mut t = flat_norm(up.cross(&n));
                        if t.dot(&tangent) < 0.0 {
                            t = -t;
                        }
                        let nn = flat_norm(n);
                        self.vel = t * a2 + up * b2 - nn * cfg.wall_stick; // gravity OFF
                        self.state = MoveState::WallRun { normal: nn, tangent: t, time_left: time_left - dt };
                    }
                    None => {
                        // wall lost (a corner, a gap, the roof edge): keep going along the plane for a moment, burning the
                        // remaining run time faster; a wall that comes back picks the run up again, a jump still wall-jumps.
                        // Climbing past the top edge pushes over it (onto the roof), not away from it.
                        let left = time_left - dt * (1.0 + cfg.wall_coast_burn);
                        if cfg.wall_coast_burn > 0.0 && left > 0.0 {
                            let over = if b2 > 0.5 { cfg.wall_topout_push } else { 0.0 };
                            self.vel = tangent * a2 + up * b2 - normal * over;
                            self.state = MoveState::WallRun { normal, tangent, time_left: left };
                        } else {
                            self.wall_cd = 0.15;
                            self.state = MoveState::Airborne;
                        }
                    }
                }
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
                // horizontal accel: weighty on the ground (ramp up, hard brake on release / reversal), steering-only in the air
                // flow (`combat::flow`) adds a small fraction back on top of a drawn yumi's walk-slow, never below the plain run
                let run_speed = cfg.run_speed * (1.0 - input.walk_slow.clamp(0.0, 0.9)) * (1.0 + input.speed_boost.max(0.0));
                let target = dir_in * run_speed;
                let h = flat(self.vel);
                let delta = target - h;
                let max = if grounded_state {
                    let speed = h.norm();
                    let turning = dir_in.norm_squared() > 0.0 && speed > 0.5 && h.dot(&dir_in) < 0.8 * speed * dir_in.norm();
                    if dir_in.norm_squared() == 0.0 || turning { cfg.ground_decel * dt } else { cfg.ground_accel * dt }
                } else {
                    cfg.air_accel * dt
                };
                let airborne_free = !grounded_state && !self.reeling();
                let speed_now = h.norm();
                let step = if airborne_free && (speed_now > cfg.run_speed || dir_in.norm_squared() == 0.0) {
                    // momentum: a jump keeps its speed. Faster than a run (or no stick): only a slow drag; the stick
                    // steers sideways and can brake, but never adds forward speed past the carry
                    let hd = if speed_now > 1e-4 { h / speed_now } else { Vector::zeros() };
                    let floor_speed = if dir_in.norm_squared() == 0.0 { 0.0 } else { cfg.run_speed };
                    let drag = (cfg.air_drag * dt).min((speed_now - floor_speed).max(0.0));
                    let mut st = dir_in * (cfg.air_accel * dt);
                    st -= hd * st.dot(&hd).max(0.0);
                    st - hd * drag
                } else if self.reeling() {
                    // on the rope the stick only nudges; it never brakes the swing or the reel, and once the player is
                    // faster than a run it can only steer (sideways), never add speed (no runaway on the rope)
                    let mut st = dir_in * (cfg.air_accel * dt * cfg.grapple_steer);
                    let speed = h.norm();
                    if speed > cfg.run_speed {
                        let hd = h / speed;
                        st -= hd * st.dot(&hd);
                    }
                    st
                } else if delta.norm() <= max {
                    delta
                } else {
                    delta.normalize() * max
                };
                let mut nh = h + step;
                if !grounded_state && !self.reeling() {
                    // rope / strike momentum past the rope's speed ceiling settles back down over a second or two
                    let sp = nh.norm();
                    if sp > cfg.grapple_max_speed {
                        nh *= (sp - (sp - cfg.grapple_max_speed) * (cfg.overspeed_bleed * dt).min(1.0)) / sp;
                    }
                }
                self.vel.x = nh.x;
                self.vel.z = nh.z;
            }

            // jumps: ground/coyote, then double jump at apex
            if self.jump_buf > 0.0 {
                if self.coyote > 0.0 {
                    self.vel.y = cfg.jump_speed;
                    self.carry_horizontal(&cfg, dir_in, false, cfg.jump_carry);
                    self.coyote = 0.0;
                    self.jump_buf = 0.0;
                    jumped = true;
                } else if !grounded_state
                    && !self.double_used
                    && self.vel.y.abs() <= cfg.peak_window
                {
                    self.vel.y = cfg.double_jump_speed;
                    // redirect: the momentum turns to where the stick points, and gets a push
                    self.carry_horizontal(&cfg, dir_in, true, cfg.double_jump_carry);
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
                // variable jump: hold to rise higher and glide down, release to hop short / fall at full weight
                let g = if self.reeling() {
                    cfg.gravity
                } else if self.vel.y > 0.0 {
                    cfg.gravity * if input.jump_held { cfg.rise_hold_mult } else { cfg.rise_cut_mult }
                } else if input.jump_held {
                    cfg.gravity * cfg.fall_glide_mult
                } else {
                    cfg.gravity
                };
                self.vel.y = (self.vel.y - g * dt).max(-cfg.terminal);
            }

            // wall-run entry: airborne + jump held + a wall beside you (run along it) or in front (run up it)
            if !grounded_state && (input.jump_held || input.wall_cling) && self.wall_cd <= 0.0 && self.evasion.is_none() {
                let fwd = if dir_in.norm_squared() > 0.0 { dir_in } else { flat_norm(self.vel) };
                let mut entered = false;
                if flat(self.vel).norm() >= cfg.wall_min_speed {
                    if let Some((n, t)) = self.probe_wall(w, pos, fwd) {
                        self.state = MoveState::WallRun { normal: n, tangent: t, time_left: cfg.wall_max_time };
                        // snap to the run along the wall; a rising entry keeps (most of) its climb, a fall is eased
                        self.vel = t * cfg.wall_speed + up * (self.vel.y.max(-2.0) * 0.6) - n * cfg.wall_stick;
                        entered = true;
                        self.wallrun_age = 0.0; // a fresh wall-run entry is a flow move (`combat::flow`)
                    }
                }
                if !entered {
                    // head-on: running or jumping at a face climbs it
                    if let Some(n) = self.probe_wall_front(w, pos, fwd) {
                        let toward = -flat(self.vel).dot(&n);
                        if toward >= cfg.wall_front_min_speed {
                            let t = flat_norm(up.cross(&n));
                            let side = flat(self.vel).dot(&t).clamp(-cfg.wall_speed, cfg.wall_speed);
                            let t = if side < 0.0 { -t } else { t };
                            self.state = MoveState::WallRun { normal: n, tangent: t, time_left: cfg.wall_max_time };
                            self.vel = t * side.abs() + up * self.vel.y.max(cfg.wall_up_enter) - n * cfg.wall_stick;
                            self.wallrun_age = 0.0; // a fresh wall-run entry is a flow move (`combat::flow`)
                        }
                    }
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

        // ---- wrist grapple: reel + taut rope (slack during a dash/roll) ----
        if self.evasion.is_none() {
            self.grapple_pull(dt, pos, input.slide);
        }
        // ---- ledge assist: near the anchor, or just after the reel ended (not when the player is jumping off on purpose) ----
        if cfg.mantle_height > 0.0 && !input.jump_held && self.evasion.is_none() && !matches!(self.state, MoveState::WallRun { .. }) {
            let near_rope = self.reeling(); // blocked by a steep face is the only trigger, so any rope distance is fine
            let recent = self.grapple.is_none() && self.released_within(cfg.mantle_window);
            if near_rope || recent {
                self.mantle_assist(w, pos);
            }
        }

        // ---- KCC ----
        let shape = Capsule::new_y(cfg.half_height, cfg.radius);
        let desired = self.vel * dt;
        let mut wall_hits: Vec<(ColliderHandle, Vector<Real>)> = Vec::new();
        let mut mv = self.kcc.move_shape(
            dt,
            &w.bodies,
            &w.colliders,
            &w.queries,
            &shape,
            &Isometry::translation(pos.x, pos.y, pos.z),
            desired,
            self.filter(),
            |c| wall_hits.push((c.handle, c.hit.normal1.into_inner())),
        );
        // Wedge recovery: pressed flat against a wall face inside the skin (a wall-run exit, a grapple into a wall, a
        // corner seam) the controller can return no motion at all, even along the wall. Step 3 cm out of the face along
        // its normal and retry with the move projected onto the wall plane. Stateless, so replay / parity are unaffected.
        let dh = flat(desired);
        let mut new_pos = pos + mv.translation;
        if dh.norm() > 0.05 && flat(mv.translation).norm() < dh.norm() * 0.1 {
            // static geometry only: another player's capsule is not a wall (and the predictor may not even see it)
            let is_static = |h: &ColliderHandle| {
                w.colliders.get(*h).map_or(false, |c| {
                    let player = c.collision_groups().memberships == crate::combat::GROUP_PLAYER;
                    !player && c.parent().map_or(true, |b| w.bodies.get(b).map_or(true, |rb| rb.is_fixed()))
                })
            };
            // ...and only when no player is part of the pile-up: a body in the way is the server's call, not a wedge
            let touching_player = wall_hits.iter().any(|(h, _)| w.colliders.get(*h).map_or(false, |c| c.collision_groups().memberships == crate::combat::GROUP_PLAYER));
            let wall = if touching_player { None } else { wall_hits.iter().filter(|(h, _)| is_static(h)).map(|(_, n)| flat(*n)).find(|n| n.norm() > 0.5) };
            if let Some(n) = wall.map(|n| n.normalize()) {
                let away = -n * WEDGE_NUDGE; // `n` points from the character into the wall
                let tangent = dh - n * dh.dot(&n).max(0.0);
                let retry = Vector::new(tangent.x, desired.y, tangent.z);
                let from = pos + away;
                let mv2 = self.kcc.move_shape(
                    dt,
                    &w.bodies,
                    &w.colliders,
                    &w.queries,
                    &shape,
                    &Isometry::translation(from.x, from.y, from.z),
                    retry,
                    self.filter(),
                    |_| {},
                );
                if flat(mv2.translation).norm() > flat(mv.translation).norm() + 1e-3 {
                    new_pos = from + mv2.translation;
                    mv = mv2;
                    mv.translation += away;
                }
            }
        }
        // far hooks drag a grounded player along the floor: a ledge they cannot step stops them cold, so hop (see Cfg)
        if let Some(g) = self.grapple.filter(|_| !self.pulling_enemy()) {
            let want = desired.norm();
            if mv.grounded && want > 0.05 && flat(mv.translation).norm() < flat(desired).norm() * 0.2 && self.vel.y < cfg.grapple_hop {
                let eye = pos + Vector::y() * cfg.eye_above_center;
                if (g.anchor - eye).norm() >= cfg.grapple_arrive + 0.6 {
                    self.vel.y = cfg.grapple_hop;
                }
            }
        }
        // wedged against a ledge just short of the release radius: the reel is driving into geometry and the capsule
        // barely moves, so let go (carrying the real speed, not the phantom reel speed) instead of hanging there
        if let Some(g) = self.grapple.filter(|_| !self.pulling_enemy()) {
            let want = desired.norm();
            if want > 0.05 && mv.translation.norm() < want * 0.2 {
                let eye = pos + Vector::y() * cfg.eye_above_center;
                if (g.anchor - eye).norm() < cfg.grapple_arrive + 0.6 {
                    self.grapple_release();
                    self.vel = mv.translation / dt;
                }
            }
        }
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

    /// Static geometry only: sensors, this body, and anything parented to a non-fixed body (players, enemies) are skipped.
    fn grapple_cast(&self, w: &PhysicsWorld, o: Vector<Real>, d: Vector<Real>, max: f32) -> Option<f32> {
        if !(o.iter().all(|v| v.is_finite()) && d.iter().all(|v| v.is_finite()) && d.norm_squared() > 1e-12) {
            return None;
        }
        let pred = |_h: ColliderHandle, c: &Collider| c.parent().map_or(true, |b| w.bodies.get(b).map_or(true, |rb| rb.is_fixed()));
        let filter = QueryFilter::default().exclude_rigid_body(self.body).exclude_sensors().predicate(&pred);
        let ray = Ray::new(Point::from(o), d);
        w.queries.cast_ray(&w.bodies, &w.colliders, &ray, max, true, filter).map(|(_, t)| t)
    }

    /// First hookable point along `dir` from `eye`: (distance, point). Same rule `grapple_fire` uses, shared with the
    /// client's crosshair "hookable" probe. `None` = sky, out of range, closer than `grapple_min_len`, or only players /
    /// moving geometry in the way. Callers must have refreshed `w.queries` (the solid pipeline).
    pub fn grapple_probe(&self, w: &PhysicsWorld, eye: Vector<Real>, dir: Vector<Real>) -> Option<(f32, Vector<Real>)> {
        if dir.norm_squared() < 1e-6 {
            return None;
        }
        let dir = dir.normalize();
        let toi = self.grapple_cast(w, eye, dir, self.cfg.grapple_range)?;
        (toi >= self.cfg.grapple_min_len && toi.is_finite()).then(|| (toi, eye + dir * toi))
    }

    /// Geometry or player: the nearest of `grapple_probe` and a living player's hitbox along `dir` (the anchor is the
    /// player's centre and follows them while the rope is on), else the anchor assist. `(rope length, anchor)`.
    pub fn grapple_target(&self, w: &PhysicsWorld, eye: Vector<Real>, dir: Vector<Real>) -> Option<(f32, Vector<Real>)> {
        self.grapple_target_kind(w, eye, dir).map(|(l, a, _)| (l, a))
    }

    /// As `grapple_target`, plus whether the anchor is a player.
    pub fn grapple_target_kind(&self, w: &PhysicsWorld, eye: Vector<Real>, dir: Vector<Real>) -> Option<(f32, Vector<Real>, bool)> {
        let geo = self.grapple_probe(w, eye, dir);
        if dir.norm_squared() >= 1e-6 {
            let reach = geo.map_or(self.cfg.grapple_range, |g| g.0);
            if let Some((t, centre)) = self.player_cast(w, eye, dir.normalize(), reach) {
                if t >= self.cfg.grapple_min_len {
                    return Some(((centre - eye).norm(), centre, true));
                }
            }
        }
        if let Some((l, a)) = geo {
            return Some((l, a, false));
        }
        self.grapple_assist(w, eye, dir).map(|(l, a)| (l, a, false))
    }

    /// Anchor assist: the best hookable point in a cone of `grapple_assist_deg` around `dir` (3 rings x 8 rays), for an
    /// aim that found nothing. Rim / edge points and tops are preferred so a building edge is easy to hook and mantle.
    /// Deterministic (libm, fixed ray order): the client predicts exactly what the server picks.
    pub fn grapple_assist(&self, w: &PhysicsWorld, eye: Vector<Real>, dir: Vector<Real>) -> Option<(f32, Vector<Real>)> {
        let cfg = &self.cfg;
        if cfg.grapple_assist_deg <= 0.0 || dir.norm_squared() < 1e-6 || !dir.iter().all(|v| v.is_finite()) {
            return None;
        }
        let dir = dir.normalize();
        let mut right = dir.cross(&Vector::y());
        if right.norm_squared() < 1e-6 {
            right = Vector::x();
        }
        let right = right.normalize();
        let up = right.cross(&dir);
        let mut best: Option<(f32, f32, Vector<Real>)> = None; // (score, rope length, point)
        for ring in 1..=3 {
            let deg = cfg.grapple_assist_deg * ring as f32 / 3.0;
            let t = libm::tanf(deg.to_radians());
            for k in 0..8 {
                let a = k as f32 * core::f32::consts::FRAC_PI_4;
                let d = (dir + (right * libm::cosf(a) + up * libm::sinf(a)) * t).normalize();
                let Some((toi, n)) = self.grapple_cast_n(w, eye, d, cfg.grapple_range) else { continue };
                if toi < cfg.grapple_min_len || !toi.is_finite() {
                    continue;
                }
                let p = eye + d * toi;
                let mut score = deg;
                let edge = self.near_edge(w, p, n);
                if n.y > 0.7 {
                    score -= cfg.grapple_assist_edge * 0.5;
                }
                // only rims and ledges are worth snapping to: the assist forgives an edge shot, it does not turn every
                // miss into a hook on whatever wall or floor is nearby
                if !edge || p.y < eye.y - 0.5 {
                    continue;
                }
                score -= cfg.grapple_assist_edge;
                if best.map_or(true, |b| score < b.0 - 1e-4) {
                    best = Some((score, toi, p));
                }
            }
        }
        best.map(|b| (b.1, b.2))
    }

    /// Is `p` (on a face with normal `n`) within ~0.6 m of the face's rim? Probes four tangents for missing surface.
    fn near_edge(&self, w: &PhysicsWorld, p: Vector<Real>, n: Vector<Real>) -> bool {
        let mut t1 = n.cross(&Vector::y());
        if t1.norm_squared() < 0.01 {
            t1 = Vector::x();
        }
        let t1 = t1.normalize();
        let t2 = n.cross(&t1).normalize();
        [t1, -t1, t2, -t2].iter().any(|t| self.grapple_cast_n(w, p + n * 0.1 + t * 0.6, -n, 0.3).is_none())
    }

    fn grapple_cast_n(&self, w: &PhysicsWorld, o: Vector<Real>, d: Vector<Real>, max: f32) -> Option<(f32, Vector<Real>)> {
        if !(o.iter().all(|v| v.is_finite()) && d.iter().all(|v| v.is_finite()) && d.norm_squared() > 1e-12) {
            return None;
        }
        let pred = |_h: ColliderHandle, c: &Collider| c.parent().map_or(true, |b| w.bodies.get(b).map_or(true, |rb| rb.is_fixed()));
        let filter = QueryFilter::default().exclude_rigid_body(self.body).exclude_sensors().predicate(&pred);
        let ray = Ray::new(Point::from(o), d);
        w.queries.cast_ray_and_get_normal(&w.bodies, &w.colliders, &ray, max, true, filter).map(|(_, h)| (h.time_of_impact, h.normal))
    }

    /// Nearest living player hitbox (sensor capsule in `GROUP_HITBOX`) along the ray: `(distance, centre)`. Sensors are not
    /// in the solid pipeline `update` runs on, so this scans the collider set: a few microseconds, and only on a hook
    /// press or while a player hook is on. Hitboxes within 1.6 m of the body are skipped (our own, a tick behind).
    fn player_cast(&self, w: &PhysicsWorld, eye: Vector<Real>, dir: Vector<Real>, max: f32) -> Option<(f32, Vector<Real>)> {
        if !(eye.iter().all(|v| v.is_finite()) && dir.iter().all(|v| v.is_finite())) {
            return None;
        }
        let ray = Ray::new(Point::from(eye), dir);
        let me = *w.bodies[self.body].translation();
        let mut best: Option<(f32, Vector<Real>)> = None;
        for (_, c) in w.colliders.iter() {
            if !c.is_sensor() || !c.collision_groups().memberships.intersects(crate::combat::GROUP_HITBOX) {
                continue;
            }
            let centre = *c.translation();
            if (centre - me).norm() < 1.6 {
                continue;
            }
            let Some(t) = c.shape().cast_ray(c.position(), &ray, max, true) else { continue };
            if best.map_or(true, |b| t < b.0) {
                best = Some((t, centre));
            }
        }
        best
    }

    /// The living player hitbox centre nearest `p` within `r` metres (a player hook re-anchors onto it every tick).
    fn player_near(&self, w: &PhysicsWorld, p: Vector<Real>, r: f32) -> Option<Vector<Real>> {
        let me = *w.bodies[self.body].translation();
        let mut best: Option<(f32, Vector<Real>)> = None;
        for (_, c) in w.colliders.iter() {
            if !c.is_sensor() || !c.collision_groups().memberships.intersects(crate::combat::GROUP_HITBOX) {
                continue;
            }
            let centre = *c.translation();
            if (centre - me).norm() < 1.6 {
                continue; // our own hitbox (a tick behind; the client has none): never re-anchor onto ourselves
            }
            let d = (centre - p).norm();
            if d <= r && best.map_or(true, |b| d < b.0) {
                best = Some((d, centre));
            }
        }
        best.map(|b| b.1)
    }

    /// Katana strike launch (see `StrikeKind`): redirect the momentum along the flat aim `dir`, keeping any speed above
    /// the strike floor. Hook: drop the rope. Wall: leave the wall (never into it). Both give the double jump back.
    fn strike_launch(&mut self, dir: Vector<Real>, jumped: &mut bool) {
        let cfg = self.cfg;
        if dir.norm_squared() == 0.0 {
            return;
        }
        match self.strike_kind() {
            Some(StrikeKind::Wall) => {
                if let MoveState::WallRun { normal, .. } = self.state {
                    let d = if dir.dot(&normal) < 0.3 { flat_norm(dir + normal * 0.8) } else { dir };
                    let sp = flat(self.vel).norm().max(cfg.wallslash_speed);
                    self.vel = Vector::new(d.x * sp, cfg.wallslash_lift, d.z * sp);
                    self.state = MoveState::Airborne;
                    self.wall_cd = 0.25;
                    self.double_used = false;
                    *jumped = true;
                }
            }
            Some(StrikeKind::Hook) => {
                if self.grapple.is_some() {
                    self.grapple_release();
                }
                let sp = flat(self.vel).norm().max(cfg.hookstrike_speed);
                self.vel = Vector::new(dir.x * sp, self.vel.y.max(cfg.hookstrike_lift), dir.z * sp);
                self.coyote = 0.0;
                self.double_used = false;
                *jumped = true;
            }
            None => {}
        }
    }

    /// Seconds until the wrist grapple can fire again (0 = ready).
    pub fn grapple_cooldown(&self) -> f32 {
        self.grapple_cd
    }

    /// Drop the rope without an event or cooldown (death, respawn, teleport back).
    pub fn clear_grapple(&mut self) {
        self.grapple = None;
        self.grapple_cd = 0.0;
    }

    fn grapple_fire(&mut self, w: &PhysicsWorld, pos: Vector<Real>, aim: Vector<Real>, jumped: &mut bool) {
        let cfg = self.cfg;
        if self.grapple_cd > 0.0 || aim.norm_squared() < 1e-6 || !aim.iter().all(|v| v.is_finite()) {
            return;
        }
        let dir = aim.normalize();
        let eye = pos + Vector::y() * self.eye_above();
        match self.grapple_target_kind(w, eye, dir) {
            Some((toi, anchor, on_player)) => {
                self.grapple = Some(GrappleState { anchor, len: toi, time: 0.0 });
                self.rope_player = Some(on_player);
                // the rope takes over from wall-run and slide (both overwrite velocity every tick); a dash / roll in
                // progress keeps going and the rope is slack until it ends
                if matches!(self.state, MoveState::WallRun { .. }) {
                    self.state = MoveState::Airborne;
                    self.wall_cd = 0.15;
                }
                if self.slide.take().is_some() {
                    self.slide_cd = cfg.slide_cd;
                }
                if self.state == MoveState::Grounded && !self.pulling_enemy() {
                    self.vel.y = self.vel.y.max(0.0) + cfg.grapple_lift;
                    self.coyote = 0.0;
                    *jumped = true; // leave the ground this tick (no ground stick, no landing snap)
                }
                self.grapple_event = GrappleEvent::Attached { origin: eye, anchor };
            }
            None => {
                self.grapple_cd = cfg.grapple_miss_cd;
                self.grapple_event = GrappleEvent::Miss { origin: eye, dir };
            }
        }
    }

    /// The rope is on, and its anchor is not geometry: a player hook (hook key again = yank). Needs the solid queries.
    pub fn rope_on_player(&self, w: &PhysicsWorld) -> bool {
        self.grapple.map_or(false, |g| self.rope_player.unwrap_or_else(|| !self.grapple_anchor_present(w, g.anchor)))
    }

    /// Is hookable geometry still within a hair of `anchor`?
    fn grapple_anchor_present(&self, w: &PhysicsWorld, anchor: Vector<Real>) -> bool {
        if !anchor.iter().all(|v| v.is_finite()) {
            return false;
        }
        let pred = |_h: ColliderHandle, c: &Collider| c.parent().map_or(true, |b| w.bodies.get(b).map_or(true, |rb| rb.is_fixed()));
        let filter = QueryFilter::default().exclude_rigid_body(self.body).exclude_sensors().predicate(&pred);
        let ball = Ball::new(0.08);
        w.queries
            .intersection_with_shape(&w.bodies, &w.colliders, &Isometry::translation(anchor.x, anchor.y, anchor.z), &ball, filter)
            .is_some()
    }

    fn grapple_release(&mut self) {
        let player_rope = self.pulling_enemy();
        self.grapple = None;
        self.rope_player = None;
        self.grapple_cd = if player_rope { self.cfg.grapple_player_cd } else { self.cfg.grapple_cd };
        self.grapple_event = GrappleEvent::Released;
        self.hook_age = 0.0;
    }

    /// Detach conditions, evaluated at the start of the tick (velocity is kept on release).
    fn grapple_release_check(&mut self, w: &PhysicsWorld, pos: Vector<Real>, input: &PlayerInput, jumped: &mut bool) {
        let cfg = self.cfg;
        let Some(mut g) = self.grapple else { return };
        let eye = pos + Vector::y() * cfg.eye_above_center;
        let to = g.anchor - eye;
        let d = to.norm();
        let hook_key = input.grapple_aim.is_some();
        let present = self.grapple_anchor_present(w, g.anchor);
        // what the rope is on: set at the fire; after a reconcile restore, re-derived (a player rope has no geometry at its anchor)
        let on_player = match self.rope_player {
            Some(v) => v,
            None => !present && self.player_near(w, g.anchor, 2.5).is_some(),
        };
        self.rope_player = Some(on_player);
        let pull = on_player && cfg.grapple_pull_max > 0.0;
        let arrived = d < if pull { cfg.grapple_pull_end } else { cfg.grapple_arrive };
        let mut release = hook_key || g.time > cfg.grapple_max_time || arrived;
        // soft arrival / cancel: the speed is bled off unless the player is deliberately launching (jump held, dash)
        let launch = input.jump_held || input.dash_pressed || self.jump_buf > 0.0;
        let keep = if launch {
            1.0
        } else if hook_key {
            cfg.cancel_keep
        } else if arrived {
            cfg.arrive_speed_keep
        } else {
            1.0
        };
        let mut sling = false;
        if !release && self.jump_buf > 0.0 {
            release = true;
            sling = true;
            if !matches!(self.state, MoveState::WallRun { .. }) {
                self.jump_buf = 0.0;
                self.vel.y = self.vel.y.max(0.0) + cfg.grapple_jump_boost;
                *jumped = true;
            }
        }
        if !release && d > 0.4 {
            // line of sight: anything solid before the anchor cuts the rope
            if self.grapple_cast(w, eye, to / d, d - 0.3).is_some() {
                release = true;
            }
        }
        // the surface must still be there (a block can be evicted or removed mid-swing)
        if !release && !present {
            // a player hook follows the player (their hitbox centre near the old anchor); a geometry anchor that is gone
            // (the box broke, was evicted) lets go and keeps the slingshot momentum; so does a player who left
            match if on_player { self.player_near(w, g.anchor, 2.5) } else { None } {
                Some(c) => {
                    g.anchor = c;
                    self.grapple = Some(g);
                }
                None => {
                    release = true;
                    sling = true;
                }
            }
        }
        if release {
            // hook key on a rope that is not on geometry = a player rope: yank (same physics as the cancel brake; the
            // server finds the victim; the longer cooldown rides in `grapple_cd`, so it is predicted exactly)
            let yank = hook_key && cfg.yank_speed > 0.0 && on_player;
            self.grapple_release();
            if yank {
                self.grapple_cd = cfg.yank_cd;
                self.grapple_event = GrappleEvent::Yank { origin: eye, anchor: g.anchor };
            }
            if keep < 1.0 {
                self.vel *= keep;
            } else if sling || launch {
                // slingshot: a launching release adds a share of the speed along the line of flight
                let sp = self.vel.norm();
                if sp > 6.0 && cfg.grapple_sling > 0.0 {
                    self.vel += self.vel / sp * (sp * cfg.grapple_sling).min(cfg.grapple_sling_cap);
                }
            }
        }
    }

    /// Ledge assist (see `Cfg::mantle_*`): a deck / ramp / ledge top up to `mantle_height` above the feet and just ahead
    /// lifts the player onto it with a little forward carry, instead of letting them hang against the face or fly over.
    /// `fwd`: the direction of travel (toward the anchor on the rope, else the velocity / facing).
    fn mantle_assist(&mut self, w: &PhysicsWorld, pos: Vector<Real>) {
        let cfg = self.cfg;
        let fwd = match self.grapple {
            Some(g) => flat_norm(g.anchor - pos),
            None => flat_norm(self.vel),
        };
        let fwd = if fwd.norm_squared() > 0.0 { fwd } else { self.facing };
        let feet = pos.y - cfg.half_height - cfg.radius;
        // only when something steep blocks the way (a deck face, a lip, a wall): free flight and walkable slopes need no help
        let reach = cfg.radius + 0.45;
        let blocked = [0.25f32, 1.0, 1.7].iter().any(|dy| {
            self.cast(w, Vector::new(pos.x, feet + dy, pos.z), fwd, reach).map_or(false, |(_, n)| n.y.abs() < 0.6)
        });
        if !blocked {
            return;
        }
        for off in [0.45f32, 0.85, 1.3] {
            let o = Vector::new(pos.x + fwd.x * off, feet + cfg.mantle_height + 0.6, pos.z + fwd.z * off);
            let Some((toi, n)) = self.cast(w, o, -Vector::y(), cfg.mantle_height + 1.1) else { continue };
            if n.y < 0.7 {
                continue;
            }
            let top = o.y - toi;
            let rise = top - feet;
            if rise > 0.04 && rise <= cfg.mantle_height + 0.05 {
                // room to stand: nothing within a capsule's height above the landing spot
                if self.cast(w, Vector::new(o.x, top + 0.05, o.z), Vector::y(), 1.9).is_some() {
                    continue;
                }
                let vy = ((rise + 0.35) / 0.14).min(28.0);
                // proportional approach: the climb speed follows the rise still needed, so the player arrives at the lip
                // gently instead of keeping the lift speed and shooting past the deck
                self.vel.y = vy;
                let h = flat(self.vel);
                if h.dot(&fwd) < cfg.mantle_carry {
                    self.vel.x = fwd.x * cfg.mantle_carry;
                    self.vel.z = fwd.z * cfg.mantle_carry;
                }
                return;
            }
        }
    }

    /// Reel toward the anchor (keeping any faster existing speed), then remove outward velocity on a taut rope.
    ///
    /// `swing` (slide / crouch held): keep the rope length instead of reeling. No pull, no gravity cancel, no braking, just
    /// the taut-rope constraint: a pendulum / orbit around the anchor under gravity, steered by the stick.
    fn grapple_pull(&mut self, dt: f32, pos: Vector<Real>, swing: bool) {
        let cfg = self.cfg;
        let Some(mut g) = self.grapple else { return };
        let eye = pos + Vector::y() * cfg.eye_above_center;
        let to = g.anchor - eye;
        let d = to.norm();
        if d < 1e-3 {
            return;
        }
        if self.pulling_enemy() {
            // enemy hook: the server drags THEM to us; we stay put, the rope is slack and only keeps time
            g.time += dt;
            g.len = d;
            self.grapple = Some(g);
            return;
        }
        let dir = to / d;
        let along = self.vel.dot(&dir);
        // ease-in: the first 0.25 s of the reel pulls at 35 % -> 100 %, so the hook yanks instead of slamming
        let ease = (0.35 + 0.65 * g.time / cfg.grapple_ease_in).min(1.0);
        // far hooks reel faster (`grapple_reel_speed`); the soft arrival and the brake scale with it so a 60 m/s reel
        // still slows down before the wall instead of hitting it at full speed
        let reel = cfg.grapple_reel_speed(d);
        let scale = reel / cfg.grapple_speed;
        // soft arrival: inside `grapple_soft_dist` of the release radius the closing speed is eased down so the player
        // is not driven into the surface at full reel speed (momentum past a free-hanging anchor still carries)
        let near = (d - cfg.grapple_arrive).max(0.0);
        let cap_near = cfg.grapple_arrive_speed + near * (reel - cfg.grapple_arrive_speed) / (cfg.grapple_soft_dist * scale);
        if swing {
            // hold the rope length (see above)
        } else if along > cap_near {
            self.vel -= dir * (cfg.grapple_brake * scale * dt).min(along - cap_near);
        } else if along < reel {
            self.vel += dir * (cfg.grapple_accel * scale * ease * dt).min(reel - along);
            // a reeling rope carries the weight: cancel this tick's gravity so shallow shots still lift the player
            self.vel.y += cfg.gravity * dt;
        }
        g.time += dt;
        let stretch = d - g.len; // > 0: integration drift carried the player past the rope's length
        g.len = g.len.min(d);
        if d >= g.len - 1e-3 {
            let out = -self.vel.dot(&dir);
            if out > 0.0 {
                self.vel += dir * out;
            }
            // position drift: a taut rope cannot stretch; pull the excess back over a few ticks (no snap, no jitter)
            if stretch > 0.01 {
                self.vel += dir * (stretch * 0.4 / dt).min(3.0);
            }
        }
        let speed = self.vel.norm();
        let max_speed = cfg.grapple_max_speed.max(reel + 2.0);
        if speed > max_speed {
            self.vel *= max_speed / speed;
        }
        self.grapple = Some(g);
    }

    /// Three flat rays per side; all must hit a near-vertical, coplanar wall.
    /// Jump carry: add `carry` m/s of horizontal speed along the current heading (or, `redirect`, along `hint`, the
    /// stick, which also turns the momentum), never past `air_speed_cap` (a faster entry speed is kept as it is).
    fn carry_horizontal(&mut self, cfg: &Cfg, hint: Vector<Real>, redirect: bool, carry: f32) {
        let h = flat(self.vel);
        let speed = h.norm();
        let dir = if redirect && hint.norm_squared() > 0.0 {
            hint
        } else if speed > 0.5 {
            h / speed
        } else if hint.norm_squared() > 0.0 {
            hint
        } else {
            return;
        };
        let base = if redirect { speed.max(cfg.run_speed) } else { speed };
        let new = if base >= cfg.air_speed_cap { base } else { (base + carry).min(cfg.air_speed_cap) };
        self.vel.x = dir.x * new;
        self.vel.z = dir.z * new;
    }

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

    /// A near-vertical face straight ahead along `fwd`: three flat rays (feet, centre, head) must hit one coplanar wall
    /// that faces us. Returns the wall normal (flat, pointing out of the wall).
    fn probe_wall_front(&self, w: &PhysicsWorld, pos: Vector<Real>, fwd: Vector<Real>) -> Option<Vector<Real>> {
        let cfg = self.cfg;
        let f = flat_norm(fwd);
        if f.norm_squared() == 0.0 {
            return None;
        }
        let reach = cfg.radius + cfg.wall_probe * 2.0;
        let mut n0: Option<Vector<Real>> = None;
        for dy in [-cfg.half_height, 0.0, cfg.half_height] {
            match self.cast(w, pos + Vector::y() * dy, f, reach) {
                Some((_, n)) if n.y.abs() < 0.15 && flat_norm(n).dot(&f) < -0.6 => match n0 {
                    None => n0 = Some(flat_norm(n)),
                    Some(a) if a.dot(&flat_norm(n)) > 0.95 => {}
                    _ => return None,
                },
                _ => return None,
            }
        }
        n0
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
        assert!(w.bodies[ch.body].translation().z < -6.0, "ran along the wall");
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

    /// Ticks to reach full run speed from rest, and the distance covered stopping from it after the stick is released.
    fn ground_run_numbers() -> (u32, f32, f32) {
        let mut w = world();
        floor(&mut w);
        let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
        let mut now = 0.0;
        settle(&mut ch, &mut w, &mut now);
        let run = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], ..Default::default() };
        let mut to_full = 0;
        while flat(ch.vel).norm() < ch.cfg.run_speed - 1e-3 && to_full < 120 {
            tick(&mut ch, &mut w, &run, &mut now);
            to_full += 1;
        }
        let top = flat(ch.vel).norm();
        for _ in 0..5 {
            tick(&mut ch, &mut w, &run, &mut now); // cruising
        }
        let z0 = w.bodies[ch.body].translation().z;
        let mut stop_ticks = 0;
        while flat(ch.vel).norm() > 1e-4 && stop_ticks < 120 {
            tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
            stop_ticks += 1;
        }
        let stop_dist = (z0 - w.bodies[ch.body].translation().z).abs();
        eprintln!("ground feel: top {top:.2} m/s, {to_full} ticks ({:.3} s) to full, stop in {stop_ticks} ticks ({:.3} s) over {stop_dist:.3} m", to_full as f32 * DT, stop_ticks as f32 * DT);
        (to_full, stop_dist, top)
    }

    #[test]
    fn ground_movement_has_weight_ramps_up_and_stops_dead_without_skating() {
        let (to_full, stop_dist, top) = ground_run_numbers();
        assert!((top - Cfg::default().run_speed).abs() < 1e-3);
        assert!((5..=10).contains(&to_full), "about 0.1 s to full speed: {to_full} ticks");
        assert!(stop_dist < 0.6, "stops inside 0.6 m from a full run: {stop_dist}");
        // after the stick is released there is no drift: a few ticks later the velocity is exactly zero and stays zero
        let mut w = world();
        floor(&mut w);
        let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
        let mut now = 0.0;
        settle(&mut ch, &mut w, &mut now);
        let run = PlayerInput { move_dir: vector![0.3, 0.0, -1.0].normalize(), ..Default::default() };
        for _ in 0..30 {
            tick(&mut ch, &mut w, &run, &mut now);
        }
        for _ in 0..8 {
            tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        }
        let p = *w.bodies[ch.body].translation();
        for _ in 0..30 {
            tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        }
        assert!(flat(ch.vel).norm() == 0.0, "no residual velocity");
        // horizontal only: the capsule may re-seat a few mm vertically (kcc skin) after a stop
        assert!(flat(*w.bodies[ch.body].translation() - p).norm() < 1e-3, "and no drift");
    }

    #[test]
    fn reversing_direction_on_the_ground_brakes_hard_instead_of_arcing() {
        let mut w = world();
        floor(&mut w);
        let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
        let mut now = 0.0;
        settle(&mut ch, &mut w, &mut now);
        let fwd = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], ..Default::default() };
        for _ in 0..20 {
            tick(&mut ch, &mut w, &fwd, &mut now);
        }
        let back = PlayerInput { move_dir: vector![0.0, 0.0, 1.0], ..Default::default() };
        let mut ticks = 0;
        while ch.vel.z < ch.cfg.run_speed - 1e-3 && ticks < 60 {
            tick(&mut ch, &mut w, &back, &mut now);
            ticks += 1;
            assert!(ch.vel.x.abs() < 1e-4, "straight reverse does not drift sideways: {}", ch.vel.x);
        }
        assert!(ticks <= 14, "a full reversal takes ~0.2 s, took {ticks} ticks");
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
        // a tap is a short hop, a held jump rises higher and floats (variable jump), the double jump adds height
        let (h1, t1) = jump_peak(false);
        assert!((1.7..2.2).contains(&h1), "tap jump peak {h1} m");
        assert!((0.30..0.50).contains(&t1), "time to apex {t1} s");
        let (h2, _) = jump_peak(true);
        assert!(h2 > h1 + 0.6, "double jump at the apex must add real height: {h1} -> {h2}");
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
        for i in 0..260 {
            tick(&mut ch, &mut w, &run, &mut now);
            if i > 110 && ch.state != MoveState::Grounded {
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
        for _ in 0..12 {
            tick(&mut ch, &mut w, &run, &mut now);
        }
        (w, ch, now)
    }

    #[test]
    fn slide_boosts_then_decays_and_ends_on_low_speed() {
        let (mut w, mut ch, mut now) = slide_scene();
        let input = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], slide: true, ..Default::default() };
        tick(&mut ch, &mut w, &input, &mut now);
        assert!(ch.slide.is_some());
        assert!(flat(ch.vel).norm() > ch.cfg.run_speed + 2.0, "boost over run speed: {}", flat(ch.vel).norm());
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

    // ---------- wrist grapple ----------

    /// Airborne at (0,5,0); a big wall faces it at z = -30 (its face is the z = -29 plane).
    fn grapple_scene() -> (PhysicsWorld, CharacterSystem) {
        let mut w = world();
        boxc(&mut w, [0.0, 10.0, -40.0], [30.0, 40.0, 11.0]);
        boxc(&mut w, [0.0, -50.0, 0.0], [100.0, 0.5, 100.0]); // far floor, never reached
        let ch = player(&mut w, [0.0, 5.0, 0.0]);
        (w, ch)
    }

    fn fire(dir: [f32; 3]) -> PlayerInput {
        PlayerInput { grapple_aim: Some(vector![dir[0], dir[1], dir[2]]), ..Default::default() }
    }

    #[test]
    fn grapple_hooks_static_geometry_and_reels_the_player_in() {
        let (mut w, mut ch) = grapple_scene();
        let mut now = 0.0;
        tick(&mut ch, &mut w, &fire([0.0, 0.3, -1.0]), &mut now);
        let g = ch.grapple.expect("hooked");
        assert!(matches!(ch.grapple_event, GrappleEvent::Attached { .. }));
        assert!((g.anchor.z + 29.0).abs() < 0.01, "anchor on the wall face: {:?}", g.anchor);
        let idle = PlayerInput::default();
        let mut released = false;
        for _ in 0..240 {
            tick(&mut ch, &mut w, &idle, &mut now);
            if ch.grapple.is_none() {
                released = true;
                break;
            }
        }
        assert!(released, "arrives and releases");
        assert_eq!(ch.grapple_event, GrappleEvent::Released);
        let p = *w.bodies[ch.body].translation();
        assert!(p.z < -20.0, "was pulled most of the way to the wall: z = {}", p.z);
        // soft arrival (traversal round): the rope's end bleeds the speed instead of launching past the anchor
        assert!(ch.vel.norm() < 8.0, "soft arrival: speed {:?}", ch.vel);
    }

    #[test]
    fn grapple_ignores_players_and_out_of_range_and_too_close() {
        let (mut w, mut ch) = grapple_scene();
        let _other = player(&mut w, [0.0, 5.0, -8.0]); // a player in the line of fire is not hookable
        let mut now = 0.0;
        tick(&mut ch, &mut w, &fire([0.0, 0.0, -1.0]), &mut now);
        // the ray passes through the other capsule and hooks the wall behind it, never the player
        let g = ch.grapple.expect("wall behind the player is hooked");
        assert!(g.anchor.z < -28.0, "{:?}", g.anchor);

        let mut w2 = world();
        boxc(&mut w2, [0.0, 5.0, -1200.0], [30.0, 40.0, 1.0]); // beyond grapple_range (1000 m, 25x the old 40)
        let mut far = player(&mut w2, [0.0, 5.0, 0.0]);
        tick(&mut far, &mut w2, &fire([0.0, 0.0, -1.0]), &mut now);
        assert!(far.grapple.is_none(), "beyond grapple_range");

        let mut w3 = world();
        boxc(&mut w3, [0.0, 5.0, -2.0], [30.0, 40.0, 0.5]); // face 1.5 m away < min_len
        let mut near = player(&mut w3, [0.0, 5.0, 0.0]);
        tick(&mut near, &mut w3, &fire([0.0, 0.0, -1.0]), &mut now);
        assert!(near.grapple.is_none(), "closer than grapple_min_len");
    }

    #[test]
    fn grapple_releases_on_zip_or_jump_and_respects_cooldown() {
        for jump in [false, true] {
            let (mut w, mut ch) = grapple_scene();
            let mut now = 0.0;
            tick(&mut ch, &mut w, &fire([0.0, 0.2, -1.0]), &mut now);
            assert!(ch.grapple.is_some());
            for _ in 0..10 {
                tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
            }
            let vy_before = ch.vel.y;
            let input = if jump {
                PlayerInput { jump_pressed: true, jump_held: true, ..Default::default() }
            } else {
                fire([0.0, 0.2, -1.0])
            };
            tick(&mut ch, &mut w, &input, &mut now);
            assert!(ch.grapple.is_none(), "jump = {jump}");
            if jump {
                assert!(ch.vel.y > vy_before, "release hop");
            }
            // cooldown: an immediate re-fire does nothing
            tick(&mut ch, &mut w, &fire([0.0, 0.2, -1.0]), &mut now);
            assert!(ch.grapple.is_none(), "cooldown blocks refire");
            for _ in 0..40 {
                tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
            }
            tick(&mut ch, &mut w, &fire([0.0, 0.2, -1.0]), &mut now);
            assert!(ch.grapple.is_some(), "refire after cooldown");
        }
    }

    #[test]
    fn grapple_rope_is_cut_when_line_of_sight_breaks() {
        let (mut w, mut ch) = grapple_scene();
        let mut now = 0.0;
        tick(&mut ch, &mut w, &fire([0.0, 0.0, -1.0]), &mut now);
        assert!(ch.grapple.is_some());
        boxc(&mut w, [0.0, 5.0, -10.0], [10.0, 10.0, 0.5]); // slab drops between player and anchor
        tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        assert!(ch.grapple.is_none());
    }

    #[test]
    fn grapple_survives_a_dash_and_keeps_pulling_after() {
        let (mut w, mut ch) = grapple_scene();
        let mut now = 0.0;
        tick(&mut ch, &mut w, &fire([0.0, 0.2, -1.0]), &mut now);
        let dash = PlayerInput { dash_pressed: true, move_dir: vector![1.0, 0.0, 0.0], ..Default::default() };
        tick(&mut ch, &mut w, &dash, &mut now);
        for _ in 0..8 {
            tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        }
        assert!(ch.grapple.is_some(), "a dash leaves the rope slack, not cut");
        for _ in 0..20 {
            tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        }
        assert!(ch.vel.z < -5.0, "reel resumes after the dash: {:?}", ch.vel);
    }

    #[test]
    fn grapple_replay_from_a_snapshot_is_bit_exact() {
        let script = |i: u32| match i {
            0 => fire([0.2, 0.25, -1.0]),
            30 => PlayerInput { dash_pressed: true, move_dir: vector![-1.0, 0.0, 0.0], ..Default::default() },
            60 => PlayerInput { jump_pressed: true, jump_held: true, ..Default::default() },
            _ => PlayerInput { move_dir: vector![0.5, 0.0, -0.5], ..Default::default() },
        };
        let (mut w, mut ch) = grapple_scene();
        let mut now = 0.0;
        for i in 0..40 {
            tick(&mut ch, &mut w, &script(i), &mut now);
        }
        let snap = ch.snapshot(&w);
        let wire = crate::own_state::OwnState::from_snapshot(&snap);
        let mut a = Vec::new();
        for i in 40..120 {
            tick(&mut ch, &mut w, &script(i), &mut now);
            a.push((*w.bodies[ch.body].translation(), ch.vel));
        }
        // replay from the exact snapshot and from the compact wire blob: identical to the bit
        for use_wire in [false, true] {
            let (mut w2, mut ch2) = grapple_scene();
            if use_wire {
                ch2.restore_own(&mut w2, &wire);
            } else {
                ch2.restore(&mut w2, &snap);
            }
            let mut now2 = 0.0;
            for (k, i) in (40..120).enumerate() {
                tick(&mut ch2, &mut w2, &script(i), &mut now2);
                assert_eq!((*w2.bodies[ch2.body].translation(), ch2.vel), a[k], "wire = {use_wire}, tick {i}");
            }
        }
    }

    #[test]
    fn grapple_from_the_ground_lifts_off_and_reels() {
        let mut w = world();
        boxc(&mut w, [0.0, 10.0, -40.0], [30.0, 40.0, 11.0]);
        boxc(&mut w, [0.0, -0.5, 0.0], [100.0, 0.5, 100.0]); // floor, top at y = 0
        let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
        let mut now = 0.0;
        for _ in 0..30 {
            tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        }
        assert_eq!(ch.state, MoveState::Grounded);
        tick(&mut ch, &mut w, &fire([0.0, 0.1, -1.0]), &mut now);
        assert!(ch.grapple.is_some());
        for _ in 0..90 {
            tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        }
        let p = *w.bodies[ch.body].translation();
        assert!(p.z < -15.0 && p.y > 2.0, "lifted off and pulled: {p:?}");
    }

    // ---------- wrist grapple: edge cases ----------

    fn finite(ch: &CharacterSystem, w: &PhysicsWorld) -> bool {
        w.bodies[ch.body].translation().iter().all(|v| v.is_finite()) && ch.vel.iter().all(|v| v.is_finite())
    }

    #[test]
    fn firing_at_the_sky_or_a_player_reports_a_miss_and_throttles() {
        let mut w = world();
        boxc(&mut w, [0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
        let _other = player(&mut w, [0.0, 0.81, -8.0]);
        let mut now = 0.0;
        tick(&mut ch, &mut w, &fire([0.0, 1.0, 0.1]), &mut now); // straight up: sky
        assert!(ch.grapple.is_none());
        assert!(matches!(ch.grapple_event, GrappleEvent::Miss { .. }), "{:?}", ch.grapple_event);
        tick(&mut ch, &mut w, &fire([0.0, 1.0, 0.1]), &mut now); // spam: on the miss cooldown, no new event
        assert_eq!(ch.grapple_event, GrappleEvent::None);
        for _ in 0..12 {
            tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        }
        tick(&mut ch, &mut w, &fire([0.0, 0.0, -1.0]), &mut now); // only a player in the way: not hookable
        assert!(ch.grapple.is_none() && matches!(ch.grapple_event, GrappleEvent::Miss { .. }));
    }

    #[test]
    fn moving_geometry_is_not_hookable() {
        let mut w = world();
        boxc(&mut w, [0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        let body = w.bodies.insert(RigidBodyBuilder::kinematic_position_based().translation(vector![0.0, 3.0, -15.0]).build());
        w.colliders.insert_with_parent(ColliderBuilder::cuboid(3.0, 3.0, 0.5).build(), body, &mut w.bodies);
        let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
        let mut now = 0.0;
        tick(&mut ch, &mut w, &fire([0.0, 0.15, -1.0]), &mut now);
        assert!(ch.grapple.is_none(), "a kinematic obstacle must not take the hook");
    }

    #[test]
    fn rope_releases_cleanly_when_its_block_is_removed_mid_swing() {
        let mut w = world();
        let block = w.colliders.insert(ColliderBuilder::cuboid(1.0, 1.0, 1.0).translation(vector![0.0, 9.0, -20.0]).build());
        boxc(&mut w, [0.0, -50.0, 0.0], [100.0, 0.5, 100.0]);
        let mut ch = player(&mut w, [0.0, 5.0, 0.0]);
        let mut now = 0.0;
        tick(&mut ch, &mut w, &fire([0.0, 0.2, -1.0]), &mut now);
        assert!(ch.grapple.is_some(), "hooked the block");
        for _ in 0..10 {
            tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        }
        assert!(ch.grapple.is_some());
        w.colliders.remove(block, &mut w.islands, &mut w.bodies, false); // evicted / removed
        tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        assert!(ch.grapple.is_none(), "anchor gone: rope must release");
        assert_eq!(ch.grapple_event, GrappleEvent::Released);
        for _ in 0..60 {
            tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        }
        assert!(finite(&ch, &w));
    }

    #[test]
    fn hooking_at_a_box_corner_and_edge_is_stable() {
        for aim_at in [[2.0, 8.0, -20.0], [2.0, 6.0, -20.0], [1.99, 8.01, -19.99], [2.0, 7.0, -20.0]] {
            let mut w = world();
            boxc(&mut w, [0.0, 7.0, -21.0], [2.0, 1.0, 1.0]); // top face y = 8, front face z = -20, side x = 2
            boxc(&mut w, [0.0, -50.0, 0.0], [100.0, 0.5, 100.0]);
            let mut ch = player(&mut w, [0.0, 3.0, 0.0]);
            let mut now = 0.0;
            tick(&mut ch, &mut w, &fire([aim_at[0], aim_at[1] - 3.6, aim_at[2]]), &mut now);
            for i in 0..700 {
                tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
                assert!(finite(&ch, &w), "NaN at tick {i} for {aim_at:?}");
            }
            assert!(ch.grapple.is_none(), "rope must end (arrive / timeout) for {aim_at:?}");
        }
    }

    #[test]
    fn hooking_while_wall_running_leaves_the_wall_and_reels() {
        let (mut w, mut ch) = wall_scene();
        boxc(&mut w, [-20.0, 8.0, -30.0], [3.0, 8.0, 1.0]); // an anchor ahead and to the side
        let input = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], wall_cling: true, ..Default::default() };
        let mut now = 0.0;
        for _ in 0..30 {
            tick(&mut ch, &mut w, &input, &mut now);
        }
        assert!(matches!(ch.state, MoveState::WallRun { .. }));
        let mut f = fire([-20.0, 3.0, -24.0]);
        f.move_dir = input.move_dir;
        f.wall_cling = true;
        tick(&mut ch, &mut w, &f, &mut now);
        assert!(ch.grapple.is_some(), "hook from a wall-run");
        assert!(!matches!(ch.state, MoveState::WallRun { .. }), "the rope takes over from the wall-run");
        for i in 0..120 {
            tick(&mut ch, &mut w, &input, &mut now);
            assert!(finite(&ch, &w), "tick {i}");
        }
    }

    #[test]
    fn wall_jump_kicks_away_from_the_wall_and_up() {
        let (mut w, mut ch) = wall_scene(); // the wall is on +x
        let run = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], wall_cling: true, ..Default::default() };
        let mut now = 0.0;
        for _ in 0..20 {
            tick(&mut ch, &mut w, &run, &mut now);
        }
        assert!(matches!(ch.state, MoveState::WallRun { .. }));
        let mut jump = run;
        jump.jump_pressed = true;
        jump.jump_held = true;
        tick(&mut ch, &mut w, &jump, &mut now);
        assert!(ch.vel.x < -cfg_push() * 0.5, "pushed off the wall (-x), vel = {:?}", ch.vel);
        assert!(ch.vel.y > 2.0, "and up, vel = {:?}", ch.vel);
        assert!(ch.vel.z < -3.0, "keeping the run direction, vel = {:?}", ch.vel);
        assert!(!matches!(ch.state, MoveState::WallRun { .. }));
    }

    fn cfg_push() -> f32 {
        Cfg::default().wall_jump_push
    }

    #[test]
    fn chaining_wall_run_to_grapple_to_the_opposite_wall_keeps_moving_and_finite() {
        let mut w = world();
        boxc(&mut w, [1.5, 5.0, -40.0], [1.0, 10.0, 60.0]); // wall A on +x
        boxc(&mut w, [-14.0, 5.0, -40.0], [1.0, 10.0, 60.0]); // wall B on -x, 15 m away
        boxc(&mut w, [0.0, -50.0, 0.0], [100.0, 0.5, 100.0]);
        let mut ch = player(&mut w, [0.0, 5.0, 0.0]);
        ch.vel = vector![0.0, 0.0, -9.0];
        let run = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], wall_cling: true, ..Default::default() };
        let mut now = 0.0;
        for _ in 0..25 {
            tick(&mut ch, &mut w, &run, &mut now);
        }
        assert!(matches!(ch.state, MoveState::WallRun { .. }), "on wall A");
        // hook wall B ahead-left, reel across, then cling to B
        let mut f = fire([-12.0, 2.0, -14.0]);
        f.move_dir = run.move_dir;
        tick(&mut ch, &mut w, &f, &mut now);
        assert!(ch.grapple.is_some(), "hooked wall B");
        let mut on_b = false;
        for i in 0..240 {
            tick(&mut ch, &mut w, &run, &mut now);
            assert!(finite(&ch, &w), "tick {i}");
            if let MoveState::WallRun { normal, .. } = ch.state {
                if normal.x > 0.5 {
                    on_b = true; // B's face looks toward +x
                    break;
                }
            }
        }
        assert!(on_b, "should catch wall B after the pull, state {:?} grapple {:?}", ch.state, ch.grapple.is_some());
    }

    /// Grounded capsule pressed against a wall face at `gap` m: it must still slide along the wall.
    fn slide_along_wall(gap: f32, into_wall: f32, vy: f32) -> f32 {
        let mut w = world();
        boxc(&mut w, [0.0, -0.5, 0.0], [50.0, 0.5, 50.0]); // floor, top at y = 0
        boxc(&mut w, [3.0, 5.0, 0.0], [2.0, 5.0, 50.0]); // wall, face at x = 1
        let cfg = Cfg::default();
        let x = 1.0 - cfg.radius - gap;
        let mut ch = player(&mut w, [x, cfg.half_height + cfg.radius + 0.02, 0.0]);
        ch.vel.y = vy;
        let mut now = 0.0;
        settle(&mut ch, &mut w, &mut now);
        let z0 = w.bodies[ch.body].translation().z;
        let input = PlayerInput { move_dir: vector![into_wall, 0.0, -1.0].normalize(), ..Default::default() };
        for _ in 0..30 {
            tick(&mut ch, &mut w, &input, &mut now);
        }
        z0 - w.bodies[ch.body].translation().z
    }

    #[test]
    fn a_grounded_capsule_pressed_against_a_wall_still_slides_along_it() {
        let run = Cfg::default().run_speed;
        for gap in [-0.02, -0.01, -0.003, 0.0, 0.002, 0.005, 0.01, 0.015, 0.02, 0.05] {
            for into in [0.0, 0.2, 1.0, 3.0] {
                let d = slide_along_wall(gap, into, 0.0);
                let along = 1.0 / (1.0 + into * into as f32).sqrt(); // the move_dir's share along the wall
                assert!(d > run * along * 0.5 * 0.75, "gap {gap} into-wall {into}: moved only {d:.3} m along the wall in 0.5 s");
            }
        }
    }

    /// The same slide along a wall built from several boxes (seams at z = -2, -5 and a rolled-edge neighbour), grounded or
    /// falling, from a standing start and with the stick pushed into the wall.
    fn slide_along_seamed_wall(gap: f32, into: f32, airborne: bool) -> f32 {
        let mut w = world();
        boxc(&mut w, [0.0, -0.5, 0.0], [50.0, 0.5, 50.0]);
        for (i, zc) in [(0, 0.0f32), (1, -4.0), (2, -8.0), (3, -12.0), (4, -16.0)] {
            // alternating wall thickness so seams are not coplanar in depth, like a built district
            let half_x = if i % 2 == 0 { 2.0 } else { 1.5 };
            boxc(&mut w, [1.0 + half_x, 5.0, zc], [half_x, 5.0, 2.0]);
        }
        let cfg = Cfg::default();
        let x = 1.0 - cfg.radius - gap;
        let y = if airborne { 3.0 } else { cfg.half_height + cfg.radius + 0.02 };
        let mut ch = player(&mut w, [x, y, 1.0]);
        let mut now = 0.0;
        if !airborne {
            settle(&mut ch, &mut w, &mut now);
        }
        let z0 = w.bodies[ch.body].translation().z;
        let input = PlayerInput { move_dir: vector![into, 0.0, -1.0].normalize(), ..Default::default() };
        for _ in 0..90 {
            tick(&mut ch, &mut w, &input, &mut now);
        }
        z0 - w.bodies[ch.body].translation().z
    }

    #[test]
    fn sliding_along_a_wall_built_from_boxes_never_catches_on_the_seams() {
        let cfg = Cfg::default();
        for airborne in [false, true] {
            for gap in [0.0, 0.005, 0.01, 0.02] {
                for into in [0.0, 0.3, 1.0] {
                    let d = slide_along_seamed_wall(gap, into, airborne);
                    let along = 1.0 / (1.0 + into * into as f32).sqrt();
                    // 1.5 s of input: it must cover most of the 17 m run / a good share of the speed (air accel is slower)
                    let want = cfg.run_speed * along * 1.5 * if airborne { 0.35 } else { 0.8 };
                    assert!(d > want.min(14.0), "airborne {airborne} gap {gap} into {into}: moved {d:.2} m, want > {want:.2}");
                }
            }
        }
    }

    #[test]
    fn hooking_while_sliding_cancels_the_slide_and_lifts_off() {
        let mut w = world();
        boxc(&mut w, [0.0, 10.0, -40.0], [30.0, 40.0, 11.0]);
        boxc(&mut w, [0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
        let mut now = 0.0;
        let run = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], ..Default::default() };
        for _ in 0..40 {
            tick(&mut ch, &mut w, &run, &mut now);
        }
        tick(&mut ch, &mut w, &PlayerInput { slide: true, ..run }, &mut now);
        assert!(ch.sliding(), "sliding");
        let mut f = fire([0.0, 0.3, -1.0]);
        f.slide = true;
        f.move_dir = run.move_dir;
        tick(&mut ch, &mut w, &f, &mut now);
        assert!(ch.grapple.is_some() && !ch.sliding(), "slide cancelled by the hook");
        let mut peak = 0.0f32;
        for _ in 0..30 {
            tick(&mut ch, &mut w, &run, &mut now);
            peak = peak.max(w.bodies[ch.body].translation().y);
        }
        assert!(peak > 1.5, "left the floor even with slide held (peak y {peak})");
    }

    #[test]
    fn hooking_during_a_roll_or_dash_keeps_the_evasion_and_stays_finite() {
        for roll in [false, true] {
            let mut w = world();
            boxc(&mut w, [0.0, 10.0, -40.0], [30.0, 40.0, 11.0]);
            boxc(&mut w, [0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
            let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
            let mut now = 0.0;
            for _ in 0..5 {
                tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
            }
            let mut start = PlayerInput { move_dir: vector![1.0, 0.0, 0.0], dash_pressed: !roll, roll_pressed: roll, ..Default::default() };
            tick(&mut ch, &mut w, &start, &mut now);
            assert_eq!(ch.evading(), Some(roll));
            start = fire([0.0, 0.3, -1.0]);
            start.move_dir = vector![1.0, 0.0, 0.0];
            tick(&mut ch, &mut w, &start, &mut now);
            assert!(ch.grapple.is_some(), "hook during evasion (roll {roll})");
            assert!(ch.evading().is_some(), "evasion keeps going");
            for i in 0..200 {
                tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
                assert!(finite(&ch, &w), "tick {i}");
            }
        }
    }

    #[test]
    fn zip_spam_every_tick_never_panics_or_sticks() {
        let (mut w, mut ch) = grapple_scene();
        let mut now = 0.0;
        let mut attaches = 0;
        let mut ev = 0;
        for i in 0..900 {
            let dir = [((i % 7) as f32 - 3.0) * 0.1, 0.2 + (i % 5) as f32 * 0.05, -1.0];
            tick(&mut ch, &mut w, &fire(dir), &mut now);
            attaches += matches!(ch.grapple_event, GrappleEvent::Attached { .. }) as u32;
            ev += (ch.grapple_event != GrappleEvent::None) as u32;
            assert!(finite(&ch, &w), "tick {i}");
        }
        // cooldown 0.5 s after a release, 0.15 s after a miss: spam is rate-limited, not an event per tick
        assert!(attaches <= 15 * 4, "attach spam: {attaches}");
        assert!(ev < 600, "events per tick: {ev}");
    }

    #[test]
    fn clearing_the_rope_on_death_or_respawn_leaves_no_ghost_rope() {
        let (mut w, mut ch) = grapple_scene();
        let mut now = 0.0;
        tick(&mut ch, &mut w, &fire([0.0, 0.2, -1.0]), &mut now);
        assert!(ch.grapple.is_some());
        ch.clear_grapple();
        assert!(ch.grapple.is_none() && ch.grapple_cooldown() == 0.0);
        for _ in 0..5 {
            tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        }
        assert!(ch.grapple.is_none(), "must not reattach by itself");
        tick(&mut ch, &mut w, &fire([0.0, 0.2, -1.0]), &mut now);
        assert!(ch.grapple.is_some(), "no cooldown after a respawn");
    }

    #[test]
    fn corrupt_state_cannot_poison_the_character() {
        // anchor NaN / inf (hostile or corrupt restore) is released, never propagated
        let (mut w, mut ch) = grapple_scene();
        let mut now = 0.0;
        ch.grapple = Some(GrappleState { anchor: vector![f32::NAN, 0.0, 0.0], len: 5.0, time: 0.0 });
        tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        assert!(ch.grapple.is_none() && finite(&ch, &w));
        ch.grapple = Some(GrappleState { anchor: vector![f32::INFINITY, 0.0, 0.0], len: 5.0, time: 0.0 });
        tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        assert!(ch.grapple.is_none() && finite(&ch, &w));
    }

    // ---------- wrist grapple: swing / orbit ----------

    /// Hang a player on an anchor `len` away from the eye without going through the hook ray (swing tests).
    fn hang(w: &mut PhysicsWorld, eye: [f32; 3], anchor: [f32; 3], vel: [f32; 3]) -> CharacterSystem {
        let mut ch = player(w, [eye[0], eye[1] - 0.6, eye[2]]);
        let a = vector![anchor[0], anchor[1], anchor[2]];
        ch.vel = vector![vel[0], vel[1], vel[2]];
        ch.grapple = Some(GrappleState { anchor: a, len: (a - vector![eye[0], eye[1], eye[2]]).norm(), time: 0.0 });
        ch.state = MoveState::Airborne;
        ch
    }

    fn swing_scene() -> PhysicsWorld {
        let mut w = world();
        boxc(&mut w, [0.0, 41.0, 0.0], [60.0, 1.0, 60.0]); // ceiling whose underside (y = 40) holds the anchor
        boxc(&mut w, [0.0, -80.0, 0.0], [200.0, 0.5, 200.0]); // far floor
        w
    }

    #[test]
    fn a_held_rope_swings_like_a_pendulum_without_gaining_energy_or_stretching() {
        let mut w = swing_scene();
        let mut ch = hang(&mut w, [10.0, 30.0, 0.0], [0.0, 41.0 - 1.0, 0.0], [0.0, 0.0, 0.0]);
        let len = ch.grapple.unwrap().len;
        let hold = PlayerInput { slide: true, ..Default::default() };
        let mut now = 0.0;
        let (mut worst_stretch, mut max_y, mut prev_v, mut worst_turn) = (0.0f32, 0.0f32, ch.vel, 0.0f32);
        let a = ch.grapple.unwrap().anchor;
        for i in 0..540 {
            tick(&mut ch, &mut w, &hold, &mut now);
            let p = *w.bodies[ch.body].translation();
            let eye = vector![p.x, p.y + 0.6, p.z];
            worst_stretch = worst_stretch.max(((a - eye).norm() - len).abs());
            max_y = max_y.max(eye.y);
            assert!(finite(&ch, &w), "tick {i}");
            assert!(ch.grapple.is_some(), "rope must stay on (tick {i})");
            if ch.vel.norm() > 3.0 && prev_v.norm() > 3.0 {
                worst_turn = worst_turn.max(ch.vel.normalize().dot(&prev_v.normalize()).clamp(-1.0, 1.0).acos());
            }
            prev_v = ch.vel;
        }
        assert!(worst_stretch < 0.35, "rope length drifted by {worst_stretch} m");
        assert!(max_y < 30.0 + 0.4, "pendulum gained energy: rose to {max_y}");
        assert!(worst_turn < 0.25, "velocity direction jitter: {worst_turn} rad in one tick");
    }

    #[test]
    fn tangential_speed_on_a_held_rope_orbits_the_anchor() {
        // conical pendulum: v^2 = g r^2 / h
        let mut w = swing_scene();
        let (r, h) = (8.0f32, 14.0f32);
        let v = (Cfg::default().gravity * r * r / h).sqrt();
        let mut ch = hang(&mut w, [r, 40.0 - h, 0.0], [0.0, 40.0, 0.0], [0.0, 0.0, v]);
        let len = ch.grapple.unwrap().len;
        let hold = PlayerInput { slide: true, ..Default::default() };
        let mut now = 0.0;
        let a = ch.grapple.unwrap().anchor;
        let (mut rmin, mut rmax, mut ymin, mut ymax, mut smin, mut smax) = (f32::MAX, 0.0f32, f32::MAX, 0.0f32, f32::MAX, 0.0f32);
        let mut angle = 0.0f32;
        let mut last = (r, 0.0f32);
        for i in 0..540 {
            tick(&mut ch, &mut w, &hold, &mut now);
            let p = *w.bodies[ch.body].translation();
            assert!(finite(&ch, &w), "tick {i}");
            assert!(ch.grapple.is_some(), "rope must stay on (tick {i})");
            let hr = (p.x * p.x + p.z * p.z).sqrt();
            rmin = rmin.min(hr);
            rmax = rmax.max(hr);
            ymin = ymin.min(p.y + 0.6);
            ymax = ymax.max(p.y + 0.6);
            if i > 60 {
                smin = smin.min(ch.vel.norm());
                smax = smax.max(ch.vel.norm());
            }
            let cur = (p.x, p.z);
            angle += (last.0 * cur.1 - last.1 * cur.0).atan2(last.0 * cur.0 + last.1 * cur.1).abs();
            last = cur;
        }
        let _ = (a, len);
        assert!(angle > 1.6 * std::f32::consts::TAU, "it must keep circling: swept {angle} rad in 9 s");
        assert!(rmax - rmin < 3.0, "orbit radius wandered {rmin}..{rmax} (started at {r})");
        assert!(ymax - ymin < 4.0, "orbit height wandered {ymin}..{ymax}");
        assert!(smax - smin < 6.0, "orbit speed wandered {smin}..{smax} (started at {v})");
    }

    #[test]
    fn no_fall_damage_from_any_height() {
        // The movement core has no health at all and a landing only zeroes velocity: a long fall must end on the floor
        // with the character intact (the only death from height is the kill plane, handled in pvp).
        let mut w = world();
        boxc(&mut w, [0.0, -0.5, 0.0], [100.0, 0.5, 100.0]);
        let mut ch = player(&mut w, [0.0, 400.0, 0.0]);
        let mut now = 0.0;
        for _ in 0..1500 {
            tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        }
        assert_eq!(ch.state, MoveState::Grounded, "landed");
        let y = w.bodies[ch.body].translation().y;
        assert!((y - 0.81).abs() < 0.05, "standing on the floor, y = {y}");
        assert!(ch.vel.iter().all(|v| v.is_finite()));
    }

    // ---------- jump feel numbers (apex / air time / reach), see docs/movement.md ----------

    /// (apex height m, air time s, horizontal reach m) of a run-in jump with the stick held forward.
    /// `hold`: jump held through the rise; `double`: double jump at the apex.
    fn run_jump(cfg: Cfg, hold: bool, double: bool) -> (f32, f32, f32) {
        let mut w = world();
        floor(&mut w);
        let mut ch = player_cfg(&mut w, [0.0, 0.81, 0.0], cfg);
        let mut now = 0.0;
        settle(&mut ch, &mut w, &mut now);
        let fwd = vector![0.0, 0.0, -1.0];
        for _ in 0..40 {
            tick(&mut ch, &mut w, &PlayerInput { move_dir: fwd, ..Default::default() }, &mut now);
        }
        let (z0, y0) = (w.bodies[ch.body].translation().z, w.bodies[ch.body].translation().y);
        let (mut peak, mut air, mut dbl, mut prev_vy) = (0.0f32, 0u32, false, 0.0f32);
        for i in 0..400 {
            let at_apex = double && !dbl && i > 2 && ch.vel.y <= 0.5 && prev_vy > ch.vel.y;
            dbl |= at_apex;
            prev_vy = ch.vel.y;
            let held = hold && (ch.vel.y > 0.0 || i == 0 || dbl);
            let input = PlayerInput { move_dir: fwd, jump_pressed: i == 0 || at_apex, jump_held: held, ..Default::default() };
            tick(&mut ch, &mut w, &input, &mut now);
            peak = peak.max(w.bodies[ch.body].translation().y - y0);
            if i > 2 && ch.state == MoveState::Grounded {
                break;
            }
            air += 1;
        }
        (peak, air as f32 * DT, z0 - w.bodies[ch.body].translation().z)
    }

    /// Horizontal speed of a body (m/s).
    fn hspeed(ch: &CharacterSystem) -> f32 {
        flat(ch.vel).norm()
    }

    #[test]
    fn run_speed_is_the_faster_9_5_retune() {
        let cfg = Cfg::default();
        // "still too slow" round: run 7.2 -> 9.5 m/s (a player felt the base run was sluggish against a 19 m/s dash)
        assert!((9.0..10.0).contains(&cfg.run_speed), "run {}", cfg.run_speed);
        assert!(cfg.dash_speed < 20.0 && cfg.roll_speed < 13.0 && cfg.slide_boost < 14.0);
        assert!(cfg.roll_speed > cfg.run_speed && cfg.slide_boost > cfg.run_speed, "roll and slide must stay faster than a plain run");
        // wall-runs stay a notch above the run, but under a dash
        assert!(cfg.wall_speed > cfg.run_speed && cfg.wall_speed < 2.0 * cfg.dash_speed);
    }

    #[test]
    fn sprint_ten_metres_is_fast() {
        let mut w = world();
        floor(&mut w);
        let mut ch = player(&mut w, [0.0, 0.81, 0.0]);
        let mut now = 0.0;
        settle(&mut ch, &mut w, &mut now);
        let z0 = w.bodies[ch.body].translation().z;
        let fwd = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], ..Default::default() };
        let mut n = 0;
        while z0 - w.bodies[ch.body].translation().z < 10.0 && n < 600 {
            tick(&mut ch, &mut w, &fwd, &mut now);
            n += 1;
        }
        eprintln!("SPRINT10 {:.2} s", n as f32 * DT);
        assert!(n as f32 * DT < 1.6, "10 m sprint took {} s", n as f32 * DT);
    }

    #[test]
    fn variable_jump_tap_hops_hold_floats_and_a_double_jump_crosses_a_gap() {
        let cfg = Cfg::default();
        let (tap_h, tap_t, tap_d) = run_jump(cfg, false, false);
        let (hold_h, hold_t, hold_d) = run_jump(cfg, true, false);
        let (dbl_h, dbl_t, dbl_d) = run_jump(cfg, true, true);
        eprintln!("FEEL tap {tap_h:.2} m {tap_t:.2} s {tap_d:.1} m | hold {hold_h:.2} m {hold_t:.2} s {hold_d:.1} m | hold+double {dbl_h:.2} m {dbl_t:.2} s {dbl_d:.1} m");
        assert!((1.7..2.2).contains(&tap_h), "tap hop {tap_h} (an ordinary press must be a real jump: the first retune left it at the old ~1.5 m)");
        assert!((2.9..3.6).contains(&hold_h), "held jump {hold_h}");
        assert!((1.0..1.6).contains(&hold_t), "a held jump floats about a second: {hold_t}");
        assert!(hold_d >= 9.5, "a held run-in jump covers {hold_d} m");
        assert!((4.6..5.6).contains(&dbl_h), "double jump apex {dbl_h}");
        assert!(dbl_d >= 18.0, "jump + double jump crosses {dbl_d} m with two inputs");
        // the arc is a glide, not a moon jump: a double jump stays under ~2 s
        assert!(dbl_t < 2.4, "air time {dbl_t}");
        // cfg helpers agree with the simulation (bots plan with them)
        assert!((cfg.jump_apex(true) - hold_h).abs() < 0.15 && (cfg.jump_apex(false) - tap_h).abs() < 0.2);
    }

    #[test]
    fn a_jump_keeps_its_forward_carry_in_the_air_and_never_exceeds_the_cap() {
        let cfg = Cfg::default();
        let mut w = world();
        floor(&mut w);
        let mut ch = player_cfg(&mut w, [0.0, 0.81, 0.0], cfg);
        let mut now = 0.0;
        settle(&mut ch, &mut w, &mut now);
        let fwd = vector![0.0, 0.0, -1.0];
        for _ in 0..40 {
            tick(&mut ch, &mut w, &PlayerInput { move_dir: fwd, ..Default::default() }, &mut now);
        }
        tick(&mut ch, &mut w, &PlayerInput { move_dir: fwd, jump_pressed: true, jump_held: true, ..Default::default() }, &mut now);
        let mut peak_speed = 0.0f32;
        for _ in 0..20 {
            tick(&mut ch, &mut w, &PlayerInput { move_dir: fwd, jump_held: true, ..Default::default() }, &mut now);
            peak_speed = peak_speed.max(hspeed(&ch));
        }
        assert!(hspeed(&ch) > cfg.run_speed + 1.0, "carry survives 0.33 s of flight: {}", hspeed(&ch));
        assert!(peak_speed <= cfg.air_speed_cap + 1e-3, "cap {peak_speed}");
        // letting go of the stick in the air does not stop you dead
        for _ in 0..15 {
            tick(&mut ch, &mut w, &PlayerInput { jump_held: true, ..Default::default() }, &mut now);
        }
        assert!(hspeed(&ch) > 3.0, "momentum carries with no stick: {}", hspeed(&ch));
    }

    #[test]
    fn double_jump_redirects_momentum_to_the_stick() {
        let cfg = Cfg::default();
        let mut w = world();
        floor(&mut w);
        let mut ch = player_cfg(&mut w, [0.0, 0.81, 0.0], cfg);
        let mut now = 0.0;
        settle(&mut ch, &mut w, &mut now);
        let fwd = vector![0.0, 0.0, -1.0];
        for _ in 0..40 {
            tick(&mut ch, &mut w, &PlayerInput { move_dir: fwd, ..Default::default() }, &mut now);
        }
        tick(&mut ch, &mut w, &PlayerInput { move_dir: fwd, jump_pressed: true, jump_held: true, ..Default::default() }, &mut now);
        let right = vector![1.0, 0.0, 0.0];
        let mut done = false;
        for _ in 0..120 {
            let at_apex = !done && ch.vel.y.abs() < 1.5 && ch.vel.y < 1.0;
            done |= at_apex;
            tick(&mut ch, &mut w, &PlayerInput { move_dir: right, jump_pressed: at_apex, jump_held: true, ..Default::default() }, &mut now);
            if at_apex {
                assert!(ch.double_jumped);
                assert!(ch.vel.x > 0.8 * hspeed(&ch), "momentum turned to the stick: {:?}", ch.vel);
                assert!(hspeed(&ch) >= cfg.run_speed, "and keeps at least a run: {}", hspeed(&ch));
                break;
            }
        }
        assert!(done, "double jump fired");
    }

    #[test]
    fn wall_run_exit_keeps_the_run_speed_along_the_wall() {
        let cfg = Cfg::default();
        let (mut w, mut ch) = wall_scene(); // wall on +x, runner heads -z
        ch.vel = vector![0.0, 0.0, -cfg.run_speed - 1.0];
        let run = PlayerInput { move_dir: vector![0.0, 0.0, -1.0], wall_cling: true, jump_held: true, ..Default::default() };
        let mut now = 0.0;
        for _ in 0..30 {
            tick(&mut ch, &mut w, &run, &mut now);
        }
        assert!(matches!(ch.state, MoveState::WallRun { .. }));
        tick(&mut ch, &mut w, &PlayerInput { jump_pressed: true, jump_held: true, ..run }, &mut now);
        assert!(!matches!(ch.state, MoveState::WallRun { .. }) && ch.vel.y > 0.0);
        assert!(-ch.vel.z >= cfg.wall_speed * 0.95, "wall-jump keeps the run along the wall: {:?}", ch.vel);
        assert!(ch.vel.x < -cfg.wall_jump_push * 0.9, "and kicks off the wall: {:?}", ch.vel);
    }

    /// Ride the rope to the end with `input` held every tick; (peak speed on the rope, horizontal speed at release, 0.4 s later).
    fn ride_rope(input: PlayerInput) -> (f32, f32, f32, MoveState) {
        let (mut w, mut ch) = grapple_scene();
        let mut now = 0.0;
        tick(&mut ch, &mut w, &fire([0.0, 0.3, -1.0]), &mut now);
        assert!(ch.grapple.is_some());
        let mut best = 0.0f32;
        while ch.grapple.is_some() {
            tick(&mut ch, &mut w, &input, &mut now);
            best = best.max(ch.vel.norm());
            assert!(now < 8.0, "rope never ended");
        }
        let h_release = hspeed(&ch);
        for _ in 0..24 {
            tick(&mut ch, &mut w, &input, &mut now);
        }
        (best, h_release, hspeed(&ch), ch.state)
    }

    #[test]
    fn grapple_arrival_bleeds_the_speed_unless_the_player_launches() {
        let cfg = Cfg::default();
        // idle: the rope's end is a soft landing, not a launch past the anchor
        let (best, h_release, h_later, _) = ride_rope(PlayerInput::default());
        eprintln!("FEEL grapple arrival (idle): peak {best:.1} m/s, h at release {h_release:.1}, 0.4 s later {h_later:.1}");
        assert!(best <= cfg.grapple_far_speed + 2.5, "rope speed {best}");
        assert!(h_release <= best * (cfg.arrive_speed_keep + 0.2), "arrival bleeds: {best:.1} -> {h_release:.1}");
        // jump held = a deliberate launch: the momentum is theirs (or it chains into a wall-run on the face)
        let (best, h_release, h_later, state) = ride_rope(PlayerInput { jump_held: true, ..Default::default() });
        eprintln!("FEEL grapple arrival (jump held): peak {best:.1} m/s, h at release {h_release:.1}, 0.4 s later {h_later:.1}, {state:?}");
        assert!(matches!(state, MoveState::WallRun { .. }) || h_release >= 0.5 * best, "jump held keeps the momentum: {best:.1} -> {h_release:.1}");
    }

    #[test]
    fn the_hook_key_on_a_rope_is_a_brake() {
        let cfg = Cfg::default();
        let (mut w, mut ch) = grapple_scene();
        let mut now = 0.0;
        tick(&mut ch, &mut w, &fire([0.0, 0.3, -1.0]), &mut now);
        for _ in 0..40 {
            tick(&mut ch, &mut w, &PlayerInput::default(), &mut now);
        }
        let before = ch.vel.norm();
        assert!(before > 15.0 && ch.grapple.is_some());
        tick(&mut ch, &mut w, &fire([0.0, 0.3, -1.0]), &mut now); // hook key again: cancel
        assert!(ch.grapple.is_none());
        eprintln!("hook key brake: {before:.1} m/s -> {:.1} m/s", ch.vel.norm());
        assert!(ch.vel.norm() < before * (cfg.cancel_keep + 0.15), "cancel kills most of the speed");
    }

    #[test]
    fn falling_glides_only_while_jump_is_held_and_free_fall_keeps_its_weight() {
        let cfg = Cfg::default();
        let fall = |held: bool| {
            let mut w = world();
            boxc(&mut w, [0.0, -50.0, 0.0], [100.0, 0.5, 100.0]);
            let mut ch = player_cfg(&mut w, [0.0, 20.0, 0.0], cfg);
            let mut now = 0.0;
            for _ in 0..60 {
                tick(&mut ch, &mut w, &PlayerInput { jump_held: held, ..Default::default() }, &mut now);
            }
            -ch.vel.y
        };
        let (free, glide) = (fall(false), fall(true));
        assert!(glide < free * 0.8, "holding jump glides down: {glide} vs {free}");
        assert!(free <= cfg.terminal + 1e-3 && free > 12.0, "a free fall still has weight: {free}");
    }
}
