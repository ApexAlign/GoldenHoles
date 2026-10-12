//! World FX of the flash star (docs/stars.md). Fed by `NetEvent::StarFx` (the one-shot burst). Purely cosmetic; the sim
//! never reads any of it.
//!
//! * FLASH: a white shell, a ring out to the 14 m reach, a light flash. (The screen white-out and the DAZED cue come from
//!   the server's `StarBlind` through `RenderView::blind` / `dazed`.)

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use shinobi_sim::combat::stars::StarCfg;
use shinobi_sim::events::{NetEvent, STAR_FX_FLASH_BURST};

use crate::impacts::{Impact, ImpactKind, ImpactQueue};
use crate::net::ViewRes;

fn ring_mesh() -> Mesh {
    let (mut pos, mut idx): (Vec<[f32; 3]>, Vec<u32>) = (Vec::new(), Vec::new());
    let n = 64u32;
    for i in 0..n {
        let (a0, a1) = (i as f32 / n as f32 * std::f32::consts::TAU, (i + 1) as f32 / n as f32 * std::f32::consts::TAU);
        let b = pos.len() as u32;
        pos.extend([[0.86 * a0.cos(), 0.0, 0.86 * a0.sin()], [a0.cos(), 0.0, a0.sin()], [a1.cos(), 0.0, a1.sin()], [0.86 * a1.cos(), 0.0, 0.86 * a1.sin()]]);
        idx.extend([b, b + 1, b + 2, b, b + 2, b + 3]);
    }
    let col = vec![[1.0f32; 4]; pos.len()];
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
        .with_inserted_indices(Indices::U32(idx))
}

// ---------------------------------------------------------------- pure curves

/// Fireball size factor over its life `u` (0..1): pops out fast (ease-out cubic over the first 30 %), then keeps
/// swelling a little as it dies.
pub fn fireball_scale(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    let pop = 1.0 - (1.0 - (u / 0.3).min(1.0)).powi(3);
    pop * (1.0 + 0.18 * u)
}

/// Fireball brightness over its life: hot at the start, gone at the end.
pub fn fireball_alpha(u: f32) -> f32 {
    (1.0 - u.clamp(0.0, 1.0)).powi(2)
}

fn hash01(mut x: u32) -> f32 {
    x = x.wrapping_mul(0x9e37_79b1) ^ (x >> 15);
    x = x.wrapping_mul(0x85eb_ca6b) ^ (x >> 13);
    (x & 0xffff) as f32 / 65536.0
}

/// Tiny deterministic random stream for the particles.
struct Rng(u32);

impl Rng {
    fn f(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        hash01(self.0)
    }
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.f()
    }
    fn dir(&mut self) -> Vec3 {
        Vec3::new(self.f() - 0.5, self.f() - 0.5, self.f() - 0.5).normalize_or(Vec3::Y)
    }
}


// ---------------------------------------------------------------- components

#[derive(Component)]
struct Shell {
    age: f32,
    life: f32,
    size: f32,
    col: [f32; 3],
}

#[derive(Component)]
struct Ring {
    age: f32,
    life: f32,
    size: f32,
    col: [f32; 3],
}

/// A spark / ember / debris chunk: ballistic, shrinks out.
#[derive(Component)]
struct Bit {
    vel: Vec3,
    grav: f32,
    drag: f32,
    age: f32,
    life: f32,
    size: f32,
    spin: Vec3,
}

#[derive(Component)]
struct BlastLight {
    age: f32,
    life: f32,
    peak: f32,
}

pub struct StarFxPlugin;

impl Plugin for StarFxPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup.after(crate::play::setup))
            .add_systems(Update, (spawn_bursts, animate_shells, animate_rings, animate_bits, animate_lights).chain());
    }
}

fn add_mat(mats: &mut Assets<StandardMaterial>, c: [f32; 4], mode: AlphaMode) -> Handle<StandardMaterial> {
    mats.add(StandardMaterial { base_color: Color::linear_rgba(c[0], c[1], c[2], c[3]), unlit: true, alpha_mode: mode, double_sided: true, cull_mode: None, ..default() })
}

#[derive(Resource)]
struct Look {
    sphere: Handle<Mesh>,
    ring: Handle<Mesh>,
    spark: Handle<StandardMaterial>,
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let sphere = meshes.add(Sphere::new(1.0).mesh().ico(3).unwrap());
    let ring = meshes.add(ring_mesh());
    commands.insert_resource(Look { sphere, ring, spark: add_mat(&mut mats, [7.0, 3.2, 0.7, 1.0], AlphaMode::Add) });
}

#[allow(clippy::too_many_arguments)]
fn spawn_bit(commands: &mut Commands, mesh: &Handle<Mesh>, mat: Handle<StandardMaterial>, pos: Vec3, vel: Vec3, grav: f32, drag: f32, life: f32, size: f32, spin: Vec3) {
    commands.spawn((
        Mesh3d(mesh.clone()),
        MeshMaterial3d(mat),
        Transform::from_translation(pos).with_scale(Vec3::splat(size)),
        Bit { vel, grav, drag, age: 0.0, life, size, spin },
        NoFrustumCulling,
    ));
}

// ---------------------------------------------------------------- bursts

fn spawn_bursts(mut commands: Commands, look: Option<Res<Look>>, view: Res<ViewRes>, mut mats: ResMut<Assets<StandardMaterial>>, mut impacts: ResMut<ImpactQueue>, time: Res<Time>) {
    let Some(look) = look else { return };
    let sc = StarCfg::default();
    for (n, e) in view.0.events.iter().enumerate() {
        let NetEvent::StarFx { what, pos, .. } = e else { continue };
        let p = Vec3::from(shinobi_proto::quant::dq_pos3(*pos));
        let mut rng = Rng(((p.x * 97.0 + p.z * 61.0) as i32 as u32) ^ (time.elapsed_secs() * 1000.0) as u32 ^ n as u32);
        if *what == STAR_FX_FLASH_BURST {
            flash_burst(&mut commands, &look, &mut mats, &mut impacts, &mut rng, p, sc.flash_radius);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn shell(commands: &mut Commands, look: &Look, mats: &mut Assets<StandardMaterial>, p: Vec3, size: f32, life: f32, col: [f32; 3]) {
    let m = add_mat(mats, [col[0], col[1], col[2], 1.0], AlphaMode::Add);
    commands.spawn((Mesh3d(look.sphere.clone()), MeshMaterial3d(m), Transform::from_translation(p).with_scale(Vec3::splat(0.05)), Shell { age: 0.0, life, size, col }, NoFrustumCulling));
}

fn ring(commands: &mut Commands, look: &Look, mats: &mut Assets<StandardMaterial>, p: Vec3, size: f32, life: f32, col: [f32; 3]) {
    let m = add_mat(mats, [col[0], col[1], col[2], 1.0], AlphaMode::Add);
    commands.spawn((Mesh3d(look.ring.clone()), MeshMaterial3d(m), Transform::from_translation(p).with_scale(Vec3::splat(0.05)), Ring { age: 0.0, life, size, col }, NoFrustumCulling));
}

fn light(commands: &mut Commands, p: Vec3, col: Color, peak: f32, range: f32, life: f32) {
    commands.spawn((PointLight { color: col, intensity: 0.0, range, shadow_maps_enabled: false, ..default() }, Transform::from_translation(p), BlastLight { age: 0.0, life, peak }));
}

#[allow(clippy::too_many_arguments)]
fn flash_burst(commands: &mut Commands, look: &Look, mats: &mut Assets<StandardMaterial>, impacts: &mut ImpactQueue, rng: &mut Rng, p: Vec3, radius: f32) {
    shell(commands, look, mats, p, radius * 0.45, 0.4, [10.0, 10.0, 10.0]);
    ring(commands, look, mats, p, radius, 0.35, [8.0, 8.0, 8.0]);
    light(commands, p, Color::WHITE, 6.0e6, radius * 2.0, 0.4);
    for _ in 0..22 {
        let (vel, size, life) = (rng.dir() * rng.range(8.0, 20.0), rng.range(0.025, 0.05), rng.range(0.25, 0.6));
        spawn_bit(commands, &look.sphere, look.spark.clone(), p, vel, 3.0, 0.6, life, size, Vec3::ZERO);
    }
    impacts.0.push(Impact { pos: p, color: [1.0, 1.0, 1.0], kind: ImpactKind::Hit { kill: false } });
}

fn animate_shells(mut commands: Commands, time: Res<Time>, mut mats: ResMut<Assets<StandardMaterial>>, mut q: Query<(Entity, &mut Transform, &mut Shell, &MeshMaterial3d<StandardMaterial>)>) {
    let dt = time.delta_secs();
    for (e, mut t, mut s, m) in &mut q {
        s.age += dt;
        if s.age >= s.life {
            mats.remove(&m.0);
            commands.entity(e).despawn();
            continue;
        }
        let u = s.age / s.life;
        t.scale = Vec3::splat(s.size * fireball_scale(u).max(0.02));
        if let Some(mut mat) = mats.get_mut(&m.0) {
            let a = fireball_alpha(u);
            mat.base_color = Color::linear_rgb(s.col[0] * a, s.col[1] * a, s.col[2] * a);
        }
    }
}

fn animate_rings(mut commands: Commands, time: Res<Time>, mut mats: ResMut<Assets<StandardMaterial>>, mut q: Query<(Entity, &mut Transform, &mut Ring, &MeshMaterial3d<StandardMaterial>)>) {
    let dt = time.delta_secs();
    for (e, mut t, mut r, m) in &mut q {
        r.age += dt;
        if r.age >= r.life {
            mats.remove(&m.0);
            commands.entity(e).despawn();
            continue;
        }
        let u = r.age / r.life;
        let out = 1.0 - (1.0 - u) * (1.0 - u);
        t.scale = Vec3::new(r.size * out.max(0.02), 1.0, r.size * out.max(0.02));
        if let Some(mut mat) = mats.get_mut(&m.0) {
            let a = (1.0 - u).powi(2);
            mat.base_color = Color::linear_rgb(r.col[0] * a, r.col[1] * a, r.col[2] * a);
        }
    }
}

fn animate_bits(mut commands: Commands, time: Res<Time>, mut q: Query<(Entity, &mut Transform, &mut Bit)>) {
    let dt = time.delta_secs();
    for (e, mut t, mut b) in &mut q {
        b.age += dt;
        if b.age >= b.life {
            commands.entity(e).despawn();
            continue;
        }
        b.vel.y -= b.grav * dt;
        let drag = (1.0 - b.drag * dt).max(0.0);
        b.vel *= drag;
        t.translation += b.vel * dt;
        t.scale = Vec3::splat(b.size * (1.0 - b.age / b.life).powf(0.7));
        if b.spin != Vec3::ZERO {
            let ax = b.spin.normalize_or(Vec3::Y);
            t.rotate(Quat::from_axis_angle(ax, b.spin.length() * dt));
        }
    }
}

fn animate_lights(mut commands: Commands, time: Res<Time>, mut q: Query<(Entity, &mut PointLight, &mut BlastLight)>) {
    let dt = time.delta_secs();
    for (e, mut l, mut b) in &mut q {
        b.age += dt;
        if b.age >= b.life {
            commands.entity(e).despawn();
            continue;
        }
        let u = b.age / b.life;
        l.intensity = b.peak * (1.0 - u).powi(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fireball_pops_fast_then_swells_slowly_and_fades_out() {
        assert!(fireball_scale(0.0) < 0.01);
        assert!(fireball_scale(0.15) > 0.8, "most of the size in the first third");
        let mut last = 0.0;
        for i in 0..=100 {
            let s = fireball_scale(i as f32 / 100.0);
            assert!(s >= last - 1e-6, "never shrinks: {s} after {last}");
            last = s;
        }
        assert!(fireball_scale(1.0) < 1.25, "bounded");
        assert!((fireball_alpha(0.0) - 1.0).abs() < 1e-6 && fireball_alpha(1.0) == 0.0);
        assert!(fireball_alpha(0.3) > fireball_alpha(0.6));
    }

    #[test]
    fn ring_is_flat_with_unit_outer_radius() {
        let m = ring_mesh();
        let pos = m.attribute(Mesh::ATTRIBUTE_POSITION).unwrap().as_float3().unwrap().to_vec();
        let hi = pos.iter().fold(f32::MIN, |a, p| a.max(p[0]));
        assert!(pos.iter().all(|p| p[1] == 0.0) && (hi - 1.0).abs() < 1e-4);
    }
}
