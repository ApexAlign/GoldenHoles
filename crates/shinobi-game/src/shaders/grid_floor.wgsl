// Bevy material fragment shader: infinite neon grid on a large ground quad.
// Mirrors shinobi-client/shaders/tron_surface.wgsl (planar grid + distance fade), minus the baked `edge`
// attribute (no edge-distance vertex data yet) and using Bevy's view/globals bindings.
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::{view, globals}

struct GridParams {
    base: vec4<f32>,   // rgb dark surface
    glow: vec4<f32>,   // rgb neon line (HDR)
    grid: vec4<f32>,   // x cell, y major_every, z line px, w fog_far
    boxc: vec4<f32>,   // level box centre xyz, roll about Z (rad) in w
    boxh: vec4<f32>,   // level box half extents xyz, edge glow width (m) in w
    tint: vec4<f32>,   // rgb line/edge colour (HDR); w = mode: 0 floor plane, 1 box surface, 2 ceiling skylight
};
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> P: GridParams;

const TAU: f32 = 6.28318530718;

fn grid_line(p: vec2<f32>, cell: f32, w: f32) -> f32 {
    let q = p / cell;
    let fw = max(fwidth(q), vec2<f32>(1e-4));
    let d = abs(fract(q - 0.5) - 0.5) / fw;
    return 1.0 - clamp(min(d.x, d.y) - w, 0.0, 1.0);
}

// second-smallest of the three face distances: ~0 only near an edge, where two faces meet
fn second_min(e: vec3<f32>) -> f32 {
    return max(min(e.x, e.y), min(max(e.x, e.y), e.z));
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let mode = P.tint.w;
    var uv = in.world_position.xz;
    var edge = 0.0;
    if (mode > 0.5) {
        // box surface: work in the box's own frame so ramps (rolled boxes) get a grid and edges that follow them
        let d = in.world_position.xyz - P.boxc.xyz;
        let cs = cos(P.boxc.w);
        let sn = sin(P.boxc.w);
        let lp = vec3<f32>(cs * d.x + sn * d.y, -sn * d.x + cs * d.y, d.z);
        let e = max(P.boxh.xyz - abs(lp), vec3<f32>(0.0));
        if (e.x <= e.y && e.x <= e.z) {
            uv = lp.zy;
        } else if (e.y <= e.z) {
            uv = lp.xz;
        } else {
            uv = lp.xy;
        }
        edge = exp(-second_min(e) / max(P.boxh.w, 1e-3));
    } else {
        // floor plane (and anything else): grid on the dominant world axis
        let n = abs(normalize(in.world_normal));
        if (n.x > n.y && n.x > n.z) {
            uv = in.world_position.zy;
        } else if (n.z > n.y && n.z > n.x) {
            uv = in.world_position.xy;
        }
    }

    let minor = grid_line(uv, P.grid.x, P.grid.z);
    let major = grid_line(uv, P.grid.x * P.grid.y, P.grid.z * 1.8);
    let lines = minor * 0.35 + major;
    let dist = length(in.world_position.xyz - view.world_position);
    let fade = 1.0 - smoothstep(P.grid.w * 0.4, P.grid.w, dist);
    let pulse = 0.85 + 0.15 * sin(TAU * 0.25 * globals.time - (uv.x + uv.y) * 0.08);

    if (mode > 1.5) {
        // ceiling: a faint skylight grid, mostly see-through
        let a = clamp(lines * 0.35 * fade, 0.0, 0.5);
        return vec4<f32>(P.tint.rgb * lines, a);
    }
    if (mode > 0.5) {
        let col = P.base.rgb + P.tint.rgb * (lines * 0.55 * fade * pulse + edge * 3.2);
        return vec4<f32>(col, 1.0);
    }
    // floor: a soft glow halo around the major lines, and data pulses sliding diagonally across them
    // (only near the camera: far away the wide halos of converging lines would merge into a flat bright haze)
    let near = 1.0 - smoothstep(P.grid.w * 0.04, P.grid.w * 0.22, dist);
    let halo = grid_line(uv, P.grid.x * P.grid.y, P.grid.z * 7.0) * 0.22 * near;
    let wave = pow(max(0.0, sin(TAU * (uv.x * 0.012 + uv.y * 0.017) - globals.time * 1.1)), 14.0);
    let col = P.base.rgb + P.glow.rgb * ((lines + halo) * fade * pulse + major * wave * 3.2 * fade * near);
    return vec4<f32>(col, 1.0);
}
