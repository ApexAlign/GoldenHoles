# Reference frames
`ffa_first_frame_b2b384a.png`: first software-rendered frame of the FFA map (levels/ffa.lvl, spawn 3, 960x540) at head
b2b384a. Reproduce: `apt-get install -y --no-install-recommends mesa-vulkan-drivers libvulkan1 vulkan-tools; pip install pillow;
VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json WGPU_BACKEND=vulkan shinobi-game --shot-offline out.ppm --secs 2.0 --level levels/ffa.lvl --spawn 3`
(then convert PPM to PNG with PIL and look at it). Issues seen in this frame are tracked in the Engine status messages.
