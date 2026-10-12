//! Wrist-grapple rope: a glowing tron cable from the hand to the anchor, with light pulses running toward the anchor,
//! a slight sag when slack, an attach flash and a release snap. Own rope only (`RenderView::rope`); remote ropes need
//! the anchor on the wire. Attach / release are detected from the rope appearing / vanishing, same moment as
//! `CombatEvent::GrappleAttach` / `GrappleRelease`. Hooking an ENEMY (`LocalFx::grapple_pulling_enemy`): the rope
//! goes taut-bright and white-hot with a tension pulse that snaps on at the catch and settles to a throb, and the
//! packets race to the victim (`hook_tension`). Purely cosmetic.

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::impacts::{Impact, ImpactKind, ImpactQueue};
use crate::net::{Net, ViewRes};
use crate::team::LocalColour;

const SEGS: usize = 28;

/// One rope: the entity/mesh it draws into plus its attach/release edge state.
struct Slot {
    entity: Entity,
    mesh: Handle<Mesh>,
    /// Anchor and eye of the previous frame while attached (to place the release snap).
    last: Option<(Vec3, Vec3)>,
    /// Seconds since release; the snapping rope fades over `SNAP_S`.
    snap: Option<(f32, Vec3, Vec3)>,
    /// Seconds since the rope caught an enemy (own rope only); `None` = not hooking one.
    hooked: Option<f32>,
}

/// Slot `None` is our own rope; `Some(id)` is a remote player's.
#[derive(Resource, Default)]
struct Ropes {
    slots: std::collections::HashMap<Option<u16>, Slot>,
    mat: Option<Handle<StandardMaterial>>,
}

const SNAP_S: f32 = 0.18;

/// Brightness / width multiplier of a rope that has hooked an enemy, `age` seconds after the catch: a hard snap
/// (2.6x) that decays in a fraction of a second into a steady throb (1.0 .. 1.2x) while the victim is reeled in.
pub fn hook_tension(age: f32) -> f32 {
    let age = age.max(0.0);
    1.0 + 1.6 * (-6.0 * age).exp() + 0.2 * (age * std::f32::consts::TAU * 3.0).sin().abs()
}

/// Colour of a rope on an enemy: the team colour pushed halfway to white-hot.
pub fn hooked_colour(team: [f32; 3]) -> [f32; 3] {
    team.map(|c| c + (1.0 - c) * 0.55)
}

pub struct RopePlugin;

impl Plugin for RopePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Ropes>().add_systems(Startup, setup).add_systems(Update, update);
    }
}

fn empty() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3])
        .with_inserted_indices(Indices::U32(vec![0, 1, 2]))
}

fn setup(mut ropes: ResMut<Ropes>, mut mats: ResMut<Assets<StandardMaterial>>) {
    ropes.mat = Some(mats.add(StandardMaterial {
        base_color: Color::WHITE,
        unlit: true,
        alpha_mode: AlphaMode::Add,
        double_sided: true,
        cull_mode: None,
        ..default()
    }));
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
            // a packet every 2 m on short ropes, every len/18 on long ones (a 300 m hook is 18 packets, not 150 aliased ones)
            let spacing = (len / 18.0).max(2.0);
            let packets = (u * len / spacing - t * (4.0 * (len / 40.0).max(1.0)) / spacing).fract().abs(); // moving anchor-ward
            let pulse = (1.0 - packets * 4.0).max(0.0);
            (p, pulse)
        })
        .collect()
}

fn geometry(pts: &[(Vec3, f32)], fade: f32, tc: [f32; 3], eye: Vec3, boost: f32) -> (Vec<[f32; 3]>, Vec<[f32; 4]>, Vec<u32>) {
    let (mut pos, mut col, mut idx) = (Vec::new(), Vec::new(), Vec::new());
    for k in 0..pts.len() - 1 {
        let fwd = (pts[k + 1].0 - pts[k].0).normalize_or(Vec3::NEG_Z);
        let side = fwd.cross(Vec3::Y).normalize_or(Vec3::X);
        let up = side.cross(fwd).normalize_or(Vec3::Y);
        for axis in [up, side] {
            let base = pos.len() as u32;
            for (p, pulse) in [pts[k], pts[k + 1]] {
                // width follows distance from the eye (about constant on screen); a fixed world width balloons near the hand
                // (clamp is 3 m: the old 0.12 m cap made a rope beyond ~55 m shrink to a sub-pixel line on screen)
                let w = (0.006 + 0.0022 * p.distance(eye)).min(3.0) * (1.0 + 0.8 * pulse) * (1.0 + 0.5 * (boost - 1.0));
                let g = (1.4 + 2.2 * pulse) * fade * boost;
                let c = [tc[0] * g + 0.4 * pulse, tc[1] * g + 0.4 * pulse, tc[2] * g + 0.4 * pulse, fade];
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
    mut commands: Commands,
    time: Res<Time>,
    net: Res<Net>,
    view: Res<ViewRes>,
    local: Res<LocalColour>,
    mut ropes: ResMut<Ropes>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut vis: Query<&mut Visibility>,
    mut impacts: ResMut<ImpactQueue>,
) {
    let (t, dt) = (time.elapsed_secs(), time.delta_secs());
    let Some(mat) = ropes.mat.clone() else { return };

    // current ropes: ours from the prediction, others from the wire (anchor) + their drawn position
    let mut now: Vec<(Option<u16>, Option<(Vec3, Vec3)>, [f32; 3])> =
        vec![(None, view.0.rope.map(|(e, a)| (Vec3::from(e), Vec3::from(a))), local.0)];
    for (id, smp) in &view.0.remotes {
        if let Some(a) = net.0.game.net.rope_anchor(*id) {
            let eye = Vec3::from(smp.pos) + Vec3::Y * 0.5;
            let team = crate::team::colour_of(&net, *id);
            now.push((Some(*id), Some((eye, Vec3::from(a))), team));
        }
    }
    // slots that exist but have no live rope this frame must still run their release snap
    for id in ropes.slots.keys().copied().collect::<Vec<_>>() {
        if !now.iter().any(|(i, _, _)| *i == id) {
            let team = id.map_or(local.0, |i| crate::team::colour_of(&net, i));
            now.push((id, None, team));
        }
    }

    // own grapple that hit nothing: fizzle at the wrist, a little way along the aim
    if view.0.fx.grapple_missed {
        let (yaw, pitch) = view.0.look_angles();
        let fwd = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0) * Vec3::NEG_Z;
        let eye = Vec3::from(view.0.eye);
        impacts.0.push(Impact { pos: hand_from(eye, eye + fwd * 10.0) + fwd * 0.3, color: local.0, kind: ImpactKind::Fizzle });
    }

    for (id, cur, team) in now {
        if cur.is_none() && !ropes.slots.contains_key(&id) {
            continue;
        }
        let slot = ropes.slots.entry(id).or_insert_with(|| {
            let mesh = meshes.add(empty());
            let entity = commands
                .spawn((Mesh3d(mesh.clone()), MeshMaterial3d(mat.clone()), Transform::default(), Visibility::Hidden, NoFrustumCulling))
                .id();
            Slot { entity, mesh, last: None, snap: None, hooked: None }
        });
        // edges: attach flash at the anchor (shooter's team colour), release snap
        match (slot.last, cur) {
            (None, Some((_, a))) => impacts.0.push(Impact { pos: a, color: team, kind: ImpactKind::Anchor }),
            (Some((a, e)), None) => slot.snap = Some((0.0, a, e)),
            _ => {}
        }
        slot.last = cur.map(|(e, a)| (a, e));

        // our rope on an enemy: the catch lands a bright burst on them and the rope snaps taut
        let pulling = id.is_none() && cur.is_some() && view.0.fx.grapple_pulling_enemy;
        match (slot.hooked, pulling, cur) {
            (None, true, Some((_, a))) => {
                impacts.0.push(Impact { pos: a, color: [1.0, 0.3, 0.2], kind: ImpactKind::Hit { kill: false } });
                slot.hooked = Some(0.0);
            }
            (Some(age), true, _) => slot.hooked = Some(age + dt),
            (_, false, _) => slot.hooked = None,
            _ => {}
        }
        let draw = if let Some((eye, anchor)) = cur {
            match slot.hooked {
                Some(age) => Some((cable(hand_from(eye, anchor), anchor, 1.0, t * 2.5), 1.0, eye, hook_tension(age), hooked_colour(team))),
                None => Some((cable(hand_from(eye, anchor), anchor, 1.0, t), 1.0, eye, 1.0, team)),
            }
        } else if let Some((age, a, e)) = slot.snap.as_mut() {
            *age += dt;
            if *age >= SNAP_S {
                slot.snap = None;
                None
            } else {
                let (k, a, e) = (1.0 - *age / SNAP_S, *a, *e);
                Some((cable(hand_from(e, a), a, 0.0, t), k, e, 1.0, team))
            }
        } else {
            None
        };

        let Ok(mut v) = vis.get_mut(slot.entity) else { continue };
        match draw {
            Some((pts, fade, eye, boost, tc)) => {
                let (pos, col, idx) = geometry(&pts, fade, tc, eye, boost);
                if let Some(mut m) = meshes.get_mut(&slot.mesh) {
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

    // drop slots of players who left (hidden, no snap running)
    let gone: Vec<_> = ropes
        .slots
        .iter()
        .filter(|(id, sl)| id.map_or(false, |i| !view.0.remotes.iter().any(|(r, _)| *r == i)) && sl.last.is_none() && sl.snap.is_none())
        .map(|(id, _)| *id)
        .collect();
    for id in gone {
        if let Some(sl) = ropes.slots.remove(&id) {
            commands.entity(sl.entity).despawn();
        }
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
    fn hook_tension_snaps_on_then_settles_to_a_throb() {
        assert!(hook_tension(0.0) > 2.4);
        assert!(hook_tension(0.0) > hook_tension(0.15) && hook_tension(0.15) > hook_tension(0.6));
        for i in 0..200 {
            let v = hook_tension(1.0 + i as f32 * 0.05);
            assert!((1.0..1.3).contains(&v), "settled throb stays near 1: {v}");
        }
        assert_eq!(hook_tension(-1.0), hook_tension(0.0));
    }

    #[test]
    fn hooked_rope_is_whiter_than_the_team_colour() {
        let c = hooked_colour([0.0, 0.9, 1.0]);
        assert!(c[0] > 0.5 && c[1] > 0.9 && c[2] >= 1.0 && c.iter().all(|v| *v <= 1.0));
    }

    #[test]
    fn hand_is_beside_and_below_the_eye() {
        let eye = Vec3::new(0.0, 1.7, 0.0);
        let h = hand_from(eye, Vec3::new(0.0, 1.7, -10.0));
        assert!(h.y < eye.y && h.x.abs() > 0.1 && h.z < eye.z);
    }
}
