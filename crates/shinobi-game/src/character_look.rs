//! Tron x shinobi look for remote players: a hooded ninja with a glowing visor slit, shoulder and chest plates with
//! circuit lines in the player colour, a sash, glowing greaves, a katana with a lit edge plus a sheath on the back, a
//! wrist grapple launcher, a spinning star glyph and a trailing scarf; and cheap procedural poses (arm swing, tuck,
//! wall-run, grapple, block, strike). All cosmetic and client-only.
//!
//! The four-box body from `shinobi_client::remote::ninja_draw` stays the readable core (bright body, black rim, beacons,
//! gem and aura live in `play.rs`). This rig only decorates it, so readability is never traded for detail:
//!   * beyond `DARK_MAX` m the dark armour hides and the bright core shows again,
//!   * beyond `GLOW_MAX` m the whole rig hides (the body, beacon and gem carry the read).
//!
//! Every part is an instance of the shared unit cube. Static parts are children of a few animated pivots, so a frame
//! costs ~12 transform writes per player.

use bevy::prelude::*;
use std::collections::HashMap;

use shinobi_client::remote::NinjaDraw;
use shinobi_proto::character::*;
use shinobi_proto::snapshot::{MoveKind, FLAG_ROLL, FX_BLOCK, FX_OPEN, FX_STAGGER};

/// Dark armour is hidden beyond this distance (m): it would only dim the bright core.
pub const DARK_MAX: f32 = 22.0;
/// Whole rig hidden beyond this distance (m).
pub const GLOW_MAX: f32 = 70.0;

const SCARF_SEGS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    /// Armour plates, hood: dim hue.
    Dark,
    /// Sash, scarf, hands: the plain hue.
    Accent,
    /// Circuit lines, bracers, greaves: bright hue lifted toward white.
    Glow,
    /// Visor, blade edge, launcher muzzle: near white.
    Hot,
    /// A custom character's own colour, drawn exactly as given (skin, hair, glove, cloth).
    Plain,
}

/// Where a part's colour comes from. `Match` = the player's match colour (the stock look). The others read the
/// player's `CharacterSpec`; a slot the spec leaves unset falls back to `Match`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Src {
    Match,
    Base,
    Trim,
    Glow,
    Visor,
    Glove,
    Scarf,
    Hair,
    Skin,
    Edge,
    Star,
}

impl Src {
    fn rgb(self, s: &CharacterSpec) -> Option<[u8; 3]> {
        match self {
            Src::Match => None,
            Src::Base => s.rgb(C_BASE),
            Src::Trim => s.rgb(C_TRIM),
            Src::Glow => s.rgb(C_GLOW),
            Src::Visor => s.rgb(C_VISOR),
            Src::Glove => s.rgb(C_GLOVE).or_else(|| s.rgb(C_SKIN)),
            Src::Scarf => s.rgb(C_SCARF),
            Src::Hair => s.rgb(C_HAIR),
            Src::Skin => s.rgb(C_SKIN),
            Src::Edge => s.rgb(C_EDGE),
            Src::Star => s.rgb(C_STAR),
        }
    }
}

/// Material handles per final colour (quantised, see `colour_key`).
#[derive(Resource, Default)]
pub struct LookMats(HashMap<u64, Handle<StandardMaterial>>);

static STARTUP_LOOK: std::sync::OnceLock<Option<CharacterSpec>> = std::sync::OnceLock::new();

/// Called once by `main` with the look chosen on the command line / environment (before the app is built).
pub fn set_startup_look(spec: Option<CharacterSpec>) {
    let _ = STARTUP_LOOK.set(spec);
}

impl MyLook {
    /// The startup choice (`set_startup_look`); `SHOT_CHAR=file` overrides it for review frames.
    pub fn startup() -> MyLook {
        let mut look = STARTUP_LOOK.get().cloned().flatten();
        if let Ok(p) = std::env::var("SHOT_CHAR").map(|p| p.trim().to_string()).and_then(|p| if p == "presets" { Err(std::env::VarError::NotPresent) } else { Ok(p) }) {
            match shinobi_client::character_file::load_noted(std::path::Path::new(&p)) {
                Ok((s, warn)) => {
                    for w in warn {
                        eprintln!("SHOT_CHAR: {w}");
                    }
                    look = Some(s)
                }
                Err(e) => eprintln!("SHOT_CHAR: {e}"),
            }
        }
        MyLook(look)
    }
}

/// The local player's own look (`--character`, `SHINOBI_CHARACTER`, `/character`): first-person gloves and the blade
/// follow it, and `sync_my_look` sends it to the server. `None` = the stock ninja.
#[derive(Resource, Default, Clone, Debug)]
pub struct MyLook(pub Option<CharacterSpec>);

/// Push the local look to the network client (idempotent: it re-sends only on change, and again after a reconnect).
pub fn sync_my_look(look: Res<MyLook>, mut net: ResMut<crate::net::Net>) {
    net.0.game.net.set_look(look.0.as_ref());
}

fn role_colour(role: Role, hue: [f32; 3]) -> [f32; 3] {
    let lift = |w: f32, g: f32| -> [f32; 3] { std::array::from_fn(|i| (hue[i] + (1.0 - hue[i]) * w) * g) };
    match role {
        Role::Dark => lift(0.0, 0.32),
        Role::Accent => lift(0.1, 1.0),
        Role::Glow => lift(0.3, 3.0),
        Role::Hot => lift(0.85, 4.5),
        Role::Plain => hue,
    }
}

/// Final linear colour of a part: the stock role colour of the match hue, or the spec's colour for its slot
/// (dim/plain slots exactly as given, glow and hot slots lifted the same way as the stock look).
fn paint(role: Role, src: Src, hue: [f32; 3], spec: Option<&CharacterSpec>) -> [f32; 3] {
    match spec.and_then(|s| src.rgb(s)) {
        None => role_colour(role, hue),
        Some(c) => {
            let c = c.map(|v| v as f32 / 255.0);
            match role {
                Role::Glow | Role::Hot => role_colour(role, c),
                _ => c,
            }
        }
    }
}

fn colour_key(c: [f32; 3]) -> u64 {
    c.iter().fold(0u64, |k, v| (k << 16) | (v.clamp(0.0, 60.0) * 1000.0).round() as u64)
}

/// Scale of the glyph sheet segments (half extent of a glyph, metres) on the chest.
const GLYPH_R: f32 = 0.075;

/// The 15 chest emblems (1..=15) as line segments `[cx, cy, length, thickness, angle]` in a -1..1 box.
pub fn glyph_segments(id: u8) -> &'static [[f32; 5]] {
    const T: f32 = 0.34;
    const P4: f32 = std::f32::consts::FRAC_PI_4;
    const H: f32 = std::f32::consts::FRAC_PI_2;
    match id {
        1 => &[[0.0, 0.0, 2.0, T, 0.0], [0.0, 0.0, 2.0, T, H]],
        2 => &[[0.0, 0.0, 2.2, T, P4], [0.0, 0.0, 2.2, T, -P4]],
        3 => &[[0.0, 0.7, 2.0, T, 0.0], [0.0, 0.0, 2.0, T, 0.0], [0.0, -0.7, 2.0, T, 0.0]],
        4 => &[[-0.7, 0.0, 2.0, T, H], [0.0, 0.0, 2.0, T, H], [0.7, 0.0, 2.0, T, H]],
        5 => &[[-0.5, 0.5, 1.4, T, -P4], [0.5, 0.5, 1.4, T, P4], [-0.5, -0.5, 1.4, T, P4], [0.5, -0.5, 1.4, T, -P4]],
        6 => &[[0.0, -0.8, 2.0, T, 0.0], [-0.5, 0.0, 1.9, T, 1.1], [0.5, 0.0, 1.9, T, -1.1]],
        7 => &[[-0.5, 0.0, 1.5, T, 0.9], [0.5, 0.0, 1.5, T, -0.9]],
        8 => &[[-0.5, 0.45, 1.2, T, 0.9], [0.5, 0.45, 1.2, T, -0.9], [-0.5, -0.45, 1.2, T, 0.9], [0.5, -0.45, 1.2, T, -0.9]],
        9 => &[[0.0, 0.9, 2.0, T, 0.0], [0.0, -0.9, 2.0, T, 0.0], [-0.9, 0.0, 2.0, T, H], [0.9, 0.0, 2.0, T, H]],
        10 => &[[0.0, 0.0, 2.4, T, 0.9], [0.8, -0.8, 0.4, 0.5, 0.0]],
        11 => &[[0.0, 0.0, 2.0, T, 0.0], [0.0, 0.0, 2.0, T, H], [0.0, 0.0, 2.0, T, P4], [0.0, 0.0, 2.0, T, -P4]],
        12 => &[[-0.7, 0.0, 2.0, T, H], [0.0, -0.9, 1.6, T, 0.0]],
        13 => &[[0.0, 0.9, 1.8, T, 0.0], [0.0, 0.0, 2.3, T, 0.9], [0.0, -0.9, 1.8, T, 0.0]],
        14 => &[[0.0, 0.7, 0.5, 0.5, 0.0], [-0.7, -0.6, 0.5, 0.5, 0.0], [0.7, -0.6, 0.5, 0.5, 0.0]],
        15 => &[[0.0, 0.0, 2.2, 0.5, P4], [0.0, 0.0, 2.2, 0.5, -P4], [0.0, 0.0, 0.5, 0.5, 0.0]],
        _ => &[],
    }
}

/// Handles of the animated pivots and the per-frame state (stride phase, scarf springs).
pub struct Rig {
    torso: Entity,
    head: Entity,
    arm_l: Entity,
    arm_r: Entity,
    legs: Entity,
    foot_l: Entity,
    foot_r: Entity,
    katana: Entity,
    glyph: Entity,
    scarf: [Entity; SCARF_SEGS],
    /// v1.1 idle-sway pivots: ears (pivot, rest rotation, phase) under the head, tail under the torso.
    ears: Vec<(Entity, Quat, f32)>,
    tail: Option<Entity>,
    /// Shoulder width factor (1.0 = stock).
    shoulders: f32,
    /// Materials are applied per (role, colour source), so remember who has which.
    roles: Vec<(Entity, Role, Src)>,
    /// Entities hidden beyond `DARK_MAX`.
    dark: Vec<Entity>,
    /// Pivots hidden beyond `GLOW_MAX` (children follow).
    pivots: Vec<Entity>,
    phase: f32,
    /// Scarf points in world space (anchor first); `None` until the first update.
    scarf_pts: Option<[Vec3; SCARF_SEGS + 1]>,
    /// Visual build: root scale (width, height) and the scarf's style (shown?), length and width factors.
    bulk: f32,
    height: f32,
    scarf_on: bool,
    scarf_len: f32,
    scarf_w: f32,
}

/// Per-frame inputs for one remote player.
pub struct Ctx<'a> {
    pub draw: &'a NinjaDraw,
    pub kind: MoveKind,
    pub flags: u8,
    pub fx: u8,
    /// World velocity (m/s).
    pub vel: Vec3,
    /// Eye distance to the player (m).
    pub dist: f32,
    pub dt: f32,
    pub t: f32,
    /// Body fattening used for far players (`play::size_boost`).
    pub boost: (f32, f32),
    /// Grapple anchor when this player has a rope out.
    pub anchor: Option<Vec3>,
}

/// Arm pose: `swing` forward (+) / back (-) about the shoulder, `splay` outward, both radians.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Arm {
    pub swing: f32,
    pub splay: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pose {
    pub left: Arm,
    pub right: Arm,
    /// Foot stride (m, forward/back) and lift (m).
    pub stride: f32,
    pub lift: f32,
}

/// Procedural pose from the move state. Pure, so it is unit-tested: `phase` is the accumulated stride angle.
pub fn pose(kind: MoveKind, flags: u8, fx: u8, speed: f32, phase: f32, t: f32, grapple: bool) -> Pose {
    let run = (speed / 9.0).clamp(0.0, 1.0);
    let breathe = 0.04 * (t * 2.0).sin();
    let mut p = Pose { left: Arm { swing: breathe, splay: 0.08 }, right: Arm { swing: -breathe, splay: 0.08 }, stride: 0.0, lift: 0.0 };
    match kind {
        MoveKind::Grounded => {
            let s = phase.sin();
            p.left.swing = 0.95 * run * s + breathe;
            p.right.swing = -0.55 * run * s - breathe; // the katana arm swings less
            p.stride = 0.3 * run * s;
            p.lift = 0.1 * run * (phase.cos()).max(0.0);
        }
        MoveKind::Airborne => {
            p.left = Arm { swing: 0.45, splay: 0.85 };
            p.right = Arm { swing: 0.2, splay: 0.95 };
            p.stride = 0.18;
            p.lift = 0.22;
        }
        MoveKind::WallRun => {
            p.left = Arm { swing: 0.5, splay: 0.8 };
            p.right = Arm { swing: -0.3, splay: 0.7 };
            p.stride = 0.25 * phase.sin();
            p.lift = 0.1;
        }
        MoveKind::Evading => {
            if flags & FLAG_ROLL != 0 {
                // crouch roll: arms hugged across the chest
                p.left = Arm { swing: 1.0, splay: -0.3 };
                p.right = Arm { swing: 1.0, splay: -0.3 };
                p.lift = 0.15;
            } else {
                // dash: arms swept back behind the body
                p.left = Arm { swing: -1.25, splay: 0.15 };
                p.right = Arm { swing: -1.15, splay: 0.15 };
                p.stride = -0.3;
                p.lift = 0.12;
            }
        }
    }
    if fx & FX_OPEN != 0 {
        // recovering from a swing: the katana arm is still out front
        p.right = Arm { swing: 1.35, splay: 0.1 };
    }
    if fx & FX_STAGGER != 0 {
        p.left = Arm { swing: 0.6 + 0.3 * (t * 22.0).sin(), splay: 1.0 };
        p.right = Arm { swing: 0.6 - 0.3 * (t * 22.0).sin(), splay: 1.0 };
    }
    if fx & FX_BLOCK != 0 {
        // guard: both arms raised in front behind the shield panel
        p.left = Arm { swing: 1.35, splay: 0.12 };
        p.right = Arm { swing: 1.15, splay: -0.15 };
    }
    if grapple && fx & FX_BLOCK == 0 {
        p.right.swing = p.right.swing.min(-0.5); // the free arm trails; the left one aims (see `update`)
    }
    p
}

/// One scarf spring step: each point trails the previous one, pushed back by the player's velocity and sagging with
/// gravity. Deterministic in its inputs; `dt` is clamped so a hitch never explodes the chain.
pub fn scarf_step(pts: &mut [Vec3; SCARF_SEGS + 1], anchor: Vec3, back: Vec3, vel: Vec3, dt: f32) {
    scarf_step_len(pts, anchor, back, vel, dt, 1.0)
}

/// `scarf_step` with every link `len` times as long (custom scarf length).
pub fn scarf_step_len(pts: &mut [Vec3; SCARF_SEGS + 1], anchor: Vec3, back: Vec3, vel: Vec3, dt: f32, len: f32) {
    let dt = dt.clamp(0.0, 0.05);
    pts[0] = anchor;
    let speed = vel.length();
    let wind = if speed > 0.1 { -vel / speed } else { Vec3::ZERO };
    for i in 1..=SCARF_SEGS {
        let seg = (0.2 - 0.02 * i as f32) * len;
        let streaming = (speed / 12.0).clamp(0.0, 1.0);
        // at rest the scarf hangs down and a little back; at speed it streams straight out behind
        let want = (back * (0.5 + 0.9 * streaming) + wind * (1.4 * streaming) - Vec3::Y * (0.9 - 0.75 * streaming)).normalize_or(Vec3::NEG_Y);
        let target = pts[i - 1] + want * seg;
        let k = 1.0 - (-(14.0 - 1.5 * i as f32) * dt).exp();
        pts[i] += (target - pts[i]) * k;
        // keep the link length so a fast turn cannot stretch it
        let d = pts[i] - pts[i - 1];
        pts[i] = pts[i - 1] + d.normalize_or(Vec3::NEG_Y) * seg;
    }
}

struct Spawner<'a, 'w, 's> {
    commands: &'a mut Commands<'w, 's>,
    cube: &'a Handle<Mesh>,
    roles: Vec<(Entity, Role, Src)>,
    dark: Vec<Entity>,
}

impl Spawner<'_, '_, '_> {
    /// A part whose colour comes from `src`. Dark armour and hair/skin hide beyond `DARK_MAX`.
    fn part_s(&mut self, parent: Entity, role: Role, src: Src, pos: [f32; 3], size: [f32; 3], rot: Quat) -> Entity {
        let e = self
            .commands
            .spawn((
                Mesh3d(self.cube.clone()),
                Transform { translation: Vec3::from(pos), rotation: rot, scale: Vec3::from(size) },
                Visibility::default(),
                ChildOf(parent),
            ))
            .id();
        self.roles.push((e, role, src));
        if role == Role::Dark || matches!(src, Src::Skin | Src::Hair) {
            self.dark.push(e);
        }
        e
    }

    /// Like `part_s` but always hidden beyond `DARK_MAX` (head dressing that would dim the bright core from afar).
    fn part_far(&mut self, parent: Entity, role: Role, src: Src, pos: [f32; 3], size: [f32; 3], rot: Quat) -> Entity {
        let e = self.part_s(parent, role, src, pos, size, rot);
        if !self.dark.contains(&e) {
            self.dark.push(e);
        }
        e
    }

    fn pivot(&mut self, parent: Entity) -> Entity {
        self.commands.spawn((Transform::default(), Visibility::default(), ChildOf(parent))).id()
    }

    fn pivot_at(&mut self, parent: Entity, pos: [f32; 3], rot: Quat) -> Entity {
        self.commands.spawn((Transform { translation: Vec3::from(pos), rotation: rot, ..default() }, Visibility::default(), ChildOf(parent))).id()
    }
}

impl Rig {
    /// Spawn the whole rig under `root` (model space: feet at the origin, facing -Z). `spec` = `None` or a default
    /// spec builds exactly the stock rig.
    pub fn spawn(commands: &mut Commands, root: Entity, cube: &Handle<Mesh>, spec: Option<&CharacterSpec>) -> Rig {
        let dflt = CharacterSpec::default();
        let sp = spec.unwrap_or(&dflt);
        let mut s = Spawner { commands, cube, roles: Vec::new(), dark: Vec::new() };
        let id = Quat::IDENTITY;
        let rz = Quat::from_rotation_z;
        let rx = Quat::from_rotation_x;
        let sh = sp.shoulders();
        let tabi = if sp.has_acc(ACC_TABI) { Src::Glow } else { Src::Match };

        // ---- torso (nominal half extents 0.22 x 0.357 x 0.12, front is -Z) ----
        let torso = s.pivot(root);
        // chest plate + circuit pattern + core diamond / emblem
        if sp.plates >= 1 {
            s.part_s(torso, Role::Dark, Src::Base, [0.0, 0.10, -0.125], [0.36, 0.30, 0.03], id);
        }
        match sp.pattern {
            0 => {
                // lines: a spine and up to three cross bars by density (4 = the stock two)
                s.part_s(torso, Role::Glow, Src::Glow, [0.0, 0.10, -0.145], [0.022, 0.28, 0.012], id);
                let bars: &[f32] = match sp.density_q {
                    0..=1 => &[],
                    2..=3 => &[0.17],
                    4..=5 => &[0.17, 0.03],
                    _ => &[0.17, 0.03, 0.235],
                };
                for y in bars {
                    s.part_s(torso, Role::Glow, Src::Glow, [0.0, *y, -0.145], [0.31, 0.018, 0.012], id);
                }
            }
            1 => {
                const SPOTS: [[f32; 2]; 8] = [[-0.1, 0.19], [0.1, 0.19], [-0.1, 0.01], [0.1, 0.01], [0.0, 0.215], [0.0, -0.015], [-0.14, 0.10], [0.14, 0.10]];
                for p in SPOTS.iter().take(1 + sp.density_q as usize) {
                    s.part_s(torso, Role::Glow, Src::Glow, [p[0], p[1], -0.145], [0.05, 0.05, 0.012], rz(0.785));
                }
            }
            2 => {
                for (i, x) in [-0.12f32, 0.12, -0.04, 0.04].iter().take(1 + sp.density_q as usize / 2).enumerate() {
                    let tilt = if i % 2 == 0 { 0.5 } else { -0.5 };
                    s.part_s(torso, Role::Glow, Src::Glow, [*x, 0.10, -0.145], [0.018, 0.27, 0.012], rz(tilt));
                }
            }
            _ => {}
        }
        if sp.emblem == 0 {
            s.part_s(torso, Role::Hot, Src::Glow, [0.0, 0.10, -0.152], [0.06, 0.06, 0.012], rz(0.785));
        } else {
            for g in glyph_segments(sp.emblem) {
                // segment centre (cx, cy), its long axis rotated by the angle, flat on the chest
                s.part_s(torso, Role::Hot, Src::Glow, [g[0] * GLYPH_R, 0.10 + g[1] * GLYPH_R, -0.152], [g[2] * GLYPH_R * 0.5, g[3] * GLYPH_R * 0.25, 0.012], rz(g[4]));
            }
        }
        // back plate + spine line
        if sp.plates >= 1 {
            s.part_s(torso, Role::Dark, Src::Base, [0.0, 0.10, 0.125], [0.36, 0.30, 0.03], id);
        }
        if sp.pattern != 3 {
            s.part_s(torso, Role::Glow, Src::Glow, [0.0, 0.10, 0.145], [0.022, 0.28, 0.012], id);
        }
        // shoulder plates with a lit edge (heavy: a second layer and an abdomen guard)
        if sp.plates >= 1 {
            for sx in [-1.0f32, 1.0] {
                s.part_s(torso, Role::Dark, Src::Base, [0.27 * sx * sh, 0.30, 0.0], [0.17, 0.05, 0.24], rz(-0.25 * sx));
                s.part_s(torso, Role::Glow, Src::Glow, [0.335 * sx * sh, 0.275, 0.0], [0.016, 0.02, 0.24], rz(-0.25 * sx));
                if sp.plates == 2 {
                    s.part_s(torso, Role::Dark, Src::Base, [0.285 * sx * sh, 0.345, 0.0], [0.2, 0.05, 0.3], rz(-0.25 * sx));
                }
            }
            if sp.plates == 2 {
                s.part_s(torso, Role::Dark, Src::Base, [0.0, -0.12, -0.125], [0.34, 0.12, 0.03], id);
                s.part_s(torso, Role::Glow, Src::Glow, [0.0, -0.12, -0.145], [0.3, 0.014, 0.01], id);
            }
        }
        if sp.has_acc(ACC_FINS) {
            for sx in [-1.0f32, 1.0] {
                s.part_s(torso, Role::Glow, Src::Glow, [0.40 * sx * sh, 0.40, 0.04], [0.02, 0.26, 0.14], rz(0.5 * sx));
            }
        }
        // sash: waist band + knot + hanging tail
        if sp.sash {
            s.part_s(torso, Role::Accent, Src::Trim, [0.0, -0.30, 0.0], [0.47, 0.075, 0.27], id);
            s.part_s(torso, Role::Glow, Src::Glow, [0.0, -0.30, -0.138], [0.45, 0.014, 0.01], id);
            s.part_s(torso, Role::Accent, Src::Trim, [0.19, -0.33, -0.14], [0.07, 0.07, 0.04], rz(0.785));
            s.part_s(torso, Role::Accent, Src::Trim, [0.21, -0.52, -0.145], [0.05, 0.27, 0.015], rz(0.12));
        }
        if sp.has_acc(ACC_KUNAI) {
            for x in [-0.17f32, -0.1, -0.03] {
                s.part_s(torso, Role::Dark, Src::Base, [x, -0.27, -0.15], [0.022, 0.05, 0.02], id);
                s.part_s(torso, Role::Hot, Src::Edge, [x, -0.34, -0.15], [0.026, 0.10, 0.012], id);
            }
        }
        // back item: katana sheath (dark scabbard, lit seam, hilt and tip glow), banner, pack or nothing
        match sp.back_item {
            0 => {
                let sheath_q = rz(0.8);
                s.part_s(torso, Role::Dark, Src::Base, [0.0, -0.02, 0.19], [0.05, 0.86, 0.05], sheath_q);
                s.part_s(torso, Role::Glow, Src::Glow, [0.0, -0.02, 0.222], [0.012, 0.8, 0.012], sheath_q);
                s.part_s(torso, Role::Hot, Src::Edge, [-0.31, 0.30, 0.19], [0.06, 0.1, 0.06], sheath_q);
                s.part_s(torso, Role::Glow, Src::Glow, [0.31, -0.34, 0.19], [0.06, 0.05, 0.06], sheath_q);
            }
            1 => {
                s.part_s(torso, Role::Dark, Src::Base, [0.0, 0.05, 0.19], [0.012, 1.0, 0.012], id);
                s.part_s(torso, Role::Accent, Src::Trim, [0.0, -0.02, 0.20], [0.22, 0.72, 0.014], id);
                s.part_s(torso, Role::Glow, Src::Glow, [0.0, -0.39, 0.205], [0.22, 0.02, 0.016], id);
                s.part_s(torso, Role::Glow, Src::Glow, [0.0, 0.34, 0.205], [0.24, 0.02, 0.016], id);
            }
            2 => {
                s.part_s(torso, Role::Dark, Src::Base, [0.0, 0.0, 0.205], [0.32, 0.36, 0.14], id);
                s.part_s(torso, Role::Glow, Src::Glow, [0.0, 0.08, 0.278], [0.28, 0.016, 0.012], id);
                s.part_s(torso, Role::Glow, Src::Glow, [0.0, -0.08, 0.278], [0.28, 0.016, 0.012], id);
                s.part_s(torso, Role::Accent, Src::Trim, [0.0, 0.0, 0.282], [0.05, 0.3, 0.01], id);
            }
            _ => {}
        }

        // v1.1 tail on the lower back (sways in `update`)
        let tail = (sp.tail != 0).then(|| {
            let t = s.pivot_at(torso, [0.0, -0.28, 0.15], id);
            let rx = Quat::from_rotation_x;
            match sp.tail {
                1 => {
                    // fox: bushy, rising, pale tip
                    s.part_far(t, Role::Plain, Src::Hair, [0.0, 0.0, 0.13], [0.11, 0.11, 0.24], rx(-0.15));
                    s.part_far(t, Role::Plain, Src::Hair, [0.0, 0.06, 0.34], [0.17, 0.17, 0.24], rx(-0.45));
                    s.part_far(t, Role::Accent, Src::Trim, [0.0, 0.15, 0.52], [0.12, 0.12, 0.14], rx(-0.7));
                }
                2 => {
                    // cat: thin and long, curling up
                    s.part_far(t, Role::Plain, Src::Hair, [0.0, 0.0, 0.12], [0.05, 0.05, 0.24], id);
                    s.part_far(t, Role::Plain, Src::Hair, [0.0, 0.05, 0.3], [0.05, 0.05, 0.22], rx(-0.5));
                    s.part_far(t, Role::Plain, Src::Hair, [0.0, 0.17, 0.4], [0.05, 0.05, 0.22], rx(-1.1));
                }
                _ => {
                    // lizard: tapered, low, lit ridge
                    s.part_far(t, Role::Plain, Src::Skin, [0.0, -0.02, 0.13], [0.13, 0.1, 0.24], rx(0.1));
                    s.part_far(t, Role::Plain, Src::Skin, [0.0, -0.05, 0.34], [0.09, 0.07, 0.24], rx(0.15));
                    s.part_far(t, Role::Plain, Src::Skin, [0.0, -0.08, 0.54], [0.05, 0.045, 0.2], rx(0.2));
                    s.part_far(t, Role::Glow, Src::Glow, [0.0, 0.035, 0.3], [0.02, 0.02, 0.5], rx(0.1));
                }
            }
            t
        });

        // ---- head: hood shell, mask, glowing visor (nominal half 0.11 x 0.12 x 0.11) ----
        let head = s.pivot(root);
        let hooded = sp.hood == 0;
        match sp.hood {
            0 => {
                s.part_s(head, Role::Dark, Src::Base, [0.0, 0.145, 0.0], [0.27, 0.05, 0.27], id); // hood top
                s.part_s(head, Role::Dark, Src::Base, [0.0, 0.0, 0.125], [0.27, 0.29, 0.04], id); // hood back
                s.part_s(head, Role::Dark, Src::Base, [-0.13, 0.0, 0.0], [0.04, 0.29, 0.25], id);
                s.part_s(head, Role::Dark, Src::Base, [0.13, 0.0, 0.0], [0.04, 0.29, 0.25], id);
                s.part_s(head, Role::Dark, Src::Base, [0.0, 0.17, 0.17], [0.2, 0.04, 0.2], rx(-0.45)); // hood peak
            }
            1 => {
                // hood thrown back: the back and short side panels frame the face and hair
                s.part_s(head, Role::Dark, Src::Base, [0.0, -0.01, 0.14], [0.27, 0.31, 0.05], id);
                s.part_s(head, Role::Dark, Src::Base, [-0.13, -0.04, 0.07], [0.04, 0.2, 0.12], id);
                s.part_s(head, Role::Dark, Src::Base, [0.13, -0.04, 0.07], [0.04, 0.2, 0.12], id);
            }
            _ => {}
        }
        if !hooded {
            s.part_s(head, Role::Plain, Src::Skin, [0.0, 0.0, 0.0], [0.235, 0.26, 0.235], id); // bare head
        }
        // face covering
        match sp.mask {
            0 => {
                if hooded {
                    s.part_s(head, Role::Plain, Src::Skin, [0.0, -0.02, -0.118], [0.2, 0.17, 0.02], id); // face in the hood opening
                }
            }
            1 => {
                s.part_s(head, Role::Dark, Src::Base, [0.0, -0.065, -0.118], [0.22, 0.11, 0.03], id); // face mask
                if hooded {
                    s.part_s(head, Role::Dark, Src::Base, [0.0, 0.10, -0.118], [0.22, 0.07, 0.03], id); // brow band
                }
            }
            2 => {
                s.part_s(head, Role::Dark, Src::Base, [0.0, -0.01, -0.118], [0.225, 0.24, 0.03], id);
            }
            3 => {
                s.part_far(head, Role::Accent, Src::Trim, [0.0, -0.01, -0.118], [0.225, 0.24, 0.03], id);
                s.part_s(head, Role::Dark, Src::Base, [0.0, 0.075, -0.13], [0.2, 0.03, 0.04], id); // brow ridge
                for sx in [-1.0f32, 1.0] {
                    s.part_s(head, Role::Hot, Src::Glow, [0.05 * sx, -0.10, -0.138], [0.025, 0.06, 0.02], id); // fangs
                    s.part_s(head, Role::Glow, Src::Glow, [0.085 * sx, 0.17, -0.06], [0.03, 0.09, 0.03], rz(-0.35 * sx)); // small horns
                }
            }
            _ => {
                // fox: pointed ears, snout, whisker lines
                s.part_far(head, Role::Accent, Src::Trim, [0.0, -0.03, -0.118], [0.225, 0.2, 0.03], id);
                s.part_far(head, Role::Accent, Src::Trim, [0.0, -0.075, -0.15], [0.08, 0.06, 0.07], id);
                for sx in [-1.0f32, 1.0] {
                    s.part_far(head, Role::Accent, Src::Trim, [0.075 * sx, 0.20, -0.02], [0.07, 0.12, 0.03], rz(-0.3 * sx));
                    for dy in [-0.045f32, -0.085] {
                        s.part_s(head, Role::Glow, Src::Glow, [0.13 * sx, dy, -0.122], [0.07, 0.012, 0.012], rz(0.15 * sx));
                    }
                }
            }
        }
        // glowing eyes
        match sp.visor_style {
            0 => {
                s.part_s(head, Role::Hot, Src::Visor, [0.0, 0.03, -0.124], [0.20, 0.034, 0.02], id); // visor slit
            }
            1 => {
                for sx in [-1.0f32, 1.0] {
                    s.part_s(head, Role::Hot, Src::Visor, [0.055 * sx, 0.03, -0.124], [0.075, 0.03, 0.02], id);
                }
            }
            3 => {
                // cat: big slanted eyes with a dark vertical pupil
                for sx in [-1.0f32, 1.0] {
                    s.part_s(head, Role::Hot, Src::Visor, [0.058 * sx, 0.035, -0.126], [0.085, 0.07, 0.02], rz(0.25 * sx));
                    s.part_s(head, Role::Dark, Src::Base, [0.058 * sx, 0.035, -0.139], [0.014, 0.06, 0.008], id);
                }
            }
            4 => {
                // round: two big square eyes with a dark dot
                for sx in [-1.0f32, 1.0] {
                    s.part_s(head, Role::Hot, Src::Visor, [0.056 * sx, 0.035, -0.126], [0.07, 0.07, 0.02], id);
                    s.part_s(head, Role::Dark, Src::Base, [0.056 * sx, 0.035, -0.139], [0.026, 0.026, 0.008], id);
                }
            }
            5 => {
                // angry: slits tilted down toward the nose
                for sx in [-1.0f32, 1.0] {
                    s.part_s(head, Role::Hot, Src::Visor, [0.058 * sx, 0.03, -0.126], [0.095, 0.032, 0.02], rz(0.4 * sx));
                }
            }
            _ => {
                s.part_s(head, Role::Hot, Src::Visor, [0.0, 0.03, -0.126], [0.225, 0.05, 0.02], id);
            }
        }
        for sx in [-1.0f32, 1.0] {
            s.part_s(head, Role::Glow, Src::Glow, [0.105 * sx, 0.03, -0.085], [0.016, 0.03, 0.07], id);
        }
        // headband tails (and the band itself when there is no hood over it)
        if sp.headband {
            s.part_s(head, Role::Accent, Src::Trim, [0.0, 0.07, 0.15], [0.05, 0.05, 0.26], rx(0.25));
            if !hooded {
                s.part_far(head, Role::Accent, Src::Trim, [0.0, 0.075, 0.0], [0.24, 0.045, 0.24], id);
            }
        }
        // hair (only shows when the hood is open or off)
        if !hooded && sp.hair_style != 0 {
            s.part_s(head, Role::Plain, Src::Hair, [0.0, 0.115, 0.0], [0.245, 0.07, 0.245], id); // cap of hair
            s.part_s(head, Role::Plain, Src::Hair, [0.0, 0.02, 0.121], [0.245, 0.2, 0.04], id);
            match sp.hair_style {
                1 => {
                    s.part_s(head, Role::Plain, Src::Hair, [0.0, 0.17, 0.02], [0.05, 0.06, 0.05], id);
                    s.part_s(head, Role::Plain, Src::Hair, [0.0, 0.215, 0.02], [0.1, 0.09, 0.1], id);
                }
                2 => {
                    s.part_s(head, Role::Plain, Src::Hair, [0.0, 0.08, 0.16], [0.07, 0.07, 0.1], id);
                    s.part_s(head, Role::Plain, Src::Hair, [0.0, 0.0, 0.2], [0.06, 0.13, 0.06], id);
                    s.part_s(head, Role::Plain, Src::Hair, [0.0, -0.13, 0.215], [0.045, 0.15, 0.045], id);
                }
                3 => {
                    for k in -2i32..=2 {
                        let x = k as f32 * 0.05;
                        s.part_s(head, Role::Plain, Src::Hair, [x, 0.2, 0.0], [0.04, 0.15, 0.04], rz(-x * 3.0));
                    }
                }
                _ => {
                    s.part_s(head, Role::Plain, Src::Hair, [0.0, -0.1, 0.135], [0.25, 0.42, 0.04], id);
                    for sx in [-1.0f32, 1.0] {
                        s.part_s(head, Role::Plain, Src::Hair, [0.125 * sx, -0.08, -0.03], [0.04, 0.3, 0.06], id);
                    }
                }
            }
        }
        if sp.scarf_style == 3 {
            // scarf tails also hang from the back of the head
            for sx in [-1.0f32, 1.0] {
                s.part_s(head, Role::Accent, Src::Scarf, [0.05 * sx, -0.12, 0.17], [0.04, 0.3, 0.015], rz(0.08 * sx));
            }
        }
        if sp.has_acc(ACC_HORNS) {
            for sx in [-1.0f32, 1.0] {
                s.part_far(head, Role::Accent, Src::Trim, [0.09 * sx, 0.25, 0.0], [0.045, 0.2, 0.045], rz(-0.4 * sx));
            }
        }
        if sp.has_acc(ACC_EARRING) {
            s.part_s(head, Role::Glow, Src::Glow, [0.128, -0.045, 0.0], [0.025, 0.05, 0.025], id);
        }
        if sp.has_acc(ACC_GLASSES) {
            for sx in [-1.0f32, 1.0] {
                s.part_s(head, Role::Glow, Src::Glow, [0.058 * sx, 0.035, -0.14], [0.09, 0.06, 0.012], id);
            }
            s.part_s(head, Role::Glow, Src::Glow, [0.0, 0.045, -0.14], [0.04, 0.012, 0.012], id);
        }
        // v1.1 head parts (far LOD hides them all: the bright core carries the read)
        let mut ears = Vec::new();
        if sp.ears != 0 {
            for (i, sx) in [-1.0f32, 1.0].into_iter().enumerate() {
                let (pos, tilt) = match sp.ears {
                    1 => ([0.135 * sx, 0.06, 0.02], 1.0),
                    2 => ([0.135 * sx, 0.03, 0.02], 0.55),
                    3 => ([0.07 * sx, 0.11, 0.03], 0.18),
                    4 => ([0.085 * sx, 0.12, 0.02], 0.3),
                    _ => ([0.075 * sx, 0.12, 0.0], 0.28),
                };
                let rest = rz(-tilt * sx);
                let e = s.pivot_at(head, pos, rest);
                match sp.ears {
                    1 => {
                        s.part_far(e, Role::Plain, Src::Skin, [0.0, 0.07, 0.0], [0.045, 0.14, 0.07], id);
                        s.part_far(e, Role::Plain, Src::Skin, [0.0, 0.17, 0.0], [0.025, 0.08, 0.05], id);
                    }
                    2 => {
                        s.part_far(e, Role::Plain, Src::Skin, [0.0, 0.1, 0.0], [0.04, 0.2, 0.17], id);
                        s.part_far(e, Role::Accent, Src::Trim, [-0.012 * sx, 0.1, -0.005], [0.014, 0.14, 0.11], id);
                    }
                    3 => {
                        s.part_far(e, Role::Plain, Src::Skin, [0.0, 0.17, 0.0], [0.05, 0.34, 0.07], id);
                        s.part_far(e, Role::Accent, Src::Trim, [0.0, 0.17, -0.03], [0.022, 0.26, 0.016], id);
                    }
                    4 => {
                        s.part_far(e, Role::Plain, Src::Skin, [0.0, 0.03, 0.0], [0.09, 0.09, 0.05], id);
                        s.part_far(e, Role::Accent, Src::Trim, [0.0, 0.03, -0.022], [0.05, 0.05, 0.012], id);
                    }
                    _ => {
                        // cat: a diamond with a lit inner
                        s.part_far(e, Role::Plain, Src::Skin, [0.0, 0.05, 0.0], [0.085, 0.085, 0.05], rz(0.785));
                        s.part_far(e, Role::Glow, Src::Glow, [0.0, 0.045, -0.026], [0.04, 0.04, 0.01], rz(0.785));
                    }
                }
                ears.push((e, rest, i as f32 * 1.7));
            }
        }
        match sp.horns {
            1 => {
                for sx in [-1.0f32, 1.0] {
                    s.part_far(head, Role::Accent, Src::Trim, [0.07 * sx, 0.17, -0.02], [0.04, 0.1, 0.04], rz(-0.3 * sx));
                }
            }
            2 => {
                for sx in [-1.0f32, 1.0] {
                    s.part_far(head, Role::Accent, Src::Trim, [0.08 * sx, 0.18, -0.02], [0.065, 0.13, 0.065], rz(-0.25 * sx));
                    s.part_far(head, Role::Glow, Src::Glow, [0.125 * sx, 0.275, -0.02], [0.04, 0.12, 0.04], rz(-0.5 * sx));
                }
            }
            3 => {
                for sx in [-1.0f32, 1.0] {
                    s.part_far(head, Role::Accent, Src::Trim, [0.09 * sx, 0.27, 0.0], [0.03, 0.26, 0.03], rz(-0.3 * sx));
                    s.part_far(head, Role::Accent, Src::Trim, [0.15 * sx, 0.34, 0.0], [0.025, 0.12, 0.025], rz(-0.8 * sx));
                    s.part_far(head, Role::Accent, Src::Trim, [0.075 * sx, 0.3, 0.0], [0.025, 0.12, 0.025], rz(0.7 * sx));
                }
            }
            _ => {}
        }
        match sp.nose {
            1 => {
                s.part_far(head, Role::Plain, Src::Skin, [0.0, -0.02, -0.15], [0.075, 0.085, 0.07], id);
            }
            2 => {
                s.part_far(head, Role::Plain, Src::Skin, [0.0, -0.055, -0.19], [0.1, 0.075, 0.14], id);
                s.part_far(head, Role::Dark, Src::Base, [0.0, -0.04, -0.262], [0.05, 0.04, 0.03], id);
            }
            _ => {}
        }
        // expression: small lit marks on the face, only where a face is actually visible
        if (sp.hood != 0 || sp.mask <= 1) && sp.expression != 0 {
            let z = -0.143;
            let mut mark = |p: [f32; 2], size: [f32; 2], rot: f32| {
                s.part_far(head, Role::Glow, Src::Glow, [p[0], p[1], z], [size[0], size[1], 0.01], rz(rot));
            };
            match sp.expression {
                1 => {
                    mark([0.0, -0.085, ], [0.06, 0.014], 0.0);
                    for sx in [-1.0f32, 1.0] {
                        mark([0.045 * sx, -0.072], [0.03, 0.014], -0.6 * sx);
                    }
                }
                2 => {
                    mark([0.0, -0.08], [0.15, 0.04], 0.0);
                    mark([0.0, -0.08], [0.012, 0.04], 0.0);
                    for sx in [-1.0f32, 1.0] {
                        mark([0.085 * sx, -0.062], [0.025, 0.014], -0.7 * sx);
                    }
                }
                3 => {
                    for sx in [-1.0f32, 1.0] {
                        mark([0.058 * sx, 0.085], [0.09, 0.016], 0.4 * sx);
                    }
                    mark([0.0, -0.085], [0.07, 0.014], 0.0);
                }
                4 => {
                    mark([0.0, -0.085], [0.04, 0.05], 0.0);
                    for sx in [-1.0f32, 1.0] {
                        mark([0.058 * sx, 0.09], [0.07, 0.014], 0.0);
                    }
                }
                _ => {
                    for sx in [-1.0f32, 1.0] {
                        mark([0.058 * sx, 0.065], [0.09, 0.016], 0.0);
                    }
                    mark([0.0, -0.085], [0.04, 0.012], 0.0);
                }
            }
        }

        // ---- arms: pivot at the shoulder (torso space is applied at update time) ----
        let hand_role = if sp.rgb(C_GLOVE).is_some() || sp.rgb(C_SKIN).is_some() { Role::Plain } else { Role::Accent };
        let arm = |s: &mut Spawner, launcher: bool| -> Entity {
            let a = s.pivot(root);
            s.part_s(a, Role::Dark, Src::Base, [0.0, -0.29, 0.0], [0.09, 0.58, 0.09], id);
            s.part_s(a, Role::Glow, Src::Glow, [0.0, -0.44, 0.0], [0.108, 0.06, 0.108], id);
            s.part_s(a, hand_role, Src::Glove, [0.0, -0.61, 0.0], [0.075, 0.085, 0.075], id);
            if launcher {
                // wrist grapple launcher: a chunky bracer with a lit barrel (its muzzle is where the rope starts)
                s.part_s(a, Role::Dark, Src::Base, [0.0, -0.40, -0.07], [0.13, 0.17, 0.08], id);
                s.part_s(a, Role::Glow, Src::Glow, [0.0, -0.40, -0.113], [0.05, 0.15, 0.012], id);
                s.part_s(a, Role::Hot, Src::Glow, [0.0, -0.50, -0.07], [0.05, 0.04, 0.05], id);
            }
            a
        };
        let arm_l = arm(&mut s, true);
        let arm_r = arm(&mut s, false);

        // ---- legs: lit greaves on the existing leg box, and two animated feet ----
        // (greaves keep the match colour unless the character has glowing tabi, so teams still read from the legs)
        let legs = s.pivot(root);
        s.part_s(legs, Role::Glow, tabi, [0.0, -0.62, 0.0], [0.375, 0.045, 0.255], id); // ankle
        s.part_s(legs, Role::Glow, tabi, [0.0, 0.05, 0.0], [0.375, 0.03, 0.255], id); // knee
        s.part_s(legs, Role::Dark, Src::Base, [0.0, -0.3, -0.115], [0.3, 0.3, 0.03], id); // shin guard
        let foot = |s: &mut Spawner| -> Entity {
            let f = s.pivot(root);
            s.part_s(f, Role::Dark, Src::Base, [0.0, 0.0, -0.03], [0.13, 0.08, 0.3], id);
            s.part_s(f, Role::Glow, tabi, [0.0, 0.043, -0.03], [0.08, 0.012, 0.26], id);
            if tabi == Src::Glow {
                s.part_s(f, Role::Glow, tabi, [0.0, -0.036, -0.03], [0.15, 0.012, 0.32], id); // sole glow
            }
            f
        };
        let foot_l = foot(&mut s);
        let foot_r = foot(&mut s);

        // ---- katana at the hip: lit edge + guard + wrapped grip (the body box is the blade core) ----
        let katana = s.pivot(root);
        s.part_s(katana, Role::Hot, Src::Edge, [0.0, 0.0, 0.0], [0.016, 0.05, 1.12], id);
        s.part_s(katana, Role::Accent, Src::Trim, [0.0, 0.0, -0.22], [0.11, 0.11, 0.02], id);
        s.part_s(katana, Role::Dark, Src::Base, [0.0, 0.0, -0.4], [0.055, 0.055, 0.26], id);

        // ---- spinning star glyph at the left hip: two crossed bars + a 45 degree pair ----
        let glyph = s.pivot(root);
        for a in [0.0f32, 0.785, 1.571, 2.356] {
            s.part_s(glyph, Role::Hot, Src::Star, [0.0, 0.0, 0.0], [0.2, 0.012, 0.028], Quat::from_rotation_y(a));
        }

        // ---- scarf: loose segments, positioned each frame from the spring chain ----
        let scarf = std::array::from_fn(|i| {
            let w = 0.14 - 0.015 * i as f32;
            let (role, src) = if i + 1 == SCARF_SEGS { (Role::Glow, Src::Glow) } else { (Role::Accent, Src::Scarf) };
            s.part_s(root, role, src, [0.0, 1.3, 0.1], [w, 0.02, 0.2], id)
        });

        let pivots = vec![torso, head, arm_l, arm_r, legs, foot_l, foot_r, katana, glyph];
        let mut roles = std::mem::take(&mut s.roles);
        let dark = std::mem::take(&mut s.dark);
        roles.shrink_to_fit();
        // scarf length: 4 = stock, +-15% per step; "long" is longer again; "tails" are narrow
        let scarf_len = (1.0 + (sp.scarf_len_q as f32 - 4.0) * 0.15) * if sp.scarf_style == 2 { 1.35 } else { 1.0 };
        let scarf_w = match sp.scarf_style {
            3 => 0.55,
            2 => 1.2,
            _ => 1.0,
        };
        Rig {
            torso,
            head,
            arm_l,
            arm_r,
            legs,
            foot_l,
            foot_r,
            katana,
            glyph,
            scarf,
            ears,
            tail,
            shoulders: sh,
            roles,
            dark,
            pivots,
            phase: 0.0,
            scarf_pts: None,
            bulk: sp.bulk(),
            height: sp.height(),
            scarf_on: sp.scarf_style != 0,
            scarf_len,
            scarf_w,
        }
    }

    /// Root scale for the visual build (about the model origin; the hitbox never changes). Stock = identity.
    pub fn root_scale(&self) -> Vec3 {
        Vec3::new(self.bulk, self.height, self.bulk)
    }

    /// Vertical lift that keeps the feet on the ground under `root_scale`.
    pub fn foot_lift(&self) -> f32 {
        0.81 * (self.height - 1.0)
    }

    /// Remove every entity of this rig (before spawning another look).
    pub fn despawn(&self, commands: &mut Commands) {
        for e in self.pivots.iter().chain(self.scarf.iter()) {
            commands.entity(*e).despawn();
        }
    }

    /// (Re)skin every part for a player colour and (optionally) a custom look.
    pub fn skin(&self, commands: &mut Commands, cache: &mut LookMats, mats: &mut Assets<StandardMaterial>, hue: [f32; 3], spec: Option<&CharacterSpec>) {
        for (e, role, src) in &self.roles {
            let c = paint(*role, *src, hue, spec);
            let h = cache
                .0
                .entry(colour_key(c))
                .or_insert_with(|| mats.add(StandardMaterial { base_color: Color::linear_rgb(c[0], c[1], c[2]), unlit: true, ..default() }))
                .clone();
            commands.entity(*e).insert(MeshMaterial3d(h));
        }
    }

    /// Paint every rig part with one material (the hit flash); `skin` restores the colours.
    pub fn flash(&self, commands: &mut Commands, mat: &Handle<StandardMaterial>) {
        for (e, _, _) in &self.roles {
            commands.entity(*e).insert(MeshMaterial3d(mat.clone()));
        }
    }

    /// Pose and place everything for this frame.
    pub fn update(&mut self, c: &Ctx, tf: &mut Query<&mut Transform>, vis: &mut Query<&mut Visibility>) {
        let near = c.dist < GLOW_MAX;
        let dark_on = c.dist < DARK_MAX;
        for e in &self.pivots {
            if let Ok(mut v) = vis.get_mut(*e) {
                v.set_if_neq(if near { Visibility::Inherited } else { Visibility::Hidden });
            }
        }
        for e in &self.scarf {
            if let Ok(mut v) = vis.get_mut(*e) {
                v.set_if_neq(if near && self.scarf_on { Visibility::Inherited } else { Visibility::Hidden });
            }
        }
        for e in &self.dark {
            if let Ok(mut v) = vis.get_mut(*e) {
                v.set_if_neq(if dark_on { Visibility::Inherited } else { Visibility::Hidden });
            }
        }
        if !near {
            self.scarf_pts = None;
            return;
        }

        let speed = c.vel.length();
        self.phase = (self.phase + c.dt * speed * 1.25) % std::f32::consts::TAU;
        let pz = pose(c.kind, c.flags, c.fx, speed, self.phase, c.t, c.anchor.is_some());
        let part = |n: &str| c.draw.parts.iter().find(|p| p.name == n);
        let (bw, bh) = c.boost;
        let root_q = Quat::from_rotation_y(c.draw.yaw) * Quat::from_rotation_z(c.draw.roll);
        // the root carries the visual build (scale + foot lift); model-space maths below is unscaled, world points
        // are mapped through the same transform
        let rs = self.root_scale();
        let root_pos = Vec3::from(c.draw.pos) + Vec3::Y * self.foot_lift();

        let mut shoulder_y = 0.0;
        let mut torso_xf = Transform::IDENTITY;
        if let Some(p) = part("torso") {
            let sy = p.half[1] * bh / 0.357;
            torso_xf = Transform { translation: Vec3::from(p.offset), rotation: Quat::from_rotation_x(p.pitch), scale: Vec3::new(bw, sy, bw) };
            shoulder_y = 0.30 * sy;
            if let Ok(mut t) = tf.get_mut(self.torso) {
                *t = torso_xf;
            }
        }
        if let Some(p) = part("head") {
            if let Ok(mut t) = tf.get_mut(self.head) {
                *t = Transform { translation: Vec3::from(p.offset), rotation: Quat::from_rotation_x(p.pitch), scale: Vec3::new(bw, bh, bw) };
            }
        }
        if let Some(p) = part("legs") {
            if let Ok(mut t) = tf.get_mut(self.legs) {
                // nominal leg half height 0.3645 (standing): the greave bands ride the box as it tucks
                *t = Transform {
                    translation: Vec3::from(p.offset),
                    rotation: Quat::from_rotation_x(p.pitch),
                    scale: Vec3::new(bw, p.half[1] * bh / 0.3645, bw),
                };
            }
            // feet: alternate fore/aft with the stride, raised and folded in the air
            let foot_y = 0.04 + pz.lift;
            for (e, sign, sx) in [(self.foot_l, 1.0f32, -1.0f32), (self.foot_r, -1.0, 1.0)] {
                if let Ok(mut t) = tf.get_mut(e) {
                    t.translation = Vec3::new(0.09 * sx * bw, foot_y + 0.05 * pz.stride.abs(), -pz.stride * sign);
                    t.scale = Vec3::new(bw, 1.0, bw);
                }
            }
        }
        if let Some(p) = part("katana") {
            if let Ok(mut t) = tf.get_mut(self.katana) {
                *t = Transform { translation: Vec3::from(p.offset), rotation: Quat::from_rotation_x(p.pitch), scale: Vec3::new(bw, bw, 1.0) };
            }
        }
        // arms hang from the shoulders in torso space
        let sx = 0.29 * bw * self.shoulders;
        let shoulder = |side: f32| torso_xf.translation + torso_xf.rotation * Vec3::new(sx * side, shoulder_y, 0.0);
        let arm_rot = |a: Arm, side: f32| torso_xf.rotation * Quat::from_rotation_z(-a.splay * side) * Quat::from_rotation_x(a.swing);
        // right arm is +X (the katana side), left is -X (launcher side)
        let mut left_q = arm_rot(pz.left, -1.0);
        if let Some(anchor) = c.anchor {
            // the launcher arm points at the anchor
            let local = (root_q.inverse() * (anchor - root_pos)) / rs - shoulder(-1.0);
            left_q = Quat::from_rotation_arc(Vec3::NEG_Y, local.normalize_or(Vec3::NEG_Z));
        }
        for (e, side, q) in [(self.arm_l, -1.0f32, left_q), (self.arm_r, 1.0, arm_rot(pz.right, 1.0))] {
            if let Ok(mut t) = tf.get_mut(e) {
                *t = Transform { translation: shoulder(side), rotation: q, scale: Vec3::new(bw, bh, bw) };
            }
        }
        // glyph: spins over the left hip, bobbing
        if let Ok(mut t) = tf.get_mut(self.glyph) {
            t.translation = Vec3::new(-0.36 * bw, 0.9 * bh + 0.03 * (c.t * 3.0).sin(), 0.05);
            t.rotation = Quat::from_rotation_x(1.4) * Quat::from_rotation_z(c.t * 7.0);
            t.scale = Vec3::splat(bw.min(2.0));
        }

        // idle sway: ears and tail wag slowly and trail back with speed
        let run = (speed / 9.0).clamp(0.0, 1.0);
        for (e, rest, ph) in &self.ears {
            if let Ok(mut t) = tf.get_mut(*e) {
                t.rotation = *rest * Quat::from_rotation_z(0.07 * (c.t * 2.3 + ph).sin()) * Quat::from_rotation_x(0.35 * run + 0.03 * (c.t * 1.3 + ph).sin());
            }
        }
        if let Some(tail) = self.tail {
            if let Ok(mut t) = tf.get_mut(tail) {
                t.rotation = Quat::from_rotation_y(0.28 * (c.t * 1.7).sin() * (1.0 - 0.6 * run)) * Quat::from_rotation_x(-0.05 + 0.3 * run + 0.04 * (c.t * 1.1).sin());
            }
        }

        // scarf: spring chain in world space, drawn in root space
        let neck_local = torso_xf.translation + torso_xf.rotation * Vec3::new(0.0, 0.357 * torso_xf.scale.y * 0.9, 0.10);
        let anchor = root_pos + root_q * (neck_local * rs);
        let back = root_q * Vec3::Z;
        let pts = self.scarf_pts.get_or_insert_with(|| {
            let mut p = [anchor; SCARF_SEGS + 1];
            for (i, q) in p.iter_mut().enumerate() {
                *q = anchor + back * 0.05 * i as f32 - Vec3::Y * 0.15 * i as f32;
            }
            p
        });
        scarf_step_len(pts, anchor, back, c.vel, c.dt, self.scarf_len);
        let inv = root_q.inverse();
        for i in 0..SCARF_SEGS {
            let (a, b) = ((inv * (pts[i] - root_pos)) / rs, (inv * (pts[i + 1] - root_pos)) / rs);
            let d = b - a;
            let len = d.length().max(1e-3);
            if let Ok(mut t) = tf.get_mut(self.scarf[i]) {
                t.translation = (a + b) * 0.5;
                t.rotation = Quat::from_rotation_arc(Vec3::Z, d / len);
                t.scale = Vec3::new((0.14 - 0.015 * i as f32) * bw.min(2.0) * self.scarf_w, 0.02, len * 1.1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_swings_arms_in_opposition_idle_barely() {
        let a = pose(MoveKind::Grounded, 0, 0, 9.0, 1.2, 0.0, false);
        assert!(a.left.swing * a.right.swing < 0.0 && a.left.swing.abs() > 0.5);
        let idle = pose(MoveKind::Grounded, 0, 0, 0.0, 1.2, 0.0, false);
        assert!(idle.left.swing.abs() < 0.1 && idle.stride == 0.0);
    }

    #[test]
    fn move_kinds_have_distinct_arms() {
        let air = pose(MoveKind::Airborne, 0, 0, 5.0, 0.0, 0.0, false);
        assert!(air.left.splay > 0.7 && air.lift > 0.1, "air: arms out, feet tucked up");
        let dash = pose(MoveKind::Evading, 0, 0, 30.0, 0.0, 0.0, false);
        assert!(dash.left.swing < -1.0, "dash: arms swept back");
        let roll = pose(MoveKind::Evading, FLAG_ROLL, 0, 10.0, 0.0, 0.0, false);
        assert!(roll.left.swing > 0.8, "roll: arms tucked forward");
        let wall = pose(MoveKind::WallRun, 0, 0, 9.0, 0.0, 0.0, false);
        assert!(wall.left.splay > 0.5 && wall.right.splay > 0.5);
    }

    #[test]
    fn block_and_strike_raise_the_arms() {
        let guard = pose(MoveKind::Grounded, 0, FX_BLOCK, 9.0, 1.0, 0.0, false);
        assert!(guard.left.swing > 1.0 && guard.right.swing > 1.0);
        let strike = pose(MoveKind::Grounded, 0, FX_OPEN, 0.0, 0.0, 0.0, false);
        assert!(strike.right.swing > 1.0 && strike.left.swing.abs() < 0.2);
        let grapple = pose(MoveKind::Airborne, 0, 0, 20.0, 0.0, 0.0, true);
        assert!(grapple.right.swing < 0.0, "free arm trails during a grapple");
    }

    fn settle(vel: Vec3) -> [Vec3; SCARF_SEGS + 1] {
        let anchor = Vec3::new(0.0, 1.4, 0.0);
        let mut p = [anchor - Vec3::Y * 0.1; SCARF_SEGS + 1];
        for _ in 0..240 {
            scarf_step(&mut p, anchor, Vec3::Z, vel, 1.0 / 60.0);
        }
        p
    }

    #[test]
    fn scarf_hangs_at_rest_and_streams_behind_at_speed() {
        let rest = settle(Vec3::ZERO);
        let fast = settle(Vec3::new(0.0, 0.0, -14.0)); // running toward -Z, so the scarf trails toward +Z
        let tip = |p: &[Vec3; SCARF_SEGS + 1]| p[SCARF_SEGS] - p[0];
        assert!(tip(&rest).y < -0.3, "hangs down at rest: {:?}", tip(&rest));
        assert!(tip(&fast).z > 0.45 && tip(&fast).y > tip(&rest).y + 0.2, "streams back at speed: {:?}", tip(&fast));
        for w in fast.windows(2) {
            assert!((w[0].distance(w[1]) - w[0].distance(w[1]).clamp(0.05, 0.2)).abs() < 1e-5, "link length kept");
        }
    }

    #[test]
    fn scarf_survives_a_frame_hitch() {
        let mut p = settle(Vec3::ZERO);
        scarf_step(&mut p, Vec3::new(50.0, 1.4, 0.0), Vec3::Z, Vec3::new(40.0, 0.0, 0.0), 5.0);
        assert!(p.iter().all(|v| v.is_finite()));
    }

    const ALL_SRC: [Src; 11] = [Src::Match, Src::Base, Src::Trim, Src::Glow, Src::Visor, Src::Glove, Src::Scarf, Src::Hair, Src::Skin, Src::Edge, Src::Star];
    const ALL_ROLES: [Role; 5] = [Role::Dark, Role::Accent, Role::Glow, Role::Hot, Role::Plain];

    #[test]
    fn stock_and_default_specs_paint_exactly_the_stock_colours() {
        let hue = [0.1, 0.9, 0.7];
        let d = CharacterSpec::default();
        for role in ALL_ROLES {
            for src in ALL_SRC {
                assert_eq!(paint(role, src, hue, None), role_colour(role, hue));
                assert_eq!(paint(role, src, hue, Some(&d)), role_colour(role, hue), "{role:?} {src:?}");
            }
        }
    }

    #[test]
    fn custom_colours_replace_only_their_own_slot_and_keep_the_hot_core_rules() {
        let mut sp = CharacterSpec::default();
        sp.col[C_GLOW] = pack565([255, 79, 216]);
        sp.col[C_SKIN] = pack565([224, 180, 148]);
        sp.sanitize();
        let hue = [0.0, 0.9, 1.0];
        // glow slot: lifted like the stock glow role, from the character's colour
        let g = paint(Role::Glow, Src::Glow, hue, Some(&sp));
        assert!(g[0] > g[1] && g[2] > g[1], "pink glow stays pink: {g:?}");
        assert!(g.iter().copied().fold(0.0, f32::max) > 2.5, "HDR boosted like the stock glow");
        // edge falls back to the glow colour; the visor is unset so it keeps the match colour
        assert_eq!(paint(Role::Hot, Src::Edge, hue, Some(&sp)), role_colour(Role::Hot, sp.rgb(C_GLOW).unwrap().map(|v| v as f32 / 255.0)));
        assert_eq!(paint(Role::Hot, Src::Visor, hue, Some(&sp)), role_colour(Role::Hot, hue));
        // gloves fall back to skin; plain slots are drawn exactly as given
        let skin = sp.rgb(C_SKIN).unwrap().map(|v| v as f32 / 255.0);
        assert_eq!(paint(Role::Plain, Src::Glove, hue, Some(&sp)), skin);
    }

    #[test]
    fn glyph_sheet_has_fifteen_distinct_glyphs() {
        assert!(glyph_segments(0).is_empty() && glyph_segments(16).is_empty());
        let all: Vec<_> = (1..=15u8).map(glyph_segments).collect();
        for (i, g) in all.iter().enumerate() {
            assert!(!g.is_empty() && g.len() <= 4, "glyph {}", i + 1);
            assert!(g.iter().all(|s| s[..2].iter().all(|v| v.abs() <= 1.0) && s[2] > 0.0 && s[3] > 0.0));
            for h in &all[i + 1..] {
                assert_ne!(g, h);
            }
        }
    }

    #[test]
    fn longer_scarf_links_reach_further() {
        let anchor = Vec3::new(0.0, 1.4, 0.0);
        let reach = |len: f32| {
            let mut p = [anchor - Vec3::Y * 0.1; SCARF_SEGS + 1];
            for _ in 0..240 {
                scarf_step_len(&mut p, anchor, Vec3::Z, Vec3::ZERO, 1.0 / 60.0, len);
            }
            (p[SCARF_SEGS] - p[0]).length()
        };
        assert!(reach(1.5) > reach(1.0) * 1.3 && reach(0.5) < reach(1.0) * 0.7);
    }

    #[test]
    fn colour_keys_distinguish_nearby_colours() {
        assert_ne!(colour_key([0.1, 0.2, 0.3]), colour_key([0.1, 0.2, 0.31]));
        assert_eq!(colour_key([1.0, 2.0, 3.0]), colour_key([1.0, 2.0, 3.0]));
    }

    #[test]
    fn role_colours_order_dark_to_hot() {
        let hue = [0.1, 0.8, 1.0];
        let lum = |r: Role| role_colour(r, hue).iter().sum::<f32>();
        assert!(lum(Role::Dark) < lum(Role::Accent) && lum(Role::Accent) < lum(Role::Glow) && lum(Role::Glow) < lum(Role::Hot));
    }
}
