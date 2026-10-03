//! One neon colour per player, derived from the player id (no wire traffic: every client computes the same colour).
//!
//! `PLAYER_PALETTE` is 16 saturated, bright colours picked by greedy farthest-point selection in CIELAB (min pairwise
//! distance > 30 dE, all readable on the near-black grid), cyan first. Index order is also the preference order: any
//! prefix is as separated as the palette allows.
//!
//! * `player_colour(id)` is stable (`id % 16`). Humans take ids 1..=200 in join order and bots 201..=255, so the
//!   usual 8 humans + 8 bots (ids 1..=8, 201..=208) all land on different colours.
//! * Ids that are 16 apart (a human who joined later) collide. When every player on screen must differ, use
//!   `unique_colour_indices` over the current player list: deterministic, id order, linear probing, so every client
//!   that sees the same players computes the same assignment.

pub const PALETTE_LEN: usize = 16;

/// sRGB, 0..=255.
pub const PLAYER_PALETTE: [[u8; 3]; PALETTE_LEN] = [
    [0, 240, 255],  // tron cyan
    [255, 0, 0],    // red
    [179, 0, 255],  // violet
    [242, 255, 0],  // yellow
    [64, 140, 255], // azure
    [255, 64, 159], // hot pink
    [0, 255, 140],  // spring green
    [255, 140, 0],  // orange
    [115, 255, 0],  // lime
    [0, 153, 204],  // steel cyan
    [204, 74, 51],  // brick
    [255, 25, 221], // magenta
    [143, 204, 51], // olive lime
    [0, 204, 153],  // sea green
    [255, 0, 89],   // crimson
    [255, 204, 0],  // gold
];

/// Palette slot for `id` (`id % 16`).
pub fn player_colour_index(id: u16) -> usize {
    id as usize % PALETTE_LEN
}

/// sRGB colour for `id`.
pub fn player_colour(id: u16) -> [u8; 3] {
    PLAYER_PALETTE[player_colour_index(id)]
}

/// Distinct palette slots for up to 16 players: each id (processed in ascending id order) takes its preferred slot
/// `id % 16`, else the next free one. Returns `(id, slot)` in ascending id order; players beyond 16 share slots.
pub fn unique_colour_indices(ids: &[u16]) -> Vec<(u16, usize)> {
    let mut sorted: Vec<u16> = ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut taken = [false; PALETTE_LEN];
    sorted
        .into_iter()
        .map(|id| {
            let mut slot = player_colour_index(id);
            if taken.iter().all(|t| *t) {
                return (id, slot); // more than 16 players: reuse
            }
            while taken[slot] {
                slot = (slot + 1) % PALETTE_LEN;
            }
            taken[slot] = true;
            (id, slot)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lab(c: [u8; 3]) -> [f64; 3] {
        let f = |t: u8| {
            let t = t as f64 / 255.0;
            if t > 0.04045 { ((t + 0.055) / 1.055).powf(2.4) } else { t / 12.92 }
        };
        let (r, g, b) = (f(c[0]), f(c[1]), f(c[2]));
        let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
        let h = |t: f64| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
        [116.0 * h(y) - 16.0, 500.0 * (h(x) - h(y)), 200.0 * (h(y) - h(z))]
    }

    #[test]
    fn palette_is_well_separated_and_bright() {
        let mut min = f64::MAX;
        for i in 0..PALETTE_LEN {
            assert!(lab(PLAYER_PALETTE[i])[0] > 40.0, "colour {i} is too dark for the grid: {:?}", PLAYER_PALETTE[i]);
            for j in i + 1..PALETTE_LEN {
                let (a, b) = (lab(PLAYER_PALETTE[i]), lab(PLAYER_PALETTE[j]));
                min = min.min(((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt());
            }
        }
        assert!(min > 25.0, "two palette colours are only {min:.1} dE apart");
    }

    #[test]
    fn stable_and_the_usual_eight_humans_plus_eight_bots_are_all_different() {
        assert_eq!(player_colour(5), player_colour(5));
        let ids: Vec<u16> = (1..=8).chain(201..=208).collect();
        let mut slots: Vec<usize> = ids.iter().map(|id| player_colour_index(*id)).collect();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), 16, "ids 1..=8 and bots 201..=208 must not collide");
    }

    #[test]
    fn unique_assignment_resolves_collisions_deterministically() {
        // 3 and 19 and 35 all prefer slot 3
        let a = unique_colour_indices(&[35, 3, 19]);
        assert_eq!(a, vec![(3, 3), (19, 4), (35, 5)]);
        assert_eq!(unique_colour_indices(&[19, 35, 3, 3]), a, "order of the input does not matter");
        // full house: 16 distinct slots, a 17th player reuses its preferred one
        let ids: Vec<u16> = (1..=17).collect();
        let r = unique_colour_indices(&ids);
        let mut slots: Vec<usize> = r[..16].iter().map(|x| x.1).collect();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), 16);
        assert_eq!(r[16], (17, 1));
    }
}
