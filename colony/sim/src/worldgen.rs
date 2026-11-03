//! Seeded cave worlds from integer value noise. Every number is an integer, so the
//! same seed gives the same world on every platform, which replay depends on.

use crate::element::{builtin, ElementTable};
use crate::fixed::{Mass, Temp};
use crate::hash::Fnv1a;
use crate::sim::Sim;

/// A pseudo-random 16-bit value for a lattice point.
fn lattice(seed: u64, x: u64, y: u64) -> i64 {
    let mut h = Fnv1a::new();
    h.write_u64(seed);
    h.write_u64(x);
    h.write_u64(y);
    (h.finish() >> 48) as i64
}

/// Smoothstep `3t² − 2t³` for `t` in Q16.16 `[0, 1]`.
fn smooth(t: i64) -> i64 {
    let t2 = (t * t) >> 16;
    let t3 = (t2 * t) >> 16;
    3 * t2 - 2 * t3
}

/// Bilinear value noise in `0..=65535` (a Q16.16 fraction) with lattice spacing `period`.
pub fn value_noise(seed: u64, x: usize, y: usize, period: usize) -> i64 {
    let (ix, iy) = ((x / period) as u64, (y / period) as u64);
    let fx = smooth((((x % period) as i64) << 16) / period as i64);
    let fy = smooth((((y % period) as i64) << 16) / period as i64);
    let lerp = |a: i64, b: i64, t: i64| a + (((b - a) * t) >> 16);
    let top = lerp(lattice(seed, ix, iy), lattice(seed, ix + 1, iy), fx);
    let bottom = lerp(lattice(seed, ix, iy + 1), lattice(seed, ix + 1, iy + 1), fx);
    lerp(top, bottom, fy)
}

/// Three octaves of value noise weighted 4:2:1, in `0..=65535`.
pub fn fbm(seed: u64, x: usize, y: usize, base_period: usize) -> i64 {
    let p = base_period.max(4);
    (4 * value_noise(seed, x, y, p)
        + 2 * value_noise(seed ^ 0x9e37_79b9, x, y, p / 2)
        + value_noise(seed ^ 0x7f4a_7c15, x, y, p / 4))
        / 7
}

/// Cave threshold on the Q16.16 fraction: about 55% of the volume stays rock.
const OPEN_ABOVE: i64 = 36_000;

/// A sandstone body riddled with air pockets, with carbon dioxide collecting in the
/// lower third and water pools in the bottom quarter, inside a one-cell border.
pub fn cave(width: usize, height: usize, seed: u64) -> Sim {
    assert!(
        width >= 3 && height >= 3,
        "worlds need a 3×3 interior or larger"
    );
    let mut sim = Sim::new(ElementTable::builtin(), width, height);
    let rock = Mass::from_kg(1800);
    let rock_temp = Temp::from_kelvin(290);
    let period = (width.min(height) / 8).max(4);
    for y in 0..height {
        for x in 0..width {
            let border = x == 0 || y == 0 || x + 1 == width || y + 1 == height;
            if border || fbm(seed, x, y, period) <= OPEN_ABOVE {
                sim.set_cell(x, y, builtin::SANDSTONE, rock, rock_temp);
                continue;
            }
            let pools = value_noise(seed ^ 0x5151, x, y, period.max(6)) > 32_768;
            let heavy = value_noise(seed ^ 0xc0c0, x, y, period) > 39_000;
            if y < height / 4 && pools {
                sim.set_cell(
                    x,
                    y,
                    builtin::WATER,
                    Mass::from_kg(1000),
                    Temp::from_kelvin(285),
                );
            } else if y < height / 3 && heavy {
                sim.set_cell(
                    x,
                    y,
                    builtin::CARBON_DIOXIDE,
                    Mass::from_grams(500),
                    Temp::from_kelvin(300),
                );
            } else {
                sim.set_cell(
                    x,
                    y,
                    builtin::OXYGEN,
                    Mass::from_kg(1),
                    Temp::from_kelvin(295),
                );
            }
        }
    }
    sim
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_stays_in_range_and_is_continuous_across_lattice_points() {
        for x in 0..64 {
            for y in 0..64 {
                let v = value_noise(7, x, y, 8);
                assert!((0..=65_535).contains(&v), "{v}");
                let f = fbm(7, x, y, 16);
                assert!((0..=65_535).contains(&f), "{f}");
            }
        }
        // Stepping one cell never jumps more than a fraction of the range.
        for x in 0..63 {
            let a = value_noise(7, x, 5, 8);
            let b = value_noise(7, x + 1, 5, 8);
            assert!((a - b).abs() < 20_000, "jump of {} at x={x}", (a - b).abs());
        }
    }

    #[test]
    fn same_seed_same_world_and_different_seeds_differ() {
        let a = cave(96, 64, 42);
        let b = cave(96, 64, 42);
        let c = cave(96, 64, 43);
        assert_eq!(a.state_hash(), b.state_hash());
        assert_ne!(a.state_hash(), c.state_hash());
    }

    #[test]
    fn cave_has_rock_air_heavy_gas_and_water_in_plausible_shares() {
        let sim = cave(128, 128, 1);
        let w = sim.world();
        let mut counts = [0usize; 4];
        for y in 0..w.height {
            for x in 0..w.width {
                let el = w.get(x, y).0;
                let slot = if el == builtin::SANDSTONE {
                    0
                } else if el == builtin::OXYGEN {
                    1
                } else if el == builtin::CARBON_DIOXIDE {
                    2
                } else if el == builtin::WATER {
                    3
                } else {
                    panic!("unexpected element {el:?}")
                };
                counts[slot] += 1;
            }
        }
        let total = (w.width * w.height) as f64;
        let share = |n: usize| n as f64 / total;
        assert!(
            share(counts[0]) > 0.35 && share(counts[0]) < 0.85,
            "rock share {}",
            share(counts[0])
        );
        assert!(share(counts[1]) > 0.05, "oxygen share {}", share(counts[1]));
        assert!(counts[2] > 0, "no carbon dioxide");
        assert!(counts[3] > 0, "no water");
    }
}
