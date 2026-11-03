//! A dependency-free 64-bit FNV-1a hash of kernel state, used for replay equality and
//! golden tests. It is not a cryptographic hash; it only has to be deterministic.

const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const PRIME: u64 = 0x0000_0100_0000_01b3;

/// Incremental FNV-1a over little-endian bytes.
#[derive(Clone, Copy, Debug)]
pub struct Fnv1a(u64);

impl Default for Fnv1a {
    fn default() -> Self {
        Fnv1a(OFFSET)
    }
}

impl Fnv1a {
    pub fn new() -> Fnv1a {
        Fnv1a::default()
    }

    #[inline]
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(PRIME);
        }
    }

    pub fn write_u16s(&mut self, values: &[u16]) {
        for v in values {
            self.write_bytes(&v.to_le_bytes());
        }
    }

    pub fn write_u32s(&mut self, values: &[u32]) {
        for v in values {
            self.write_bytes(&v.to_le_bytes());
        }
    }

    pub fn write_i32s(&mut self, values: &[i32]) {
        for v in values {
            self.write_bytes(&v.to_le_bytes());
        }
    }

    pub fn write_u64(&mut self, value: u64) {
        self.write_bytes(&value.to_le_bytes());
    }

    pub fn finish(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_fnv1a_vectors() {
        let mut h = Fnv1a::new();
        h.write_bytes(b"a");
        assert_eq!(h.finish(), 0xaf63_dc4c_8601_ec8c);
        let mut h = Fnv1a::new();
        h.write_bytes(b"foobar");
        assert_eq!(h.finish(), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn order_matters() {
        let mut a = Fnv1a::new();
        a.write_u32s(&[1, 2]);
        let mut b = Fnv1a::new();
        b.write_u32s(&[2, 1]);
        assert_ne!(a.finish(), b.finish());
    }
}
