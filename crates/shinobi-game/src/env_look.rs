//! Environment look for the floating sky city: the Tron x Shinobi fusion. Everything here is cosmetic and only runs on
//! floating levels (`shinobi_sim::level::is_floating`); ground maps are untouched (their goldens stay exact).
//!
//! Surfaces: shader mode 3 in `shaders/grid_floor.wgsl` paints every box in the colour of the tier it is at (cyan / violet /
//! amber / jade / vermilion by height), with circuit traces, a climbing scan band and falling glyph rain on tall faces.

use bevy::prelude::*;

/// (tint, mode) for a floating-city box in `play::setup`: shader mode 3 picks the colour per fragment from the height.
pub fn city_surface() -> ([f32; 3], f32) {
    ([1.0, 1.0, 1.0], 3.0)
}

pub struct EnvLookPlugin;

impl Plugin for EnvLookPlugin {
    fn build(&self, _app: &mut App) {}
}
