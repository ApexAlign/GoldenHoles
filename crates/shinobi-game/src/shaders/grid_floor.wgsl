// Bevy material fragment shader: infinite neon grid on a large ground quad, plus the surfaces of level boxes.
// Mirrors shinobi-client/shaders/tron_surface.wgsl (planar grid + distance fade), minus the baked `edge`
// attribute (no edge-distance vertex data yet) and using Bevy's view/globals bindings.
//
// modes (P.tint.w): 0 floor plane, 1 box surface, 2 ceiling skylight (the original three, untouched),
//                   3 FLOATING-CITY surface (tier colours by height, circuit traces, scan bands, glyph rain),
//                   4 shoji window (warm paper light behind a lattice).
#import bevy_pbr::forward_io::VertexOutput
#import bevy_pbr::mesh_view_bindings::{view, globals}

struct GridParams {
    base: vec4<f32>,   // rgb dark surface
    glow: vec4<f32>,   // rgb neon line (HDR)
    grid: vec4<f32>,   // x cell, y major_every, z line px, w fog_far
    boxc: vec4<f32>,   // level box centre xyz, roll about Z (rad) in w
    boxh: vec4<f32>,   // level box half extents xyz, edge glow width (m) in w
    tint: vec4<f32>,   // rgb line/edge colour (HDR); w = mode (see above)
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

// ---------------------------------------------------------------------------------------------- floating city helpers

fn h11(p: f32) -> f32 {
    var x = fract(p * 0.1031);
    x = x * (x + 33.33);
    x = x * (x + x);
    return fract(x);
}

fn h21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 = p3 + dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

// One hue per tier (decks at y 24 / 55 / 86 / 117; crowns above): cyan gate, violet, amber, jade, vermilion crown. The
// colour changes along a tall face, so a tower shows every tier it passes; blends are 5 m wide.
fn tier_colour(y: f32) -> vec3<f32> {
    let c0 = vec3<f32>(0.04, 0.80, 1.00); // cyan
    let c1 = vec3<f32>(0.92, 0.22, 1.00); // violet
    let c2 = vec3<f32>(1.00, 0.60, 0.10); // amber
    let c3 = vec3<f32>(0.12, 1.00, 0.50); // jade
    let c4 = vec3<f32>(1.00, 0.20, 0.16); // vermilion
    var c = c0;
    c = mix(c, c1, smoothstep(39.5, 44.5, y));
    c = mix(c, c2, smoothstep(70.5, 75.5, y));
    c = mix(c, c3, smoothstep(101.5, 106.5, y));
    c = mix(c, c4, smoothstep(132.5, 137.5, y));
    return c;
}

// Circuit-board traces on a face: per 1.6 m cell a horizontal run, a vertical run, a bend or nothing, with solder nodes.
fn circuit(uv: vec2<f32>, lw: f32) -> f32 {
    let cell = 1.6;
    let id = floor(uv / cell);
    let f = fract(uv / cell) - 0.5;
    let h = h21(id);
    var d = 10.0;
    if (h < 0.28) {
        d = abs(f.y);                           // horizontal run
    } else if (h < 0.52) {
        d = abs(f.x);                           // vertical run
    } else if (h < 0.66) {
        d = min(abs(f.x) + select(0.0, 9.0, f.y > 0.0), abs(f.y) + select(0.0, 9.0, f.x < 0.0));
    }
    let line = 1.0 - smoothstep(lw * 0.5, lw, d * cell);
    let node = 1.0 - smoothstep(0.08, 0.14, length(f) * cell) ;
    return max(line * 0.8, node * step(0.78, h));
}

// Falling pseudo-katakana: columns of 3x5 dot glyphs with a bright head and a fading trail (cosmetic).
fn glyph_rain(x: f32, y: f32, half_h: f32, dist: f32) -> f32 {
    let cw = 0.62;
    let ch = 0.9;
    let col = floor(x / cw);
    let lit_col = step(0.80, h11(col * 7.31 + 3.0));
    if (lit_col < 0.5) {
        return 0.0;
    }
    let speed = 0.05 + 0.10 * h11(col * 1.77);
    let phase = fract(globals.time * speed + h11(col * 5.1));
    let head = half_h - phase * 2.0 * half_h;
    let d = y - head;                       // above the head = trail
    if (d < 0.0 || d > 11.0) {
        return 0.0;
    }
    let row = floor(y / ch);
    let lx = fract(x / cw);
    let ly = fract(y / ch);
    if (lx < 0.2 || lx > 0.8 || ly < 0.12 || ly > 0.88) {
        return 0.0;
    }
    let gx = floor((lx - 0.2) / 0.6 * 3.0);
    let gy = floor((ly - 0.12) / 0.76 * 5.0);
    let bits = u32(h21(vec2<f32>(col, row)) * 32767.0);
    let on = f32((bits >> u32(gy * 3.0 + gx)) & 1u);
    let trail = exp(-d * 0.42) + select(0.0, 1.6, d < 0.9);
    let near = 1.0 - smoothstep(18.0, 60.0, dist);
    return on * trail * near;
}

fn city_surface(in: VertexOutput) -> vec4<f32> {
    let d = in.world_position.xyz - P.boxc.xyz;
    let cs = cos(P.boxc.w);
    let sn = sin(P.boxc.w);
    let lp = vec3<f32>(cs * d.x + sn * d.y, -sn * d.x + cs * d.y, d.z);
    let e = max(P.boxh.xyz - abs(lp), vec3<f32>(0.0));
    var uv = lp.xz;
    var vertical = false;
    if (e.x <= e.y && e.x <= e.z) {
        uv = lp.zy;
        vertical = true;
    } else if (e.y <= e.z) {
        uv = lp.xz;
    } else {
        uv = lp.xy;
        vertical = true;
    }
    let sm = second_min(e);
    let dist = length(in.world_position.xyz - view.world_position);
    let fade = 1.0 - smoothstep(P.grid.w * 0.4, P.grid.w, dist);
    let tc = tier_colour(in.world_position.y);

    // grid lines in the tier colour (minor faint, major crisp), pulsing slowly with the world
    let minor = grid_line(uv, P.grid.x, P.grid.z);
    let major = grid_line(uv, P.grid.x * P.grid.y, P.grid.z * 1.8);
    let lines = minor * 0.28 + major;
    let pulse = 0.85 + 0.15 * sin(TAU * 0.25 * globals.time - (uv.x + uv.y) * 0.08);

    // edges: a tight neon core and a soft halo (every box outline is a light strip)
    let edge = exp(-sm / 0.045) * 3.4 + exp(-sm / 0.45) * 0.17;

    // circuit traces on large faces; a scan band climbs the walls and lights them as it passes
    let cfade = 1.0 - smoothstep(70.0, 190.0, dist);
    let big = step(6.0, max(P.boxh.x, max(P.boxh.y, P.boxh.z)));
    let lw = max(0.05, length(fwidth(uv)) * 1.3);
    let tr = circuit(uv + vec2<f32>(P.boxc.x * 0.37, P.boxc.z * 0.41), lw) * cfade * big;
    let seed = h21(floor(vec2<f32>(P.boxc.x, P.boxc.z)));
    let band = pow(max(0.0, sin(in.world_position.y * 0.16 - globals.time * 0.85 + seed * 6.0)), 28.0);
    let scan = select(0.0, 1.0, vertical);

    var rgb = P.base.rgb + tc * (lines * 0.5 * fade * pulse + edge + tr * (0.22 + 2.2 * band * scan));

    // glyph rain on the vertical faces of tall bodies
    if (vertical && P.boxh.y > 5.0) {
        let g = glyph_rain(uv.x, lp.y, P.boxh.y, dist);
        rgb = rgb + mix(tc, vec3<f32>(1.0, 1.0, 1.0), 0.35) * g * 1.15;
    }

    // atmospheric haze: far geometry sinks into the violet night but its edge lines (the silhouette) stay
    let haze = smoothstep(120.0, 460.0, dist) * 0.55;
    let hazec = vec3<f32>(0.035, 0.012, 0.06) + tc * 0.035;
    rgb = mix(rgb, hazec + tc * edge * 0.85, haze);
    return vec4<f32>(rgb, 1.0);
}

fn shoji_window(in: VertexOutput) -> vec4<f32> {
    var uv = in.world_position.xz;
#ifdef VERTEX_UVS_A
    uv = in.uv;
#endif
    let tc = tier_colour(in.world_position.y);
    let paper_c = mix(vec3<f32>(1.0, 0.74, 0.38), tc, 0.30);
    // lattice: 2 vertical x 3 horizontal slats in a dark frame
    let fx = abs(fract(uv.x * 2.0 + 0.5) - 0.5);
    let fy = abs(fract(uv.y * 3.0 + 0.5) - 0.5);
    let slat = max(smoothstep(0.055, 0.035, fx), smoothstep(0.055, 0.035, fy));
    let frame = max(smoothstep(0.08, 0.05, min(uv.x, 1.0 - uv.x)), smoothstep(0.06, 0.035, min(uv.y, 1.0 - uv.y)));
    let flicker = 0.9 + 0.1 * sin(globals.time * 2.1 + floor(in.world_position.y) * 1.7 + in.world_position.x + in.world_position.z);
    let paper = paper_c * (1.5 + 0.5 * (1.0 - uv.y)) * flicker;
    let dark = vec3<f32>(0.02, 0.008, 0.01);
    let rgb = mix(paper, dark, max(slat, frame));
    return vec4<f32>(rgb, 1.0);
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let mode = P.tint.w;
    if (mode > 3.5) {
        return shoji_window(in);
    }
    if (mode > 2.5) {
        return city_surface(in);
    }
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
