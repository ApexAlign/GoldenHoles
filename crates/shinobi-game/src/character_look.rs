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
}

/// Material handles per quantised colour, indexed by `Role as usize`.
#[derive(Resource, Default)]
pub struct LookMats(HashMap<u32, [Handle<StandardMaterial>; 4]>);

fn role_colour(role: Role, hue: [f32; 3]) -> [f32; 3] {
    let lift = |w: f32, g: f32| -> [f32; 3] { std::array::from_fn(|i| (hue[i] + (1.0 - hue[i]) * w) * g) };
    match role {
        Role::Dark => lift(0.0, 0.32),
        Role::Accent => lift(0.1, 1.0),
        Role::Glow => lift(0.3, 3.0),
        Role::Hot => lift(0.85, 4.5),
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
    /// Materials are applied per role, so remember who has which.
    roles: Vec<(Entity, Role)>,
    /// Entities hidden beyond `DARK_MAX`.
    dark: Vec<Entity>,
    /// Pivots hidden beyond `GLOW_MAX` (children follow).
    pivots: Vec<Entity>,
    phase: f32,
    /// Scarf points in world space (anchor first); `None` until the first update.
    scarf_pts: Option<[Vec3; SCARF_SEGS + 1]>,
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
    let dt = dt.clamp(0.0, 0.05);
    pts[0] = anchor;
    let speed = vel.length();
    let wind = if speed > 0.1 { -vel / speed } else { Vec3::ZERO };
    for i in 1..=SCARF_SEGS {
        let seg = 0.2 - 0.02 * i as f32;
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
    roles: Vec<(Entity, Role)>,
    dark: Vec<Entity>,
}

impl Spawner<'_, '_, '_> {
    fn part(&mut self, parent: Entity, role: Role, pos: [f32; 3], size: [f32; 3], rot: Quat) -> Entity {
        let e = self
            .commands
            .spawn((
                Mesh3d(self.cube.clone()),
                Transform { translation: Vec3::from(pos), rotation: rot, scale: Vec3::from(size) },
                Visibility::default(),
                ChildOf(parent),
            ))
            .id();
        self.roles.push((e, role));
        if role == Role::Dark {
            self.dark.push(e);
        }
        e
    }

    fn pivot(&mut self, parent: Entity) -> Entity {
        self.commands.spawn((Transform::default(), Visibility::default(), ChildOf(parent))).id()
    }
}

impl Rig {
    /// Spawn the whole rig under `root` (model space: feet at the origin, facing -Z).
    pub fn spawn(commands: &mut Commands, root: Entity, cube: &Handle<Mesh>) -> Rig {
        let mut s = Spawner { commands, cube, roles: Vec::new(), dark: Vec::new() };
        let id = Quat::IDENTITY;
        let rz = Quat::from_rotation_z;
        let rx = Quat::from_rotation_x;

        // ---- torso (nominal half extents 0.22 x 0.357 x 0.12, front is -Z) ----
        let torso = s.pivot(root);
        // chest plate + circuit lines + core diamond
        s.part(torso, Role::Dark, [0.0, 0.10, -0.125], [0.36, 0.30, 0.03], id);
        s.part(torso, Role::Glow, [0.0, 0.10, -0.145], [0.022, 0.28, 0.012], id);
        s.part(torso, Role::Glow, [0.0, 0.17, -0.145], [0.31, 0.018, 0.012], id);
        s.part(torso, Role::Glow, [0.0, 0.03, -0.145], [0.31, 0.018, 0.012], id);
        s.part(torso, Role::Hot, [0.0, 0.10, -0.152], [0.06, 0.06, 0.012], rz(0.785));
        // back plate + spine line
        s.part(torso, Role::Dark, [0.0, 0.10, 0.125], [0.36, 0.30, 0.03], id);
        s.part(torso, Role::Glow, [0.0, 0.10, 0.145], [0.022, 0.28, 0.012], id);
        // shoulder plates with a lit edge
        for sx in [-1.0f32, 1.0] {
            s.part(torso, Role::Dark, [0.27 * sx, 0.30, 0.0], [0.17, 0.05, 0.24], rz(-0.25 * sx));
            s.part(torso, Role::Glow, [0.335 * sx, 0.275, 0.0], [0.016, 0.02, 0.24], rz(-0.25 * sx));
        }
        // sash: waist band + knot + hanging tail
        s.part(torso, Role::Accent, [0.0, -0.30, 0.0], [0.47, 0.075, 0.27], id);
        s.part(torso, Role::Glow, [0.0, -0.30, -0.138], [0.45, 0.014, 0.01], id);
        s.part(torso, Role::Accent, [0.19, -0.33, -0.14], [0.07, 0.07, 0.04], rz(0.785));
        s.part(torso, Role::Accent, [0.21, -0.52, -0.145], [0.05, 0.27, 0.015], rz(0.12));
        // katana sheath across the back: dark scabbard, lit seam, hilt and tip glow
        let sheath_q = rz(0.8);
        s.part(torso, Role::Dark, [0.0, -0.02, 0.19], [0.05, 0.86, 0.05], sheath_q);
        s.part(torso, Role::Glow, [0.0, -0.02, 0.222], [0.012, 0.8, 0.012], sheath_q);
        s.part(torso, Role::Hot, [-0.31, 0.30, 0.19], [0.06, 0.1, 0.06], sheath_q);
        s.part(torso, Role::Glow, [0.31, -0.34, 0.19], [0.06, 0.05, 0.06], sheath_q);

        // ---- head: hood shell, mask, glowing visor slit (nominal half 0.11 x 0.12 x 0.11) ----
        let head = s.pivot(root);
        s.part(head, Role::Dark, [0.0, 0.145, 0.0], [0.27, 0.05, 0.27], id); // hood top
        s.part(head, Role::Dark, [0.0, 0.0, 0.125], [0.27, 0.29, 0.04], id); // hood back
        s.part(head, Role::Dark, [-0.13, 0.0, 0.0], [0.04, 0.29, 0.25], id);
        s.part(head, Role::Dark, [0.13, 0.0, 0.0], [0.04, 0.29, 0.25], id);
        s.part(head, Role::Dark, [0.0, 0.17, 0.17], [0.2, 0.04, 0.2], rx(-0.45)); // hood peak
        s.part(head, Role::Dark, [0.0, -0.065, -0.118], [0.22, 0.11, 0.03], id); // face mask
        s.part(head, Role::Dark, [0.0, 0.10, -0.118], [0.22, 0.07, 0.03], id); // brow band
        s.part(head, Role::Hot, [0.0, 0.03, -0.124], [0.20, 0.034, 0.02], id); // visor slit
        for sx in [-1.0f32, 1.0] {
            s.part(head, Role::Glow, [0.105 * sx, 0.03, -0.085], [0.016, 0.03, 0.07], id);
        }
        // headband tails
        s.part(head, Role::Accent, [0.0, 0.07, 0.15], [0.05, 0.05, 0.26], rx(0.25));

        // ---- arms: pivot at the shoulder (torso space is applied at update time) ----
        let arm = |s: &mut Spawner, launcher: bool| -> Entity {
            let a = s.pivot(root);
            s.part(a, Role::Dark, [0.0, -0.29, 0.0], [0.09, 0.58, 0.09], id);
            s.part(a, Role::Glow, [0.0, -0.44, 0.0], [0.108, 0.06, 0.108], id);
            s.part(a, Role::Accent, [0.0, -0.61, 0.0], [0.075, 0.085, 0.075], id);
            if launcher {
                // wrist grapple launcher: a chunky bracer with a lit barrel (its muzzle is where the rope starts)
                s.part(a, Role::Dark, [0.0, -0.40, -0.07], [0.13, 0.17, 0.08], id);
                s.part(a, Role::Glow, [0.0, -0.40, -0.113], [0.05, 0.15, 0.012], id);
                s.part(a, Role::Hot, [0.0, -0.50, -0.07], [0.05, 0.04, 0.05], id);
            }
            a
        };
        let arm_l = arm(&mut s, true);
        let arm_r = arm(&mut s, false);

        // ---- legs: lit greaves on the existing leg box, and two animated feet ----
        let legs = s.pivot(root);
        s.part(legs, Role::Glow, [0.0, -0.62, 0.0], [0.375, 0.045, 0.255], id); // ankle
        s.part(legs, Role::Glow, [0.0, 0.05, 0.0], [0.375, 0.03, 0.255], id); // knee
        s.part(legs, Role::Dark, [0.0, -0.3, -0.115], [0.3, 0.3, 0.03], id); // shin guard
        let foot = |s: &mut Spawner| -> Entity {
            let f = s.pivot(root);
            s.part(f, Role::Dark, [0.0, 0.0, -0.03], [0.13, 0.08, 0.3], id);
            s.part(f, Role::Glow, [0.0, 0.043, -0.03], [0.08, 0.012, 0.26], id);
            f
        };
        let foot_l = foot(&mut s);
        let foot_r = foot(&mut s);

        // ---- katana at the hip: lit edge + guard + wrapped grip (the body box is the blade core) ----
        let katana = s.pivot(root);
        s.part(katana, Role::Hot, [0.0, 0.0, 0.0], [0.016, 0.05, 1.12], id);
        s.part(katana, Role::Accent, [0.0, 0.0, -0.22], [0.11, 0.11, 0.02], id);
        s.part(katana, Role::Dark, [0.0, 0.0, -0.4], [0.055, 0.055, 0.26], id);

        // ---- spinning star glyph at the left hip: two crossed bars + a 45 degree pair ----
        let glyph = s.pivot(root);
        for a in [0.0f32, 0.785, 1.571, 2.356] {
            s.part(glyph, Role::Hot, [0.0, 0.0, 0.0], [0.2, 0.012, 0.028], Quat::from_rotation_y(a));
        }

        // ---- scarf: loose segments, positioned each frame from the spring chain ----
        let scarf = std::array::from_fn(|i| {
            let w = 0.14 - 0.015 * i as f32;
            let role = if i + 1 == SCARF_SEGS { Role::Glow } else { Role::Accent };
            s.part(root, role, [0.0, 1.3, 0.1], [w, 0.02, 0.2], id)
        });

        let pivots = vec![torso, head, arm_l, arm_r, legs, foot_l, foot_r, katana, glyph];
        let mut roles = std::mem::take(&mut s.roles);
        let dark = std::mem::take(&mut s.dark);
        roles.shrink_to_fit();
        Rig { torso, head, arm_l, arm_r, legs, foot_l, foot_r, katana, glyph, scarf, roles, dark, pivots, phase: 0.0, scarf_pts: None }
    }

    /// (Re)skin every part for a player colour.
    pub fn skin(&self, commands: &mut Commands, cache: &mut LookMats, mats: &mut Assets<StandardMaterial>, hue: [f32; 3], key: u32) {
        let set = cache.0.entry(key).or_insert_with(|| {
            [Role::Dark, Role::Accent, Role::Glow, Role::Hot].map(|r| {
                let c = role_colour(r, hue);
                mats.add(StandardMaterial { base_color: Color::linear_rgb(c[0], c[1], c[2]), unlit: true, ..default() })
            })
        });
        for (e, r) in &self.roles {
            commands.entity(*e).insert(MeshMaterial3d(set[*r as usize].clone()));
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
                v.set_if_neq(if near { Visibility::Inherited } else { Visibility::Hidden });
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
        let root_pos = Vec3::from(c.draw.pos);

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
        let sx = 0.29 * bw;
        let shoulder = |side: f32| torso_xf.translation + torso_xf.rotation * Vec3::new(sx * side, shoulder_y, 0.0);
        let arm_rot = |a: Arm, side: f32| torso_xf.rotation * Quat::from_rotation_z(-a.splay * side) * Quat::from_rotation_x(a.swing);
        // right arm is +X (the katana side), left is -X (launcher side)
        let mut left_q = arm_rot(pz.left, -1.0);
        if let Some(anchor) = c.anchor {
            // the launcher arm points at the anchor
            let local = root_q.inverse() * (anchor - root_pos) - shoulder(-1.0);
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

        // scarf: spring chain in world space, drawn in root space
        let neck_local = torso_xf.translation + torso_xf.rotation * Vec3::new(0.0, 0.357 * torso_xf.scale.y * 0.9, 0.10);
        let anchor = root_pos + root_q * neck_local;
        let back = root_q * Vec3::Z;
        let pts = self.scarf_pts.get_or_insert_with(|| {
            let mut p = [anchor; SCARF_SEGS + 1];
            for (i, q) in p.iter_mut().enumerate() {
                *q = anchor + back * 0.05 * i as f32 - Vec3::Y * 0.15 * i as f32;
            }
            p
        });
        scarf_step(pts, anchor, back, c.vel, c.dt);
        let inv = root_q.inverse();
        for i in 0..SCARF_SEGS {
            let (a, b) = (inv * (pts[i] - root_pos), inv * (pts[i + 1] - root_pos));
            let d = b - a;
            let len = d.length().max(1e-3);
            if let Ok(mut t) = tf.get_mut(self.scarf[i]) {
                t.translation = (a + b) * 0.5;
                t.rotation = Quat::from_rotation_arc(Vec3::Z, d / len);
                t.scale = Vec3::new((0.14 - 0.015 * i as f32) * bw.min(2.0), 0.02, len * 1.1);
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

    #[test]
    fn role_colours_order_dark_to_hot() {
        let hue = [0.1, 0.8, 1.0];
        let lum = |r: Role| role_colour(r, hue).iter().sum::<f32>();
        assert!(lum(Role::Dark) < lum(Role::Accent) && lum(Role::Accent) < lum(Role::Glow) && lum(Role::Glow) < lum(Role::Hot));
    }
}
