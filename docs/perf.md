# Rendering performance + GPU selection (Graphics)

## What exists
- **Quality tiers** (`shinobi-client/src/quality.rs`): High (bloom, CRT, native res), Medium (cheaper bloom, CRT, dynamic
  resolution 0.70-1.0), Low (no bloom, no CRT, dyn-res 0.55-1.0). Gore budgets follow the tier: 160/32/24 live drops,
  pools and wound decals on High, 90/20/14 on Medium, 40/10/6 on Low (`shinobi-game/src/gore.rs`).
- **Auto tier** (live game, no `--tier`): the adapter's device type picks the start tier (discrete -> High, integrated /
  unknown -> Medium, software -> Low); `--tier high|medium|low` pins it, `--tier auto` is the default. A `FrameGovernor`
  watches real frame times: p95 > 16.6 ms (+5%) over a 2 s window -> drop one tier (6 s warm-up, 4 s cooldown).
  Live drop works Medium -> Low (bloom + CRT removed, dyn-res config swapped). A slow High tier only logs `QUALITY:` and
  tells you to restart with `--tier medium` (High has no scaled scene image to fall back on).
  `SHINOBI_DEVICE=discrete|integrated|software` overrides the detected class (testing, odd drivers).
- **Present mode**: `--present auto|mailbox|immediate|relaxed|fifo` or `SHINOBI_PRESENT`. Default `auto` = AutoVsync (Fifo):
  smoothest + coolest. `mailbox` / `immediate` = lower latency but the GPU runs flat out.
- **Adapter**: `SHINOBI_GPU=igpu|dgpu|auto` sets wgpu's power preference; `WGPU_ADAPTER_NAME=<substring>` picks by name.
  Startup prints `GPU: <name> | <type> | <backend> | driver ... -> quality tier ...` and `WINDOW: present mode ...`.
- **Native Wayland** is now a default cargo feature (`wayland`, winit dlopen, no system headers). Before, the game was
  X11-only and ran through XWayland on Wayland desktops: XWayland has its own frame pacing (a classic judder source) and
  NVIDIA PRIME windows via XWayland are fragile. Falls back to X11 when there is no Wayland session (verified under Xvfb).
  `--x11` (or `WINIT_UNIX_BACKEND=x11`) forces X11/XWayland; `--no-default-features` builds without Wayland.
- **Profiling aid**: `SHINOBI_DISABLE=sky,crt,vm,fx,gore` leaves plugins out. A `PERF` line prints every 3 s (fps, worst frame,
  fixed steps/s).

## dGPU (RTX 5060) never opens a window under Wayland/PRIME
Cannot be reproduced here (no NVIDIA). Likely cause: the game went through XWayland, and NVIDIA's Vulkan WSI through
XWayland/PRIME often fails to present. Things to try on the laptop, in order:
1. Native Wayland (now default): `cargo run --release` (iGPU) and `SHINOBI_GPU=dgpu cargo run --release`.
2. Offload env for the NVIDIA ICD: `__NV_PRIME_RENDER_OFFLOAD=1 __VK_LAYER_NV_optimus=NVIDIA_only SHINOBI_GPU=dgpu ...`
3. Pin by name: `WGPU_ADAPTER_NAME=NVIDIA ...` (or `AMD` for the iGPU).
4. Read the `GPU:` line on stderr: it says which adapter was really chosen and whether it is software.
There is no automatic dGPU -> iGPU fallback after a failed present: wgpu picks the adapter before the surface exists
and a dead dGPU surface does not return an error, it just never shows a window. The reliable fallback is the default
(iGPU / power preference unset) and the explicit env switches above.

## Per-frame churn fixed
- HUD mesh was replaced every frame; now skipped when the tessellated geometry is bit-identical (idle / menus).
- Beacon / crack visibility toggles no longer insert a component every frame (`set_if_neq`).
- Not changed (rebuild only while active): lock beams, rope, disc trails, impact bursts.

## Numbers (lavapipe = CPU rasteriser, 4 cores, 1280x720, FFA map + 7 bots; RELATIVE only)
| config | fps | worst frame |
|---|---|---|
| High | 4 | ~260 ms |
| Medium (dyn-res) | 4 | ~275 ms |
| Low (no bloom/CRT) | 5 | ~215 ms |
| Low, scene scale 0.3 | 5-6 | ~200 ms |
| Low, 29-box arena, scale 0.3 | 5 | ~210 ms |
Scene scale and box count barely matter: lavapipe's cost is a fixed per-submit software-raster cost (gdb samples sit in
`vkQueueSubmit` and the llvmpipe threads), so these numbers say nothing about iGPU fill-rate. Only the Low-vs-High gap
(bloom + CRT passes) is a real signal: ~20% of frame time. Real iGPU numbers need the laptop: run with `--tier` each and
read the `PERF` lines.
