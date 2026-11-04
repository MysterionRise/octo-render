//! Human units to the kernel's fixed point. Every conversion goes through integer
//! micro-units (`round(x × 10^k)`) and then `q16`, so a value with at most six decimals
//! in the kernel unit lands on exactly the integer a hand-written `q16(whole, micro)`
//! gives, which is what lets the base pack reproduce `ElementTable::builtin()` bit for
//! bit.

use colony_sim::fixed::q16;
use colony_sim::{Mass, Temp};

/// Seconds per tick, as the divisor applied to per-second rates.
pub const TICKS_PER_SECOND: f64 = 10.0;

/// A value already in the kernel unit (J/(kg·K), kg/m³, …) to Q16.16.
pub fn q16_of(value: f64) -> i64 {
    q16(0, (value * 1e6).round() as i64)
}

/// kJ/(kg·K) to Q16.16 J/(kg·K).
pub fn shc(kj_per_kg_k: f64) -> i64 {
    q16_of(kj_per_kg_k * 1000.0)
}

/// W/(m·K) to Q16.16 J/(K·tick): the conductivity times the tick length.
pub fn conductivity(w_per_m_k: f64) -> i64 {
    q16(0, (w_per_m_k * 1e6 / TICKS_PER_SECOND).round() as i64)
}

/// kg/m³ to Q16.16.
pub fn density(kg_per_m3: f64) -> i64 {
    q16_of(kg_per_m3)
}

/// Kilograms to a mass, to the gram.
pub fn mass_kg(kg: f64) -> Mass {
    Mass::from_grams((kg * 1000.0).round().max(0.0) as u64)
}

/// A per-second mass rate to a per-tick mass.
pub fn mass_per_tick(kg_per_s: f64) -> Mass {
    Mass::from_grams((kg_per_s * 1000.0 / TICKS_PER_SECOND).round().max(0.0) as u64)
}

/// Kelvin to a temperature, to the millikelvin.
pub fn kelvin(k: f64) -> Temp {
    Temp::from_millikelvin((k * 1000.0).round() as i64)
}

/// Watts to Q16.16 joules per tick.
pub fn watts(w: f64) -> i64 {
    q16(0, (w * 1e6 / TICKS_PER_SECOND).round() as i64)
}

/// Kilojoules to Q16.16 joules.
pub fn kilojoules(kj: f64) -> i64 {
    q16_of(kj * 1000.0)
}

/// `#rrggbb` to bytes.
pub fn colour(hex: &str) -> Option<[u8; 3]> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([channel(0)?, channel(2)?, channel(4)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_land_on_the_hand_written_constants() {
        assert_eq!(shc(1.005), q16(1005, 0));
        assert_eq!(shc(0.83), q16(830, 0));
        assert_eq!(conductivity(0.024), q16(0, 2_400));
        assert_eq!(conductivity(0.0146), q16(0, 1_460));
        assert_eq!(conductivity(2.9), q16(0, 290_000));
        assert_eq!(density(1.429), q16(1, 429_000));
        assert_eq!(density(0.6), q16(0, 600_000));
        assert_eq!(mass_kg(1000.0), Mass::from_kg(1000));
        assert_eq!(mass_kg(0.0015), Mass::from_grams(2), "rounds to the gram");
        assert_eq!(mass_per_tick(0.5), Mass::from_grams(50));
        assert_eq!(kelvin(273.15), Temp::from_millikelvin(273_150));
        assert_eq!(watts(60.0), q16(6, 0));
        assert_eq!(watts(-240.0), q16(-24, 0));
        assert_eq!(kilojoules(40.0), q16(40_000, 0));
        assert_eq!(colour("#78c8ff"), Some([120, 200, 255]));
        assert_eq!(colour("78c8ff"), None);
        assert_eq!(colour("#78c8f"), None);
        assert_eq!(colour("#zz0000"), None);
    }
}
