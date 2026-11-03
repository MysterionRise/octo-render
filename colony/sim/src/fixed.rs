//! Q16.16 fixed-point units. Every quantity the kernel stores or steps is an integer
//! in these units; conversion from human units happens in content loading, never here.

/// Number of fractional bits in every fixed-point quantity.
pub const FRAC_BITS: u32 = 16;

/// `1.0` in Q16.16.
pub const ONE: i64 = 1 << FRAC_BITS;

/// Builds a Q16.16 constant from a decimal written as `whole` plus `micro` millionths,
/// so `q16(1, 5_000)` is `1.005` and `q16(0, 2_400)` is `0.0024`. Rounds to nearest,
/// half away from zero.
pub const fn q16(whole: i64, micro: i64) -> i64 {
    let micros = whole * 1_000_000 + micro;
    let scaled = micros << FRAC_BITS;
    if scaled >= 0 {
        (scaled + 500_000) / 1_000_000
    } else {
        (scaled - 500_000) / 1_000_000
    }
}

/// `(mag * k) >> 16` for a magnitude and a non-negative Q16.16 factor.
///
/// Antisymmetric exchanges must scale the magnitude and reapply the sign afterwards:
/// an arithmetic shift of a negative product rounds toward negative infinity, so the
/// two endpoints of an exchange would otherwise disagree by one unit and conservation
/// would silently break.
#[inline]
pub fn scale_mag(mag: u64, k: i64) -> u64 {
    debug_assert!(k >= 0, "scale factors are non-negative");
    match mag.checked_mul(k as u64) {
        Some(product) => product >> FRAC_BITS,
        None => ((mag as u128 * k as u128) >> FRAC_BITS) as u64,
    }
}

/// Mass in Q16.16 kilograms. One raw unit is about 15.26 mg; the maximum is just under
/// 65,536 kg.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Mass(pub u32);

impl Mass {
    pub const ZERO: Mass = Mass(0);

    /// The smallest mass a gas cell keeps (about 1 g). Below it, the gas merges into a
    /// neighbour and the cell becomes vacuum, which removes micro-residue.
    pub const FLOOR: Mass = Mass(66);

    /// Whole kilograms; panics above the representable range.
    pub const fn from_kg(kg: u32) -> Mass {
        assert!(kg < 65_536, "mass above 65,535 kg is not representable");
        Mass(kg << FRAC_BITS)
    }

    /// Whole grams, rounded down to the raw resolution.
    pub const fn from_grams(grams: u64) -> Mass {
        let raw = (grams << FRAC_BITS) / 1000;
        assert!(
            raw <= u32::MAX as u64,
            "mass above 65,535 kg is not representable"
        );
        Mass(raw as u32)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Whole grams, rounded down.
    pub const fn to_grams(self) -> u64 {
        ((self.0 as u64) * 1000) >> FRAC_BITS
    }
}

/// Temperature in Q16.16 kelvin.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Temp(pub i32);

impl Temp {
    pub const ZERO: Temp = Temp(0);

    /// Whole kelvin; panics outside ±32,767 K.
    pub const fn from_kelvin(k: i32) -> Temp {
        assert!(
            k > -32_768 && k < 32_768,
            "temperature outside the representable range"
        );
        Temp(k << FRAC_BITS)
    }

    /// Whole millikelvin, rounded toward zero.
    pub const fn from_millikelvin(mk: i64) -> Temp {
        let raw = (mk << FRAC_BITS) / 1000;
        assert!(
            raw >= i32::MIN as i64 && raw <= i32::MAX as i64,
            "temperature outside the representable range"
        );
        Temp(raw as i32)
    }

    pub const fn raw(self) -> i32 {
        self.0
    }

    /// Whole millikelvin, rounded toward negative infinity.
    pub const fn to_millikelvin(self) -> i64 {
        ((self.0 as i64) * 1000) >> FRAC_BITS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q16_rounds_decimals_to_nearest() {
        assert_eq!(q16(1, 0), ONE);
        assert_eq!(q16(0, 500_000), ONE / 2);
        assert_eq!(q16(0, 2_400), 157); // 0.0024 * 65536 = 157.3
        assert_eq!(q16(-1, 0), -ONE);
    }

    #[test]
    fn mass_round_trips_through_grams() {
        assert_eq!(Mass::from_kg(2).to_grams(), 2000);
        assert_eq!(Mass::from_grams(500).to_grams(), 500);
        assert_eq!(Mass::from_grams(3).to_grams(), 2); // rounds down at both ends
        assert_eq!(Mass::from_grams(1).raw(), 65);
        assert!(Mass::from_grams(1) < Mass::FLOOR);
        assert!(Mass::from_grams(2) >= Mass::FLOOR);
    }

    #[test]
    fn temp_round_trips_through_millikelvin() {
        assert_eq!(Temp::from_kelvin(300).to_millikelvin(), 300_000);
        assert_eq!(Temp::from_millikelvin(-1_500).to_millikelvin(), -1_500);
        assert_eq!(Temp::from_millikelvin(-1).to_millikelvin(), -1); // floor after truncation
        assert_eq!(Temp::from_millikelvin(1).to_millikelvin(), 0);
    }

    #[test]
    fn scale_mag_is_exact_for_quarter() {
        assert_eq!(scale_mag(100, ONE / 4), 25);
        assert_eq!(scale_mag(3, ONE / 4), 0);
        assert_eq!(scale_mag(u32::MAX as u64, ONE), u32::MAX as u64);
    }
}
