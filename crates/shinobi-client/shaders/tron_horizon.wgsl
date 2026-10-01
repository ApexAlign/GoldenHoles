// Fullscreen pass drawn BEFORE geometry: true infinite grid floor (ray-plane y = 0, independent of
// any mesh) + horizon glow + dark sky. Geometry then draws over it. Needs fx_common.wgsl prepended.
// Depth convention agnostic: ray is rebuilt through cam_pos, not from a raw depth value.

struct Sky {
    inv_view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,       // xyz, w = floor height (y)
    floor_col: vec4<f32>,     // rgb neon line colour (HDR)
    horizon_col: vec4<f32>,   // rgb glow at horizon (HDR)
    params: vec4<f32>,        // x cell (m), y major every N, z line width px, w fade distance (m)
    sky_top: vec4<f32>,       // rgb zenith colour
};
@group(0) @binding(0) var<uniform> S: Sky;
@group(0) @binding(1) var<uniform> F: Fx;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VOut {
    // single oversized triangle
    let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var o: VOut;
    o.clip = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    o.ndc = p * 2.0 - 1.0;
    return o;
}

fn grid_line(p: vec2<f32>, cell: f32, w: f32) -> f32 {
    let q = p / cell;
    let fw = max(fwidth(q), vec2<f32>(1e-4));
    let d = abs(fract(q - 0.5) - 0.5) / fw;
    return 1.0 - clamp(min(d.x, d.y) - w, 0.0, 1.0);
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let pw = S.inv_view_proj * vec4<f32>(i.ndc, 0.5, 1.0);
    let target_pt = pw.xyz / pw.w;
    let ro = S.cam_pos.xyz;
    let rd = normalize(target_pt - ro);

    // horizon glow: tight hot band at rd.y ~ 0 plus wide falloff
    let h = abs(rd.y);
    let glow = exp(-h * 38.0) * 1.6 + exp(-h * 6.0) * 0.35;
    let pl = 0.85 + 0.15 * sin(TAU * 0.25 * F.c.z);
    var col = mix(vec3<f32>(0.0), S.sky_top.rgb, clamp(rd.y, 0.0, 1.0));
    col = col + S.horizon_col.rgb * glow * pl;

    // floor: ray hits plane below the camera. Branchless: fwidth() must run in uniform control flow.
    let dy = min(rd.y, -1e-4);
    let floor_y = S.cam_pos.w;
    let t = (floor_y - ro.y) / dy;
    let hit = ro + rd * t;
    let cell = S.params.x;
    let minor = grid_line(hit.xz, cell, S.params.z);
    let major = grid_line(hit.xz, cell * S.params.y, S.params.z * 1.8);
    let lines = minor * 0.35 + major;
    let facing_floor = select(0.0, 1.0, rd.y < -1e-4 && ro.y > floor_y);
    let fade = (1.0 - smoothstep(S.params.w * 0.2, S.params.w, t)) * facing_floor;
    // far grid dissolves into the horizon glow instead of aliasing
    col = col * (1.0 - fade * 0.5) + S.floor_col.rgb * lines * fade * pl;
    return vec4<f32>(col, 1.0);
}
