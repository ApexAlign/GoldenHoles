//! Static level description shared by the authoritative server and the client predictor, so both build the
//! exact same colliders in the exact same order. Primitive cuboids only (never render meshes).

use std::collections::BTreeMap;

use crate::character::PhysicsWorld;
use crate::combat::GROUP_WORLD;
use crate::rapier3d::prelude::*;
use crate::tick::Server;

/// An axis-aligned cuboid, optionally rolled about Z (for ramps).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LevelBox {
    pub center: [f32; 3],
    pub half: [f32; 3],
    /// Radians about the Z axis; positive tilts +X up.
    pub rot_z: f32,
    /// Hit points of a destructible box (`dbox` line); 0 = permanent (`breaks.rs`).
    pub hp: f32,
}

impl LevelBox {
    pub fn new(center: [f32; 3], half: [f32; 3]) -> Self {
        Self { center, half, rot_z: 0.0, hp: 0.0 }
    }

    pub fn destructible(mut self, hp: f32) -> Self {
        self.hp = hp;
        self
    }

    pub fn rolled(mut self, rot_z: f32) -> Self {
        self.rot_z = rot_z;
        self
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct LevelDesc {
    pub boxes: Vec<LevelBox>,
    /// Round-robin spawn points (capsule centre).
    pub spawns: Vec<[f32; 3]>,
}

/// A level of floating structures over the void (no floor, nothing reaching down near y = 0): the renderer draws no
/// ground for it and bots plan on its roofs. True when there are boxes and the lowest face is more than 4 m up.
pub fn is_floating(l: &LevelDesc) -> bool {
    !l.boxes.is_empty() && l.boxes.iter().all(|b| b.center[1] - b.half[1] > 4.0)
}

/// Decoration landmarks of the floating city (`scripts/gen_sky_level.py` emits them as ordinary boxes; the renderer
/// recognises them by these exact half extents and dresses them: torii beams between post pairs, lantern caps on pylons).
/// Both are full walls (>= `WALL_RISE` tall), so they never make a jump-only obstacle.
pub const TORII_POST_HALF: [f32; 3] = [0.4, 2.75, 0.4];
pub const PYLON_HALF: [f32; 3] = [0.35, 1.8, 0.35];

/// Is `b` (within 1 cm) a box of exactly half extents `h`?
pub fn is_landmark(b: &LevelBox, h: [f32; 3]) -> bool {
    b.rot_z == 0.0 && (0..3).all(|i| (b.half[i] - h[i]).abs() < 0.01)
}

/// A rise (m) at or above which an obstacle counts as a full wall: nobody is expected to hop it (a jump + double jump
/// reaches ~2.8 m, so anything under this is a "jump over it" block).
pub const WALL_RISE: f32 = 3.0;

/// Height of box `b`'s top face above the world at (x, z), or `None` outside its footprint. Rolled boxes (ramps) give
/// the slope surface.
fn box_top_at(b: &LevelBox, x: f32, z: f32) -> Option<f32> {
    if (z - b.center[2]).abs() > b.half[2] {
        return None;
    }
    let (s, c) = b.rot_z.sin_cos();
    let u = (x - b.center[0] + b.half[1] * s) / c;
    (u.abs() <= b.half[0]).then(|| b.center[1] + u * s + b.half[1] * c)
}

/// Indices of boxes a walking player cannot get onto or past without jumping. A (non-ramp) box is an offender when it
/// is taller than `step` (the character's autostep, `Cfg::step_height`) but lower than a full wall (`WALL_RISE`) as seen
/// from the ground beside it, and no side offers a way up (a stair / lip no higher than `step`, or a ramp whose top
/// meets it). Ground is sampled every metre round the footprint, 0.45 m out, using only surfaces within the box's own
/// height span, so floating buildings, roofs seen from below and open void never count.
pub fn unwalkable_obstacles(l: &LevelDesc, step: f32) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, b) in l.boxes.iter().enumerate() {
        if b.rot_z != 0.0 {
            continue; // ramps are the way up
        }
        let (top, bot) = (b.center[1] + b.half[1], b.center[1] - b.half[1]);
        let (cx, cz, hx, hz) = (b.center[0], b.center[2], b.half[0], b.half[2]);
        let mut samples: Vec<(f32, f32)> = Vec::new();
        let n = |h: f32| (2.0 * h).ceil().max(1.0) as i32;
        for k in 0..=n(hx) {
            let x = cx - hx + 2.0 * hx * k as f32 / n(hx) as f32;
            samples.push((x, cz - hz - 0.45));
            samples.push((x, cz + hz + 0.45));
        }
        for k in 0..=n(hz) {
            let z = cz - hz + 2.0 * hz * k as f32 / n(hz) as f32;
            samples.push((cx - hx - 0.45, z));
            samples.push((cx + hx + 0.45, z));
        }
        let (mut any, mut easy, mut min_rise) = (false, false, f32::MAX);
        for (x, z) in samples {
            let ground = l
                .boxes
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .filter_map(|(_, o)| box_top_at(o, x, z))
                .filter(|g| *g < top - 1e-3 && *g > bot - 0.2)
                .fold(f32::MIN, f32::max);
            if ground == f32::MIN {
                continue;
            }
            any = true;
            let rise = top - ground;
            min_rise = min_rise.min(rise);
            easy |= rise <= step + 0.01;
        }
        if any && !easy && min_rise < WALL_RISE {
            out.push(i);
        }
    }
    out
}

/// Where the shipped free-for-all map lives relative to the repo / install root (the demo's default level).
pub const DEFAULT_LEVEL: &str = "levels/ffa_small.lvl";
/// Fallback when `DEFAULT_LEVEL` is not installed.
pub const FALLBACK_LEVEL: &str = "levels/arena.lvl";

/// The default level file when one exists: `levels/ffa_small.lvl` (else `levels/arena.lvl`; the big `levels/ffa.lvl` stays selectable with `--level`) from the working directory, then upward from the
/// executable (so `target/release/shinobi-server` finds the repo's `levels/` however it was launched). Server and
/// game must use this same rule, or the client predicts against a different world than the server runs.
/// `None` means "use `LevelDesc::standard()`".
pub fn default_level_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok();
    find_level(std::path::Path::new(DEFAULT_LEVEL), exe.as_deref()).or_else(|| find_level(std::path::Path::new(FALLBACK_LEVEL), exe.as_deref()))
}

/// `rel` against the working directory, then against the first five ancestors of `exe`. Testable core of
/// `default_level_path`.
pub fn find_level(rel: &std::path::Path, exe: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    if rel.is_file() {
        return Some(rel.to_path_buf());
    }
    exe?.ancestors().skip(1).take(5).map(|d| d.join(rel)).find(|p| p.is_file())
}

/// The one place a static box becomes a collider.
pub fn insert_static_box(w: &mut PhysicsWorld, b: &LevelBox) {
    insert_level_box(w, b, None);
}

/// `insert_static_box` returning the handle; a destructible box (`hp > 0`) with its index carries the piece tag with the
/// reserved world cell, so weapons report hits on it (`breaks.rs`).
pub fn insert_level_box(w: &mut PhysicsWorld, b: &LevelBox, idx: Option<u16>) -> ColliderHandle {
    let mut c = ColliderBuilder::cuboid(b.half[0], b.half[1], b.half[2])
        .position(Isometry::new(vector![b.center[0], b.center[1], b.center[2]], vector![0.0, 0.0, b.rot_z]))
        .collision_groups(InteractionGroups::new(GROUP_WORLD, Group::ALL));
    if let (true, Some(i)) = (b.hp > 0.0, idx) {
        c = c.user_data(crate::breaks::world_user_data(i));
    }
    w.colliders.insert(c.build())
}

/// A pickup point from a level file (server-only: the client never needs it to predict movement).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PickupPoint {
    pub pos: [f32; 3],
    pub respawn_secs: f32,
    /// `PowerKind::None` = a data cube (`pickup`); `yumi` / `tag` lines are power-weapon pedestals (docs/powerups.md).
    pub kind: crate::power::PowerKind,
}

/// Additive level behaviour data (`zone`, `mover`, `hazard`). Kept off `LevelDesc` so the static geometry shape and
/// its existing literals stay untouched.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct LevelFeatures {
    pub zones: Vec<ZoneDef>,
    pub movers: Vec<MoverDef>,
    pub hazards: Vec<HazardDef>,
    /// `tube` / `ring` lines: static round geometry built from yawed segments (`ShapeDef::segments`).
    pub shapes: Vec<ShapeDef>,
}

/// Round static geometry. `Tube`: a hollow vertical cylinder whose inner wall is at `radius` (wall-runnable inside and
/// out). `Ring`: a flat horizontal annulus whose centre line is at `radius` with radial `width`. Both are built from
/// `segments` straight cuboids rotated about Y, in a fixed order, so server, client and renderer agree exactly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShapeKind {
    Tube,
    Ring,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ShapeDef {
    pub kind: ShapeKind,
    pub center: [f32; 3],
    pub radius: f32,
    /// Tube: wall height. Ring: radial width.
    pub extent: f32,
    pub thickness: f32,
    pub segments: u32,
    pub tag: String,
    pub tuning: BTreeMap<String, f32>,
}

/// One straight piece of a `ShapeDef`: a cuboid at `center` with `half` extents in its local frame, rotated by `yaw`
/// radians about +Y (local +X points radially outward).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ShapeSegment {
    pub center: [f32; 3],
    pub half: [f32; 3],
    pub yaw: f32,
}

pub const MAX_SHAPE_SEGMENTS: u32 = 256;

impl ShapeDef {
    pub fn token(&self) -> &'static str {
        match self.kind {
            ShapeKind::Tube => "tube",
            ShapeKind::Ring => "ring",
        }
    }

    pub fn segments(&self) -> Vec<ShapeSegment> {
        let n = self.segments.clamp(3, MAX_SHAPE_SEGMENTS);
        let step = 2.0 * core::f32::consts::PI / n as f32;
        let tan_half = libm::tanf(step * 0.5);
        let (mid, outer, half_y, half_x) = match self.kind {
            ShapeKind::Tube => (self.radius + self.thickness * 0.5, self.radius + self.thickness, self.extent * 0.5, self.thickness * 0.5),
            ShapeKind::Ring => (self.radius, self.radius + self.extent * 0.5, self.thickness * 0.5, self.extent * 0.5),
        };
        // a door: `door_count` segments from `door_start` (tuning) are left out, e.g. a tube entry at one layer
        let door_start = self.tuning.get("door_start").map_or(0, |v| v.max(0.0) as u32);
        let door_count = self.tuning.get("door_count").map_or(0, |v| v.max(0.0) as u32);
        let in_door = |i: u32| door_count > 0 && (i + n - door_start % n) % n < door_count;
        (0..n)
            .filter(|i| !in_door(*i))
            .map(|i| {
                let a = step * i as f32;
                ShapeSegment {
                    center: [self.center[0] + mid * libm::cosf(a), self.center[1], self.center[2] + mid * libm::sinf(a)],
                    half: [half_x, half_y, outer * tan_half],
                    yaw: -a,
                }
            })
            .collect()
    }
}

impl LevelFeatures {
    pub fn is_empty(&self) -> bool {
        self.zones.is_empty() && self.movers.is_empty() && self.hazards.is_empty() && self.shapes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.zones.len() + self.movers.len() + self.hazards.len() + self.shapes.len()
    }

    /// Stable fingerprint of every feature field: zones, movers and hazards in file order, each with its tag and
    /// tuning entries (`BTreeMap` order is sorted, so it is deterministic). Pickups are server-only and excluded.
    pub fn fingerprint(&self) -> u64 {
        use crate::hash::{mix, mix_f32, mix_u64, OFFSET};
        let mut h = mix_u64(OFFSET, self.zones.len() as u64);
        for z in &self.zones {
            for v in z.center.iter().chain(z.half.iter()) {
                h = mix_f32(h, *v);
            }
            h = mix(h, z.tag.as_bytes());
            h = mix_u64(h, z.tuning.len() as u64);
            for (k, v) in &z.tuning {
                h = mix(h, k.as_bytes());
                h = mix_f32(h, *v);
            }
        }
        h = mix_u64(h, self.movers.len() as u64);
        for m in &self.movers {
            for v in m.center.iter().chain(m.half.iter()) {
                h = mix_f32(h, *v);
            }
            h = mix_u64(h, m.axis.index() as u64);
            h = mix_f32(h, m.distance);
            h = mix_u64(h, m.period_ticks as u64);
            h = mix_u64(h, m.solid as u64);
            h = mix(h, m.tag.as_bytes());
            h = mix_u64(h, m.tuning.len() as u64);
            for (k, v) in &m.tuning {
                h = mix(h, k.as_bytes());
                h = mix_f32(h, *v);
            }
        }
        h = mix_u64(h, self.hazards.len() as u64);
        for hz in &self.hazards {
            for v in hz.center.iter().chain(hz.half.iter()) {
                h = mix_f32(h, *v);
            }
            h = mix_u64(h, hz.period_ticks as u64);
            h = mix_u64(h, hz.telegraph_ticks as u64);
            h = mix_f32(h, hz.damage);
            h = mix(h, hz.tag.as_bytes());
            h = mix_u64(h, hz.tuning.len() as u64);
            for (k, v) in &hz.tuning {
                h = mix(h, k.as_bytes());
                h = mix_f32(h, *v);
            }
        }
        // shapes only enter the hash when present, so files without them keep their pinned fingerprints
        if !self.shapes.is_empty() {
            h = mix_u64(h, self.shapes.len() as u64);
            for sh in &self.shapes {
                h = mix_u64(h, sh.kind as u64);
                for v in sh.center.iter().chain([sh.radius, sh.extent, sh.thickness].iter()) {
                    h = mix_f32(h, *v);
                }
                h = mix_u64(h, sh.segments as u64);
                h = mix(h, sh.tag.as_bytes());
                h = mix_u64(h, sh.tuning.len() as u64);
                for (k, v) in &sh.tuning {
                    h = mix(h, k.as_bytes());
                    h = mix_f32(h, *v);
                }
            }
        }
        h
    }
}

/// Axis-aligned trigger volume. `tag` names its gameplay role; numeric `tuning` is free-form
/// (`cooldown_ticks` is read by the runtime).
#[derive(Clone, PartialEq, Debug)]
pub struct ZoneDef {
    pub center: [f32; 3],
    pub half: [f32; 3],
    pub tag: String,
    pub tuning: BTreeMap<String, f32>,
}

/// A `speed_lane` zone's wall-run tuning, resolved for one player position. `inside` distinguishes the authored
/// AABB (boost) from the short post-exit band along the same tagged wall face (decay only).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SpeedLane {
    pub wall_run_m: f32,
    pub mult: f32,
    pub cap: f32,
    pub decay: f32,
    pub inside: bool,
    /// 0 at the nearest authored lane end, 1 after `wall_run_m` metres inward.
    pub ramp: f32,
}

impl SpeedLane {
    pub const TAG: &'static str = "speed_lane";

    /// Wall-run speed multiplier for this tick: `1 + (mult - 1) * ramp`, hard-clamped to `cap`. Unit outside the
    /// authored AABB (the post-exit band only decays carried speed).
    pub fn multiplier(&self) -> f32 {
        if !self.inside {
            return 1.0;
        }
        let m = 1.0 + (self.mult.max(1.0) - 1.0) * self.ramp.clamp(0.0, 1.0);
        m.clamp(1.0, self.cap.max(1.0))
    }

    fn from_zone(z: &ZoneDef, inside: bool, pos: [f32; 3]) -> Self {
        let wall_run_m = tuning_or(z, "wall_run_m", 0.0).max(0.0);
        let (long, _) = lane_axes(z);
        let lo = z.center[long] - z.half[long];
        let hi = z.center[long] + z.half[long];
        let ramp = if wall_run_m <= 0.0 {
            1.0
        } else {
            ((pos[long] - lo).min(hi - pos[long]) / wall_run_m).clamp(0.0, 1.0)
        };
        Self {
            wall_run_m,
            mult: tuning_or(z, "mult", 1.0),
            cap: tuning_or(z, "cap", 1.0),
            decay: tuning_or(z, "decay", 0.0).clamp(0.0, 1.0),
            inside,
            ramp,
        }
    }
}

fn tuning_or(z: &ZoneDef, key: &str, default: f32) -> f32 {
    z.tuning.get(key).copied().unwrap_or(default)
}

/// Horizontal axes of a `speed_lane` volume: (long axis along the wall face, thin axis across it).
fn lane_axes(z: &ZoneDef) -> (usize, usize) {
    if z.half[0] > z.half[2] { (0, 2) } else { (2, 0) }
}

/// A wall normal counts as the tagged face when it points mostly across the lane's thin axis and from the wall
/// toward the lane interior (the player centre sits between the wall and the lane middle).
fn lane_face_matches(z: &ZoneDef, pos: [f32; 3], normal: [f32; 3]) -> bool {
    let (_, thin) = lane_axes(z);
    normal[thin].abs() >= 0.7 && normal[thin] * (pos[thin] - z.center[thin]) <= 0.0
}

impl LevelFeatures {
    /// Resolve the `speed_lane` effect for a player centre and wall-run normal. This is the one query both the
    /// authoritative server and the client predictor call, so no per-map branching or wire state is needed.
    ///
    /// The exact AABB (`inside == true`) drives the boost. Up to `wall_run_m` metres beyond either long-axis end,
    /// on the same tagged wall face (`inside == false`), the returned tuning drives the post-exit speed decay.
    /// `None` while not on a `speed_lane` wall face: walking, airborne and unrelated walls never match.
    pub fn speed_lane_for(&self, pos: [f32; 3], wall_normal: Option<[f32; 3]>) -> Option<SpeedLane> {
        let n = wall_normal?;
        for z in self.zones.iter().filter(|z| z.tag == SpeedLane::TAG) {
            if !lane_face_matches(z, pos, n) {
                continue;
            }
            let (long, thin) = lane_axes(z);
            let lo = z.center[long] - z.half[long];
            let hi = z.center[long] + z.half[long];
            let within_other = pos[thin] >= z.center[thin] - z.half[thin]
                && pos[thin] <= z.center[thin] + z.half[thin]
                && pos[1] >= z.center[1] - z.half[1]
                && pos[1] <= z.center[1] + z.half[1];
            if within_other && pos[long] >= lo && pos[long] <= hi {
                return Some(SpeedLane::from_zone(z, true, pos));
            }
        }
        for z in self.zones.iter().filter(|z| z.tag == SpeedLane::TAG) {
            if !lane_face_matches(z, pos, n) {
                continue;
            }
            let (long, thin) = lane_axes(z);
            let lo = z.center[long] - z.half[long];
            let hi = z.center[long] + z.half[long];
            let within_other = pos[thin] >= z.center[thin] - z.half[thin]
                && pos[thin] <= z.center[thin] + z.half[thin]
                && pos[1] >= z.center[1] - z.half[1]
                && pos[1] <= z.center[1] + z.half[1];
            if !within_other {
                continue;
            }
            let out = if pos[long] < lo { lo - pos[long] } else if pos[long] > hi { pos[long] - hi } else { 0.0 };
            if out > 0.0 && out <= tuning_or(z, "wall_run_m", 0.0).max(0.0) {
                return Some(SpeedLane::from_zone(z, false, pos));
            }
        }
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MoveAxis {
    X,
    Y,
    Z,
}

impl MoveAxis {
    pub fn index(self) -> usize {
        match self {
            MoveAxis::X => 0,
            MoveAxis::Y => 1,
            MoveAxis::Z => 2,
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            MoveAxis::X => "x",
            MoveAxis::Y => "y",
            MoveAxis::Z => "z",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "x" | "X" => Some(MoveAxis::X),
            "y" | "Y" => Some(MoveAxis::Y),
            "z" | "Z" => Some(MoveAxis::Z),
            _ => None,
        }
    }
}

/// Deterministic ping-pong mover. One leg takes `period_ticks`; `phase_ticks` (optional tuning) offsets the cycle.
#[derive(Clone, PartialEq, Debug)]
pub struct MoverDef {
    pub center: [f32; 3],
    pub half: [f32; 3],
    pub axis: MoveAxis,
    pub distance: f32,
    pub period_ticks: u32,
    pub solid: bool,
    pub tag: String,
    pub tuning: BTreeMap<String, f32>,
}

/// Periodic damage volume. Phases are tick-exact: `active_ticks` (optional tuning, default 1) at the start of every
/// `period_ticks` cycle, with the last `telegraph_ticks` ticks of the cycle as warning. `phase_ticks` shifts the cycle.
#[derive(Clone, PartialEq, Debug)]
pub struct HazardDef {
    pub center: [f32; 3],
    pub half: [f32; 3],
    pub period_ticks: u32,
    pub telegraph_ticks: u32,
    pub damage: f32,
    pub tag: String,
    pub tuning: BTreeMap<String, f32>,
}

/// Everything a level file says. `LevelDesc` is the part both sides need.
#[derive(Clone, PartialEq, Debug)]
pub struct LevelFile {
    pub level: LevelDesc,
    pub pickups: Vec<PickupPoint>,
    pub features: LevelFeatures,
}

pub const DEFAULT_PICKUP_RESPAWN_SECS: f32 = 20.0;
pub const DEFAULT_WEAPON_RESPAWN_SECS: f32 = 45.0;
/// Level hazards report through `HazardSource::Obstacle` ids starting at `0xE000`; 0x1000 leaves the reserved
/// high ids (`VOID`, `SPLAT`, `LIGHTNING`) and the obstacle-wave range clear.
pub const MAX_LEVEL_HAZARDS: usize = 0x1000;

/// Parse a level file: one directive per line, `#` starts a comment.
/// ```text
/// box    cx cy cz  hx hy hz [rot_z_radians]   # static cuboid (centre, half extents), optional roll for ramps
/// dbox   cx cy cz  hx hy hz rot_z hp           # DESTRUCTIBLE cuboid: like `box` (rot_z always given, may be 0) plus hit points
/// spawn  x y z [team]                          # capsule-centre spawn point; team 0 Cyan / 1 Orange, omitted = either
/// pickup x y z [respawn_secs]                  # power-up cube, default respawn 20 s
/// yumi   x y z [respawn_secs]                  # power-weapon pedestal: digital bow (default respawn 45 s)
/// tag    x y z [respawn_secs]                  # power-weapon pedestal: explosive fuda tags (default respawn 45 s)
/// zone   cx cy cz hx hy hz tag [key=value...]  # trigger volume; `cooldown_ticks` gates re-entry
/// mover  cx cy cz hx hy hz axis distance period_ticks solid|ghost tag [key=value...]
/// hazard cx cy cz hx hy hz period_ticks telegraph_ticks damage tag [key=value...]
/// ```
/// Feature `key=value` values are finite f32; `active_ticks` and `phase_ticks` (hazard) and `phase_ticks` (mover)
/// are whole tick counts. Errors name the line. A level needs at least one `box` and one `spawn`.
pub fn parse_level_file(text: &str) -> Result<LevelFile, String> {
    let (mut boxes, mut spawns, mut pickups) = (Vec::new(), Vec::new(), Vec::new());
    let mut features = LevelFeatures::default();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let err = |m: &str| format!("line {}: {m}: `{line}`", n + 1);
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let kind = tokens[0];
        match kind {
            "box" | "dbox" | "spawn" | "pickup" | "yumi" | "tag" => {
                let nums = parse_numbers(&tokens[1..], &err)?;
                match (kind, nums.len()) {
                    ("box", 6 | 7) => {
                        if nums[3..6].iter().any(|h| *h <= 0.0) {
                            return Err(err("half extents must be > 0"));
                        }
                        let b = LevelBox::new([nums[0], nums[1], nums[2]], [nums[3], nums[4], nums[5]]);
                        boxes.push(if nums.len() == 7 { b.rolled(nums[6]) } else { b });
                    }
                    ("dbox", 8) => {
                        if nums[3..6].iter().any(|h| *h <= 0.0) {
                            return Err(err("half extents must be > 0"));
                        }
                        if nums[7] <= 0.0 {
                            return Err(err("hp must be > 0 (use `box` for permanent geometry)"));
                        }
                        boxes.push(LevelBox::new([nums[0], nums[1], nums[2]], [nums[3], nums[4], nums[5]]).rolled(nums[6]).destructible(nums[7]));
                    }
                    ("spawn", 3 | 4) => {
                        if nums.len() == 4 && !(nums[3] == 0.0 || nums[3] == 1.0) {
                            return Err(err("spawn team must be 0 (Cyan) or 1 (Orange)"));
                        }
                        spawns.push([nums[0], nums[1], nums[2]]);
                    }
                    ("pickup" | "yumi" | "tag", 3 | 4) => {
                        let (kind, default) = match kind {
                            "yumi" => (crate::power::PowerKind::Yumi, DEFAULT_WEAPON_RESPAWN_SECS),
                            "tag" => (crate::power::PowerKind::Tag, DEFAULT_WEAPON_RESPAWN_SECS),
                            _ => (crate::power::PowerKind::None, DEFAULT_PICKUP_RESPAWN_SECS),
                        };
                        pickups.push(PickupPoint { pos: [nums[0], nums[1], nums[2]], respawn_secs: nums.get(3).copied().unwrap_or(default), kind });
                    }
                    _ => return Err(err("wrong number of values")),
                }
            }
            "zone" => features.zones.push(parse_zone(&tokens[1..], &err)?),
            "mover" => features.movers.push(parse_mover(&tokens[1..], &err)?),
            "hazard" => features.hazards.push(parse_hazard(&tokens[1..], &err)?),
            "tube" => features.shapes.push(parse_shape(ShapeKind::Tube, &tokens[1..], &err)?),
            "ring" => features.shapes.push(parse_shape(ShapeKind::Ring, &tokens[1..], &err)?),
            _ => return Err(err("unknown directive (box | dbox | spawn | pickup | yumi | tag | zone | mover | hazard | tube | ring)")),
        }
    }
    for h in &features.hazards {
        if let Some(m) = h.mover_index() {
            if m >= features.movers.len() {
                return Err(format!("hazard `{}` rides mover {m}, but the file has {} mover lines", h.tag, features.movers.len()));
            }
        }
    }
    if features.hazards.len() > MAX_LEVEL_HAZARDS {
        return Err(format!("too many `hazard` directives (max {MAX_LEVEL_HAZARDS})"));
    }
    if boxes.is_empty() {
        return Err("level has no `box` (nothing to stand on)".into());
    }
    if spawns.is_empty() {
        return Err("level has no `spawn`".into());
    }
    Ok(LevelFile { level: LevelDesc { boxes, spawns }, pickups, features })
}

fn parse_numbers(tokens: &[&str], err: &impl Fn(&str) -> String) -> Result<Vec<f32>, String> {
    let mut nums = Vec::with_capacity(tokens.len());
    for t in tokens {
        let v = t.parse::<f32>().map_err(|_| err("not a number"))?;
        if !v.is_finite() {
            return Err(err("non-finite number"));
        }
        nums.push(v);
    }
    Ok(nums)
}

fn parse_number(tok: &str, err: &impl Fn(&str) -> String) -> Result<f32, String> {
    let v = tok.parse::<f32>().map_err(|_| err("not a number"))?;
    if !v.is_finite() {
        return Err(err("non-finite number"));
    }
    Ok(v)
}

fn parse_ticks(tok: &str, err: &impl Fn(&str) -> String) -> Result<u32, String> {
    tok.parse::<u32>().map_err(|_| err("not a whole tick count"))
}

fn parse_tag(tok: &str, err: &impl Fn(&str) -> String) -> Result<String, String> {
    let ok = !tok.is_empty() && tok.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'));
    if !ok {
        return Err(err("tag must be letters, digits, '_', '-', '.' or ':'"));
    }
    Ok(tok.to_string())
}

fn parse_key(tok: &str) -> bool {
    let mut chars = tok.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn parse_params(tokens: &[&str], err: &impl Fn(&str) -> String) -> Result<BTreeMap<String, f32>, String> {
    let mut out = BTreeMap::new();
    for t in tokens {
        let Some((k, v)) = t.split_once('=') else { return Err(err("expected key=value tuning")) };
        if !parse_key(k) {
            return Err(err("bad tuning key"));
        }
        let v = parse_number(v, err)?;
        if out.insert(k.to_string(), v).is_some() {
            return Err(err("duplicate tuning key"));
        }
    }
    Ok(out)
}

fn tuning_ticks(params: &BTreeMap<String, f32>, key: &str, default: u32, err: &impl Fn(&str) -> String) -> Result<u32, String> {
    match params.get(key) {
        None => Ok(default),
        Some(v) if *v >= 0.0 && v.fract() == 0.0 && *v <= u32::MAX as f32 => Ok(*v as u32),
        Some(_) => Err(err("tick tuning must be a non-negative whole number")),
    }
}

fn feature_box(nums: &[f32], err: &impl Fn(&str) -> String) -> Result<([f32; 3], [f32; 3]), String> {
    if nums.len() < 6 {
        return Err(err("wrong number of values"));
    }
    if nums[3..6].iter().any(|h| *h <= 0.0) {
        return Err(err("half extents must be > 0"));
    }
    Ok(([nums[0], nums[1], nums[2]], [nums[3], nums[4], nums[5]]))
}

fn parse_shape(kind: ShapeKind, tokens: &[&str], err: &impl Fn(&str) -> String) -> Result<ShapeDef, String> {
    if tokens.len() < 8 {
        return Err(err("wrong number of values"));
    }
    let nums = parse_numbers(&tokens[..6], err)?;
    let (center, radius, extent, thickness) = ([nums[0], nums[1], nums[2]], nums[3], nums[4], nums[5]);
    if radius <= 0.0 || extent <= 0.0 || thickness <= 0.0 {
        return Err(err("radius, height/width and thickness must be > 0"));
    }
    if kind == ShapeKind::Ring && extent >= 2.0 * radius {
        return Err(err("ring width must be < 2 * radius"));
    }
    let segments = parse_ticks(tokens[6], err)?;
    if !(3..=MAX_SHAPE_SEGMENTS).contains(&segments) {
        return Err(err("segments must be 3..=256"));
    }
    let tag = parse_tag(tokens[7], err)?;
    let tuning = parse_params(&tokens[8..], err)?;
    Ok(ShapeDef { kind, center, radius, extent, thickness, segments, tag, tuning })
}

fn parse_zone(tokens: &[&str], err: &impl Fn(&str) -> String) -> Result<ZoneDef, String> {
    if tokens.len() < 7 {
        return Err(err("wrong number of values"));
    }
    let nums = parse_numbers(&tokens[..6], err)?;
    let (center, half) = feature_box(&nums, err)?;
    let tag = parse_tag(tokens[6], err)?;
    let tuning = parse_params(&tokens[7..], err)?;
    tuning_ticks(&tuning, "cooldown_ticks", 0, err)?;
    tuning_ticks(&tuning, "fill_ticks", 90, err)?;
    tuning_ticks(&tuning, "reveal_ticks", 300, err)?;
    Ok(ZoneDef { center, half, tag, tuning })
}

fn parse_mover(tokens: &[&str], err: &impl Fn(&str) -> String) -> Result<MoverDef, String> {
    if tokens.len() < 11 {
        return Err(err("wrong number of values"));
    }
    let nums = parse_numbers(&tokens[..6], err)?;
    let (center, half) = feature_box(&nums, err)?;
    let axis = MoveAxis::parse(tokens[6]).ok_or_else(|| err("mover axis must be x, y or z"))?;
    let distance = parse_number(tokens[7], err)?;
    if distance <= 0.0 {
        return Err(err("mover distance must be > 0"));
    }
    let period_ticks = parse_ticks(tokens[8], err)?;
    if period_ticks == 0 {
        return Err(err("period_ticks must be > 0"));
    }
    if period_ticks > u16::MAX as u32 {
        return Err(err("period_ticks must be <= 65535"));
    }
    let solid = match tokens[9] {
        "solid" => true,
        "ghost" => false,
        _ => return Err(err("mover must be solid or ghost")),
    };
    let tag = parse_tag(tokens[10], err)?;
    let tuning = parse_params(&tokens[11..], err)?;
    tuning_ticks(&tuning, "phase_ticks", 0, err)?;
    Ok(MoverDef { center, half, axis, distance, period_ticks, solid, tag, tuning })
}

fn parse_hazard(tokens: &[&str], err: &impl Fn(&str) -> String) -> Result<HazardDef, String> {
    if tokens.len() < 10 {
        return Err(err("wrong number of values"));
    }
    let nums = parse_numbers(&tokens[..6], err)?;
    let (center, half) = feature_box(&nums, err)?;
    let period_ticks = parse_ticks(tokens[6], err)?;
    if period_ticks == 0 {
        return Err(err("period_ticks must be > 0"));
    }
    let telegraph_ticks = parse_ticks(tokens[7], err)?;
    if telegraph_ticks > period_ticks {
        return Err(err("telegraph_ticks must be <= period_ticks"));
    }
    let damage = parse_number(tokens[8], err)?;
    if damage < 0.0 {
        return Err(err("damage must be >= 0"));
    }
    let tag = parse_tag(tokens[9], err)?;
    let tuning = parse_params(&tokens[10..], err)?;
    let active_ticks = tuning_ticks(&tuning, "active_ticks", 1, err)?;
    if active_ticks == 0 {
        return Err(err("active_ticks must be > 0"));
    }
    if active_ticks > period_ticks {
        return Err(err("active_ticks must be <= period_ticks"));
    }
    if active_ticks.saturating_add(telegraph_ticks) > period_ticks {
        return Err(err("active_ticks + telegraph_ticks must be <= period_ticks"));
    }
    let phase_ticks = tuning_ticks(&tuning, "phase_ticks", 0, err)?;
    if phase_ticks >= period_ticks {
        return Err(err("phase_ticks must be < period_ticks"));
    }
    Ok(HazardDef { center, half, period_ticks, telegraph_ticks, damage, tag, tuning })
}

/// Team of each `spawn` line of a level file, in order (`NO_TEAM` when the line has no team). Parallel to
/// `LevelDesc::spawns`. Kept apart from `LevelDesc` so the client's copy of the level is unchanged; call after
/// `parse_level_file` accepted the text.
pub fn parse_spawn_teams(text: &str) -> Vec<u8> {
    text.lines()
        .filter_map(|raw| {
            let mut it = raw.split('#').next()?.split_whitespace();
            (it.next()? == "spawn").then(|| it.nth(3).and_then(|t| t.parse::<u8>().ok()).unwrap_or(crate::tick::NO_TEAM))
        })
        .collect()
}

impl LevelFile {
    /// Stable fingerprint of everything both sides simulate from a level file: `LevelDesc` geometry/spawns plus
    /// `zone`/`mover`/`hazard` features. Pickups are server-only and excluded. Sent in `Welcome`.
    pub fn fingerprint(&self) -> u64 {
        crate::hash::mix_u64(self.level.fingerprint(), self.features.fingerprint())
    }

    /// Inverse of `parse_level_file`: `parse(to_text(x)) == x` exactly (f32 `Display` round-trips).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for b in &self.level.boxes {
            let [cx, cy, cz] = b.center;
            let [hx, hy, hz] = b.half;
            if b.hp > 0.0 {
                out += &format!("dbox {cx} {cy} {cz} {hx} {hy} {hz} {} {}", b.rot_z, b.hp);
                out.push('\n');
                continue;
            }
            out += &format!("box {cx} {cy} {cz} {hx} {hy} {hz}");
            if b.rot_z != 0.0 {
                out += &format!(" {}", b.rot_z);
            }
            out.push('\n');
        }
        for [x, y, z] in &self.level.spawns {
            out += &format!("spawn {x} {y} {z}\n");
        }
        for p in &self.pickups {
            let word = match p.kind {
                crate::power::PowerKind::None => "pickup",
                crate::power::PowerKind::Yumi => "yumi",
                crate::power::PowerKind::Tag => "tag",
            };
            out += &format!("{word} {} {} {} {}\n", p.pos[0], p.pos[1], p.pos[2], p.respawn_secs);
        }
        for z in &self.features.zones {
            out += &format!("zone {} {} {} {} {} {} {}", z.center[0], z.center[1], z.center[2], z.half[0], z.half[1], z.half[2], z.tag);
            for (k, v) in &z.tuning {
                out += &format!(" {k}={v}");
            }
            out.push('\n');
        }
        for m in &self.features.movers {
            out += &format!(
                "mover {} {} {} {} {} {} {} {} {} {} {}",
                m.center[0], m.center[1], m.center[2], m.half[0], m.half[1], m.half[2], m.axis.token(), m.distance, m.period_ticks, if m.solid { "solid" } else { "ghost" }, m.tag
            );
            for (k, v) in &m.tuning {
                out += &format!(" {k}={v}");
            }
            out.push('\n');
        }
        for h in &self.features.hazards {
            out += &format!(
                "hazard {} {} {} {} {} {} {} {} {} {}",
                h.center[0], h.center[1], h.center[2], h.half[0], h.half[1], h.half[2], h.period_ticks, h.telegraph_ticks, h.damage, h.tag
            );
            for (k, v) in &h.tuning {
                out += &format!(" {k}={v}");
            }
            out.push('\n');
        }
        for sh in &self.features.shapes {
            out += &format!(
                "{} {} {} {} {} {} {} {} {}",
                sh.token(), sh.center[0], sh.center[1], sh.center[2], sh.radius, sh.extent, sh.thickness, sh.segments, sh.tag
            );
            for (k, v) in &sh.tuning {
                out += &format!(" {k}={v}");
            }
            out.push('\n');
        }
        out
    }
}

impl LevelDesc {
    /// Geometry and spawns from level-file text (the client's entry point; `pickup` lines are accepted and dropped).
    pub fn from_text(text: &str) -> Result<Self, String> {
        parse_level_file(text).map(|f| f.level)
    }

    /// Stable fingerprint of everything both sides simulate: box centres / half extents / roll / hit points and
    /// the spawn points. Sent in `Welcome` so a client on a different level gets a clear warning instead of
    /// silent prediction drift. FNV-1a over the raw f32 bits: identical on every platform.
    pub fn fingerprint(&self) -> u64 {
        use crate::hash::{mix_f32, mix_u64, OFFSET};
        let mut h = mix_u64(OFFSET, self.boxes.len() as u64);
        for b in &self.boxes {
            for v in b.center.iter().chain(b.half.iter()) {
                h = mix_f32(h, *v);
            }
            h = mix_f32(h, b.rot_z);
            h = mix_f32(h, b.hp);
        }
        h = mix_u64(h, self.spawns.len() as u64);
        for s in &self.spawns {
            for v in s {
                h = mix_f32(h, *v);
            }
        }
        h
    }

    /// Level-file text for just the geometry and spawns.
    pub fn to_text(&self) -> String {
        LevelFile { level: self.clone(), pickups: Vec::new(), features: LevelFeatures::default() }.to_text()
    }

    /// Half extent of the level's floor slab (largest box whose top face is y = 0), if it has one.
    pub fn floor_half(&self) -> Option<f32> {
        self.boxes
            .iter()
            .filter(|b| (b.center[1] + b.half[1]).abs() < 0.05 && b.rot_z == 0.0)
            .map(|b| b.half[0].max(b.half[2]))
            .fold(None, |m: Option<f32>, h| Some(m.map_or(h, |m| m.max(h))))
    }

    /// Named district at ground position (x, z), for the compass / radar label and the per-district grid tint. Derived
    /// from `scripts/gen_ffa_level.py` (same constants; positions scale with the floor half-extent / 112) and only
    /// meaningful on the FFA maps; any other level is one `ARENA` district.
    pub fn district_at(&self, x: f32, z: f32) -> District {
        match self.floor_half() {
            Some(h) if h >= 100.0 => ffa_district_at(h, x, z),
            _ => District::ARENA,
        }
    }

    /// What the server runs today: one big floor.
    pub fn standard() -> Self {
        Self {
            boxes: vec![LevelBox::new([0.0, -0.5, 0.0], [200.0, 0.5, 200.0])],
            spawns: vec![[0.0, 0.81, 0.0], [6.0, 0.81, 0.0], [-6.0, 0.81, 0.0], [0.0, 0.81, 6.0]],
        }
    }

    pub fn spawn(&self, i: usize) -> [f32; 3] {
        self.spawns[i % self.spawns.len()]
    }

    /// One static collider per box, in order; the handles are returned (index = box index).
    pub fn insert_into(&self, w: &mut PhysicsWorld) -> Vec<ColliderHandle> {
        self.boxes.iter().enumerate().map(|(i, b)| insert_level_box(w, b, u16::try_from(i).ok())).collect()
    }

    pub fn apply_to_server(&self, s: &mut Server) {
        let handles = self.insert_into(&mut s.world);
        let cfg = s.breaks.cfg; // a balance set before the level was loaded stays
        s.breaks = crate::breaks::LevelBreaks::build(self, &handles);
        s.breaks.cfg = cfg;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::character::Cfg;

    #[test]
    fn server_and_client_worlds_get_identical_colliders() {
        let level = LevelDesc {
            boxes: vec![
                LevelBox::new([0.0, -0.5, 0.0], [50.0, 0.5, 50.0]),
                LevelBox::new([5.0, 1.0, 0.0], [1.0, 1.0, 4.0]).rolled(0.3),
            ],
            spawns: vec![[0.0, 0.81, 0.0]],
        };
        let mut server = Server::new(Cfg::default());
        level.apply_to_server(&mut server);
        let mut w = PhysicsWorld {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            islands: IslandManager::new(),
            queries: QueryPipeline::new(),
        };
        level.insert_into(&mut w);
        let a: Vec<_> = server.world.colliders.iter().map(|(_, c)| (*c.position(), c.shape().as_cuboid().unwrap().half_extents)).collect();
        let b: Vec<_> = w.colliders.iter().map(|(_, c)| (*c.position(), c.shape().as_cuboid().unwrap().half_extents)).collect();
        assert_eq!(a, b);
        assert!((a[1].0.rotation.angle() - 0.3).abs() < 1e-6);
    }

    #[test]
    fn fingerprint_tracks_geometry_and_spawns() {
        let a = LevelDesc::from_text("box 0 -0.5 0 50 0.5 50\nbox 5 1 0 1 1 4 0.3\nspawn 0 0.81 0\nspawn 4 0.81 4\n").unwrap();
        let b = LevelDesc::from_text(&a.to_text()).unwrap();
        assert_eq!(a, b, "the file round-trip preserves the parsed level");
        assert_eq!(a.fingerprint(), b.fingerprint());
        let mut c = a.clone();
        c.boxes[0].center[0] += 0.001;
        assert_ne!(a.fingerprint(), c.fingerprint(), "a moved box changes the fingerprint");
        let mut d = a.clone();
        d.spawns[0][1] += 0.5;
        assert_ne!(a.fingerprint(), d.fingerprint(), "a moved spawn changes the fingerprint");
        assert_ne!(a.fingerprint(), LevelDesc::standard().fingerprint());
    }

    #[test]
    fn feature_fingerprint_is_stable_changes_with_features_and_ignores_pickups() {
        let text = "box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\npickup 8 1 0\n\
            zone 2 1 3 2 1 2 vent cooldown_ticks=90 fill_rate=0.5\n\
            mover 6 2 0 1 2 1 x 6 180 solid piston phase_ticks=30\n\
            hazard 0 4 0 2 1 2 180 36 20 vent_fire active_ticks=12\n";
        let a = parse_level_file(text).unwrap();
        let b = parse_level_file(&a.to_text()).unwrap();
        assert_eq!(a.features.fingerprint(), b.features.fingerprint(), "stable across the file round-trip");
        assert_eq!(a.fingerprint(), b.fingerprint());

        let mut moved = a.clone();
        moved.features.movers[0].distance += 0.5;
        assert_ne!(a.features.fingerprint(), moved.features.fingerprint(), "a mover change moves the feature hash");
        assert_ne!(a.fingerprint(), moved.fingerprint());
        assert_eq!(a.level.fingerprint(), moved.level.fingerprint(), "LevelDesc::fingerprint stays geometry-only");

        let mut retagged = a.clone();
        retagged.features.zones[0].tag.push_str("_x");
        assert_ne!(a.features.fingerprint(), retagged.features.fingerprint(), "tags are part of the hash");

        let mut retuned = a.clone();
        retuned.features.hazards[0].tuning.insert("phase_ticks".into(), 3.0);
        assert_ne!(a.features.fingerprint(), retuned.features.fingerprint(), "tuning entries are part of the hash");

        let mut picked = a.clone();
        picked.pickups.push(PickupPoint { pos: [9.0, 1.0, 9.0], respawn_secs: 7.0, kind: crate::power::PowerKind::Yumi });
        assert_eq!(a.fingerprint(), picked.fingerprint(), "pickups are server-only and excluded");
    }

    #[test]
    fn parse_good_file() {
        let f = parse_level_file(
            "# arena\nbox 0 -0.5 0  50 0.5 50\nbox 5 1 0 1 1 4 0.3  # ramp\nspawn 0 0.81 0\nspawn 4 0.81 4\npickup 8 1 0\npickup -8 1 0 5\n",
        )
        .unwrap();
        assert_eq!((f.level.boxes.len(), f.level.spawns.len(), f.pickups.len()), (2, 2, 2));
        assert_eq!(f.level.boxes[1].rot_z, 0.3);
        assert_eq!((f.pickups[0].respawn_secs, f.pickups[1].respawn_secs), (20.0, 5.0));
    }

    #[test]
    fn yumi_and_tag_lines_are_weapon_pedestals_with_a_45_second_default_and_survive_the_round_trip() {
        use crate::power::PowerKind;
        let f = parse_level_file("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\npickup 8 1 0\nyumi 0 20 0\ntag 3 1 3 30\n").unwrap();
        let kinds: Vec<_> = f.pickups.iter().map(|p| (p.kind, p.respawn_secs)).collect();
        assert_eq!(kinds, vec![(PowerKind::None, 20.0), (PowerKind::Yumi, 45.0), (PowerKind::Tag, 30.0)]);
        assert_eq!(parse_level_file(&f.to_text()).unwrap(), f);
        assert!(parse_level_file("box 0 0 0 1 1 1\nspawn 0 0 0\nyumi 1 2").unwrap_err().contains("line 3: wrong number"));
    }

    #[test]
    fn parse_errors_name_the_line() {
        for (text, needle) in [
            ("box 0 0 0 1 1\nspawn 0 0 0", "line 1: wrong number"),
            ("box 0 0 0 1 1 1\nspawn 0 x 0", "line 2: not a number"),
            ("box 0 0 0 1 0 1\nspawn 0 0 0", "half extents"),
            ("box 0 0 0 1 1 1\nwarp 1 2 3", "line 2: unknown directive"),
            ("box 0 0 0 1 1 1", "no `spawn`"),
            ("spawn 0 0 0", "no `box`"),
            ("box 0 0 0 1 1 1\nspawn nan 0 0", "non-finite"),
        ] {
            let e = parse_level_file(text).unwrap_err();
            assert!(e.contains(needle), "{text:?} -> {e}");
        }
    }

    #[test]
    fn text_roundtrip_is_exact_and_geometry_only_view_drops_pickups() {
        let f = LevelFile {
            level: LevelDesc {
                boxes: vec![
                    LevelBox::new([0.1, -0.5, 1.0 / 3.0], [50.0, 0.5, 50.0]),
                    LevelBox::new([5.0, 1.0, 0.0], [1.0, 1.0, 4.0]).rolled(-0.35),
                ],
                spawns: vec![[0.0, 0.81, 0.0], [4.25, 0.81, -4.0]],
            },
            pickups: vec![PickupPoint { pos: [8.0, 1.0, 0.0], respawn_secs: 5.5, kind: crate::power::PowerKind::None }],
            features: LevelFeatures::default(),
        };
        assert_eq!(parse_level_file(&f.to_text()).unwrap(), f);
        assert_eq!(LevelDesc::from_text(&f.to_text()).unwrap(), f.level);
        assert_eq!(LevelDesc::from_text(&f.level.to_text()).unwrap(), f.level);
        assert_eq!(LevelDesc::from_text(&LevelDesc::standard().to_text()).unwrap(), LevelDesc::standard());
    }

    #[test]
    fn feature_directives_parse_and_roundtrip_exactly() {
        let text = "box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\n\
            zone 2 1 3 2 1 2 vent cooldown_ticks=90 fill_rate=0.5\n\
            mover 6 2 0 1 2 1 x 6 180 solid piston phase_ticks=30\n\
            mover -6 2 0 1 1 1 z 4 120 ghost fan\n\
            hazard 0 4 0 2 1 2 180 36 20 vent_fire active_ticks=12\n";
        let f = parse_level_file(text).unwrap();
        assert_eq!(f.features.zones.len(), 1);
        assert_eq!(f.features.movers.len(), 2);
        assert_eq!(f.features.hazards.len(), 1);
        assert_eq!(f.features.zones[0].tag, "vent");
        assert_eq!(f.features.zones[0].tuning["cooldown_ticks"], 90.0);
        assert_eq!(f.features.movers[0].axis, MoveAxis::X);
        assert!(f.features.movers[0].solid);
        assert!(!f.features.movers[1].solid);
        assert_eq!(f.features.movers[0].tuning["phase_ticks"], 30.0);
        assert_eq!(f.features.hazards[0].tuning["active_ticks"], 12.0);
        assert_eq!(parse_level_file(&f.to_text()).unwrap(), f);
    }

    #[test]
    fn feature_parse_errors_name_the_line() {
        for (text, needle) in [
            ("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nzone 0 0 0 1 1 1\n", "line 3: wrong number"),
            ("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nzone 0 0 0 1 1 1 vent bad\n", "line 3: expected key=value"),
            ("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nmover 0 0 0 1 1 1 w 2 10 solid p\n", "line 3: mover axis"),
            ("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nmover 0 0 0 1 1 1 x 2 0 solid p\n", "line 3: period_ticks must be > 0"),
            ("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nhazard 0 0 0 1 1 1 0 1 5 h\n", "line 3: period_ticks must be > 0"),
            ("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nhazard 0 0 0 1 1 1 10 5 5 h active_ticks=6\n", "line 3: active_ticks + telegraph_ticks"),
            ("box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nzone 0 0 0 1 1 1 vent cooldown_ticks=1.5\n", "line 3: tick tuning"),
        ] {
            let e = parse_level_file(text).unwrap_err();
            assert!(e.contains(needle), "{text:?} -> {e}");
        }
    }

    #[test]
    fn speed_lane_query_uses_centre_tag_face_band_and_tuning() {
        let text = "box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\n\
            zone 27 4 4.5 14 4 0.5 speed_lane cap=1.6 decay=0.9 mult=1.15 wall_run_m=10\n\
            zone -27 4 4.5 14 4 0.5 core_vent\n";
        let f = parse_level_file(text).unwrap();
        let n = Some([0.0, 0.0, -1.0]);
        let centre = f.features.speed_lane_for([27.0, 4.0, 4.5], n).expect("inside the authored AABB");
        assert!(centre.inside);
        assert_eq!(centre.wall_run_m, 10.0);
        assert_eq!(centre.mult, 1.15);
        assert_eq!(centre.cap, 1.6);
        assert_eq!(centre.decay, 0.9);
        assert!((centre.ramp - 1.0).abs() < 1e-6);
        assert!((centre.multiplier() - 1.15).abs() < 1e-6);

        let edge = f.features.speed_lane_for([13.5, 4.0, 4.5], n).unwrap();
        assert!(edge.inside);
        assert!((edge.ramp - 0.05).abs() < 1e-6, "ramp = 0.5 m into a 10 m lane");
        assert!((edge.multiplier() - 1.0075).abs() < 1e-6);

        assert!(f.features.speed_lane_for([27.0, 4.0, 4.7], Some([0.0, 0.0, 1.0])).is_none(), "opposite face");
        assert!(f.features.speed_lane_for([27.0, 4.0, 4.5], None).is_none(), "cannot match while walking/airborne");
        assert!(f.features.speed_lane_for([27.0, 9.0, 4.5], n).is_none(), "above the volume");

        let band = f.features.speed_lane_for([3.5, 4.0, 4.5], n).expect("post-exit band");
        assert!(!band.inside);
        assert_eq!(band.multiplier(), 1.0, "the band decays carried speed, it does not add more");
        assert!(f.features.speed_lane_for([2.9, 4.0, 4.5], n).is_none(), "past wall_run_m");

        let vent_only = parse_level_file(
            "box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nzone 0 1 0 2 1 2 core_vent\n",
        )
        .unwrap();
        assert!(vent_only.features.speed_lane_for([0.0, 1.0, 0.0], Some([0.0, 0.0, -1.0])).is_none(), "tag must match");

        let capped = parse_level_file(
            "box 0 -0.5 0 50 0.5 50\nspawn 0 0.81 0\nzone 0 4 4.5 10 4 0.5 speed_lane wall_run_m=0 mult=3 cap=1.6\n",
        )
        .unwrap();
        assert!((capped.features.speed_lane_for([0.0, 4.0, 4.5], n).unwrap().multiplier() - 1.6).abs() < 1e-6);
    }

    #[test]
    fn find_level_looks_upward_from_the_executable_and_stops_after_five_levels() {
        let root = std::env::temp_dir().join(format!("shinobi-lvl-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("levels")).unwrap();
        std::fs::write(root.join(DEFAULT_LEVEL), "box 0 -0.5 0 10 0.5 10\nspawn 0 0.81 0\n").unwrap();
        let rel = std::path::Path::new(DEFAULT_LEVEL);
        // target/release/shinobi-server: root is 3 ancestors up
        let exe = root.join("target").join("release").join("shinobi-server");
        assert_eq!(find_level(rel, Some(&exe)), Some(root.join(DEFAULT_LEVEL)));
        // exe nested too deep: not found
        let deep = root.join("a").join("b").join("c").join("d").join("e").join("f").join("exe");
        assert_eq!(find_level(rel, Some(&deep)), None);
        assert_eq!(find_level(rel, None), None, "no exe, no cwd copy: built-in");
        assert_eq!(find_level(std::path::Path::new("levels/does-not-exist.lvl"), Some(&exe)), None);
        // and what it finds parses
        assert!(LevelDesc::from_text(&std::fs::read_to_string(root.join(DEFAULT_LEVEL)).unwrap()).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// A named area of the map with the colour its grid / edges should lean toward.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct District {
    pub id: DistrictId,
    pub name: &'static str,
    /// Linear RGB tint (HDR-friendly, 0..1.2).
    pub tint: [f32; 3],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DistrictId {
    Arena,
    Hub,
    Boulevard,
    Canyons,
    Towers,
    MonolithField,
    VoidEdge,
}

impl District {
    pub const ARENA: District = District { id: DistrictId::Arena, name: "ARENA", tint: [0.1, 0.9, 1.0] };
}

/// North is -z (yaw 0 faces -z), east is +x.
fn compass(x: f32, z: f32) -> &'static str {
    match (z < 0.0, x >= 0.0) {
        (true, true) => "NORTH-EAST",
        (true, false) => "NORTH-WEST",
        (false, true) => "SOUTH-EAST",
        (false, false) => "SOUTH-WEST",
    }
}

fn ffa_district_at(half: f32, x: f32, z: f32) -> District {
    let sc = half / 112.0;
    let cheb = |cx: f32, cz: f32| (x - cx).abs().max((z - cz).abs());
    // void edge: the outer 12 m (the lip and the gaps in it)
    if x.abs() > half - 12.0 || z.abs() > half - 12.0 {
        return District { id: DistrictId::VoidEdge, name: "VOID EDGE", tint: [1.0, 0.15, 0.3] };
    }
    // hub: spire plaza, cover and gate arches
    if x.abs().max(z.abs()) < 40.0 * sc.max(1.0) {
        return District { id: DistrictId::Hub, name: "HUB SPIRE", tint: [1.0, 0.85, 0.4] };
    }
    // boulevards: 8 m wide wall-run lanes along the axes from the hub gates to the void
    let lane = 8.0;
    if z.abs() < lane && x.abs() > 40.0 * sc.max(1.0) {
        return District { id: DistrictId::Boulevard, name: if x >= 0.0 { "EAST BOULEVARD" } else { "WEST BOULEVARD" }, tint: [0.2, 0.9, 1.0] };
    }
    if x.abs() < lane && z.abs() > 40.0 * sc.max(1.0) {
        return District { id: DistrictId::Boulevard, name: if z < 0.0 { "NORTH BOULEVARD" } else { "SOUTH BOULEVARD" }, tint: [0.2, 0.9, 1.0] };
    }
    // districts: A (canyons + ziggurat) at (+66, +66) and (-66, -66); B (tower clusters) at (+66, -66) and (-66, +66)
    let c = 66.0 * sc;
    for (cx, cz, towers) in [(c, c, false), (-c, -c, false), (c, -c, true), (-c, c, true)] {
        if cheb(cx, cz) <= 40.0 {
            let name = match (towers, compass(cx, cz)) {
                (true, "NORTH-EAST") => "NORTH-EAST TOWERS",
                (true, _) => "SOUTH-WEST TOWERS",
                (false, "SOUTH-EAST") => "SOUTH-EAST CANYONS",
                (false, _) => "NORTH-WEST CANYONS",
            };
            return if towers {
                District { id: DistrictId::Towers, name, tint: [1.0, 0.5, 0.15] }
            } else {
                District { id: DistrictId::Canyons, name, tint: [1.0, 0.2, 0.8] }
            };
        }
    }
    District { id: DistrictId::MonolithField, name: "MONOLITH FIELD", tint: [0.15, 0.4, 1.0] }
}
