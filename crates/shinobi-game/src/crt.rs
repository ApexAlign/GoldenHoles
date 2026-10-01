//! CRT + camera-effect composite as a Bevy `FullscreenMaterial`, ordered after bloom and before
//! tonemapping. Uniform = `FxUniforms` (16 floats) + `CrtSettings` (12 floats), both from shinobi-client.

use std::sync::OnceLock;

use bevy::core_pipeline::fullscreen_material::{FullscreenMaterial, FullscreenMaterialPlugin};
use bevy::core_pipeline::{tonemapping::tonemapping, Core3dSystems};
use bevy::ecs::schedule::{IntoScheduleConfigs, ScheduleConfigs};
use bevy::ecs::system::BoxedSystem;
use bevy::post_process::bloom::bloom;
use bevy::prelude::*;
use bevy::render::extract_component::ExtractComponent;
use bevy::render::render_resource::ShaderType;
use bevy::shader::{Shader, ShaderRef};

use shinobi_client::fx::FxUniforms;
use shinobi_client::shaders::CrtSettings;

const SRC: &str = include_str!("shaders/crt.wgsl");
static SHADER: OnceLock<Handle<Shader>> = OnceLock::new();

/// Field order is the `Crt` struct in crt.wgsl: 4 x vec4 from `FxUniforms`, then 3 x vec4 from `CrtSettings`.
#[derive(Component, ExtractComponent, Clone, Copy, ShaderType, Default)]
pub struct CrtEffect {
    pub a: Vec4,
    pub b: Vec4,
    pub c: Vec4,
    pub d: Vec4,
    pub scan: Vec4,
    pub misc: Vec4,
    pub slash_col: Vec4,
}

impl CrtEffect {
    pub fn new(fx: &FxUniforms, crt: &CrtSettings) -> Self {
        let f = fx.to_array();
        let c = crt.to_array();
        Self {
            a: Vec4::new(f[0], f[1], f[2], f[3]),
            b: Vec4::new(f[4], f[5], f[6], f[7]),
            c: Vec4::new(f[8], f[9], f[10], f[11]),
            d: Vec4::new(f[12], f[13], f[14], f[15]),
            scan: Vec4::new(c[0], c[1], c[2], c[3]),
            misc: Vec4::new(c[4], c[5], c[6], c[7]),
            slash_col: Vec4::new(c[8], c[9], c[10], c[11]),
        }
    }
}

impl FullscreenMaterial for CrtEffect {
    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(SHADER.get().expect("CrtPlugin not added").clone())
    }

    // Bloom and fullscreen effects share `PostProcess` with no relative order by default; the CRT must
    // see the bloomed image, so run strictly after bloom (and still before tonemapping).
    fn schedule_configs(system: ScheduleConfigs<BoxedSystem>) -> ScheduleConfigs<BoxedSystem> {
        system.in_set(Core3dSystems::PostProcess).after(bloom).before(tonemapping)
    }
}

pub struct CrtPlugin;

impl Plugin for CrtPlugin {
    fn build(&self, app: &mut App) {
        let h = app.world_mut().resource_mut::<Assets<Shader>>().add(Shader::from_wgsl(SRC, "shinobi/crt.wgsl"));
        let _ = SHADER.set(h);
        app.add_plugins(FullscreenMaterialPlugin::<CrtEffect>::default());
    }
}
