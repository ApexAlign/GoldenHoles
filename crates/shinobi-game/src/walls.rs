//! Motion trails: a thin neon ribbon that fades in about 0.7 s behind a moving remote player. Purely cosmetic and
//! rebuilt on the client from interpolated positions (the game has no persistent trail walls; nothing here is
//! geometry or collision). The local first-person view gets no trail (dash streaks come from `emit`).
//! Geometry and fade come from `shinobi_client::trail::wall_strip`.

use std::collections::{HashMap, VecDeque};

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::PrimitiveTopology;
use bevy::prelude::*;

use shinobi_client::fx::LightWallCfg;
use shinobi_client::hud::Palette;
use shinobi_client::trail::{ribbon_color, wall_strip, TrailPoint};

use crate::net::{Net, ViewRes};

/// Min distance between recorded points (m); matches the sim's default trail spacing order of magnitude.
const SPACING: f32 = 0.25;
/// Ribbon is a thin band at hip height, not a wall.
const WALL_HEIGHT: f32 = 0.16;
const HIP_ABOVE_FEET: f32 = 0.85;
const LIFE_S: f32 = 0.7;
/// Wall base sits at the player's feet; `Sample::pos` is the capsule centre.
const FEET_BELOW: f32 = 0.81 - HIP_ABOVE_FEET;

struct Wall {
    pts: VecDeque<TrailPoint>,
    entity: Entity,
    mesh: Handle<Mesh>,
}

#[derive(Resource, Default)]
pub struct Walls(HashMap<u16, Wall>);

#[derive(Resource)]
struct WallLook(Handle<StandardMaterial>);

pub struct WallsPlugin;

impl Plugin for WallsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Walls>().add_systems(Startup, setup_look).add_systems(Update, update_walls);
    }
}

fn setup_look(mut commands: Commands, mut mats: ResMut<Assets<StandardMaterial>>) {
    // vertex colour carries HDR neon + per-vertex alpha; unlit + additive so it glows into bloom
    commands.insert_resource(WallLook(mats.add(StandardMaterial {
        base_color: Color::WHITE,
        unlit: true,
        alpha_mode: AlphaMode::Add,
        double_sided: true,
        cull_mode: None,
        ..default()
    })));
}

/// Placeholder until a wall has 2+ points. Zero-vertex meshes upset Bevy's mesh slab allocator, so use a
/// degenerate, fully transparent triangle instead (the entity is Hidden anyway).
fn empty_mesh() -> Mesh {
    Mesh::new(PrimitiveTopology::TriangleStrip, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0f32; 4]; 3])
}

fn update_walls(
    mut commands: Commands,
    net: Res<Net>,
    view: Res<ViewRes>,
    mut walls: ResMut<Walls>,
    mut meshes: ResMut<Assets<Mesh>>,
    look: Res<WallLook>,
    quality: Res<crate::mobile::QualityRes>,
    mut vis: Query<&mut Visibility>,
) {
    let s = &net.0;
    let v = &view.0;
    if !v.connected {
        return;
    }
    let now = s.local_tick() as f32 / 60.0;
    let server_time = now as f64;
    let cfg = LightWallCfg { life_s: LIFE_S, fade_start: 0.15, max_alpha: 0.55, pulse_depth: 0.0, ..Default::default() };
    let pal = Palette::TRON;

    // remote players only: the first-person view never trails itself
    let players: Vec<(u16, [f32; 3])> = v.remotes.iter().map(|(id, smp)| (*id, smp.pos)).collect();
    let seen: Vec<u16> = players.iter().map(|(id, _)| *id).collect();

    for (id, pos) in &players {
        let id = id;
        let w = walls.0.entry(*id).or_insert_with(|| {
            let mesh = meshes.add(empty_mesh());
            let entity = commands
                .spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(look.0.clone()),
                    Transform::default(),
                    Visibility::Hidden,
                    NoFrustumCulling, // geometry changes every frame; its Aabb would be stale
                ))
                .id();
            Wall { pts: VecDeque::new(), entity, mesh }
        });

        let p = Vec3::new(pos[0], pos[1] - FEET_BELOW, pos[2]);
        let far_enough = w.pts.back().map_or(true, |l| (Vec3::from(l.pos) - p).length() >= SPACING);
        if far_enough {
            w.pts.push_back(TrailPoint { pos: p.to_array(), born: now });
            if w.pts.len() > quality.0.wall_points {
                w.pts.pop_front();
            }
        }
        while w.pts.front().map_or(false, |f| now - f.born > cfg.life_s) {
            w.pts.pop_front();
        }

        let team = crate::team::remote_team(&net, *id).unwrap_or(crate::team::Team::Orange);
        let (a, b) = w.pts.as_slices();
        let strip = wall_strip((a, b), now, WALL_HEIGHT, &cfg, server_time);
        if strip.is_empty() {
            if let Ok(mut v) = vis.get_mut(w.entity) {
                *v = Visibility::Hidden;
            }
            continue;
        }
        let positions: Vec<[f32; 3]> = strip.iter().map(|v| v.pos).collect();
        let colors: Vec<[f32; 4]> = strip
            .iter()
            .map(|v| {
                // cool tail -> hot head, hot rim on the top edge
                let c = ribbon_color(team.hdr(&pal, 0.25), team.hdr(&pal, 1.0), v.heat, v.v > 0.5);
                [c[0], c[1], c[2], v.alpha]
            })
            .collect();
        if let Some(mut m) = meshes.get_mut(&w.mesh) {
            *m = Mesh::new(PrimitiveTopology::TriangleStrip, RenderAssetUsages::default())
                .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
                .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors);
        }
        if let Ok(mut v) = vis.get_mut(w.entity) {
            *v = Visibility::Visible;
        }
    }

    walls.0.retain(|id, w| {
        let keep = seen.contains(id);
        if !keep {
            commands.entity(w.entity).despawn();
        }
        keep
    });
}
