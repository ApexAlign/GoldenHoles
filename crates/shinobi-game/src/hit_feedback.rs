//! Client-only hit feedback, driven by `RenderView::hits` (server `HitMsg`s): floating 3D damage numbers (pooled
//! 7-segment digits, billboarded, constant-ish screen size, merging for rapid star hits), a victim hit flash +
//! knock-back shake (read by `play::sync_remotes` through `HitFx`), and a brief health bar over the target with the
//! estimated remaining hp. No sim state; nothing is allocated per hit (everything lives in fixed pools).

use bevy::prelude::*;
use std::collections::HashMap;

use crate::net::ViewRes;
use shinobi_proto::gameplay::{HitMsg, KIND_SHURIKEN, KIND_SLASH};

const LABELS: usize = 24;
const BARS: usize = 6;
pub const FLASH_S: f32 = 0.12;
const SHAKE_S: f32 = 0.18;
const NUM_LIFE: f32 = 0.9;
const BAR_LIFE: f32 = 1.1;
const MERGE_WINDOW: f32 = 0.45;
/// Amount at or above this is a "heavy" (boosted) cut: same threshold as the HUD's gold number.
const HEAVY: u8 = 27;
const MAX_HP: f32 = shinobi_sim::pvp::MAX_HP;

/// Per-class unlit HDR colours: white body, gold head/bonus/kill, red taken, star (cyan-white), bar fill, bar back,
/// stealth kill (hot magenta: rare and high-skill, so it gets a colour nothing else dealt uses).
const COLS: [[f32; 3]; 7] =
    [[3.2, 3.2, 3.2], [5.0, 3.6, 0.5], [5.0, 0.35, 0.45], [1.0, 2.6, 3.2], [0.5, 2.4, 0.8], [0.02, 0.02, 0.04], [5.0, 0.4, 4.2]];
const WHITE: usize = 0;
const GOLD: usize = 1;
const RED: usize = 2;
const STAR: usize = 3;
const BAR_FILL: usize = 4;
const BAR_BACK: usize = 5;
const STEALTH: usize = 6;

/// 7-segment masks, bit order a b c d e f g (a = top, then clockwise, g = middle).
const SEG: [u8; 10] = [0b0111111, 0b0000110, 0b1011011, 0b1001111, 0b1100110, 0b1101101, 0b1111101, 0b0000111, 0b1111111, 0b1101111];
/// (x, y, w, h) of each segment in a unit digit cell (height 1, width 0.6).
const SEG_RECT: [(f32, f32, f32, f32); 7] = [
    (0.0, 0.5, 0.5, 0.15),
    (0.3, 0.25, 0.15, 0.5),
    (0.3, -0.25, 0.15, 0.5),
    (0.0, -0.5, 0.5, 0.15),
    (-0.3, -0.25, 0.15, 0.5),
    (-0.3, 0.25, 0.15, 0.5),
    (0.0, 0.0, 0.5, 0.15),
];
const ADVANCE: f32 = 0.9;

/// What `play::sync_remotes` reads: flash/shake state per victim, plus the white flash material.
#[derive(Resource, Default)]
pub struct HitFx {
    victims: HashMap<u16, Victim>,
    pub white: Handle<StandardMaterial>,
    /// Estimated remaining hp per victim (max - damage we saw), forgotten after a few quiet seconds.
    est: HashMap<u16, (f32, f32)>,
}

#[derive(Clone, Copy)]
struct Victim {
    t: f32,
    dir: Vec3,
}

impl HitFx {
    pub fn flashing(&self, id: u16) -> bool {
        self.victims.get(&id).map_or(false, |v| v.t > SHAKE_S - FLASH_S)
    }
    /// World-space knock-back wobble for this victim's rig root.
    pub fn shake(&self, id: u16) -> Vec3 {
        self.victims.get(&id).map_or(Vec3::ZERO, |v| {
            let k = (v.t / SHAKE_S).clamp(0.0, 1.0);
            v.dir * (0.16 * k * k) + Vec3::Y * ((v.t * 90.0).sin() * 0.03 * k)
        })
    }
    fn hit(&mut self, id: u16, dir: Vec3) {
        self.victims.insert(id, Victim { t: SHAKE_S, dir });
    }
    /// Remaining-hp fraction after `amount` more damage (kill = 0).
    fn damage(&mut self, id: u16, amount: f32, kill: bool, now: f32) -> f32 {
        let e = self.est.entry(id).or_insert((MAX_HP, now));
        if now - e.1 > 6.0 {
            e.0 = MAX_HP;
        }
        e.0 = if kill { 0.0 } else { (e.0 - amount).max(1.0) };
        e.1 = now;
        let f = e.0 / MAX_HP;
        if kill {
            self.est.remove(&id);
        }
        f
    }
}

struct Label {
    root: Entity,
    digits: [Entity; 3],
    segs: [[Entity; 7]; 3],
    active: bool,
    age: f32,
    base: Vec3,
    drift: f32,
    amount: u32,
    victim: u16,
    class: usize,
    size: f32,
    /// Fixed-distance label (damage taken): not scaled by distance.
    near: bool,
}

struct Bar {
    root: Entity,
    back: Entity,
    fill: Entity,
    active: bool,
    age: f32,
    victim: u16,
    frac: f32,
    prev: f32,
    pos: Vec3,
}

#[derive(Resource)]
struct Pool {
    labels: Vec<Label>,
    bars: Vec<Bar>,
    mats: [Handle<StandardMaterial>; 7],
}

pub struct HitFeedbackPlugin;

impl Plugin for HitFeedbackPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HitFx>().add_systems(Startup, setup).add_systems(Update, (intake, animate).chain());
    }
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>, mut fx: ResMut<HitFx>) {
    let cube = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    let mk = |mats: &mut Assets<StandardMaterial>, c: [f32; 3]| {
        mats.add(StandardMaterial { base_color: Color::linear_rgb(c[0], c[1], c[2]), unlit: true, ..default() })
    };
    let handles: [Handle<StandardMaterial>; 7] = std::array::from_fn(|i| mk(&mut mats, COLS[i]));
    fx.white = mk(&mut mats, [7.0, 7.0, 7.0]);
    let hidden = || (Transform::default(), Visibility::Hidden);
    let mut labels = Vec::new();
    for _ in 0..LABELS {
        let root = commands.spawn(hidden()).id();
        let mut segs = [[Entity::PLACEHOLDER; 7]; 3];
        let digits: [Entity; 3] = std::array::from_fn(|d| {
            let de = commands.spawn((Transform::default(), Visibility::Inherited, ChildOf(root))).id();
            for (s, (x, y, w, h)) in SEG_RECT.iter().enumerate() {
                segs[d][s] = commands
                    .spawn((
                        Mesh3d(cube.clone()),
                        MeshMaterial3d(handles[WHITE].clone()),
                        Transform::from_xyz(*x, *y, 0.0).with_scale(Vec3::new(*w, *h, 0.08)),
                        Visibility::Hidden,
                        ChildOf(de),
                    ))
                    .id();
            }
            de
        });
        labels.push(Label { root, digits, segs, active: false, age: 0.0, base: Vec3::ZERO, drift: 0.0, amount: 0, victim: 0, class: WHITE, size: 1.0, near: false });
    }
    let mut bars = Vec::new();
    for _ in 0..BARS {
        let root = commands.spawn(hidden()).id();
        let back = commands.spawn((Mesh3d(cube.clone()), MeshMaterial3d(handles[BAR_BACK].clone()), Transform::default(), ChildOf(root))).id();
        let fill = commands.spawn((Mesh3d(cube.clone()), MeshMaterial3d(handles[BAR_FILL].clone()), Transform::default(), ChildOf(root))).id();
        bars.push(Bar { root, back, fill, active: false, age: 0.0, victim: 0, frac: 1.0, prev: 1.0, pos: Vec3::ZERO });
    }
    commands.insert_resource(Pool { labels, bars, mats: handles });
}

/// (colour class, size multiplier) for a dealt hit. `stealth`: a kill landed while still cloaked (`cloak.rs`) --
/// rare and high-skill, so it gets its own colour and a bigger pop instead of blending into the plain gold kill.
pub fn classify(h: &HitMsg, stealth: bool) -> (usize, f32) {
    if h.kill && stealth {
        (STEALTH, if h.kind == KIND_SLASH { 2.6 } else { 2.2 })
    } else if h.kill {
        (GOLD, if h.kind == KIND_SLASH { 2.2 } else { 1.9 })
    } else if h.head || h.bonus {
        (GOLD, 1.45)
    } else if h.kind == KIND_SHURIKEN {
        (STAR, 0.85)
    } else if h.amount >= HEAVY {
        (WHITE, 1.5)
    } else {
        (WHITE, 1.0)
    }
}

fn render_digits(l: &Label, pool_mat: &Handle<StandardMaterial>, vis: &mut Query<&mut Visibility>, tf: &mut Query<&mut Transform>, commands: &mut Commands) {
    let text = l.amount.min(999);
    let n = if text >= 100 { 3 } else if text >= 10 { 2 } else { 1 };
    let ds = [text / 100 % 10, text / 10 % 10, text % 10];
    for d in 0..3 {
        let on = d >= 3 - n;
        let slot = d as f32 - (3 - n) as f32 - (n as f32 - 1.0) / 2.0;
        if let Ok(mut t) = tf.get_mut(l.digits[d]) {
            t.translation = Vec3::new(slot * ADVANCE, 0.0, 0.0);
        }
        for s in 0..7 {
            if let Ok(mut v) = vis.get_mut(l.segs[d][s]) {
                *v = if on && SEG[ds[d] as usize] >> s & 1 == 1 { Visibility::Inherited } else { Visibility::Hidden };
            }
            commands.entity(l.segs[d][s]).insert(MeshMaterial3d(pool_mat.clone()));
        }
    }
}

fn intake(
    mut commands: Commands,
    time: Res<Time>,
    view: Res<ViewRes>,
    mut fx: ResMut<HitFx>,
    mut pool: ResMut<Pool>,
    mut vis: Query<&mut Visibility>,
    mut tf: Query<&mut Transform>,
) {
    let v = &view.0;
    if v.hits.is_empty() {
        return;
    }
    let now = time.elapsed_secs();
    let eye = Vec3::from(v.eye);
    let (yaw, pitch) = v.look_angles();
    let rot = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0);
    let (fwd, right) = (rot * Vec3::NEG_Z, rot * Vec3::X);
    let p = &mut *pool;
    for h in &v.hits {
        if h.taken {
            // damage we took: a red number low in front of the view, off to the side the hit came from if known
            let side = v.remotes.iter().find(|(id, _)| *id == h.other).map_or(0.0, |(_, s)| {
                let d = Vec3::from(s.pos) - eye;
                (d.dot(right)).signum() * 0.5
            });
            if h.amount == 0 {
                continue; // parry: our shield took it
            }
            let size = 0.9 + (h.amount as f32 / 50.0).min(1.2);
            let base = eye + fwd * 2.2 + right * (side - 0.15) + Vec3::NEG_Y * 0.35;
            spawn_label(p, h.amount as u32, u16::MAX, RED, size, base, true, now, &mut vis, &mut tf, &mut commands);
            continue;
        }
        let Some((_, s)) = v.remotes.iter().find(|(id, _)| *id == h.other) else { continue };
        let body = Vec3::from(s.pos);
        let dir = {
            let d = body - eye;
            Vec3::new(d.x, 0.0, d.z).normalize_or_zero()
        };
        fx.hit(h.other, dir);
        let stealth = h.kill && h.bonus && v.fx.cloak_held;
        let (class, size) = classify(h, stealth);
        let head_up = if h.head { 0.85 } else { 0.55 };
        spawn_label(p, h.amount as u32, h.other, class, size, body + Vec3::Y * (head_up + 0.4), false, now, &mut vis, &mut tf, &mut commands);
        // health bar flash: the pool slot already showing this victim is reused
        let frac = fx.damage(h.other, h.amount as f32, h.kill, now);
        let i = p.bars.iter().position(|b| b.active && b.victim == h.other).or_else(|| p.bars.iter().position(|b| !b.active)).unwrap_or_else(|| {
            (0..BARS).max_by(|&a, &b| p.bars[a].age.total_cmp(&p.bars[b].age)).unwrap_or(0)
        });
        let b = &mut p.bars[i];
        b.prev = if b.active && b.victim == h.other { b.frac } else { (frac + h.amount as f32 / MAX_HP).min(1.0) };
        b.active = true;
        b.age = 0.0;
        b.victim = h.other;
        b.frac = frac;
        b.pos = body + Vec3::Y * 1.55;
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_label(
    p: &mut Pool,
    amount: u32,
    victim: u16,
    class: usize,
    size: f32,
    base: Vec3,
    near: bool,
    _now: f32,
    vis: &mut Query<&mut Visibility>,
    tf: &mut Query<&mut Transform>,
    commands: &mut Commands,
) {
    // rapid star hits on one victim merge into the running number
    let merge = (class == STAR).then(|| p.labels.iter().position(|l| l.active && l.class == STAR && l.victim == victim && l.age < MERGE_WINDOW)).flatten();
    let i = merge.or_else(|| p.labels.iter().position(|l| !l.active)).unwrap_or_else(|| {
        (0..LABELS).max_by(|&a, &b| p.labels[a].age.total_cmp(&p.labels[b].age)).unwrap_or(0)
    });
    let mat = p.mats[class].clone();
    let l = &mut p.labels[i];
    if merge.is_some() {
        l.amount += amount;
        l.age = 0.12; // re-pop
        l.size = (l.size + 0.04).min(1.3);
    } else {
        l.amount = amount;
        l.age = 0.0;
        l.size = size;
        l.base = base;
        l.drift = (((amount * 37 + victim as u32 * 11) % 17) as f32 / 17.0 - 0.5) * 0.7;
    }
    l.active = true;
    l.victim = victim;
    l.class = class;
    l.near = near;
    // colour class changes on merge only when the first label was a star: same class, so the swap is cheap but still safe
    render_digits(l, &mat, vis, tf, commands);
    if let Ok(mut v) = vis.get_mut(l.root) {
        *v = Visibility::Inherited;
    }
}

fn animate(
    time: Res<Time>,
    view: Res<ViewRes>,
    mut fx: ResMut<HitFx>,
    mut pool: ResMut<Pool>,
    mut tf: Query<&mut Transform>,
    mut vis: Query<&mut Visibility>,
) {
    let dt = time.delta_secs();
    fx.victims.retain(|_, v| {
        v.t -= dt;
        v.t > 0.0
    });
    let eye = Vec3::from(view.0.eye);
    let remotes = &view.0.remotes;
    let p = &mut *pool;
    for l in p.labels.iter_mut().filter(|l| l.active) {
        l.age += dt;
        let u = l.age / NUM_LIFE;
        if u >= 1.0 {
            l.active = false;
            if let Ok(mut v) = vis.get_mut(l.root) {
                *v = Visibility::Hidden;
            }
            continue;
        }
        let dist = eye.distance(l.base).clamp(2.0, 45.0);
        let rise = 1.1 * (1.0 - (1.0 - u).powi(2));
        let pos = if l.near { l.base + Vec3::Y * rise * 0.4 } else { l.base + Vec3::Y * rise + Vec3::X * (l.drift * u) };
        let pop = if l.age < 0.09 { 1.0 + 0.5 * (1.0 - l.age / 0.09) } else { 1.0 };
        let fade = if u > 0.7 { (1.0 - u) / 0.3 } else { 1.0 };
        let h = if l.near { 0.16 * l.size } else { 0.05 * dist * l.size };
        if let Ok(mut t) = tf.get_mut(l.root) {
            t.translation = pos;
            t.rotation = Transform::IDENTITY.looking_to(pos - eye, Vec3::Y).rotation;
            t.scale = Vec3::splat(h * pop * fade.max(0.01));
        }
    }
    for b in p.bars.iter_mut().filter(|b| b.active) {
        b.age += dt;
        if b.age >= BAR_LIFE {
            b.active = false;
            if let Ok(mut v) = vis.get_mut(b.root) {
                *v = Visibility::Hidden;
            }
            continue;
        }
        let dist = eye.distance(b.pos).clamp(2.0, 45.0);
        let pos = remotes.iter().find(|(id, _)| *id == b.victim).map_or(b.pos, |(_, s)| Vec3::from(s.pos) + Vec3::Y * 1.55);
        let pulse = if b.age < 0.18 { 1.0 + 0.9 * (1.0 - b.age / 0.18) } else { 1.0 };
        let fade = if b.age > BAR_LIFE - 0.25 { (BAR_LIFE - b.age) / 0.25 } else { 1.0 };
        let (w, hgt) = (0.16 * dist, 0.022 * dist * pulse);
        // the lost chunk drains from `prev` to `frac` over 0.3 s
        let shown = b.prev + (b.frac - b.prev) * (b.age / 0.3).min(1.0);
        if let Ok(mut t) = tf.get_mut(b.root) {
            t.translation = pos;
            t.rotation = Transform::IDENTITY.looking_to(pos - eye, Vec3::Y).rotation;
            t.scale = Vec3::splat(fade.max(0.01));
        }
        if let Ok(mut t) = tf.get_mut(b.back) {
            t.translation = Vec3::new(0.0, 0.0, -0.01);
            t.scale = Vec3::new(w * 1.08, hgt * 1.5, 0.01);
        }
        if let Ok(mut t) = tf.get_mut(b.fill) {
            let fw = (w * shown).max(0.001);
            t.translation = Vec3::new(-w / 2.0 + fw / 2.0, 0.0, 0.0);
            t.scale = Vec3::new(fw, hgt, 0.012);
        }
        if let Ok(mut v) = vis.get_mut(b.root) {
            *v = Visibility::Inherited;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(kind: u8, amount: u8, kill: bool, head: bool, bonus: bool) -> HitMsg {
        HitMsg { other: 3, amount, kind, taken: false, kill, head, bonus }
    }

    #[test]
    fn classes_follow_type_and_amount() {
        assert_eq!(classify(&hit(KIND_SHURIKEN, 12, false, false, false), false).0, STAR);
        assert_eq!(classify(&hit(KIND_SHURIKEN, 18, false, true, false), false).0, GOLD);
        assert_eq!(classify(&hit(KIND_SLASH, 20, false, false, true), false).0, GOLD);
        assert_eq!(classify(&hit(KIND_SLASH, 30, false, false, false), false).0, WHITE);
        assert!(classify(&hit(KIND_SLASH, 30, false, false, false), false).1 > classify(&hit(KIND_SLASH, 20, false, false, false), false).1);
        let (c, s) = classify(&hit(KIND_SLASH, 60, true, false, false), false);
        assert_eq!(c, GOLD);
        assert!(s > classify(&hit(KIND_SLASH, 20, false, false, true), false).1);
        assert!(classify(&hit(KIND_SHURIKEN, 5, false, false, false), false).1 < 1.0);
    }

    #[test]
    fn stealth_kill_gets_its_own_colour_and_a_bigger_pop() {
        let (c, s) = classify(&hit(KIND_SLASH, 90, true, false, true), true);
        assert_eq!(c, STEALTH);
        let (gc, gs) = classify(&hit(KIND_SLASH, 90, true, false, true), false);
        assert_eq!(gc, GOLD);
        assert!(s > gs, "stealth kill pops bigger than a plain kill");
        // a non-kill bonus hit (strike-window, mark) is never classified stealth, even while cloaked
        assert_ne!(classify(&hit(KIND_SLASH, 30, false, false, true), true).0, STEALTH);
    }

    #[test]
    fn seven_segment_masks_are_distinct() {
        for a in 0..10 {
            for b in a + 1..10 {
                assert_ne!(SEG[a], SEG[b]);
            }
        }
        assert_eq!(SEG[8], 0b1111111);
    }

    #[test]
    fn flash_is_short_and_shake_decays() {
        let mut fx = HitFx::default();
        fx.hit(4, Vec3::X);
        assert!(fx.flashing(4) && !fx.flashing(5));
        let a = fx.shake(4).length();
        fx.victims.get_mut(&4).unwrap().t = 0.05;
        assert!(fx.shake(4).length() < a);
        fx.victims.get_mut(&4).unwrap().t = SHAKE_S - FLASH_S - 0.01;
        assert!(!fx.flashing(4));
    }

    #[test]
    fn estimated_hp_drains_resets_and_kills() {
        let mut fx = HitFx::default();
        let a = fx.damage(1, 25.0, false, 0.0);
        let b = fx.damage(1, 25.0, false, 1.0);
        assert!(b < a && a < 1.0);
        assert!(fx.damage(1, 10.0, false, 20.0) > 0.85, "forgotten after quiet seconds");
        assert_eq!(fx.damage(1, 10.0, true, 21.0), 0.0);
    }
}
