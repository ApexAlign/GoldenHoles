//! Directed injury gore: blood spray at the authoritative hit point, wound decals, arterial spurts, trails, limb
//! severing, gibs and death pools.
//!
//! The server broadcasts `NetEvent::InjuryFx` for every weapon hit that did damage (region + quantised point), then
//! the usual `Kill` event. This module is cosmetic only: it never touches prediction or the simulation. Blood is
//! intentionally cheap (unlit drops, no shadows) and fully controlled by the GORE setting and the active quality tier.

use std::collections::HashMap;

use bevy::prelude::*;
use shinobi_client::quality::Tier;
use shinobi_sim::events::{InjuryRegion, NetEvent};

use crate::character_look::Limb;
use crate::mobile::QualityRes;
use crate::net::{Net, ViewRes};
use crate::play::Remotes;

/// Player-selected gore strength, mirrored from `SettingsRes` (default full), plus the tier's particle budget.
#[derive(Resource)]
pub struct GoreCfg {
    pub intensity: f32,
    tier: Tier,
    max_drops: usize,
    max_pools: usize,
    max_wounds: usize,
}

impl Default for GoreCfg {
    fn default() -> Self {
        let mut cfg = Self { intensity: 1.0, tier: Tier::High, max_drops: 0, max_pools: 0, max_wounds: 0 };
        cfg.set_tier(Tier::High);
        cfg
    }
}

impl GoreCfg {
    fn set_tier(&mut self, tier: Tier) {
        self.tier = tier;
        let (drops, pools, wounds) = match tier {
            Tier::High => (160, 32, 24),
            Tier::Medium => (90, 20, 14),
            Tier::Low => (40, 10, 6),
        };
        self.max_drops = drops;
        self.max_pools = pools;
        self.max_wounds = wounds;
    }
}

#[derive(Resource, Default)]
struct GoreAssets {
    drop_mesh: Handle<Mesh>,
    pool_mesh: Handle<Mesh>,
    wound_mesh: Handle<Mesh>,
    gib_mesh: Handle<Mesh>,
    blood: Handle<StandardMaterial>,
    pool: Handle<StandardMaterial>,
    wound: Handle<StandardMaterial>,
    gib: Handle<StandardMaterial>,
}

#[derive(Component)]
struct BloodDrop {
    vel: Vec3,
    age: f32,
    life: f32,
    gravity: f32,
    /// Some drops leave a small stain when they hit the floor.
    trail: bool,
}

#[derive(Component)]
struct BloodPool {
    age: f32,
    life: f32,
    radius: f32,
}

/// A blood fleck stuck to a remote body (parented to its root, so it follows the animation).
#[derive(Component)]
struct Wound;

/// A severed stump that keeps pumping for a couple of seconds.
#[derive(Component)]
struct Arterial {
    vel: Vec3,
    age: f32,
    life: f32,
    next: f32,
    seed: u32,
}

#[derive(Component)]
struct Gib {
    vel: Vec3,
    spin: Vec3,
    age: f32,
    life: f32,
}

#[derive(Resource, Default)]
struct GoreState {
    /// Damage accumulated since the last sever, per victim.
    damage: HashMap<u16, f32>,
    /// Tick, hurt region and point of the last damaging hit per victim, used by a kill in the same moment.
    last: HashMap<u16, (u32, u8, [i32; 3])>,
    /// Wound decals per victim, capped by the tier budget.
    wounds: HashMap<u16, Vec<Entity>>,
}

pub struct GorePlugin;

impl Plugin for GorePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GoreCfg>()
            .init_resource::<GoreState>()
            .init_resource::<GoreAssets>()
            .add_systems(Startup, setup)
            .add_systems(Update, (sync_cfg, consume_events, animate).chain());
    }
}

fn setup(mut gore: ResMut<GoreAssets>, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    gore.drop_mesh = meshes.add(Sphere::new(0.5));
    // a thin disc, scaled per wound; Y is its normal
    gore.pool_mesh = meshes.add(Cylinder::new(0.5, 0.012));
    gore.wound_mesh = meshes.add(Sphere::new(0.5));
    gore.gib_mesh = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    gore.blood = mats.add(StandardMaterial {
        base_color: Color::linear_rgb(0.42, 0.008, 0.012),
        unlit: true,
        ..default()
    });
    gore.pool = mats.add(StandardMaterial {
        base_color: Color::linear_rgba(0.20, 0.006, 0.010, 0.82),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        ..default()
    });
    gore.wound = mats.add(StandardMaterial {
        base_color: Color::linear_rgb(0.26, 0.004, 0.008),
        unlit: true,
        ..default()
    });
    gore.gib = mats.add(StandardMaterial {
        base_color: Color::linear_rgb(0.16, 0.003, 0.006),
        unlit: true,
        ..default()
    });
}

fn sync_cfg(settings: Option<Res<crate::hud_view::SettingsRes>>, quality: Option<Res<QualityRes>>, mut cfg: ResMut<GoreCfg>) {
    if let Some(s) = settings {
        cfg.intensity = s.0.gore_intensity.clamp(0.0, 1.0);
    }
    if let Some(q) = quality {
        if q.0.tier != cfg.tier {
            cfg.set_tier(q.0.tier);
        }
    }
}

fn hash01(n: u32) -> f32 {
    let mut x = n.wrapping_mul(0x9E37_79B1) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 65535.0
}

fn point(v: [i32; 3]) -> Vec3 {
    Vec3::new(v[0] as f32 / 512.0, v[1] as f32 / 512.0, v[2] as f32 / 512.0)
}

fn limb(region: u8) -> Option<Limb> {
    Some(match region {
        r if r == InjuryRegion::Head as u8 => Limb::Head,
        r if r == InjuryRegion::ArmL as u8 => Limb::ArmL,
        r if r == InjuryRegion::ArmR as u8 => Limb::ArmR,
        r if r == InjuryRegion::LegL as u8 => Limb::LegL,
        r if r == InjuryRegion::LegR as u8 => Limb::LegR,
        _ => return None,
    })
}

fn spawn_drop(commands: &mut Commands, assets: &GoreAssets, at: Vec3, vel: Vec3, size: f32, life: f32, trail: bool) {
    commands.spawn((
        Mesh3d(assets.drop_mesh.clone()),
        MeshMaterial3d(assets.blood.clone()),
        Transform::from_translation(at).with_scale(Vec3::splat(size)),
        BloodDrop { vel, age: 0.0, life, gravity: 9.8, trail },
    ));
}

fn spawn_blood(commands: &mut Commands, assets: &GoreAssets, at: Vec3, damage: u8, intensity: f32, seed: u32, max_new: usize) -> usize {
    if max_new == 0 {
        return 0;
    }
    let n = ((2.0 + damage as f32 * 0.10 * intensity).round().clamp(1.0, 16.0) as usize).min(max_new);
    for i in 0..n {
        let s = seed.wrapping_add(i as u32 * 7_919);
        let dir = Vec3::new(hash01(s) - 0.5, hash01(s ^ 0xAAAA) * 0.9 + 0.2, hash01(s ^ 0xBBBB) - 0.5).normalize_or_zero();
        let speed = (1.0 + 3.2 * hash01(s ^ 0xCCCC)) * (0.55 + 0.45 * intensity);
        spawn_drop(
            commands,
            assets,
            at + dir * 0.06,
            dir * speed + Vec3::Y * 0.8,
            0.022 + 0.026 * hash01(s ^ 0xDDDD),
            0.45 + 0.9 * hash01(s ^ 0xEEEE),
            hash01(s ^ 0xF00D) < 0.18,
        );
    }
    n
}

/// Vertical offset of a hurt region from the capsule centre (matches the server's `injury_point`).
fn region_y(region: u8) -> f32 {
    if region == InjuryRegion::Head as u8 {
        0.55
    } else if region == InjuryRegion::Neck as u8 {
        0.40
    } else if region == InjuryRegion::ArmL as u8 || region == InjuryRegion::ArmR as u8 {
        0.16
    } else if region == InjuryRegion::LegL as u8 || region == InjuryRegion::LegR as u8 {
        -0.45
    } else if region == InjuryRegion::Pelvis as u8 {
        -0.20
    } else {
        0.10
    }
}

fn spawn_pool(commands: &mut Commands, assets: &GoreAssets, at: Vec3, region: u8, seed: u32) {
    // `at` is a body point, not the feet: undo the region offset and the capsule-centre height.
    let floor = (at.y - region_y(region) - 0.81).max(-19.0);
    let radius = 0.45 + 0.65 * hash01(seed);
    commands.spawn((
        Mesh3d(assets.pool_mesh.clone()),
        MeshMaterial3d(assets.pool.clone()),
        Transform::from_xyz(at.x, floor + 0.008, at.z).with_scale(Vec3::new(radius * 2.0, 1.0, radius * 2.0)),
        BloodPool { age: 0.0, life: 22.0, radius },
    ));
}

fn spawn_trail_pool(commands: &mut Commands, assets: &GoreAssets, at: Vec3, seed: u32) {
    let radius = 0.07 + 0.12 * hash01(seed);
    commands.spawn((
        Mesh3d(assets.pool_mesh.clone()),
        MeshMaterial3d(assets.pool.clone()),
        Transform::from_xyz(at.x, 0.01, at.z).with_scale(Vec3::new(radius * 2.0, 1.0, radius * 2.0)),
        BloodPool { age: 0.0, life: 4.5, radius },
    ));
}

fn spawn_wound(commands: &mut Commands, assets: &GoreAssets, local: Vec3, seed: u32) -> Entity {
    let size = 0.045 + 0.05 * hash01(seed);
    let rot = Quat::from_rotation_y(hash01(seed ^ 1) * std::f32::consts::TAU)
        * Quat::from_rotation_x((hash01(seed ^ 2) - 0.5) * 0.7);
    commands
        .spawn((
            Mesh3d(assets.wound_mesh.clone()),
            MeshMaterial3d(assets.wound.clone()),
            Transform::from_translation(local).with_rotation(rot).with_scale(Vec3::new(size, size * 0.32, size)),
            Wound,
        ))
        .id()
}

fn spawn_arterial(commands: &mut Commands, at: Vec3, seed: u32) {
    let dir = Vec3::new(hash01(seed) - 0.5, 0.55 + 0.4 * hash01(seed ^ 7), hash01(seed ^ 9) - 0.5).normalize_or(Vec3::Y);
    commands.spawn((
        Transform::from_translation(at),
        Arterial { vel: dir * (0.5 + 0.9 * hash01(seed ^ 3)), age: 0.0, life: 1.4 + 0.8 * hash01(seed ^ 5), next: 0.0, seed },
    ));
}

fn spawn_gibs(commands: &mut Commands, assets: &GoreAssets, at: Vec3, count: usize, seed: u32) {
    for i in 0..count {
        let s = seed.wrapping_add(i as u32 * 4_099);
        let dir = Vec3::new(hash01(s) - 0.5, 0.35 + 0.65 * hash01(s ^ 1), hash01(s ^ 2) - 0.5).normalize_or(Vec3::Y);
        let size = 0.04 + 0.08 * hash01(s ^ 3);
        commands.spawn((
            Mesh3d(assets.gib_mesh.clone()),
            MeshMaterial3d(assets.gib.clone()),
            Transform::from_translation(at)
                .with_rotation(Quat::from_euler(EulerRot::XYZ, hash01(s ^ 4) * 3.0, hash01(s ^ 5) * 3.0, hash01(s ^ 6) * 3.0))
                .with_scale(Vec3::new(size * (0.6 + hash01(s ^ 7)), size * 0.45, size)),
            Gib {
                vel: dir * (2.0 + 3.0 * hash01(s ^ 8)),
                spin: Vec3::new(hash01(s ^ 9) - 0.5, hash01(s ^ 10) - 0.5, hash01(s ^ 11) - 0.5) * 12.0,
                age: 0.0,
                life: 2.0 + 1.5 * hash01(s ^ 12),
            },
        ));
    }
}

fn consume_events(
    mut commands: Commands,
    net: Res<Net>,
    view: Res<ViewRes>,
    cfg: Res<GoreCfg>,
    assets: Res<GoreAssets>,
    mut state: ResMut<GoreState>,
    mut remotes: ResMut<Remotes>,
    roots: Query<&GlobalTransform>,
    drops: Query<&BloodDrop>,
    pools: Query<&BloodPool>,
) {
    if cfg.intensity <= 0.0 {
        state.damage.clear();
        state.last.clear();
        for (_, list) in state.wounds.drain() {
            for e in list {
                commands.entity(e).despawn();
            }
        }
        return;
    }
    let own = net.0.game.net.player_id();
    let mut drop_live = drops.iter().count();
    let mut pool_live = pools.iter().count();
    for ev in &view.0.events {
        match *ev {
            NetEvent::InjuryFx { tick, victim, damage_q, region, point: qp, blocked, sever, .. } => {
                if blocked || damage_q == 0 {
                    continue;
                }
                let at = point(qp);
                state.last.insert(victim, (tick, region, qp));
                let seed = tick ^ (victim as u32) << 16 ^ damage_q as u32;
                let budget = cfg.max_drops.saturating_sub(drop_live);
                drop_live += spawn_blood(&mut commands, &assets, at, damage_q, cfg.intensity, seed, budget);
                if Some(victim) == own {
                    continue; // first-person screen blood is drawn by the HUD
                }
                if let Some(rig) = remotes.rig_mut(victim) {
                    rig.flinch(damage_q as f32 / 55.0);
                }
                // a blood fleck stuck to the body, parented so it follows the animation
                if cfg.max_wounds > 0 {
                    if let Some(root) = remotes.root(victim) {
                        let local = roots.get(root).map_or(at, |g| g.affine().inverse().transform_point3(at));
                        let w = spawn_wound(&mut commands, &assets, local, seed);
                        commands.entity(w).insert(ChildOf(root));
                        let list = state.wounds.entry(victim).or_default();
                        list.push(w);
                        while list.len() > cfg.max_wounds {
                            let old = list.remove(0);
                            commands.entity(old).despawn();
                        }
                    }
                }
                let d = state.damage.entry(victim).or_insert(0.0);
                *d += damage_q as f32;
                let threshold = 90.0 / cfg.intensity.max(0.25);
                let forced = sever != 0;
                let mut severed = None;
                if (forced || *d >= threshold) && cfg.intensity >= 0.35 {
                    if let Some(l) = limb(region) {
                        if l != Limb::Head {
                            severed = Some(l);
                            *d = 0.0;
                        }
                    }
                }
                if let Some(l) = severed {
                    if let Some(rig) = remotes.rig_mut(victim) {
                        rig.sever(l);
                    }
                    spawn_arterial(&mut commands, at, seed);
                    spawn_gibs(&mut commands, &assets, at, if cfg.tier == Tier::Low { 2 } else { 4 }, seed);
                }
            }
            NetEvent::Kill { victim, tick, .. } => {
                let Some((hit_tick, region, qp)) = state.last.get(&victim).copied() else { continue };
                let recent = tick.saturating_sub(hit_tick) <= 3;
                let at = point(qp);
                if recent && cfg.intensity >= 0.25 && pool_live < cfg.max_pools {
                    spawn_pool(&mut commands, &assets, at, region, tick ^ victim as u32);
                    pool_live += 1;
                }
                if recent && Some(victim) != own && cfg.intensity >= 0.35 {
                    let l = if region == InjuryRegion::Neck as u8 { Some(Limb::Head) } else { limb(region) };
                    if let Some(l) = l {
                        if let Some(rig) = remotes.rig_mut(victim) {
                            rig.sever(l);
                        }
                    }
                    spawn_gibs(&mut commands, &assets, at, if cfg.tier == Tier::Low { 4 } else { 8 }, tick ^ victim as u32);
                }
                state.damage.remove(&victim);
                state.last.remove(&victim);
            }
            NetEvent::Respawn { player, .. } => {
                state.damage.remove(&player);
                state.last.remove(&player);
                if let Some(list) = state.wounds.remove(&player) {
                    for e in list {
                        commands.entity(e).despawn();
                    }
                }
                if let Some(rig) = remotes.rig_mut(player) {
                    rig.restore();
                }
            }
            _ => {}
        }
    }
}

fn animate(
    mut commands: Commands,
    time: Res<Time>,
    cfg: Res<GoreCfg>,
    assets: Res<GoreAssets>,
    mut drops: Query<(Entity, &mut Transform, &mut BloodDrop)>,
    mut pools: Query<(Entity, &mut Transform, &mut BloodPool), Without<BloodDrop>>,
    mut arterials: Query<(Entity, &mut Transform, &mut Arterial), (Without<BloodDrop>, Without<BloodPool>)>,
    mut gibs: Query<(Entity, &mut Transform, &mut Gib), (Without<BloodDrop>, Without<BloodPool>, Without<Arterial>)>,
) {
    let dt = time.delta_secs().min(0.1);
    let mut pool_live = pools.iter().count();
    let mut drop_live = drops.iter().count();
    for (e, mut tf, mut d) in &mut drops {
        d.age += dt;
        d.vel.y -= d.gravity * dt;
        tf.translation += d.vel * dt;
        if d.age >= d.life || tf.translation.y < -20.0 {
            if d.trail && tf.translation.y < 0.05 && tf.translation.y > -2.0 && pool_live < cfg.max_pools {
                spawn_trail_pool(&mut commands, &assets, tf.translation, e.to_bits() as u32);
                pool_live += 1;
            }
            commands.entity(e).despawn();
            drop_live = drop_live.saturating_sub(1);
        }
    }
    for (e, mut tf, mut a) in &mut arterials {
        a.age += dt;
        a.next -= dt;
        a.vel.y -= 3.0 * dt;
        tf.translation += a.vel * dt;
        if a.next <= 0.0 && a.age < a.life {
            a.next = 0.055 + 0.06 * hash01(a.seed ^ (a.age * 60.0) as u32);
            if drop_live < cfg.max_drops {
                let s = a.seed ^ (a.age * 977.0) as u32;
                let dir = Vec3::new(hash01(s) - 0.5, 0.35 + 0.6 * hash01(s ^ 1), hash01(s ^ 2) - 0.5).normalize_or(Vec3::Y);
                spawn_drop(&mut commands, &assets, tf.translation, dir * (1.2 + 2.4 * hash01(s ^ 3)), 0.02 + 0.025 * hash01(s ^ 4), 0.4 + 0.7 * hash01(s ^ 5), true);
                drop_live += 1;
            }
        }
        if a.age >= a.life {
            commands.entity(e).despawn();
        }
    }
    for (e, mut tf, mut g) in &mut gibs {
        g.age += dt;
        g.vel.y -= 9.8 * dt;
        tf.translation += g.vel * dt;
        tf.rotate_local_x(g.spin.x * dt);
        tf.rotate_local_y(g.spin.y * dt);
        tf.rotate_local_z(g.spin.z * dt);
        if g.age >= g.life || tf.translation.y < -20.0 {
            commands.entity(e).despawn();
        }
    }
    for (e, mut tf, mut p) in &mut pools {
        p.age += dt;
        let grow = (p.age / 0.18).clamp(0.0, 1.0);
        let fade = if p.age > p.life - 2.0 { ((p.life - p.age) / 2.0).clamp(0.0, 1.0) } else { 1.0 };
        let r = p.radius * grow * fade.max(0.01);
        tf.scale = Vec3::new(r * 2.0, 1.0, r * 2.0);
        if p.age >= p.life {
            commands.entity(e).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_map_to_body_offsets_and_severable_limbs() {
        assert_eq!(region_y(InjuryRegion::Head as u8), 0.55);
        assert_eq!(region_y(InjuryRegion::LegR as u8), -0.45);
        assert_eq!(limb(InjuryRegion::Head as u8), Some(Limb::Head));
        assert_eq!(limb(InjuryRegion::ArmL as u8), Some(Limb::ArmL));
        assert_eq!(limb(InjuryRegion::Neck as u8), None);
        assert_eq!(limb(InjuryRegion::Pelvis as u8), None);
        assert_eq!(limb(InjuryRegion::Torso as u8), None);
    }

    #[test]
    fn quantised_points_decode_at_the_protocol_scale() {
        assert_eq!(point([512, -256, 0]), Vec3::new(1.0, -0.5, 0.0));
    }

    #[test]
    fn hash_is_deterministic_and_bounded() {
        assert_eq!(hash01(7), hash01(7));
        for i in 0..1024 {
            assert!((0.0..=1.0).contains(&hash01(i)));
        }
    }

    #[test]
    fn tier_budgets_step_down_with_the_quality_tier() {
        let mut c = GoreCfg::default();
        let high = (c.max_drops, c.max_pools, c.max_wounds);
        c.set_tier(Tier::Medium);
        let medium = (c.max_drops, c.max_pools, c.max_wounds);
        c.set_tier(Tier::Low);
        let low = (c.max_drops, c.max_pools, c.max_wounds);
        assert!(high.0 > medium.0 && medium.0 > low.0);
        assert!(high.1 > medium.1 && medium.1 > low.1);
        assert!(high.2 > medium.2 && medium.2 > low.2);
    }
}
