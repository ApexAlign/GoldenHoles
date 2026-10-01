// Final composite on the (possibly down-scaled) scene: camera-effect lens stack, then CRT look.
// Order in the frame graph: scene -> bloom -> THIS -> native-res HUD overlay (HUD skips all of it).
// Needs fx_common.wgsl prepended.
//
// Inputs: scene (HDR, post-bloom), distort (rg8snorm: katana/shockwave pixel offsets, *0.02 uv),
// slash sweep analytic in this pass (t, dir, seed from Fx) so no extra geometry is needed.

@group(0) @binding(0) var scene_tex: texture_2d<f32>;
@group(0) @binding(1) var distort_tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> F: Fx;

struct Crt {
    scan: vec4<f32>,    // x density (lines per native px), y strength, z barrel k, w chroma px
    misc: vec4<f32>,    // x vignette base, y chroma falloff power, z slash width, w slash intensity
    slash_col: vec4<f32>,
};
@group(0) @binding(4) var<uniform> C: Crt;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VOut {
    let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    var o: VOut;
    o.clip = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(p.x, 1.0 - p.y);
    return o;
}

fn barrel(uv: vec2<f32>, k: f32) -> vec2<f32> {
    let c = uv * 2.0 - 1.0;
    let r2 = dot(c, c);
    return (c * (1.0 + k * r2)) * 0.5 + 0.5;
}

@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> {
    let aspect = F.d.x / max(F.d.y, 1.0);

    // 1. lens: CRT barrel + dash FOV radial warp (F.b.x)
    var uv = barrel(i.uv, C.scan.z + F.b.x);
    let out_of_screen = uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0;

    // 2. katana sweep geometry, computed first so it can BOTH warp the sampled pixels and add glow.
    //    Band travels along slash_dir; pixels near the front/tail are pushed along the band normal.
    var slash_glow = 0.0;
    var slash_push = vec2<f32>(0.0);
    if (F.b.z >= 0.0) {
        let dir = F.c.xy;
        let n = vec2<f32>(-dir.y, dir.x);
        let p = (i.uv - 0.5) * vec2<f32>(aspect, 1.0);
        let along = dot(p, dir) + 0.9 - 1.8 * F.b.z;            // front travels -0.9..+0.9
        let across = dot(p, n) + 0.04 * sin(dot(p, dir) * 18.0 + F.b.w * TAU) * (1.0 - F.b.z);
        let band = exp(-abs(across) / max(C.misc.z, 1e-4));
        let tail = smoothstep(0.0, -0.55, along) * step(along, 0.0);   // ribbon trailing the front
        let head = exp(-abs(along) * 18.0);
        let k = (head * 2.0 + tail) * (1.0 - F.b.z * 0.6);
        slash_glow = band * k;
        // uv-space push along the normal (x un-aspected back to uv): the "distorts the pixels" part
        slash_push = n * vec2<f32>(1.0 / aspect, 1.0) * band * k * 0.03 * C.misc.w;
    }

    // 3. pixel distortion: distort buffer (shockwaves) + slash push
    let dist = textureSampleLevel(distort_tex, samp, uv, 0.0).rg;
    uv = uv + dist * 0.02 + slash_push;

    // 4. chromatic aberration weighted to the edges
    let cc = i.uv * 2.0 - 1.0;
    let edge = pow(clamp(dot(cc, cc) * 0.5, 0.0, 1.0), C.misc.y);
    let off = cc * (C.scan.w / F.d.x) * edge * 2.0;
    let r = textureSampleLevel(scene_tex, samp, uv + off, 0.0).r;
    let g = textureSampleLevel(scene_tex, samp, uv, 0.0).g;
    let b = textureSampleLevel(scene_tex, samp, uv - off, 0.0).b;
    var col = vec3<f32>(r, g, b);
    col = col + C.slash_col.rgb * C.misc.w * slash_glow;

    // 5. scanlines (subtle) on native-res pixel rows
    let row = i.uv.y * F.d.y * C.scan.x;
    let scan = 1.0 - C.scan.y * (0.5 + 0.5 * sin(row * TAU));
    col = col * scan;

    // 6. vignette: base + double-jump exertion squeeze (F.b.y)
    let v = dot(cc, cc);
    let vig = 1.0 - smoothstep(0.35, 1.6, v * (1.0 + 1.6 * F.b.y)) * (C.misc.x + 0.6 * F.b.y);
    col = col * vig;

    col = select(col, vec3<f32>(0.0), out_of_screen);
    return vec4<f32>(col, 1.0);
}
