//! PCG32: a small, fast, seedable generator with a fixed output sequence on every
//! platform. The kernel never touches any other randomness.

/// PCG-XSH-RR 64/32 with the reference multiplier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pcg32 {
    state: u64,
    inc: u64,
}

const MULTIPLIER: u64 = 6_364_136_223_846_793_005;

impl Pcg32 {
    /// Seeds the generator; `stream` selects one of 2^63 independent sequences.
    pub fn new(seed: u64, stream: u64) -> Pcg32 {
        let mut rng = Pcg32 {
            state: 0,
            inc: (stream << 1) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(seed);
        rng.next_u32();
        rng
    }

    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(MULTIPLIER).wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    pub fn next_u64(&mut self) -> u64 {
        ((self.next_u32() as u64) << 32) | self.next_u32() as u64
    }

    /// Uniform in `0..bound` without modulo bias.
    pub fn below(&mut self, bound: u32) -> u32 {
        assert!(bound > 0);
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let r = self.next_u32();
            if r >= threshold {
                return r % bound;
            }
        }
    }

    pub fn state(&self) -> (u64, u64) {
        (self.state, self.inc)
    }

    pub fn from_state(state: u64, inc: u64) -> Pcg32 {
        Pcg32 { state, inc }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_reference_sequence() {
        // First outputs of the PCG32 reference implementation for seed 42, stream 54.
        let mut rng = Pcg32::new(42, 54);
        let first: Vec<u32> = (0..3).map(|_| rng.next_u32()).collect();
        assert_eq!(first, vec![0xa15c_02b7, 0x7b47_f409, 0xba1d_3330]);
    }

    #[test]
    fn same_seed_same_sequence_and_state_round_trips() {
        let mut a = Pcg32::new(7, 1);
        let mut b = Pcg32::new(7, 1);
        for _ in 0..100 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
        let (s, i) = a.state();
        let mut c = Pcg32::from_state(s, i);
        assert_eq!(a.next_u64(), c.next_u64());
        for _ in 0..1000 {
            assert!(a.below(10) < 10);
        }
    }
}
