//! Sky dome: a large sphere (drawn from the inside) that follows the camera, with a Material that paints
//! the near-black gradient, magenta horizon glow and stars (`shaders/sky.wgsl`).

use std::sync::OnceLock;

use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::{Shader, ShaderRef};

use shinobi_client::hud::Palette;
use shinobi_client::shaders::HorizonParams;

use crate::play::PlayerCam;

const SRC: &str = include_str!("shaders/sky.wgsl");
static SHADER: OnceLock<Handle<Shader>> = OnceLock::new();
/// Well inside the camera far plane (1000 by default).
const RADIUS: f32 = 450.0;

#[derive(ShaderType, Debug, Clone)]
pub struct SkyParams {
    pub horizon: Vec4,
    pub top: Vec4,
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct SkyDome {
    #[uniform(0)]
    pub params: SkyParams,
}

impl Material for SkyDome {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(SHADER.get().expect("SkyPlugin not added").clone())
    }

    // we are inside the sphere: draw its back faces
    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

#[derive(Component)]
struct Dome;

pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        let h = app.world_mut().resource_mut::<Assets<Shader>>().add(Shader::from_wgsl(SRC, "shinobi/sky.wgsl"));
        let _ = SHADER.set(h);
        app.add_plugins(MaterialPlugin::<SkyDome>::default())
            .add_systems(Startup, spawn)
            .add_systems(PostUpdate, follow.before(TransformSystems::Propagate));
    }
}

fn spawn(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut mats: ResMut<Assets<SkyDome>>) {
    let h = HorizonParams::tron(&Palette::TRON);
    commands.spawn((
        Mesh3d(meshes.add(Sphere::new(RADIUS).mesh().uv(48, 24))),
        MeshMaterial3d(mats.add(SkyDome {
            params: SkyParams { horizon: Vec4::from(h.horizon_col), top: Vec4::new(0.004, 0.006, 0.02, 1.0) },
        })),
        Transform::default(),
        Dome,
        bevy::camera::visibility::NoFrustumCulling,
    ));
}

fn follow(cam: Query<&Transform, (With<PlayerCam>, Without<Dome>)>, mut dome: Query<&mut Transform, With<Dome>>) {
    if let (Ok(c), Ok(mut d)) = (cam.single(), dome.single_mut()) {
        d.translation = c.translation;
    }
}
