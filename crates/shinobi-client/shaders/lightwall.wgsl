// Persistent light-wall ribbon behind a fast mover. Geometry is generated in the vertex stage from a
// storage buffer of trail samples (no CPU mesh). Alpha/pulse are pure functions of SERVER tick/time,
// so every client (and spectator/replay) renders identical walls. Needs fx_common.wgsl prepended.
// CPU twin: fx::lightwall_alpha. shaders::tests::lightwall_wgsl_matches_cpu_formula pins the shared
// constants/formula text (0.35 tick phase, fade_start, smoothstep, pulse_depth mix).
//
// Draw: triangle strip, 2 verts per sample, vertex_count = 2 * WallU.count, additive, no depth write,
// cull none. Samples are ordered oldest -> newest.

struct Sample {
    pos: vec3<f32>,
    tick: u32,          // server tick the sample was laid down
};
struct WallU {
    view_proj: mat4x4<f32>,
    color: vec4<f32>,   // rgb HDR neon
    now_tick: u32,
    count: u32,
    tick_dt: f32,
    life_s: f32,
    height: f32,
    fade_start: f32,    // fraction of life
    max_alpha: f32,
    pulse_hz: f32,      // hz * 1024 integer
    pulse_depth: f32,
    _p0: f32,
    _p1: f32,
    _p2: f32,
};
@group(0) @binding(0) var<uniform> W: WallU;
@group(0) @binding(1) var<uniform> F: Fx;
@group(0) @binding(2) var<storage, read> samples: array<Sample>;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) alpha: f32,
    @location(1) v: f32,        // 0 bottom .. 1 top
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VOut {
    var o: VOut;
    let si = min(vi >> 1u, max(W.count, 1u) - 1u);
    let top = f32(vi & 1u);
    let s = samples[si];

    let age = f32(W.now_tick - s.tick) * W.tick_dt;       // u32 wrapping sub: tick-wrap safe
    let t0 = W.life_s * W.fade_start;
    let fade = 1.0 - smoothstep(t0, W.life_s, age);
    let phase = TAU * W.pulse_hz * F.c.z + f32(s.tick) * 0.35;
    let pl = 1.0 - W.pulse_depth + W.pulse_depth * (0.5 + 0.5 * sin(phase));
    let alive = select(0.0, 1.0, age < W.life_s && (vi >> 1u) < W.count);

    let p = s.pos + vec3<f32>(0.0, top * W.height, 0.0);
    o.clip = W.view_proj * vec4<f32>(p, 1.0);
    o.alpha = W.max_alpha * fade * pl * alive;
    o.v = top;
    return o;
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    // hot rim at top edge, soft body, faint foot line
    let rim = smoothstep(0.82, 1.0, i.v) * 1.8;
    let foot = smoothstep(0.12, 0.0, i.v) * 0.6;
    let body = 0.35 + 0.65 * i.v;
    let c = W.color.rgb * (body + rim + foot);
    return vec4<f32>(c * i.alpha, i.alpha);    // premultiplied-additive: src=One, dst=One
}
