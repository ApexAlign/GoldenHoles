//! Render the fixed HUD scenarios to PPM files: `cargo run -p shinobi-client --example hud_dump -- OUT_DIR [W H]`.
//! Each scenario is written on a dark and a bright backdrop (`<name>_dark.ppm`, `<name>_bright.ppm`).
use shinobi_client::hud_raster::{ppm, render, Backdrop};
use shinobi_client::hud_scenarios::scenarios;

fn main() {
    let mut a = std::env::args().skip(1);
    let dir = a.next().expect("OUT_DIR");
    let w: u32 = a.next().and_then(|v| v.parse().ok()).unwrap_or(960);
    let h: u32 = a.next().and_then(|v| v.parse().ok()).unwrap_or(540);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, prims) in scenarios() {
        for (tag, bg) in [("dark", Backdrop::Dark), ("bright", Backdrop::Bright)] {
            let img = render(&prims, w, h, bg);
            std::fs::write(format!("{dir}/{name}_{tag}.ppm"), ppm(&img, w, h)).unwrap();
        }
    }
}
