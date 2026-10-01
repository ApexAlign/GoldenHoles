// Bevy fullscreen-material port of shinobi-client/shaders/crt_composite.wgsl.
// Bindings are fixed by FullscreenMaterial: 0 = scene texture, 1 = sampler, 2 = uniform (CrtEffect).
// Differences from the client original: no separate `distort` texture (one source texture only), so the
// katana sweep warps the sampled pixels analytically. KEEP the lens/slash maths in sync with the original.
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

struct Crt {
    a: vec4<f32>,        // roll, pitch_kick, yaw_kick, fov_deg      (FxUniforms 0..3)
    b: vec4<f32>,        // fov_warp, vignette, slash_t (<0 off), slash_seed
    c: vec4<f32>,        // slash_dir.xy, server_time, grid_pulse
    d: vec4<f32>,        // resolution.xy, dyn_scale, _
    scan: vec4<f32>,     // density, strength, barrel k, chroma px
    misc: vec4<f32>,     // vignette base, chroma falloff, slash width, slash intensity
    slash_col: vec4<f32>,
};
@group(0) @binding(0) var scene_tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> C: Crt;

const TAU: f32 = 6.28318530718;

fn barrel(uv: vec2<f32>, k: f32) -> vec2<f32> {
    let c = uv * 2.0 - 1.0;
    let r2 = dot(c, c);
    return (c * (1.0 + k * r2)) * 0.5 + 0.5;
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let aspect = C.d.x / max(C.d.y, 1.0);

    // lens: CRT barrel + dash FOV radial warp
    var uv = barrel(in.uv, C.scan.z + C.b.x);
    let out_of_screen = uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0;

    // katana sweep geometry first: it both pushes sampled pixels and adds glow
    var slash_glow = 0.0;
    var slash_push = vec2<f32>(0.0);
    if (C.b.z >= 0.0) {
        let dir = C.c.xy;
        let n = vec2<f32>(-dir.y, dir.x);
        let p = (in.uv - 0.5) * vec2<f32>(aspect, 1.0);
        let along = dot(p, dir) + 0.9 - 1.8 * C.b.z;
        let across = dot(p, n) + 0.04 * sin(dot(p, dir) * 18.0 + C.b.w * TAU) * (1.0 - C.b.z);
        let band = exp(-abs(across) / max(C.misc.z, 1e-4));
        let tail = smoothstep(0.0, -0.55, along) * step(along, 0.0);
        let head = exp(-abs(along) * 18.0);
        let k = (head * 2.0 + tail) * (1.0 - C.b.z * 0.6);
        slash_glow = band * k;
        slash_push = n * vec2<f32>(1.0 / aspect, 1.0) * band * k * 0.03 * C.misc.w;
    }
    uv = uv + slash_push;

    // chromatic aberration, edge-weighted
    let cc = in.uv * 2.0 - 1.0;
    let edge = pow(clamp(dot(cc, cc) * 0.5, 0.0, 1.0), C.misc.y);
    let off = cc * (C.scan.w / C.d.x) * edge * 2.0;
    let r = textureSampleLevel(scene_tex, samp, uv + off, 0.0).r;
    let g = textureSampleLevel(scene_tex, samp, uv, 0.0).g;
    let b = textureSampleLevel(scene_tex, samp, uv - off, 0.0).b;
    var col = vec3<f32>(r, g, b);
    col = col + C.slash_col.rgb * C.misc.w * slash_glow;

    // scanlines on native-resolution rows
    let row = in.uv.y * C.d.y * C.scan.x;
    col = col * (1.0 - C.scan.y * (0.5 + 0.5 * sin(row * TAU)));

    // vignette: base + double-jump exertion squeeze
    let v = dot(cc, cc);
    col = col * (1.0 - smoothstep(0.35, 1.6, v * (1.0 + 1.6 * C.b.y)) * (C.misc.x + 0.6 * C.b.y));

    col = select(col, vec3<f32>(0.0), out_of_screen);
    return vec4<f32>(col, 1.0);
}
