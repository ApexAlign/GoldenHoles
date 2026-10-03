# Reference frames
`ffa_first_frame_b2b384a.png`: first software-rendered frame of the FFA map (levels/ffa.lvl, spawn 3, 960x540) at head
b2b384a. Reproduce: `apt-get install -y --no-install-recommends mesa-vulkan-drivers libvulkan1 vulkan-tools; pip install pillow;
VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json WGPU_BACKEND=vulkan shinobi-game --shot-offline out.ppm --secs 2.0 --level levels/ffa.lvl --spawn 3`
(then convert PPM to PNG with PIL and look at it). Issues seen in this frame are tracked in the Engine status messages.

## HUD frames (HUD UI)
`ffa_hud_frame_<sha>.png`: GPU (software Vulkan) offline shot after the HUD polish: spawn 3 on levels/ffa.lvl, 960x540.
`hud_cpu_<state>.png` (spawn, fight_lock, console, scoreboard, radial, death, results, waiting): CPU-rendered HUD draw lists on a dark backdrop
(no glow, scanlines or bloom; geometry, colour and legibility only). Regenerate with
`cargo run -p shinobi-client --example hud_dump -- OUT_DIR 960 540` (also writes bright-backdrop variants); the layouts are
guarded by `crates/shinobi-client/tests/hud_golden.rs` (`UPDATE_GOLDEN=1` to re-record).

## Graphics review frames (lavapipe)
Extra env hooks for `--shot-offline` (offline only, ignored otherwise):
- `SHOT_FX=pieces,discs,lock:0.6,locked,break:0.4` overlays synthetic pieces / discs / lock-on state on the view.
- `SHOT_EYE=x,y,z` and `SHOT_LOOK=yaw,pitch` (radians) move the camera (edges, ceilings).
- `SHOT_SLASH_LEAD=secs` how long before capture the scripted katana swing starts (default 0.12).
- `--idle` skips the scripted swing and throw so you see the REST pose.
The three huge diagonal lines in non-idle shots are the katana swing trail (3 ribbons: tip, mid, hilt; designed to
clear the whole screen in 0.16 s, see `viewmodel.rs` test `arc_clears_the_whole_screen...`); the shot script starts a
swing 0.12 s before capture. The big star in the centre of non-idle shots is the shuriken mid-throw; at rest (`--idle`)
it sits bottom-left. Frames: `graphics_*.png`.
