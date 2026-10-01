// Shared by every shader below. Backends paste/import this ahead of the pass shader
// (bevy: naga_oil `#define_import_path shinobi::fx`; raw wgpu: string concat).
// Layout mirrors `FxUniforms::to_array()` in src/fx.rs (4 x vec4<f32> = 16 floats).

struct Fx {
    a: vec4<f32>, // roll, pitch_kick, yaw_kick, fov_deg
    b: vec4<f32>, // fov_warp, vignette, slash_t (<0 inactive), slash_seed
    c: vec4<f32>, // slash_dir.xy, server_time (wrapped 1024 s), grid_pulse
    d: vec4<f32>, // resolution.xy (native), dyn_scale, _pad
};

const TAU: f32 = 6.28318530718;

// Pulse frequencies MUST satisfy hz * 1024 == integer so the wrap is seamless.
fn pulse(server_time: f32, hz: f32, phase: f32) -> f32 {
    return 0.5 + 0.5 * sin(TAU * hz * server_time + phase);
}
