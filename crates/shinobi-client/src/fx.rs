//! Screen-FX state: everything the post/CRT/grid shaders need that is not already in `camera.rs`.
//!
//! Pure math, no GPU/bevy types, so it is unit-testable and deterministic. Layering:
//!   `FpsCamera::update` -> `CameraPose` (roll, fov, y) -- owned by Cam/Mov, untouched here
//!   `FxState::update`   -> `FxOutput` (shake kick, dash FOV add + lens warp, vignette, slash sweep)
//!   `FxUniforms`        -> flat f32 block uploaded once per frame (layout mirrored in `shaders/*.wgsl`)
//! Final camera: roll = pose.roll, fov = pose.fov + out.fov_add_deg, look += (out.pitch_kick_deg, out.yaw_kick_deg).
//!
//! Determinism: shake/pulse phases derive from the server tick / synced server time, never `rand`
//! or local frame time, so spectators and replays match.

use std::f32::consts::TAU;

/// Server-time wrap for shader uniforms (f32 precision). Every shader pulse frequency must satisfy
/// `hz * TIME_WRAP_S` = integer so `sin(TAU * hz * t)` is continuous across the wrap.
pub const TIME_WRAP_S: f64 = 1024.0;

#[derive(Clone, Copy, Debug)]
pub struct FxCfg {
    pub shake_decay: f32,      // 1/s, exp decay of double-jump shake
    pub shake_amp_deg: f32,    // peak look kick
    pub shake_hz: f32,
    pub vignette_decay: f32,   // 1/s
    pub dash_fov_add: f32,     // deg at full dash
    pub dash_fov_in: f32,      // 1/s ease rate while dashing (fast)
    pub dash_fov_out: f32,     // 1/s ease rate after dash (slow)
    pub warp_max: f32,         // lens warp at full dash FOV add
    pub slash_dur: f32,        // s the sweep stays on screen
    pub grid_pulse_hz: f32,
}

impl Default for FxCfg {
    fn default() -> Self {
        Self {
            shake_decay: 14.0,
            shake_amp_deg: 0.8,
            shake_hz: 32.0,
            vignette_decay: 6.0,
            dash_fov_add: 18.0,
            dash_fov_in: 18.0,
            dash_fov_out: 7.0,
            warp_max: 0.35,
            slash_dur: 0.28,
            grid_pulse_hz: 1.25,
        }
    }
}

/// Edge-triggered + level inputs for one frame. Build from `CharacterSystem` / `CombatEvent`.
#[derive(Clone, Copy, Debug, Default)]
pub struct FxInput {
    /// Level: dash/roll in progress (`CharacterSystem::evasion().is_some()`).
    pub dash_active: bool,
    /// Edge: double jump fired this tick (same flag `CameraInput` uses).
    pub double_jumped: bool,
    /// Edge: `CombatEvent::Slash` this frame. dir = view-plane sweep dir (x right, y up), any length.
    pub slash: Option<SlashInput>,
    /// Sim tick, used to seed shake/slash deterministically.
    pub tick: u32,
    /// Server-synced clock, seconds.
    pub server_time: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct SlashInput {
    pub dir: [f32; 2],
    pub flip: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SlashFx {
    pub active: bool,
    /// 0..1 progress through the sweep.
    pub t01: f32,
    pub dir: [f32; 2], // unit
    pub seed: f32,     // 0..1 from tick, varies ribbon wobble
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FxOutput {
    pub pitch_kick_deg: f32,
    pub yaw_kick_deg: f32,
    pub fov_add_deg: f32,
    /// 0..warp_max radial lens warp (CRT pass), tracks dash FOV add.
    pub fov_warp: f32,
    /// 0..1 edge vignette compression (double jump exertion).
    pub vignette: f32,
    pub slash: SlashFx,
    /// 0..1 shared neon pulse for viewmodel grid + world grid.
    pub grid_pulse: f32,
}

pub struct FxState {
    pub cfg: FxCfg,
    shake_amp: f32,
    shake_t: f32,
    shake_phase: [f32; 2],
    vignette: f32,
    dash_add: f32,
    slash_t: f32, // seconds since slash; >= dur when idle
    slash_dir: [f32; 2],
    slash_seed: f32,
}

/// Integer hash -> [0,1). Deterministic across platforms.
pub(crate) fn hash01(mut x: u32) -> f32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

fn norm2(v: [f32; 2]) -> [f32; 2] {
    let l = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if l < 1e-5 { [1.0, 0.0] } else { [v[0] / l, v[1] / l] }
}

impl FxState {
    pub fn new(cfg: FxCfg) -> Self {
        Self {
            cfg,
            shake_amp: 0.0,
            shake_t: 0.0,
            shake_phase: [0.0; 2],
            vignette: 0.0,
            dash_add: 0.0,
            slash_t: cfg.slash_dur,
            slash_dir: [1.0, 0.0],
            slash_seed: 0.0,
        }
    }

    pub fn update(&mut self, dt: f32, inp: FxInput) -> FxOutput {
        let c = self.cfg;

        // --- double jump: shake impulse + vignette squeeze ---
        if inp.double_jumped {
            self.shake_amp = 1.0;
            self.shake_t = 0.0;
            self.shake_phase = [
                hash01(inp.tick.wrapping_mul(2)) * TAU,
                hash01(inp.tick.wrapping_mul(2).wrapping_add(1)) * TAU,
            ];
            self.vignette = 1.0;
        }
        self.shake_t += dt;
        let decay = (-c.shake_decay * dt).exp();
        self.shake_amp *= decay;
        self.vignette *= (-c.vignette_decay * dt).exp();
        let w = TAU * c.shake_hz * self.shake_t;
        let pitch_kick = c.shake_amp_deg * self.shake_amp * (w + self.shake_phase[0]).sin();
        let yaw_kick = c.shake_amp_deg * 0.6 * self.shake_amp * (w * 0.83 + self.shake_phase[1]).sin();

        // --- dash FOV warp: snap out, ease back (frame-rate independent) ---
        let (target, rate) = if inp.dash_active { (c.dash_fov_add, c.dash_fov_in) } else { (0.0, c.dash_fov_out) };
        self.dash_add += (target - self.dash_add) * (1.0 - (-rate * dt).exp());
        let fov_warp = (self.dash_add / c.dash_fov_add.max(1e-3)).clamp(0.0, 1.0) * c.warp_max;

        // --- katana sweep ---
        if let Some(s) = inp.slash {
            self.slash_t = 0.0;
            let mut d = norm2(s.dir);
            if s.flip {
                d[0] = -d[0];
            }
            self.slash_dir = d;
            self.slash_seed = hash01(inp.tick ^ 0x9e37_79b9);
        } else {
            self.slash_t += dt;
        }
        let active = self.slash_t < c.slash_dur;

        FxOutput {
            pitch_kick_deg: pitch_kick,
            yaw_kick_deg: yaw_kick,
            fov_add_deg: self.dash_add,
            fov_warp,
            vignette: self.vignette.clamp(0.0, 1.0),
            slash: SlashFx {
                active,
                t01: if active { self.slash_t / c.slash_dur } else { 1.0 },
                dir: self.slash_dir,
                seed: self.slash_seed,
            },
            grid_pulse: 0.5 + 0.5 * (TAU * c.grid_pulse_hz * wrap_time(inp.server_time)).sin(),
        }
    }
}

/// Wrap server time to f32-safe range. See `TIME_WRAP_S`.
pub fn wrap_time(server_time: f64) -> f32 {
    server_time.rem_euclid(TIME_WRAP_S) as f32
}

// ---------------------------------------------------------------------------------------------
// Uniform block. Mirrors `struct Fx` in shaders/crt_composite.wgsl (4 x vec4<f32>, std140-safe).
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FxUniforms {
    pub roll: f32,
    pub pitch_kick: f32,
    pub yaw_kick: f32,
    pub fov_deg: f32,
    pub fov_warp: f32,
    pub vignette: f32,
    pub slash_t: f32, // <0 = inactive
    pub slash_seed: f32,
    pub slash_dir: [f32; 2],
    pub server_time: f32, // wrapped
    pub grid_pulse: f32,
    pub resolution: [f32; 2], // post-upscale target (native)
    pub dyn_scale: f32,
    pub _pad: f32,
}

impl FxUniforms {
    pub fn new(
        roll: f32,
        fov_deg: f32,
        out: &FxOutput,
        server_time: f64,
        native: (u32, u32),
        dyn_scale: f32,
    ) -> Self {
        Self {
            roll,
            pitch_kick: out.pitch_kick_deg,
            yaw_kick: out.yaw_kick_deg,
            fov_deg: fov_deg + out.fov_add_deg,
            fov_warp: out.fov_warp,
            vignette: out.vignette,
            slash_t: if out.slash.active { out.slash.t01 } else { -1.0 },
            slash_seed: out.slash.seed,
            slash_dir: out.slash.dir,
            server_time: wrap_time(server_time),
            grid_pulse: out.grid_pulse,
            resolution: [native.0 as f32, native.1 as f32],
            dyn_scale,
            _pad: 0.0,
        }
    }

    pub const FLOATS: usize = 16;

    pub fn to_array(&self) -> [f32; Self::FLOATS] {
        [
            self.roll, self.pitch_kick, self.yaw_kick, self.fov_deg,
            self.fov_warp, self.vignette, self.slash_t, self.slash_seed,
            self.slash_dir[0], self.slash_dir[1], self.server_time, self.grid_pulse,
            self.resolution[0], self.resolution[1], self.dyn_scale, self._pad,
        ]
    }
}

// ---------------------------------------------------------------------------------------------
// Dynamic resolution (mobile). Scales the 3D scene targets only; HUD stays native.
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct DynResCfg {
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub down_ms: f32,       // frame time above this counts as over budget
    pub up_ms: f32,         // below this counts as headroom
    pub down_frames: u32,   // consecutive over-budget frames before dropping (fast)
    pub up_frames: u32,     // consecutive headroom frames before raising (slow)
    pub combat_enemies: u32,
    pub combat_projectiles: u32,
    pub combat_cap: f32,    // scale ceiling during a firefight
    pub quantize_px: u32,
}

impl Default for DynResCfg {
    fn default() -> Self {
        Self {
            min: 0.55,
            max: 1.0,
            step: 0.05,
            down_ms: 17.5,
            up_ms: 14.0,
            down_frames: 3,
            up_frames: 90,
            combat_enemies: 3,
            combat_projectiles: 40,
            combat_cap: 0.7,
            quantize_px: 8,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CombatLoad {
    pub visible_enemies: u32,
    pub projectiles: u32,
}

pub struct DynRes {
    pub cfg: DynResCfg,
    scale: f32,
    over: u32,
    under: u32,
}

impl DynRes {
    pub fn new(cfg: DynResCfg) -> Self {
        Self { cfg, scale: cfg.max, over: 0, under: 0 }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Pin the scale (clamped to the configured range): debugging and deterministic regression shots.
    pub fn force_scale(&mut self, s: f32) {
        self.scale = s.clamp(self.cfg.min, self.cfg.max);
        self.over = 0;
        self.under = 0;
    }

    /// Call once per frame with last frame's GPU/CPU time. Returns the scale for the next frame.
    pub fn update(&mut self, frame_ms: f32, load: CombatLoad) -> f32 {
        let c = self.cfg;
        let combat = load.visible_enemies >= c.combat_enemies || load.projectiles >= c.combat_projectiles;
        let ceiling = if combat { c.combat_cap.clamp(c.min, c.max) } else { c.max };

        if frame_ms > c.down_ms {
            self.under = 0;
            self.over += 1;
            if self.over >= c.down_frames {
                self.scale -= c.step;
                self.over = 0;
            }
        } else if frame_ms < c.up_ms {
            self.over = 0;
            self.under += 1;
            if self.under >= c.up_frames {
                self.scale += c.step;
                self.under = 0;
            }
        } else {
            self.over = 0;
            self.under = 0;
        }
        self.scale = self.scale.clamp(c.min, ceiling); // firefight drops scale immediately
        self.scale
    }

    /// Scene target size: native * scale, rounded down to `quantize_px` so the target is not
    /// reallocated on every step. Never below one quantum.
    pub fn scene_size(&self, native: (u32, u32)) -> (u32, u32) {
        let q = self.cfg.quantize_px.max(1);
        let f = |n: u32| (((n as f32 * self.scale) as u32 / q) * q).max(q).min(n);
        (f(native.0), f(native.1))
    }
}

// ---------------------------------------------------------------------------------------------
// Light-wall lifecycle. CPU twin of the math in shaders/lightwall.wgsl (kept in sync by tests).
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct LightWallCfg {
    pub life_s: f32,
    pub fade_start: f32, // fraction of life where fade begins
    pub max_alpha: f32,
    pub pulse_hz: f32,
    pub pulse_depth: f32,
    pub tick_dt: f32,
}

impl Default for LightWallCfg {
    fn default() -> Self {
        Self { life_s: 4.0, fade_start: 0.6, max_alpha: 0.55, pulse_hz: 1.25, pulse_depth: 0.2, tick_dt: 1.0 / 60.0 }
    }
}

/// Age of a wall sample in seconds from server ticks (wrap-safe).
pub fn lightwall_age(now_tick: u32, spawn_tick: u32, tick_dt: f32) -> f32 {
    now_tick.wrapping_sub(spawn_tick) as f32 * tick_dt
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Alpha of a wall sample: pure function of (server tick, spawn tick, server time).
pub fn lightwall_alpha(c: &LightWallCfg, now_tick: u32, spawn_tick: u32, server_time: f64) -> f32 {
    lightwall_alpha_age(c, lightwall_age(now_tick, spawn_tick, c.tick_dt), spawn_tick, server_time)
}

/// Same formula from an already-computed age (CPU ribbon builders that only have sim seconds).
pub fn lightwall_alpha_age(c: &LightWallCfg, age: f32, spawn_tick: u32, server_time: f64) -> f32 {
    if age >= c.life_s || age < 0.0 {
        return 0.0;
    }
    let fade = 1.0 - smoothstep(c.life_s * c.fade_start, c.life_s, age);
    let phase = TAU * c.pulse_hz * wrap_time(server_time) + spawn_tick as f32 * 0.35;
    let pulse = 1.0 - c.pulse_depth + c.pulse_depth * (0.5 + 0.5 * phase.sin());
    c.max_alpha * fade * pulse
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;

    fn step(fx: &mut FxState, n: usize, inp: FxInput) -> FxOutput {
        let mut o = FxOutput::default();
        for _ in 0..n {
            o = fx.update(DT, inp);
        }
        o
    }

    #[test]
    fn double_jump_shakes_squeezes_then_settles() {
        let mut fx = FxState::new(FxCfg::default());
        let o = fx.update(DT, FxInput { double_jumped: true, tick: 7, ..Default::default() });
        assert!(o.vignette > 0.8);
        let mut peak = 0.0f32;
        for _ in 0..20 {
            let o = fx.update(DT, FxInput::default());
            peak = peak.max(o.pitch_kick_deg.abs());
        }
        assert!(peak > 0.05, "peak {peak}");
        let o = step(&mut fx, 120, FxInput::default());
        assert!(o.pitch_kick_deg.abs() < 1e-3 && o.vignette < 0.01);
    }

    #[test]
    fn shake_is_deterministic_per_tick() {
        let run = |tick| {
            let mut fx = FxState::new(FxCfg::default());
            fx.update(DT, FxInput { double_jumped: true, tick, ..Default::default() });
            (0..5).map(|_| fx.update(DT, FxInput::default()).pitch_kick_deg).collect::<Vec<_>>()
        };
        assert_eq!(run(42), run(42));
        assert_ne!(run(42), run(43));
    }

    #[test]
    fn dash_widens_fov_fast_then_returns_slow() {
        let mut fx = FxState::new(FxCfg::default());
        let o = step(&mut fx, 15, FxInput { dash_active: true, ..Default::default() }); // 0.25 s
        assert!(o.fov_add_deg > 14.0 && o.fov_warp > 0.25, "{o:?}");
        let o1 = step(&mut fx, 6, FxInput::default());
        assert!(o1.fov_add_deg > 5.0, "should still be wide after 0.1 s");
        let o2 = step(&mut fx, 120, FxInput::default());
        assert!(o2.fov_add_deg < 0.05 && o2.fov_warp < 0.01);
    }

    #[test]
    fn slash_sweeps_then_expires_and_flip_mirrors_dir() {
        let mut fx = FxState::new(FxCfg::default());
        let o = fx.update(DT, FxInput { slash: Some(SlashInput { dir: [3.0, 0.0], flip: true }), tick: 1, ..Default::default() });
        assert!(o.slash.active && o.slash.t01 < 0.1);
        assert!((o.slash.dir[0] + 1.0).abs() < 1e-5, "flip should mirror x");
        let o = step(&mut fx, 10, FxInput::default());
        assert!(o.slash.active && o.slash.t01 > 0.4 && o.slash.t01 < 1.0);
        let o = step(&mut fx, 20, FxInput::default());
        assert!(!o.slash.active);
    }

    #[test]
    fn grid_pulse_wrap_is_continuous() {
        let c = FxCfg::default();
        let p = |t: f64| 0.5 + 0.5 * (TAU * c.grid_pulse_hz * wrap_time(t)).sin();
        let a = p(TIME_WRAP_S - 1e-3);
        let b = p(TIME_WRAP_S + 1e-3);
        assert!((a - b).abs() < 0.02, "{a} vs {b}");
    }

    #[test]
    fn uniforms_layout_is_16_floats() {
        let o = FxOutput { slash: SlashFx { active: false, ..Default::default() }, ..Default::default() };
        let u = FxUniforms::new(0.1, 90.0, &o, 5.0, (1920, 1080), 0.8);
        let a = u.to_array();
        assert_eq!(a.len(), 16);
        assert_eq!(a[6], -1.0, "inactive slash encoded as -1");
        assert_eq!((a[12], a[13], a[14]), (1920.0, 1080.0, 0.8));
    }

    #[test]
    fn dynres_drops_fast_recovers_slow_and_caps_in_combat() {
        let mut d = DynRes::new(DynResCfg::default());
        let calm = CombatLoad::default();
        assert_eq!(d.update(20.0, calm), 1.0);
        assert_eq!(d.update(20.0, calm), 1.0);
        assert!((d.update(20.0, calm) - 0.95).abs() < 1e-5, "3rd slow frame drops one step");
        for _ in 0..89 {
            d.update(10.0, calm);
        }
        assert!((d.scale() - 0.95).abs() < 1e-5, "no recovery before 90 fast frames");
        assert!((d.update(10.0, calm) - 1.0).abs() < 1e-5);
        // firefight: immediate clamp to combat cap even at good frame time
        let hot = CombatLoad { visible_enemies: 4, projectiles: 0 };
        assert!((d.update(10.0, hot) - 0.7).abs() < 1e-5);
        // never below min
        for _ in 0..500 {
            d.update(30.0, hot);
        }
        assert!((d.scale() - 0.55).abs() < 1e-5);
    }

    #[test]
    fn dynres_force_scale_clamps_and_sizes() {
        let mut d = DynRes::new(DynResCfg::default());
        d.force_scale(0.5);
        assert_eq!(d.scale(), 0.55, "clamped to min");
        d.force_scale(0.6);
        assert_eq!(d.scene_size((960, 540)), (576, 320));
    }

    #[test]
    fn dynres_scene_size_quantized_and_bounded() {
        let mut d = DynRes::new(DynResCfg::default());
        assert_eq!(d.scene_size((1920, 1080)), (1920, 1080));
        for _ in 0..30 {
            d.update(30.0, CombatLoad::default());
        }
        let (w, h) = d.scene_size((1920, 1080));
        assert!(w % 8 == 0 && h % 8 == 0 && w < 1920 && w >= 1056 - 8, "{w}x{h}");
        assert_eq!(DynRes::new(DynResCfg::default()).scene_size((4, 4)), (4, 4));
    }

    #[test]
    fn lightwall_fades_monotonic_and_dies_at_life() {
        let c = LightWallCfg { pulse_depth: 0.0, ..Default::default() };
        let spawn = 1000u32;
        let at = |age_s: f32| lightwall_alpha(&c, spawn + (age_s / c.tick_dt) as u32, spawn, 10.0);
        assert!((at(0.0) - c.max_alpha).abs() < 1e-4);
        assert!((at(2.0) - c.max_alpha).abs() < 1e-4, "no fade before fade_start (2.4 s)");
        assert!(at(3.0) < at(2.6) && at(3.9) < at(3.0));
        assert_eq!(at(4.01), 0.0);
    }

    #[test]
    fn lightwall_is_pure_fn_of_server_state_and_tick_wrap_safe() {
        let c = LightWallCfg::default();
        assert_eq!(lightwall_alpha(&c, 500, 100, 7.25), lightwall_alpha(&c, 500, 100, 7.25));
        // tick counter wrapped: now < spawn numerically, age still small and positive
        let age = lightwall_age(5, u32::MAX - 4, c.tick_dt);
        assert!((age - 10.0 * c.tick_dt).abs() < 1e-6);
    }
}
