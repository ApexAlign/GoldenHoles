//! Client-side visualisation of `LevelFeatures` (`zone`, `mover`, `hazard` level directives): one unlit,
//! translucent cuboid per feature, drawn from the level file only. Nothing here touches simulation, protocol
//! or prediction, and the render path is geometry-only: `MoverDef::pos_at(tick)` for mover position, tick-exact
//! hazard/zone phase for colour. Entity count is bounded by the feature count of the loaded level and nothing
//! is allocated per frame - meshes and materials are shared and each frame only rewrites transforms and the
//! piston charge / hazard pulse colours.
//!
//! Colours match `docs/ART.md` Circuit-Breaker: cyan data lanes, blue plasma core vent, hot piston charge,
//! neutral zones, red hazard telegraph / active.

use bevy::color::LinearRgba;
use bevy::prelude::*;
use shinobi_sim::level::{HazardDef, MoverDef, ShapeDef, ShapeKind, ShapeSegment, ZoneDef};

use crate::net::{FeaturesRes, Net, ViewRes};

/// One spawned feature entity, kept for the whole match.
#[derive(Component)]
struct FeatureEnt {
    kind: FeatureKind,
    /// Handle of this entity's own material so piston charge and hazard pulses can be retargeted per instance
    /// without touching other features.
    mat: Handle<StandardMaterial>,
}

/// Render-side copy of a level def: the update system never reads `LevelFeatures` directly.
#[derive(Clone)]
enum FeatureKind {
    Mover(MoverDef),
    Zone(ZoneDef),
    Hazard(HazardDef),
}

pub struct FeaturesVisPlugin;

impl Plugin for FeaturesVisPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_features)
            .add_systems(Update, update_features);
    }
}

/// Spawn one cuboid per feature. Runs once at start-up; features never change mid-match (`UdpGame::reconnect`
/// keeps the same features, and the shell never reloads a level), so entity count stays bounded for good.
fn spawn_features(
    mut commands: Commands,
    features: Res<FeaturesRes>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut mats: ResMut<Assets<StandardMaterial>>,
) {
    let f = &features.0;
    if f.is_empty() {
        return;
    }
    let unit = meshes.add(Cuboid::new(1.0, 1.0, 1.0));
    for m in &f.movers {
        let mat = mover_material(&mut mats, m);
        commands.spawn((
            Mesh3d(unit.clone()),
            MeshMaterial3d(mat.clone()),
            Transform::from_translation(Vec3::from(interp_pos_at(m, 0.0))).with_scale(Vec3::from(m.half) * 2.0),
            FeatureEnt { kind: FeatureKind::Mover(m.clone()), mat },
        ));
    }
    for z in &f.zones {
        let mat = zone_material(&mut mats, z);
        commands.spawn((
            Mesh3d(unit.clone()),
            MeshMaterial3d(mat.clone()),
            Transform::from_translation(Vec3::from(z.center)).with_scale(Vec3::from(z.half) * 2.0),
            FeatureEnt { kind: FeatureKind::Zone(z.clone()), mat },
        ));
    }
    for h in &f.hazards {
        let (colour, alpha, emissive) = hazard_colour_alpha(h, 0, 0.0);
        let mat = mats.add(StandardMaterial {
            base_color: Color::LinearRgba(LinearRgba::new(colour[0], colour[1], colour[2], alpha)),
            emissive: LinearRgba::new(emissive[0], emissive[1], emissive[2], 1.0),
            unlit: true,
            alpha_mode: AlphaMode::Blend,
            ..default()
        });
        commands.spawn((
            Mesh3d(unit.clone()),
            MeshMaterial3d(mat.clone()),
            Transform::from_translation(Vec3::from(h.center)).with_scale(Vec3::from(h.half) * 2.0),
            FeatureEnt { kind: FeatureKind::Hazard(h.clone()), mat },
        ));
    }
    // tubes and rings are static: one shared material per shape, no per-frame update
    for sh in &f.shapes {
        let mat = shape_material(&mut mats, sh);
        for seg in sh.segments() {
            commands.spawn((
                Mesh3d(unit.clone()),
                MeshMaterial3d(mat.clone()),
                shape_segment_transform(&seg),
            ));
        }
    }
}

/// World transform of one tube/ring segment: the same centre, half extents and yaw the collider uses.
pub fn shape_segment_transform(seg: &ShapeSegment) -> Transform {
    Transform::from_translation(Vec3::from(seg.center))
        .with_rotation(Quat::from_rotation_y(seg.yaw))
        .with_scale(Vec3::from(seg.half) * 2.0)
}

/// Tubes read as glass with a copper tint (De-Frag); rings as dark obsidian with a cyan edge glow (Vector).
fn shape_material(mats: &mut Assets<StandardMaterial>, sh: &ShapeDef) -> Handle<StandardMaterial> {
    let (base, alpha, emissive) = match sh.kind {
        ShapeKind::Tube => ([0.55, 0.75, 0.85], 0.45, [0.30, 0.16, 0.05]),
        ShapeKind::Ring => ([0.06, 0.07, 0.10], 1.0, [0.02, 0.18, 0.24]),
    };
    mats.add(StandardMaterial {
        base_color: Color::LinearRgba(LinearRgba::new(base[0], base[1], base[2], alpha)),
        emissive: LinearRgba::new(emissive[0], emissive[1], emissive[2], 1.0),
        alpha_mode: if alpha < 1.0 { AlphaMode::Blend } else { AlphaMode::Opaque },
        ..default()
    })
}

/// Mover material: pistons are forged copper that glows hot under charge; other solid movers are steel, ghost
/// movers read as translucent so they are never mistaken for solid geometry.
fn mover_material(mats: &mut Assets<StandardMaterial>, m: &MoverDef) -> Handle<StandardMaterial> {
    let piston = m.tag == "piston";
    let base = if piston { [0.55, 0.28, 0.12] } else { [0.50, 0.55, 0.60] };
    let alpha = if m.solid { 1.0 } else { 0.45 };
    mats.add(StandardMaterial {
        base_color: Color::LinearRgba(LinearRgba::new(base[0], base[1], base[2], alpha)),
        emissive: if piston {
            LinearRgba::BLACK
        } else {
            LinearRgba::new(0.06, 0.07, 0.09, 1.0)
        },
        unlit: true,
        alpha_mode: if alpha < 1.0 { AlphaMode::Blend } else { AlphaMode::Opaque },
        ..default()
    })
}

/// Zone material: translucent ambient volume. `speed_lane` cyan, `core_vent` blue plasma, everything else
/// neutral. Flat colour stays readable without bloom (Low tier).
fn zone_material(mats: &mut Assets<StandardMaterial>, z: &ZoneDef) -> Handle<StandardMaterial> {
    let (colour, alpha) = zone_colour_alpha(z);
    mats.add(StandardMaterial {
        base_color: Color::LinearRgba(LinearRgba::new(colour[0], colour[1], colour[2], alpha)),
        emissive: LinearRgba::new(colour[0] * 0.6, colour[1] * 0.6, colour[2] * 0.6, 1.0),
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        ..default()
    })
}

pub fn zone_colour_alpha(z: &ZoneDef) -> ([f32; 3], f32) {
    match z.tag.as_str() {
        "speed_lane" => ([0.15, 0.80, 1.00], 0.25), // cyan data lane: traversal
        "core_vent" => ([0.20, 0.45, 1.00], 0.35),  // blue plasma vent
        _ => ([0.55, 0.60, 0.70], 0.12),            // neutral: never competes with gameplay cues
    }
}

/// Rewrite every feature each rendered frame: mover position with sub-tick interpolation, piston charge
/// colour, zone breath and hazard telegraph/active pulse. No allocations.
fn update_features(
    net: Res<Net>,
    view: Res<ViewRes>,
    feats: Res<FeaturesRes>,
    fixed: Res<Time<Fixed>>,
    time: Res<Time>,
    mut mats: ResMut<Assets<StandardMaterial>>,
    mut query: Query<(&mut Transform, &FeatureEnt)>,
) {
    // fractional render tick: mover motion is exact at integer ticks and linear between them (each ping-pong
    // leg is linear in the sim's fixed-point math, so this matches server motion inside a leg).
    let tick_f = net.0.local_tick() + fixed.overstep_fraction() as f64;
    let tick = tick_f.floor().max(0.0) as u32;
    let t = time.elapsed_secs();
    for (mut tf, ent) in query.iter_mut() {
        match &ent.kind {
            FeatureKind::Mover(m) => {
                tf.translation = Vec3::from(interp_pos_at(m, tick_f));
                if m.tag == "piston" {
                    let c = piston_charge(m, tick_f);
                    if let Some(mut mat) = mats.get_mut(&ent.mat) {
                        // charge ramps over the first half of the cycle and peaks at full extension (the slam):
                        // for a 180-tick (3 s) piston that is a visible 90-tick wind-up.
                        let hot = c * c;
                        mat.emissive = LinearRgba::new(1.0 * hot, 0.9 * hot, 0.3 * hot, 1.0);
                    }
                }
            }
            FeatureKind::Zone(z) => {
                let (colour, alpha) = zone_colour_alpha(z);
                // gentle plasma breath so the vent looks alive even on flat Low tier; our own vent cooldown dims it
                let dim = if z.tag == ZoneDef::CORE_VENT { vent_dim(view.0.vent_cooldown) } else { 1.0 };
                let alpha = alpha * dim;
                let breath = (1.0 + 0.12 * (t * 2.5).sin()) * dim;
                if let Some(mut mat) = mats.get_mut(&ent.mat) {
                    mat.base_color = Color::LinearRgba(LinearRgba::new(colour[0], colour[1], colour[2], alpha));
                    mat.emissive = LinearRgba::new(colour[0] * breath * 0.6, colour[1] * breath * 0.6, colour[2] * breath * 0.6, 1.0);
                }
            }
            FeatureKind::Hazard(h) => {
                tf.translation = Vec3::from(h.center_at(tick, &feats.0.movers));
                let (colour, alpha, emissive) = hazard_colour_alpha(h, tick, t);
                if let Some(mut mat) = mats.get_mut(&ent.mat) {
                    mat.base_color = Color::LinearRgba(LinearRgba::new(colour[0], colour[1], colour[2], alpha));
                    mat.emissive = LinearRgba::new(emissive[0], emissive[1], emissive[2], 1.0);
                }
            }
        }
    }
}

/// Vent brightness while our `core_vent` cooldown runs: down to a quarter at the start, back to full when ready.
pub fn vent_dim(cooldown_left: f32) -> f32 {
    1.0 - 0.75 * cooldown_left.clamp(0.0, 1.0)
}

/// Mover position at a fractional render tick: exact at integer ticks, linear between them.
pub fn interp_pos_at(m: &MoverDef, tick_f: f64) -> [f32; 3] {
    let t0 = tick_f.floor().max(0.0) as u32;
    let frac = (tick_f - t0 as f64).clamp(0.0, 1.0) as f32;
    let a = m.pos_at(t0);
    let b = m.pos_at(t0 + 1);
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * frac)
}

/// Piston charge in `0..=1`: ramps up over the first half of the cycle (the wind-up), peaks at full extension
/// where the piston slams, fades back during retract. Driven by the same tick-exact cycle as `pos_at`, so a
/// 180-tick piston charges for 90 ticks and peaks at the slam.
pub fn piston_charge(m: &MoverDef, tick_f: f64) -> f32 {
    let t0 = tick_f.floor().max(0.0) as u32;
    let frac = (tick_f - t0 as f64).clamp(0.0, 1.0) as f32;
    let c0 = charge_at(m, t0);
    let c1 = charge_at(m, t0 + 1);
    c0 + (c1 - c0) * frac
}

fn charge_at(m: &MoverDef, tick: u32) -> f32 {
    let period = m.period_ticks as f32;
    let phase = ((tick as f32) + m.phase_ticks() as f32) % period;
    let s = phase / period; // [0, 1)
    if s < 0.5 { s * 2.0 } else { (1.0 - s) * 2.0 } // triangle: 0 -> 1 -> 0 across the cycle
}

/// Hazard phase for a whole-server tick (tick-exact, like the server's damage evaluation).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HazardPhase {
    Off,
    /// Warning window before the active part of the cycle.
    Telegraph,
    /// Damage is landing this tick.
    Active,
}

pub fn hazard_phase(h: &HazardDef, tick: u32) -> HazardPhase {
    if h.is_active(tick) {
        HazardPhase::Active
    } else if h.is_telegraph(tick) {
        HazardPhase::Telegraph
    } else {
        HazardPhase::Off
    }
}

/// Colour, alpha and emissive for a hazard at `tick` (phase) and render time `t` (pulse). Active = hot red;
/// telegraph = orange with a brightness pulse; off = dim ember.
pub fn hazard_colour_alpha(h: &HazardDef, tick: u32, t: f32) -> ([f32; 3], f32, [f32; 3]) {
    match hazard_phase(h, tick) {
        HazardPhase::Active => ([1.00, 0.10, 0.05], 0.55, [1.00, 0.08, 0.02]),
        HazardPhase::Telegraph => {
            let pulse = (t * 12.0).sin().abs();
            let e = 0.5 + pulse;
            ([1.00, 0.50, 0.00], 0.30 + 0.15 * pulse, [1.00 * e, 0.30 * e, 0.0])
        }
        HazardPhase::Off => ([0.40, 0.15, 0.15], 0.10, [0.0, 0.0, 0.0]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shinobi_sim::level::{LevelFeatures, MoveAxis};

    fn test_features() -> LevelFeatures {
        LevelFeatures {
            zones: vec![
                ZoneDef { center: [-10.0, 1.0, 0.0], half: [4.0, 1.0, 4.0], tag: "speed_lane".into(), tuning: Default::default() },
                ZoneDef { center: [10.0, 2.0, 0.0], half: [3.0, 3.0, 3.0], tag: "core_vent".into(), tuning: Default::default() },
                ZoneDef { center: [0.0, 0.0, 10.0], half: [2.0, 2.0, 2.0], tag: "misc".into(), tuning: Default::default() },
            ],
            movers: vec![
                MoverDef { center: [0.0, 3.0, 0.0], half: [1.0, 1.0, 1.0], axis: MoveAxis::X, distance: 8.0, period_ticks: 180, solid: true, tag: "piston".into(), tuning: Default::default() },
                MoverDef { center: [0.0, 6.0, 0.0], half: [1.0, 1.0, 1.0], axis: MoveAxis::Z, distance: 4.0, period_ticks: 120, solid: false, tag: "fan".into(), tuning: Default::default() },
            ],
            hazards: vec![
                HazardDef { center: [20.0, 1.0, 0.0], half: [2.0, 2.0, 2.0], period_ticks: 180, telegraph_ticks: 36, damage: 25.0, tag: "saw".into(), tuning: Default::default() },
            ],
            shapes: Vec::new(),
        }
    }

    #[test]
    fn zone_colour_picks_tag_palette() {
        let f = test_features();
        let (c, a) = zone_colour_alpha(&f.zones[0]);
        assert_eq!(c, [0.15, 0.8, 1.0]);
        assert!((0.15..0.4).contains(&a));
        let (c, _a) = zone_colour_alpha(&f.zones[1]);
        assert_eq!(c, [0.2, 0.45, 1.0]);
        let (c, _) = zone_colour_alpha(&f.zones[2]);
        assert!(c.iter().all(|v| *v >= 0.4 && *v <= 0.8)); // neutral, never competes with lanes/vent
    }

    #[test]
    fn a_tube_segment_renders_where_its_collider_is() {
        let sh = ShapeDef { kind: ShapeKind::Tube, center: [0.0, 5.0, 0.0], radius: 4.0, extent: 10.0, thickness: 0.5, segments: 16, tag: "tube".into(), tuning: Default::default() };
        for seg in sh.segments() {
            let t = shape_segment_transform(&seg);
            let outward = t.rotation * Vec3::X;
            let radial = Vec3::new(seg.center[0], 0.0, seg.center[2]).normalize();
            assert!(outward.dot(radial) > 0.999, "local +X points radially out: {outward:?} vs {radial:?}");
            assert!((t.translation.y - 5.0).abs() < 1e-6);
        }
    }

    #[test]
    fn vent_dims_while_cooling_and_is_full_when_ready() {
        assert_eq!(vent_dim(0.0), 1.0);
        assert!((vent_dim(1.0) - 0.25).abs() < 1e-6);
        assert!(vent_dim(0.5) > vent_dim(0.9));
        assert_eq!(vent_dim(7.0), vent_dim(1.0));
    }

    #[test]
    fn hazard_phase_colours_match_state() {
        let h = &test_features().hazards[0];
        // period 180, telegraph 36, active_ticks default 1: tick 0 active, 144 telegraph, 100 off
        let (c, a, _) = hazard_colour_alpha(h, 0, 0.0);
        assert_eq!(c, [1.0, 0.1, 0.05]);
        assert!(a > 0.4);
        let (c, a, _) = hazard_colour_alpha(h, 144, 0.0);
        assert_eq!(c, [1.0, 0.5, 0.0]);
        assert!(a >= 0.3);
        let (c, a, _) = hazard_colour_alpha(h, 100, 0.0);
        assert_eq!(c, [0.4, 0.15, 0.15]);
        assert!(a < 0.2);
        // the telegraph pulse moves with render time even at a fixed tick
        let (_, a0, _) = hazard_colour_alpha(h, 144, 0.0);
        let (_, a1, _) = hazard_colour_alpha(h, 144, 0.25); // |sin(3)| > 0 -> brighter pulse
        assert!(a1 > a0);
    }

    #[test]
    fn mover_interpolation_is_exact_at_integer_ticks() {
        let f = test_features();
        let m = &f.movers[0]; // x: 0 -> 8 over 180 ticks, period 180
        let start = interp_pos_at(m, 0.0);
        assert!((start[0] - 0.0).abs() < 1e-6 && start[1] == 3.0 && start[2] == 0.0);
        let mid = interp_pos_at(m, 90.0);
        assert!((mid[0] - 4.0).abs() < 1e-6, "{mid:?}"); // half distance at half the leg
        let end = interp_pos_at(m, 180.0);
        assert!((end[0] - 8.0).abs() < 1e-6, "{end:?}"); // full extension at leg end
        let back = interp_pos_at(m, 360.0);
        assert!((back[0] - 0.0).abs() < 1e-6, "{back:?}"); // back at the start after a full cycle
    }

    #[test]
    fn mover_interpolation_stays_inside_the_leg() {
        let f = test_features();
        let m = &f.movers[0];
        for f64_t in [0.0, 0.5, 45.25, 89.9, 90.0, 179.5, 180.0] {
            let p = interp_pos_at(m, f64_t);
            assert!(p[0] >= 0.0 && p[0] <= 8.0, "{f64_t} -> {p:?}");
            assert_eq!((p[1], p[2]), (3.0, 0.0));
        }
        // z axis ghost mover: 0 -> 4 over 120 ticks
        let z = &f.movers[1];
        let p = interp_pos_at(z, 60.0);
        assert!((p[2] - 2.0).abs() < 1e-6, "{p:?}");
    }

    #[test]
    fn piston_charge_ramps_over_half_cycle_and_peaks_at_slam() {
        let f = test_features();
        let m = &f.movers[0]; // period 180: 90-tick wind-up
        assert!((charge_at(m, 0) - 0.0).abs() < 1e-6);
        assert!((charge_at(m, 45) - 0.5).abs() < 1e-6);
        assert!((charge_at(m, 90) - 1.0).abs() < 1e-6); // full extension: peak charge, slam moment
        assert!((charge_at(m, 135) - 0.5).abs() < 1e-6);
        assert!((charge_at(m, 180) - 0.0).abs() < 1e-6); // retracted and cool again
        // fractional interpolation between ticks stays within [0, 1]
        for f64_t in [0.0, 1.5, 89.5, 90.5, 179.9] {
            let c = piston_charge(m, f64_t);
            assert!(c >= 0.0 && c <= 1.0, "{f64_t} -> {c}");
        }
    }

    #[test]
    fn placement_matches_centre_and_half() {
        let f = test_features();
        // cuboid scale is 2x half, centred on the def centre: placement readable at any distance because the
        // silhouette is the feature's true volume.
        for m in &f.movers {
            let s = Vec3::from(m.half) * 2.0;
            assert!((s - Vec3::splat(2.0)).length() < 1e-6);
        }
        for z in &f.zones {
            let s = Vec3::from(z.half) * 2.0;
            assert!(s.x > 0.0 && s.y > 0.0 && s.z > 0.0);
        }
        for h in &f.hazards {
            let s = Vec3::from(h.half) * 2.0;
            assert!(s.x > 0.0 && s.y > 0.0 && s.z > 0.0);
        }
    }

    #[test]
    fn feature_count_is_bounded_by_the_level_file() {
        let f = test_features();
        let count = f.zones.len() + f.movers.len() + f.hazards.len();
        assert_eq!(count, 6);
        assert!(f.is_empty() == false);
        assert!(LevelFeatures::default().is_empty());
    }
}
