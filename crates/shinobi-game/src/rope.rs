//! Wrist-grapple rope: a glowing tron cable from the hand to the anchor, with light pulses running toward the anchor,
//! a slight sag when slack, an attach flash and a release snap. Own rope only (`RenderView::rope`); remote ropes need
//! the anchor on the wire. Attach / release are detected from the rope appearing / vanishing, same moment as
//! `CombatEvent::GrappleAttach` / `GrappleRelease`. Purely cosmetic.

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use shinobi_client::hud::Palette;

use crate::impacts::{Impact, ImpactKind, ImpactQueue};
use crate::net::ViewRes;
use crate::team::LocalTeam;

const SEGS: usize = 28;
const HALF_W: f32 = 0.035;

#[derive(Resource)]
struct Rope {
    entity: Entity,
    mesh: Handle<Mesh>,
    /// Anchor and eye of the previous frame while attached (to place the release snap).
    last: Option<(Vec3, Vec3)>,
    /// Seconds since release; the snapping rope fades over `SNAP_S`.
    snap: Option<(f32, Vec3, Vec3)>,
}

const SNAP_S: f32 = 0.18;

pub struct RopePlugin;

impl Plugin for RopePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup).add_systems(Update, update);
    }
}

fn empty() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3])
        .with_inserted_indices(Indices::U32(vec![0, 1, 2]))
}

fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<StandardMaterial>>) {
    let mesh = meshes.add(empty());
    let mat = mats.add(StandardMaterial {
        base_color: Color::WHITE,
        unlit: true,
        alpha_mode: AlphaMode::Add,
        double_sided: true,
        cull_mode: None,
        ..default()
    });
    let entity = commands
        .spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat), Transform::default(), Visibility::Hidden, NoFrustumCulling))
        .id();
    commands.insert_resource(Rope { entity, mesh, last: None, snap: None });
}

/// Hand position: ahead of and beside the eye, below it, so the cable leaves the wrist rather than the crosshair.
pub fn hand_from(eye: Vec3, anchor: Vec3) -> Vec3 {
    let dir = (anchor - eye).normalize_or(Vec3::NEG_Z);
    let side = dir.cross(Vec3::Y).normalize_or(Vec3::X);
    eye + dir * 0.4 + side * 0.22 - Vec3::Y * 0.2
}

/// Cable points hand -> anchor with a sag that vanishes as the rope goes taut, plus a pulse value per point
/// (0..1) from light packets travelling toward the anchor.
pub fn cable(hand: Vec3, anchor: Vec3, tension: f32, t: f32) -> Vec<(Vec3, f32)> {
    let len = hand.distance(anchor);
    let sag = (1.0 - tension.clamp(0.0, 1.0)) * 0.04 * len;
    (0..=SEGS)
        .map(|i| {
            let u = i as f32 / SEGS as f32;
            let p = hand.lerp(anchor, u) - Vec3::Y * (4.0 * u * (1.0 - u) * sag);
            let packets = (u * len * 0.5 - t * 4.0).fract().abs(); // packet every 2 m, moving anchor-ward
            let pulse = (1.0 - packets * 4.0).max(0.0);
            (p, pulse)
        })
        .collect()
}

fn geometry(pts: &[(Vec3, f32)], fade: f32, pal: &Palette) -> (Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<u32>) {
    let (mut pos, mut col, mut idx) = (Vec::new(), Vec::new(), Vec::new());
    for k in 0..pts.len() - 1 {
        let fwd = (pts[k + 1].0 - pts[k].0).normalize_or(Vec3::NEG_Z);
        let side = fwd.cross(Vec3::Y).normalize_or(Vec3::X);
        let up = side.cross(fwd).normalize_or(Vec3::Y);
        for axis in [up, side] {
            let base = pos.len() as u32;
            for (p, pulse) in [pts[k], pts[k + 1]] {
                let w = HALF_W * (1.0 + 1.5 * pulse);
                let g = (1.5 + 5.0 * pulse) * fade;
                let c = [pal.cyan.0 * g, pal.cyan.1 * g + 0.4 * pulse, pal.cyan.2 * g, fade];
                pos.push((p + axis * w).to_array());
                pos.push((p - axis * w).to_array());
                col.extend([c, c]);
            }
            idx.extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
        }
    }
    (pos, col, idx)
}

fn update(
    time: Res<Time>,
    view: Res<ViewRes>,
    mut rope: ResMut<Rope>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut vis: Query<&mut Visibility>,
    mut impacts: ResMut<ImpactQueue>,
    local: Res<LocalTeam>,
) {
    let (t, dt) = (time.elapsed_secs(), time.delta_secs());
    let pal = Palette::TRON;
    let cur = view.0.rope.map(|(e, a)| (Vec3::from(e), Vec3::from(a)));

    // edges: attach flash at the anchor, release snap
    match (rope.last, cur) {
        (None, Some((_, a))) => impacts.0.push(Impact { pos: a, team: local.0, kind: ImpactKind::Gone }),
        (Some((a, e)), None) => rope.snap = Some((0.0, a, e)),
        _ => {}
    }
    rope.last = cur.map(|(e, a)| (a, e));

    let draw = if let Some((eye, anchor)) = cur {
        let hand = hand_from(eye, anchor);
        Some((cable(hand, anchor, 1.0, t), 1.0))
    } else if let Some((age, a, e)) = rope.snap.as_mut() {
        *age += dt;
        if *age >= SNAP_S {
            rope.snap = None;
            None
        } else {
            let (k, a, e) = (1.0 - *age / SNAP_S, *a, *e);
            Some((cable(hand_from(e, a), a, 0.0, t), k))
        }
    } else {
        None
    };

    let Ok(mut v) = vis.get_mut(rope.entity) else { return };
    match draw {
        Some((pts, fade)) => {
            let (pos, col, idx) = geometry(&pts, fade, &pal);
            if let Some(mut m) = meshes.get_mut(&rope.mesh) {
                *m = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
                    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, pos)
                    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, col)
                    .with_inserted_indices(Indices::U32(idx));
            }
            *v = Visibility::Visible;
        }
        None => *v = Visibility::Hidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cable_runs_hand_to_anchor_and_taut_has_no_sag() {
        let (h, a) = (Vec3::new(0.0, 1.0, 0.0), Vec3::new(10.0, 6.0, 0.0));
        let c = cable(h, a, 1.0, 0.0);
        assert_eq!(c.len(), SEGS + 1);
        assert!(c[0].0.distance(h) < 1e-5 && c[SEGS].0.distance(a) < 1e-5);
        let mid = c[SEGS / 2].0;
        assert!(mid.distance(h.lerp(a, 0.5)) < 1e-4, "taut rope is straight");
        let slack = cable(h, a, 0.0, 0.0);
        assert!(slack[SEGS / 2].0.y < h.lerp(a, 0.5).y, "slack rope sags");
    }

    #[test]
    fn pulses_move_toward_the_anchor() {
        let (h, a) = (Vec3::ZERO, Vec3::new(0.0, 0.0, -20.0));
        let at = |t: f32| cable(h, a, 1.0, t).iter().map(|p| p.1).enumerate().max_by(|x, y| x.1.total_cmp(&y.1)).unwrap().0;
        assert_ne!(at(0.0), at(0.05));
        assert!(cable(h, a, 1.0, 0.3).iter().all(|p| (0.0..=1.0).contains(&p.1)));
    }

    #[test]
    fn hand_is_beside_and_below_the_eye() {
        let eye = Vec3::new(0.0, 1.7, 0.0);
        let h = hand_from(eye, Vec3::new(0.0, 1.7, -10.0));
        assert!(h.y < eye.y && h.x.abs() > 0.1 && h.z < eye.z);
    }
}
