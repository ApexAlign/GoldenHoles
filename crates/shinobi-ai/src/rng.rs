//! Deterministic randomness. Every draw is a pure function of `(seed, bot id, tick, salt)`: no thread RNG,
//! no wall clock, no state carried between ticks, so a replay of the same intents reproduces every decision.

#[derive(Clone, Copy, Debug)]
pub struct Rng(u64);

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Rng {
    pub fn new(seed: u64, bot: u16, tick: u32, salt: u32) -> Self {
        Self(splitmix(seed ^ splitmix(((bot as u64) << 48) ^ ((tick as u64) << 16) ^ salt as u64)))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = splitmix(self.0);
        self.0
    }

    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform in [-1, 1).
    pub fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }

    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_inputs_same_stream() {
        let (mut a, mut b) = (Rng::new(7, 201, 1000, 3), Rng::new(7, 201, 1000, 3));
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert_ne!(Rng::new(7, 201, 1000, 3).next_u64(), Rng::new(7, 202, 1000, 3).next_u64());
        assert_ne!(Rng::new(7, 201, 1000, 3).next_u64(), Rng::new(7, 201, 1001, 3).next_u64());
    }

    #[test]
    fn unit_in_range() {
        let mut r = Rng::new(1, 1, 1, 1);
        for _ in 0..1000 {
            let u = r.unit();
            assert!((0.0..1.0).contains(&u));
        }
    }
}
