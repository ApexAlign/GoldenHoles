// Sky dome: near-black gradient, magenta horizon glow (same curve as shinobi-client tron_horizon.wgsl),
// and sparse stars. Drawn on the inside of a large sphere that follows the camera.
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::{view, globals}

struct SkyParams {
    horizon: vec4<f32>,  // rgb HDR glow colour
    top: vec4<f32>,      // rgb zenith tint (very dark)
};
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> S: SkyParams;

const TAU: f32 = 6.28318530718;

fn hash3(p: vec3<f32>) -> f32 {
    var q = fract(p * vec3<f32>(0.1031, 0.1030, 0.0973));
    q = q + dot(q, q.yxz + 33.33);
    return fract((q.x + q.y) * q.z);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let rd = normalize(in.world_position.xyz - view.world_position);
    let h = abs(rd.y);

    // slow pulse in step with the world grid
    let pulse = 0.85 + 0.15 * sin(TAU * 0.25 * globals.time);
    let glow = exp(-h * 38.0) * 1.6 + exp(-h * 6.0) * 0.35;

    var col = S.top.rgb * pow(clamp(rd.y, 0.0, 1.0), 0.6);
    col = col + S.horizon.rgb * glow * pulse;

    // stars: hash on a direction lattice, sparse, only above the horizon
    let cell = rd * 140.0;
    let id = floor(cell);
    let f = fract(cell) - 0.5;
    let r = hash3(id);
    let star = step(0.9965, r) * smoothstep(0.3, 0.0, length(f)) * smoothstep(0.04, 0.25, rd.y);
    let twinkle = 0.6 + 0.4 * sin(globals.time * 3.0 + r * 40.0);
    col = col + vec3<f32>(0.7, 0.9, 1.0) * star * twinkle * 1.6;

    // ground side stays dark; the floor shader draws its own grid
    col = col * select(1.0, 0.25, rd.y < 0.0);
    return vec4<f32>(col, 1.0);
}
