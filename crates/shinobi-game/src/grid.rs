//! Bevy `Material` for the neon grid ground plane. The WGSL is embedded (no assets dir).

use std::sync::OnceLock;

use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::{Shader, ShaderRef};

use shinobi_client::shaders::GridMaterial as GridData;

const SRC: &str = include_str!("shaders/grid_floor.wgsl");
static SHADER: OnceLock<Handle<Shader>> = OnceLock::new();

/// Same field order as the `GridParams` struct in grid_floor.wgsl.
#[derive(ShaderType, Debug, Clone)]
pub struct GridParams {
    pub base: Vec4,
    pub glow: Vec4,
    pub grid: Vec4, // cell, major_every, line px, fog_far
    pub boxc: Vec4,
    pub boxh: Vec4,
    pub tint: Vec4,
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct GridFloor {
    #[uniform(0)]
    pub params: GridParams,
}

impl GridFloor {
    pub fn from_client(d: &GridData, fog_far: f32) -> Self {
        Self {
            params: GridParams {
                base: Vec4::from(d.base),
                glow: Vec4::from(d.glow),
                grid: Vec4::new(d.cell, d.major_every, d.line_px, fog_far),
                boxc: Vec4::ZERO,
                boxh: Vec4::ONE,
                tint: Vec4::ZERO, // mode 0: floor plane
            },
        }
    }

    /// A level box: neon edges and grid in its own frame. `mode` 1 = solid surface, 2 = ceiling skylight.
    pub fn for_box(d: &GridData, b: &shinobi_sim::level::LevelBox, tint: [f32; 3], mode: f32, fog_far: f32) -> Self {
        let mut g = Self::from_client(d, fog_far);
        g.params.boxc = Vec4::new(b.center[0], b.center[1], b.center[2], b.rot_z);
        g.params.boxh = Vec4::new(b.half[0], b.half[1], b.half[2], 0.07);
        g.params.tint = Vec4::new(tint[0], tint[1], tint[2], mode);
        g
    }
}

impl Material for GridFloor {
    fn alpha_mode(&self) -> AlphaMode {
        if self.params.tint.w > 1.5 { AlphaMode::Blend } else { AlphaMode::Opaque }
    }

    fn fragment_shader() -> ShaderRef {
        ShaderRef::Handle(SHADER.get().expect("GridPlugin not added").clone())
    }
}

pub struct GridPlugin;

impl Plugin for GridPlugin {
    fn build(&self, app: &mut App) {
        let h = app.world_mut().resource_mut::<Assets<Shader>>().add(Shader::from_wgsl(SRC, "shinobi/grid_floor.wgsl"));
        let _ = SHADER.set(h);
        app.add_plugins(MaterialPlugin::<GridFloor>::default());
    }
}
