# Reference frames
`ffa_first_frame_b2b384a.png`: first software-rendered frame of the FFA map (levels/ffa.lvl, spawn 3, 960x540) at head
b2b384a. Reproduce: `apt-get install -y --no-install-recommends mesa-vulkan-drivers libvulkan1 vulkan-tools; pip install pillow;
VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json WGPU_BACKEND=vulkan shinobi-game --shot-offline out.ppm --secs 2.0 --level levels/ffa.lvl --spawn 3`
(then convert PPM to PNG with PIL and look at it). Issues seen in this frame are tracked in the Engine status messages.

## HUD frames (HUD UI)
`ffa_hud_frame_<sha>.png`: GPU (software Vulkan) offline shot after the HUD polish: spawn 3 on levels/ffa.lvl, 960x540.
`hud_cpu_<state>.png` (spawn, fight_lock, console, scoreboard, radial): CPU-rendered HUD draw lists on a dark backdrop
(no glow, scanlines or bloom; geometry, colour and legibility only). Regenerate with
`cargo run -p shinobi-client --example hud_dump -- OUT_DIR 960 540` (also writes bright-backdrop variants); the layouts are
guarded by `crates/shinobi-client/tests/hud_golden.rs` (`UPDATE_GOLDEN=1` to re-record).
