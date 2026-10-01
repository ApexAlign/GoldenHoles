// GPU-instanced particle ribbons: shuriken/laser trails, katana arc segments, dash streaks, sparks.
// One draw for everything: instance = segment p0->p1, camera-facing quad (4 verts, strip).
// Two fragment entry points: fs_ribbon (soft tapered bar) and fs_disc (shuriken SDF, spins).
// Needs fx_common.wgsl prepended.
//
// Instance = 64 B (4 x vec4<f32>): segment ends + widths, HDR colour, timing/spin. Age comes from synced
// server time so trails fade identically on every client.

struct Inst {
    p0: vec4<f32>,      // xyz, width at p0 (m)
    p1: vec4<f32>,      // xyz, width at p1 (m)
    color: vec4<f32>,   // rgb HDR, a = intensity
    params: vec4<f32>,  // x spawn_time (server s), y life (s), z spin (rad/s), w seed
};
struct RibU {
    view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
};
@group(0) @binding(0) var<uniform> R: RibU;
@group(0) @binding(1) var<uniform> F: Fx;
@group(0) @binding(2) var<storage, read> inst: array<Inst>;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,     // x along segment 0..1, y across -1..1
    @location(1) color: vec3<f32>,
    @location(2) fade: f32,
    @location(3) spin: f32,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VOut {
    let I = inst[ii];
    let along = f32(vi >> 1u);              // 0,0,1,1
    let side = f32(vi & 1u) * 2.0 - 1.0;    // -1,1,-1,1
    let p = mix(I.p0.xyz, I.p1.xyz, along);
    let w = mix(I.p0.w, I.p1.w, along);

    let axis = normalize(I.p1.xyz - I.p0.xyz + vec3<f32>(1e-6, 0.0, 0.0));
    let to_cam = normalize(R.cam_pos.xyz - p);
    let right = normalize(cross(axis, to_cam));   // billboard about the segment axis

    // age from synced server time: same on every client. wrapped clock -> handle wrap.
    var age = F.c.z - I.params.x;
    if (age < 0.0) { age = age + 1024.0; }
    let fade = 1.0 - smoothstep(I.params.y * 0.5, I.params.y, age);

    var o: VOut;
    o.clip = R.view_proj * vec4<f32>(p + right * side * w * 0.5, 1.0);
    o.uv = vec2<f32>(along, side);
    o.color = I.color.rgb * I.color.a;
    o.fade = fade * select(0.0, 1.0, age < I.params.y);
    o.spin = I.params.z * age + I.params.w * TAU;
    return o;
}

@fragment
fn fs_ribbon(i: VOut) -> @location(0) vec4<f32> {
    let across = 1.0 - abs(i.uv.y);
    let core = pow(across, 3.0);                       // hot centre
    let taper = smoothstep(0.0, 0.25, i.uv.x);         // tail end fades
    let a = (core * 1.5 + across * 0.25) * taper * i.fade;
    return vec4<f32>(i.color * a, a);
}

@fragment
fn fs_disc(i: VOut) -> @location(0) vec4<f32> {
    // 4-point star SDF inside the quad, spinning; ring + hot core
    // quad spans the segment: x 0..1 -> -1..1. Disc instances need p0 != p1 (length = diameter).
    let p = vec2<f32>(i.uv.x * 2.0 - 1.0, i.uv.y);
    let c = cos(i.spin);
    let s = sin(i.spin);
    let q = vec2<f32>(c * p.x - s * p.y, s * p.x + c * p.y);
    let r = length(q);
    let ang = atan2(q.y, q.x);
    let star = 0.55 + 0.35 * cos(4.0 * ang);           // blade radius by angle
    let edge = smoothstep(star, star - 0.08, r);
    let ring = smoothstep(0.08, 0.0, abs(r - star * 0.85));
    let core = smoothstep(0.25, 0.0, r);
    let a = (edge * 0.5 + ring * 1.2 + core * 2.0) * i.fade;
    return vec4<f32>(i.color * a, a);
}
