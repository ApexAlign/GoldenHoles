//! Per-player neon colour for 3D bodies, discs, trails and HUD tags. The table and the id -> slot rule live in
//! `shinobi_proto::colour` (no wire traffic: every client derives the same colour from the player id).
use std::collections::HashMap;

use shinobi_proto::colour::{player_colour, unique_colour_indices, PLAYER_PALETTE};

use crate::hud::Rgba;

fn rgba(c: [u8; 3]) -> Rgba {
    Rgba(c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0)
}

/// Stable colour for `id` (`id % 16`). Cheap; may repeat for ids 16 apart.
pub fn player_rgba(id: u16) -> Rgba {
    rgba(player_colour(id))
}

/// Colours that are pairwise distinct for the players currently present (up to 16): call when the player list
/// changes and look colours up by id. Every client with the same player set computes the same map.
pub fn unique_player_rgba(ids: &[u16]) -> HashMap<u16, Rgba> {
    unique_colour_indices(ids).into_iter().map(|(id, slot)| (id, rgba(PLAYER_PALETTE[slot]))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_matches_the_palette_and_unique_map_is_distinct() {
        assert_eq!(player_rgba(0), Rgba(0.0, 240.0 / 255.0, 1.0, 1.0));
        let ids: Vec<u16> = vec![1, 17, 33, 201];
        let m = unique_player_rgba(&ids);
        assert_eq!(m.len(), 4);
        let mut seen: Vec<[u32; 3]> = m.values().map(|c| [(c.0 * 255.0) as u32, (c.1 * 255.0) as u32, (c.2 * 255.0) as u32]).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 4, "colliding ids still get different colours");
    }
}
