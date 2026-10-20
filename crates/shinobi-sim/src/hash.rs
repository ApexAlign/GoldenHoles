//! Stable, platform-independent 64-bit fingerprints.
//!
//! Used to compare the client's and server's world without sending either one: the level geometry and the
//! balance preset are hashed to the same bits on both sides. FNV-1a is deliberate here — tiny, no dependency,
//! and identical on x86 / ARM / wasm (the same reason the sim uses `libm`).

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a over `bytes`.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = FNV_OFFSET;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Continue a hash with a length-prefixed field, so `["ab", "c"]` and `["a", "bc"]` cannot collide.
pub fn mix(h: u64, bytes: &[u8]) -> u64 {
    let mut h = mix_u64(h, bytes.len() as u64);
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Continue a hash with one 64-bit value (little-endian, so the wire and every architecture agree).
pub fn mix_u64(mut h: u64, v: u64) -> u64 {
    for b in v.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Continue a hash with an `f32` by its IEEE bit pattern (`-0.0` and `0.0` stay distinct).
pub fn mix_f32(h: u64, v: f32) -> u64 {
    mix_u64(h, v.to_bits() as u64)
}

pub const OFFSET: u64 = FNV_OFFSET;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_matches_the_reference_vector() {
        assert_eq!(fnv1a64(b""), FNV_OFFSET);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn mixing_is_order_and_length_sensitive() {
        let a = mix(mix(OFFSET, b"ab"), b"c");
        let b = mix(mix(OFFSET, b"a"), b"bc");
        assert_ne!(a, b);
        assert_ne!(mix_f32(OFFSET, 0.0), mix_f32(OFFSET, -0.0));
    }
}
