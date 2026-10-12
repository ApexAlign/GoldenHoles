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

UPDATE: plain `--shot-offline` is now IDLE (rest pose, no scripted swing/throw). Pass `--action` for the old scripted swing + throw (golden_check.sh does). Frame: `graphics_default_offline_spawn3.png`.

More `SHOT_FX` hooks: `print` (pieces appear at frame 108: scan-in mid-build), `derez` (pieces vanish at frame 100: voxel dissolve), `cracks:N` (damage bucket 0-8 on every piece), `katana` / `katanakill` (katana connect at frame 114: flash, sparks, arc flare, hit-stop). Flashes are capped to 30% of the eye distance so melee hits never white out the screen.

`ffa_small_overview.png`: levels/ffa_small.lvl (88 m demo arena) from the SW corner, 16 m up. `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json WGPU_BACKEND=vulkan SHOT_EYE=-40,16,-40 SHOT_LOOK=-2.356,-0.38 shinobi-game --shot-offline out.ppm --secs 2.0 --level levels/ffa_small.lvl --spawn 0`

## HUD energy frames
`hud_cpu_star_locked.png` (dry pool, RECHARGE, out-of-stars hint), `hud_cpu_guard_low.png` (red guard pulse, shield-low hint), `hud_cpu_star_mark_charge.png`, `hud_cpu_star_refill_flash.png`; `hud_cpu_spawn.png` / `hud_cpu_fight_lock.png` re-rendered (controls card block + star rows, pips). CPU-rendered, same as the other hud_cpu frames.

## Star variants (docs/stars.md)
World frames of the reworked weapons (lavapipe, same recipe as above, `SHOT_WEAPONS=flash|dazed|hook|hookaim`,
plus `SHOT_FX=blind:0.7`): `weapons_flash.png` (burst, 0.13 s in; the barrel and canister frames are from before protocol v23 removed them, and `SHOT_FX=reload` shows the bow's reload bar),
`weapons_dazed.png` (white-out clearing, DAZED cue), `weapons_hook.png` (rope on an enemy: white-hot). `hookaim` (red reticle on a player) has no
frame: the reticle is hidden by the viewmodel in these shots. `star_world_blind.png` (full white-out; predates the retune). HUD frames (CPU, `hud_dump`):
`hud_cpu_star_sel_*.png` (old selector frames; only STAR and FLASH exist now), `hud_cpu_star_cycle_hint.png`, `hud_cpu_star_blind.png`.

## Power weapons (docs/powerups.md)
World frames (lavapipe, same recipe as above, `SHOT_FX=yumi|arrows|tags|tagblast|pedestals`): `power_world_yumi.png` (our own drawn bow bottom left
with the string back, a bow drawn straight at us with its bright laser glint and a faint one aimed off to the side, an arrow and a parried-back cyan arrow),
`power_world_arrows.png` (three streaking arrows), `power_world_tags.png` (tags arming on the floor, a flying one, one stuck on a player with its pip ring),
`power_world_tagblast.png` (blast shell mid-pop), `power_world_pedestals.png` (a bow and tag pedestals glowing, one dimmed after being taken).
HUD frames (CPU, `hud_dump`): `hud_cpu_power_draw.png`, `hud_cpu_power_full.png` (ring closed, RELEASE), `hud_cpu_power_tags_live.png` (DETONATE prompt),
`hud_cpu_power_tag_on_me.png` (TAG ON YOU countdown), `hud_cpu_power_glint.png` (bow drawn on you + a pickup call-out).

## Bow zoom
`bow_zoom_hip.png`, `bow_zoom_half.png`, `bow_zoom_full.png`: the yumi at draw 0 / 0.65 / 1 (`SHOT_FX=bowzoom:D,lineup SHOT_LINE=80 SHOT_LINE_PICK=1`, levels/ffa.lvl, spawn 3, lavapipe). FOV 90 / ~57 / 30, ring tightens, hands and bow leave the frame, range readout shows a fixed 118 M (synthetic).
