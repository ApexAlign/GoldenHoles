//! Destructible level boxes (docs/destructible.md), drawn: broken boxes are hidden (and shown again when they re-form),
//! a break throws tier-coloured neon debris, dust and sparks (staggered ~0.05 s per box on a cascade so a falling tower
//! reads as a collapse), a re-form pops a short neon flash, damage darkens the box with a crack overlay. Landmark
//! dressing (torii beams, lantern caps, pagoda canopies) is matched by exact half extents and goes with its supports.
//! Events drained during the first second after the level loads (the join catch-up) only hide, they never spawn debris.
//! Cosmetic only. `WorldFxFeed` republishes the drained events for other systems (audio) after `WorldFxSet`.

use std::collections::{BTreeSet, HashMap};

use bevy::prelude::*;
use shinobi_client::netclient::WorldFx;
use shinobi_sim::level::{is_landmark, LevelBox, PYLON_HALF, TORII_POST_HALF};

use crate::net::{LevelRes, Net};

// ----------------------------------------------------------------------------------------------- pure logic

/// Seconds after load during which drained events are quiet (hide only).
pub const QUIET_SECS: f32 = 1.0;
/// Delay between successive cascade breaks drained in one frame.
pub const STAGGER_STEP: f32 = 0.05;
const STAGGER_MAX: f32 = 1.0;
/// Live debris / dust / spark entities allowed at once.
pub const MAX_LIVE: usize = 300;
const DEBRIS_LIFE: f32 = 2.0;

/// Joining catch-up rule: events drained before `QUIET_SECS` have elapsed spawn no effects.
pub fn is_quiet(t_since_load: f32) -> bool {
    t_since_load < QUIET_SECS
}

/// Delay of the `k`-th cascade break of one frame (0-based).
pub fn stagger_delay(k: u32) -> f32 {
    (k as f32 * STAGGER_STEP).min(STAGGER_MAX)
}

/// Level box indices (= entity index, = wire id) of destructible boxes.
#[allow(dead_code)] // the index rule play::setup applies inline; kept testable here
pub fn destructible_indices(boxes: &[LevelBox]) -> Vec<u16> {
    boxes.iter().enumerate().filter(|(_, b)| b.hp > 0.0).map(|(i, _)| i as u16).collect()
}

/// Mirror of `tier_colour` in shaders/grid_floor.wgsl (the colour of the tier at height `y`).
pub fn tier_colour(y: f32) -> [f32; 3] {
    let ss = |a: f32, b: f32, x: f32| {
        let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let mut c = [0.04, 0.80, 1.00];
    for (edge, to) in [(39.5, [0.92, 0.22, 1.0]), (70.5, [1.0, 0.6, 0.1]), (101.5, [0.12, 1.0, 0.5]), (132.5, [1.0, 0.2, 0.16])] {
        let k = ss(edge, edge + 5.0, y);
        for i in 0..3 {
            c[i] += (to[i] - c[i]) * k;
        }
    }
    c
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum DressKind {
    ToriiBeam,
    LanternCap,
    PagodaCanopy,
}

/// A decoration drawn over level boxes; it is hidden while any box in `deps` is broken.
#[derive(Clone, PartialEq, Debug)]
pub struct Dressing {
    pub kind: DressKind,
    pub center: [f32; 3],
    pub half: [f32; 3],
    pub deps: Vec<u16>,
}

/// Find the landmarks (exact half extents, see `shinobi_sim::level`) and the destructible boxes each depends on.
/// Only boxes with `hp > 0` go into `deps` (permanent boxes cannot break).
pub fn find_dressing(boxes: &[LevelBox]) -> Vec<Dressing> {
    let dep = |v: &mut Vec<u16>, i: usize| {
        if boxes[i].hp > 0.0 {
            v.push(i as u16)
        }
    };
    let mut out = Vec::new();
    let posts: Vec<usize> = (0..boxes.len()).filter(|&i| is_landmark(&boxes[i], TORII_POST_HALF)).collect();
    for (a, &i) in posts.iter().enumerate() {
        for &j in &posts[a + 1..] {
            let (p, q) = (boxes[i].center, boxes[j].center);
            let (dx, dz) = ((p[0] - q[0]).abs(), (p[2] - q[2]).abs());
            let gap = dx.max(dz);
            if (p[1] - q[1]).abs() > 0.01 || dx.min(dz) > 0.01 || !(5.5..=7.0).contains(&gap) {
                continue;
            }
            let top = p[1] + TORII_POST_HALF[1];
            let along_x = dx > dz;
            let half = if along_x { [gap * 0.5 + 0.8, 0.18, 0.4] } else { [0.4, 0.18, gap * 0.5 + 0.8] };
            let mut deps = Vec::new();
            dep(&mut deps, i);
            dep(&mut deps, j);
            out.push(Dressing { kind: DressKind::ToriiBeam, center: [(p[0] + q[0]) * 0.5, top + 0.18, (p[2] + q[2]) * 0.5], half, deps });
        }
    }
    for (i, b) in boxes.iter().enumerate() {
        if is_landmark(b, PYLON_HALF) {
            let mut deps = Vec::new();
            dep(&mut deps, i);
            out.push(Dressing { kind: DressKind::LanternCap, center: [b.center[0], b.center[1] + b.half[1] + 0.3, b.center[2]], half: [0.5, 0.3, 0.5], deps });
        }
    }
    // pagoda: a square dais (half y 0.15) with a smaller one stacked on it; the canopy floats over the pair
    for (i, d) in boxes.iter().enumerate() {
        let sq = (d.half[0] - d.half[2]).abs() < 0.01 && (d.half[1] - 0.15).abs() < 0.01 && d.rot_z == 0.0;
        if !sq {
            continue;
        }
        let up = boxes.iter().position(|u| {
            u.rot_z == 0.0
                && (u.half[1] - 0.15).abs() < 0.01
                && (u.half[0] - (d.half[0] - 0.7)).abs() < 0.02
                && (u.half[0] - u.half[2]).abs() < 0.01
                && (u.center[0] - d.center[0]).abs() < 0.01
                && (u.center[2] - d.center[2]).abs() < 0.01
                && (u.center[1] - d.center[1] - 0.3).abs() < 0.01
        });
        let Some(j) = up else { continue };
        let mut deps = Vec::new();
        dep(&mut deps, i);
        dep(&mut deps, j);
        // the tower top it stands on: the box whose top face is the dais bottom and which covers the dais centre
        let bottom = d.center[1] - d.half[1];
        for (k, s) in boxes.iter().enumerate() {
            let covers = (s.center[0] - d.center[0]).abs() <= s.half[0] && (s.center[2] - d.center[2]).abs() <= s.half[2];
            if k != i && s.rot_z == 0.0 && (s.center[1] + s.half[1] - bottom).abs() < 0.02 && covers {
                dep(&mut deps, k);
            }
        }
        let r = d.half[0] + 1.2;
        out.push(Dressing { kind: DressKind::PagodaCanopy, center: [d.center[0], boxes[j].center[1] + 3.4, d.center[2]], half: [r, 0.2, r], deps });
    }
    out
}

fn hash01(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B1) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 65535.0
}

/// Crack overlay level 1..=4 from `hp_q` (255 untouched); 0 = none.
pub fn crack_level(hp_q: u8) -> usize {
    if hp_q >= 250 {
        0
    } else {
        (((255 - hp_q as u32) * 4 + 254) / 255).clamp(1, 4) as usize
    }
}

// ----------------------------------------------------------------------------------------------- bevy side

/// Marks a level box entity that can break; the value is its box index (the wire id).
#[derive(Component)]
pub struct DestructBox(pub u16);

#[derive(Component)]
struct DressMark(#[allow(dead_code)] usize);

#[derive(Component)]
struct CrackOverlay;

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Debris,
    Dust,
    Spark,
    /// Re-form shell: flickers out while shrinking back onto the box.
    Pop,
}

#[derive(Component)]
struct Bit {
    vel: Vec3,
    spin: Vec3,
    age: f32,
    life: f32,
    size: Vec3,
    phase: f32,
    gravity: f32,
    floor_y: f32,
    shape: Shape,
}

/// Events drained this frame, republished for other systems (audio). Order after `WorldFxSet`.
#[derive(Resource, Default)]
pub struct WorldFxFeed {
    pub events: Vec<WorldFx>,
    /// True while the drained events are the join catch-up (no effects, no sound).
    pub quiet: bool,
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorldFxSet;

struct Pending {
    idx: u16,
    due: f32,
}

#[derive(Resource)]
struct State {
    t: f32,
    ents: Vec<Option<Entity>>,
    hidden: Vec<bool>,
    hidden_list: Vec<u16>,
    pending: Vec<Pending>,
    live: usize,
    dress: Vec<Dressing>,
    dress_ents: Vec<Entity>,
    dress_hidden: Vec<bool>,
    dirty: bool,
    crack: HashMap<u16, Entity>,
    flash: Vec<(u16, f32, usize)>,
    pops: Vec<(u16, f32)>,
    floating: bool,
    glow: HashMap<(u32, u8), Handle<StandardMaterial>>,
    cube: Option<Handle<Mesh>>,
    crack_mats: Vec<Handle<StandardMaterial>>,
    flash_mat: Option<Handle<StandardMaterial>>,
    /// Review-shot aid: `SHOT_BREAK=idx,idx` breaks those boxes (locally, no server) at `SHOT_BREAK_AT` seconds.
    fake: BTreeSet<u16>,
    fake_plan: Vec<u16>,
    fake_at: f32,
}

impl Default for State {
    fn default() -> Self {
        let num = |k: &str| std::env::var(k).ok();
        let fake_plan = num("SHOT_BREAK").map(|s| s.split(',').filter_map(|n| n.trim().parse().ok()).collect()).unwrap_or_default();
        State {
            t: 0.0,
            ents: Vec::new(),
            hidden: Vec::new(),
            hidden_list: Vec::new(),
            pending: Vec::new(),
            live: 0,
            dress: Vec::new(),
            dress_ents: Vec::new(),
            dress_hidden: Vec::new(),
            dirty: false,
            crack: HashMap::new(),
            flash: Vec::new(),
            pops: Vec::new(),
            floating: false,
            glow: HashMap::new(),
            cube: None,
            crack_mats: Vec::new(),
            flash_mat: None,
            fake: BTreeSet::new(),
            fake_plan,
            fake_at: num("SHOT_BREAK_AT").and_then(|s| s.parse().ok()).unwrap_or(1.6),
        }
    }
}

pub struct DestructibleFxPlugin;

impl Plugin for DestructibleFxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<State>()
            .init_resource::<WorldFxFeed>()
            .add_systems(Startup, setup)
            .add_systems(Update, (register, gather.in_set(WorldFxSet), tick_pending, apply_visibility, animate_misc, animate_bits).chain());
    }
}

fn setup(
    mut st: ResMut<State>,
    mut commands: Commands,
    level: Option<Res<LevelRes>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    st.cube = Some(meshes.add(Cuboid::new(1.0, 1.0, 1.0)));
    for a in [0.18, 0.32, 0.5, 0.68] {
        st.crack_mats.push(mats.add(StandardMaterial { base_color: Color::srgba(0.0, 0.0, 0.0, a), unlit: true, alpha_mode: AlphaMode::Blend, ..default() }));
    }
    st.flash_mat = Some(mats.add(StandardMaterial { base_color: Color::linear_rgb(6.0, 6.0, 6.0), unlit: true, alpha_mode: AlphaMode::Add, ..default() }));
    let Some(l) = level else { return };
    let boxes = &l.0.boxes;
    st.floating = shinobi_sim::level::is_floating(&l.0);
    st.hidden = vec![false; boxes.len()];
    st.ents = vec![None; boxes.len()];
    st.dress = find_dressing(boxes);
    st.dress_hidden = vec![false; st.dress.len()];
    let cube = st.cube.clone().unwrap();
    for (n, d) in st.dress.clone().iter().enumerate() {
        let c = tier_colour(d.center[1]);
        let g = 2.2;
        let m = mats.add(StandardMaterial { base_color: Color::linear_rgb(c[0] * g, c[1] * g, c[2] * g), unlit: true, ..default() });
        let e = commands
            .spawn((Mesh3d(cube.clone()), MeshMaterial3d(m), Transform::from_translation(Vec3::from(d.center)).with_scale(Vec3::from(d.half) * 2.0), DressMark(n)))
            .id();
        st.dress_ents.push(e);
    }
}

fn register(mut st: ResMut<State>, new: Query<(Entity, &DestructBox), Added<DestructBox>>) {
    for (e, b) in &new {
        if let Some(slot) = st.ents.get_mut(b.0 as usize) {
            *slot = Some(e);
        }
    }
}

fn glow(st: &mut State, mats: &mut Assets<StandardMaterial>, c: [f32; 3], class: u8) -> Handle<StandardMaterial> {
    st.glow
        .entry((crate::team::key(c), class))
        .or_insert_with(|| {
            let (g, mix) = match class {
                1 => (5.0, 0.55),
                2 => (0.7, 0.0),
                _ => (2.4, 0.0),
            };
            let ch = |v: f32| (v + (1.0 - v) * mix) * g;
            mats.add(StandardMaterial { base_color: Color::linear_rgb(ch(c[0]), ch(c[1]), ch(c[2])), unlit: true, alpha_mode: AlphaMode::Add, ..default() })
        })
        .clone()
}

fn gather(
    time: Res<Time>,
    mut net: ResMut<Net>,
    level: Option<Res<LevelRes>>,
    mut st: ResMut<State>,
    mut feed: ResMut<WorldFxFeed>,
    mut commands: Commands,
) {
    st.t += time.delta_secs();
    feed.events.clear();
    let evs = net.0.game.net.take_world_fx();
    feed.events.extend(evs);
    // review aid: scripted local breaks
    if !st.fake_plan.is_empty() && st.t >= st.fake_at {
        let plan = std::mem::take(&mut st.fake_plan);
        for idx in plan {
            st.fake.insert(idx);
            feed.events.push(WorldFx::Break { idx, cascade: true });
        }
    }
    let quiet = is_quiet(st.t);
    feed.quiet = quiet;
    if feed.events.is_empty() {
        return;
    }
    let Some(level) = level else { return };
    st.dirty = true;
    let mut k = 0u32;
    let now = st.t;
    for i in 0..feed.events.len() {
        match feed.events[i] {
            WorldFx::Break { idx, cascade } => {
                if let Some(e) = st.crack.remove(&idx) {
                    commands.entity(e).despawn();
                }
                if quiet || level.0.boxes.get(idx as usize).is_none() {
                    continue;
                }
                let due = if cascade {
                    let d = stagger_delay(k);
                    k += 1;
                    now + d
                } else {
                    now
                };
                st.pending.push(Pending { idx, due });
            }
            WorldFx::Rez { idx } => {
                st.pending.retain(|p| p.idx != idx);
                st.fake.remove(&idx);
                if let Some(e) = st.crack.remove(&idx) {
                    commands.entity(e).despawn();
                }
                if !quiet {
                    st.pops.push((idx, 0.0));
                    spawn_pop(&mut commands, &mut st, &level.0.boxes, idx);
                }
            }
            WorldFx::Crack { idx, hp_q } => {
                let lvl = crack_level(hp_q);
                if lvl == 0 {
                    continue;
                }
                let Some(Some(parent)) = st.ents.get(idx as usize).copied() else { continue };
                let mat = st.crack_mats[lvl - 1].clone();
                if let Some(&e) = st.crack.get(&idx) {
                    commands.entity(e).insert(MeshMaterial3d(mat));
                } else {
                    let cube = st.cube.clone().unwrap();
                    let o = commands
                        .spawn((Mesh3d(cube), MeshMaterial3d(mat), Transform::from_scale(Vec3::splat(1.004)), CrackOverlay, ChildOf(parent)))
                        .id();
                    st.crack.insert(idx, o);
                }
                if !quiet {
                    let bright = st.flash_mat.clone().unwrap();
                    if let Some(&o) = st.crack.get(&idx) {
                        commands.entity(o).insert(MeshMaterial3d(bright));
                    }
                    if let Some(f) = st.flash.iter_mut().find(|f| f.0 == idx) {
                        f.1 = 0.0;
                        f.2 = lvl;
                    } else {
                        st.flash.push((idx, 0.0, lvl));
                    }
                }
            }
        }
    }
}

fn spawn_pop(commands: &mut Commands, st: &mut State, boxes: &[LevelBox], idx: u16) {
    let Some(b) = boxes.get(idx as usize) else { return };
    if st.live >= MAX_LIVE {
        return;
    }
    let size = Vec3::from(b.half) * 2.0;
    commands.spawn((
        Mesh3d(st.cube.clone().unwrap()),
        MeshMaterial3d(st.flash_mat.clone().unwrap()),
        Transform::from_translation(Vec3::from(b.center)).with_rotation(Quat::from_rotation_z(b.rot_z)).with_scale(size * 1.06),
        Bit { vel: Vec3::ZERO, spin: Vec3::ZERO, age: 0.0, life: 0.35, size: size * 1.06, phase: idx as f32, gravity: 0.0, floor_y: f32::MIN, shape: Shape::Pop },
    ));
    st.live += 1;
}

fn tick_pending(mut commands: Commands, mut st: ResMut<State>, level: Option<Res<LevelRes>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    if st.pending.is_empty() {
        return;
    }
    let Some(level) = level else { return };
    let mut i = 0;
    while i < st.pending.len() {
        if st.pending[i].due <= st.t {
            let p = st.pending.swap_remove(i);
            st.dirty = true;
            burst(&mut commands, &mut st, &mut mats, &level.0.boxes[p.idx as usize], p.idx);
        } else {
            i += 1;
        }
    }
}

/// Debris chunks, a dust puff and a few sparks for one broken box, tinted to its tier colour.
fn burst(commands: &mut Commands, st: &mut State, mats: &mut Assets<StandardMaterial>, b: &LevelBox, idx: u16) {
    let cube = st.cube.clone().unwrap();
    let c = tier_colour(b.center[1]);
    let body = glow(st, mats, c, 0);
    let hot = glow(st, mats, c, 1);
    let dust = glow(st, mats, c, 2);
    let centre = Vec3::from(b.center);
    let full = Vec3::from(b.half) * 2.0;
    let rot = Quat::from_rotation_z(b.rot_z);
    let floor_y = if !st.floating && b.center[1] - b.half[1] > -0.05 { 0.0 } else { f32::MIN };
    let vol = (full.x * full.y * full.z).max(0.01);
    let chunks = (4.0 + vol.cbrt() * 2.0).clamp(4.0, 9.0) as u32;
    let sparks = 5u32;
    let dusts = 4u32;
    let need = (chunks + sparks + dusts) as usize;
    if st.live + need > MAX_LIVE {
        return;
    }
    st.live += need;
    let seed = (idx as u32).wrapping_mul(2_654_435_761);
    for i in 0..chunks + sparks + dusts {
        let s = seed.wrapping_add(i.wrapping_mul(104_729));
        let r = |n: u32| hash01(s ^ n);
        let off = Vec3::new(r(1) - 0.5, r(2) - 0.5, r(3) - 0.5) * full;
        let pos = centre + rot * off;
        let out = Vec3::new(off.x, 0.0, off.z).normalize_or_zero();
        let (shape, size, vel, life, mat, grav) = if i < chunks {
            let sz = (full * (0.18 + 0.25 * r(4))).clamp(Vec3::splat(0.12), Vec3::splat(1.4));
            let v = out * (1.0 + 3.0 * r(5)) + Vec3::Y * (0.5 + 3.5 * r(6));
            (Shape::Debris, sz, v, DEBRIS_LIFE * (0.75 + 0.25 * r(7)), body.clone(), 14.0)
        } else if i < chunks + sparks {
            let v = (out + Vec3::new(r(8) - 0.5, r(9), r(10) - 0.5)).normalize_or_zero() * (3.0 + 5.0 * r(11));
            (Shape::Spark, Vec3::splat(0.07 + 0.06 * r(12)), v, 0.5 + 0.5 * r(13), hot.clone(), 9.0)
        } else {
            let v = out * (0.6 + 1.2 * r(14)) + Vec3::Y * (0.3 + 0.8 * r(15));
            (Shape::Dust, Vec3::splat(0.5 + 0.7 * r(16)), v, 0.9 + 0.5 * r(17), dust.clone(), -0.2)
        };
        commands.spawn((
            Mesh3d(cube.clone()),
            MeshMaterial3d(mat),
            Transform::from_translation(pos).with_scale(size).with_rotation(rot),
            Bit {
                vel,
                spin: Vec3::new(r(18) - 0.5, r(19) - 0.5, r(20) - 0.5) * 10.0,
                age: 0.0,
                life,
                size,
                phase: r(21) * 6.28,
                gravity: grav,
                floor_y,
                shape,
            },
        ));
    }
}

fn apply_visibility(
    mut st: ResMut<State>,
    net: Res<Net>,
    mut vis: Query<&mut Visibility>,
) {
    let st = &mut *st;
    let broken = net.0.game.net.world_broken();
    // hide newly broken (not while their stagger delay is pending), allocation-free
    for &idx in broken.iter().chain(st.fake.iter()) {
        let i = idx as usize;
        if i < st.hidden.len() && !st.hidden[i] && !st.pending.iter().any(|p| p.idx == idx) {
            st.hidden[i] = true;
            st.hidden_list.push(idx);
            st.dirty = true;
            if let Some(Some(e)) = st.ents.get(i) {
                if let Ok(mut v) = vis.get_mut(*e) {
                    *v = Visibility::Hidden;
                }
            }
        }
    }
    // show re-formed
    let mut n = 0;
    while n < st.hidden_list.len() {
        let idx = st.hidden_list[n];
        if broken.contains(&idx) || st.fake.contains(&idx) {
            n += 1;
            continue;
        }
        st.hidden[idx as usize] = false;
        st.hidden_list.swap_remove(n);
        st.dirty = true;
        if let Some(Some(e)) = st.ents.get(idx as usize) {
            if let Ok(mut v) = vis.get_mut(*e) {
                *v = Visibility::Inherited;
            }
        }
    }
    if st.dirty {
        st.dirty = false;
        for n in 0..st.dress.len() {
            let gone = st.dress[n].deps.iter().any(|&d| st.hidden.get(d as usize).copied().unwrap_or(false));
            if gone != st.dress_hidden[n] {
                st.dress_hidden[n] = gone;
                if let Ok(mut v) = vis.get_mut(st.dress_ents[n]) {
                    *v = if gone { Visibility::Hidden } else { Visibility::Inherited };
                }
            }
        }
    }
}

/// Crack flashes (overlay turns bright for a moment) and the scale pop of a re-formed box.
fn animate_misc(
    time: Res<Time>,
    mut st: ResMut<State>,
    level: Option<Res<LevelRes>>,
    mut tf: Query<&mut Transform, With<DestructBox>>,
    mut commands: Commands,
) {
    if st.flash.is_empty() && st.pops.is_empty() {
        return;
    }
    let dt = time.delta_secs();
    let st = &mut *st;
    let mut n = 0;
    while n < st.flash.len() {
        st.flash[n].1 += dt;
        let (idx, t, lvl) = st.flash[n];
        if t > 0.12 {
            if let Some(&o) = st.crack.get(&idx) {
                commands.entity(o).insert(MeshMaterial3d(st.crack_mats[lvl - 1].clone()));
            }
            st.flash.swap_remove(n);
        } else {
            n += 1;
        }
    }
    let Some(level) = level else { return };
    let mut n = 0;
    while n < st.pops.len() {
        st.pops[n].1 += dt;
        let (idx, t) = st.pops[n];
        let k = (t / 0.3).min(1.0);
        if let (Some(Some(e)), Some(b)) = (st.ents.get(idx as usize), level.0.boxes.get(idx as usize)) {
            if let Ok(mut tr) = tf.get_mut(*e) {
                let s = 0.85 + 0.15 * (1.0 - (1.0 - k).powi(3));
                tr.scale = Vec3::from(b.half) * 2.0 * s;
            }
        }
        if k >= 1.0 {
            st.pops.swap_remove(n);
        } else {
            n += 1;
        }
    }
}

fn animate_bits(mut commands: Commands, time: Res<Time>, mut st: ResMut<State>, mut q: Query<(Entity, &mut Transform, &mut Bit)>) {
    let dt = time.delta_secs();
    for (e, mut t, mut b) in &mut q {
        b.age += dt;
        if b.age >= b.life {
            commands.entity(e).despawn();
            st.live = st.live.saturating_sub(1);
            continue;
        }
        let k = b.age / b.life;
        match b.shape {
            Shape::Pop => {
                let on = if k > 0.3 && (b.age * 50.0 + b.phase).sin() < 0.0 { 0.0 } else { 1.0 };
                t.scale = b.size * (1.0 - 0.06 * k) * on;
            }
            Shape::Dust => {
                t.translation += b.vel * dt;
                t.scale = b.size * (0.6 + 1.4 * k) * (1.0 - k).powf(0.7);
            }
            Shape::Debris | Shape::Spark => {
                t.translation += b.vel * dt;
                b.vel.y -= b.gravity * dt;
                if t.translation.y - b.size.y * 0.5 < b.floor_y {
                    t.translation.y = b.floor_y + b.size.y * 0.5;
                    b.vel.y = b.vel.y.abs() * 0.25;
                    b.vel.x *= 0.6;
                    b.vel.z *= 0.6;
                    b.spin *= 0.6;
                }
                let sp = b.spin * dt;
                t.rotation = Quat::from_euler(EulerRot::XYZ, sp.x, sp.y, sp.z) * t.rotation;
                let fade = (1.0 - k).powf(0.8);
                let on = if k > 0.5 && (b.age * 38.0 + b.phase).sin() < -0.35 { 0.0 } else { 1.0 };
                t.scale = b.size * fade * on;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(c: [f32; 3], h: [f32; 3], hp: f32) -> LevelBox {
        LevelBox { center: c, half: h, rot_z: 0.0, hp }
    }

    #[test]
    fn destructible_index_is_the_box_index() {
        let boxes = vec![bx([0.; 3], [1.; 3], 0.0), bx([5., 0., 0.], [1.; 3], 50.0), bx([9., 0., 0.], [1.; 3], 0.0), bx([12., 0., 0.], [1.; 3], 10.0)];
        assert_eq!(destructible_indices(&boxes), vec![1, 3]);
    }

    #[test]
    fn cascade_stagger_steps_and_caps() {
        assert_eq!(stagger_delay(0), 0.0);
        assert!((stagger_delay(3) - 0.15).abs() < 1e-6);
        assert_eq!(stagger_delay(10_000), 1.0);
        assert!(stagger_delay(5) > stagger_delay(4));
    }

    #[test]
    fn join_events_are_quiet_for_the_first_second() {
        assert!(is_quiet(0.0));
        assert!(is_quiet(0.99));
        assert!(!is_quiet(1.0));
        assert!(!is_quiet(5.0));
    }

    #[test]
    fn tier_colours_follow_height() {
        assert_eq!(tier_colour(0.0), [0.04, 0.80, 1.00]);
        let v = tier_colour(60.0);
        assert!((v[0] - 0.92).abs() < 1e-5 && (v[1] - 0.22).abs() < 1e-5);
        let top = tier_colour(200.0);
        assert!((top[0] - 1.0).abs() < 1e-5 && (top[1] - 0.2).abs() < 1e-5);
    }

    #[test]
    fn crack_levels() {
        assert_eq!(crack_level(255), 0);
        assert_eq!(crack_level(200), 1);
        assert_eq!(crack_level(1), 4);
        assert!(crack_level(100) >= crack_level(200));
    }

    #[test]
    fn torii_beam_depends_on_both_posts() {
        let boxes = vec![
            bx([0., 52.75, -3.1], TORII_POST_HALF, 90.0),
            bx([0., 52.75, 3.1], TORII_POST_HALF, 90.0),
            bx([40., 52.75, 3.1], TORII_POST_HALF, 90.0), // lone post: no beam
        ];
        let d = find_dressing(&boxes);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, DressKind::ToriiBeam);
        assert_eq!(d[0].deps, vec![0, 1]);
        assert!((d[0].center[1] - (52.75 + 2.75 + 0.18)).abs() < 1e-4);
    }

    #[test]
    fn lantern_cap_goes_with_its_pylon_and_permanent_pylon_has_no_deps() {
        let boxes = vec![bx([1., 11.8, 1.], PYLON_HALF, 60.0), bx([9., 11.8, 9.], PYLON_HALF, 0.0), bx([20., 5., 5.], [0.35, 1.7, 0.35], 60.0)];
        let d = find_dressing(&boxes);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].deps, vec![0]);
        assert!(d[1].deps.is_empty());
    }

    #[test]
    fn pagoda_canopy_goes_with_daises_and_the_tower_top() {
        let boxes = vec![
            bx([0., 20., 0.], [4., 10., 4.], 450.0), // tower top: top face y = 30
            bx([0., 30.15, 0.], [3.5, 0.15, 3.5], 40.0),
            bx([0., 30.45, 0.], [2.8, 0.15, 2.8], 40.0),
        ];
        let d = find_dressing(&boxes);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, DressKind::PagodaCanopy);
        let mut deps = d[0].deps.clone();
        deps.sort();
        assert_eq!(deps, vec![0, 1, 2]);
    }
}
