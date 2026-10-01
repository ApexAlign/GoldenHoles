// Master world-surface shader: infinite glowing vector grid on all flat faces + hot neon edges.
// Output is HDR (>1.0) on purpose; bloom threshold picks it up. Needs fx_common.wgsl prepended.
//
// Vertex inputs: pos, normal, and `edge` = distance (world units) from the fragment to each of the
// triangle's 3 edges, baked at import (barycentric * altitude). No bake -> edge = vec3(1e3), edge
// glow off, grid still works.

struct Globals {
    view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,      // xyz, w = fog_far
};
struct Mat {
    base: vec4<f32>,         // rgb dark surface tint
    glow: vec4<f32>,         // rgb neon line colour (HDR ok)
    grid: vec4<f32>,         // x cell (m), y major every N cells, z line width (px), w unused
    edge: vec4<f32>,         // x falloff (m), y gain, z,w unused
};

@group(0) @binding(0) var<uniform> G: Globals;
@group(0) @binding(1) var<uniform> F: Fx;
@group(1) @binding(0) var<uniform> M: Mat;

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    @location(2) edge: vec3<f32>,
};
struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) wp: vec3<f32>,
    @location(1) n: vec3<f32>,
    @location(2) edge: vec3<f32>,
};

@vertex
fn vs(v: VIn) -> VOut {
    var o: VOut;
    o.wp = v.pos;
    o.n = v.nrm;
    o.edge = v.edge;
    o.clip = G.view_proj * vec4<f32>(v.pos, 1.0);
    return o;
}

// Anti-aliased line mask on a 2D coordinate; w = half width in pixels.
fn grid_line(p: vec2<f32>, cell: f32, w: f32) -> f32 {
    let q = p / cell;
    let fw = max(fwidth(q), vec2<f32>(1e-4));
    let d = abs(fract(q - 0.5) - 0.5) / fw;      // distance to nearest line, in pixels
    return 1.0 - clamp(min(d.x, d.y) - w, 0.0, 1.0);
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    // planar projection along the dominant normal axis: floors, walls, ceilings all world-aligned
    let a = abs(normalize(i.n));
    var uv = i.wp.xy;
    if (a.y >= a.x && a.y >= a.z) {
        uv = i.wp.xz;
    } else if (a.x >= a.z) {
        uv = i.wp.zy;
    }

    let cell = M.grid.x;
    let major_n = M.grid.y;
    let w = M.grid.z;
    let minor = grid_line(uv, cell, w);
    let major = grid_line(uv, cell * major_n, w * 1.8);
    let lines = minor * 0.35 + major;

    let e = min(min(i.edge.x, i.edge.y), i.edge.z);
    let edge_glow = exp(-e / max(M.edge.x, 1e-3)) * M.edge.y;

    // fade with distance -> infinite-grid illusion, kills far aliasing
    let dist = length(i.wp - G.cam_pos.xyz);
    let fog_far = G.cam_pos.w;
    let fade = 1.0 - smoothstep(fog_far * 0.4, fog_far, dist);

    // slow travelling pulse, deterministic (server time, hz * 1024 integer: 0.25 -> 256)
    let p = 0.85 + 0.15 * sin(TAU * 0.25 * F.c.z - (i.wp.x + i.wp.z) * 0.08);

    let col = M.base.rgb + M.glow.rgb * (lines * fade * p + edge_glow);
    return vec4<f32>(col, 1.0);
}
