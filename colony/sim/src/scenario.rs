//! Built-in scenarios for tests, benches, and the headless runner. Each returns a
//! fresh [`Sim`] on the builtin element table. Worlds must be at least 3×3 because
//! every scenario keeps a one-cell sandstone border.

use crate::element::{builtin, ElementTable};
use crate::fixed::{Mass, Temp};
use crate::rng::Pcg32;
use crate::sim::Sim;

/// Names accepted by [`by_name`].
pub const NAMES: [&str; 6] = [
    "gas-mixing",
    "settled",
    "pressure-burst",
    "hot-rock",
    "noise",
    "pool",
];

/// Builds a scenario by name, or `None` for an unknown name or a world under 3×3.
pub fn by_name(name: &str, width: usize, height: usize) -> Option<Sim> {
    if width < 3 || height < 3 {
        return None;
    }
    match name {
        "gas-mixing" => Some(gas_mixing(width, height)),
        "settled" => Some(settled(width, height)),
        "pressure-burst" => Some(pressure_burst(width, height)),
        "hot-rock" => Some(hot_rock(width, height)),
        "noise" => Some(noise(width, height, 1)),
        "pool" => Some(pool(width, height)),
        _ => None,
    }
}

const BORDER_MASS_KG: u32 = 1800;

fn bordered(width: usize, height: usize, temp: Temp) -> Sim {
    assert!(
        width >= 3 && height >= 3,
        "scenarios need a 3×3 world or larger"
    );
    let mut sim = Sim::new(ElementTable::builtin(), width, height);
    let mass = Mass::from_kg(BORDER_MASS_KG);
    sim.fill_rect(0, 0, width, 1, builtin::SANDSTONE, mass, temp);
    sim.fill_rect(0, height - 1, width, height, builtin::SANDSTONE, mass, temp);
    sim.fill_rect(0, 0, 1, height, builtin::SANDSTONE, mass, temp);
    sim.fill_rect(width - 1, 0, width, height, builtin::SANDSTONE, mass, temp);
    sim
}

/// Left half oxygen at 2 kg and 280 K, right half carbon dioxide at 0.5 kg and 320 K,
/// inside a sandstone border: displacement, flux, and conduction all happen.
pub fn gas_mixing(width: usize, height: usize) -> Sim {
    let mut sim = bordered(width, height, Temp::from_kelvin(300));
    let mid = width / 2;
    sim.fill_rect(
        1,
        1,
        mid,
        height - 1,
        builtin::OXYGEN,
        Mass::from_kg(2),
        Temp::from_kelvin(280),
    );
    sim.fill_rect(
        mid,
        1,
        width - 1,
        height - 1,
        builtin::CARBON_DIOXIDE,
        Mass::from_grams(500),
        Temp::from_kelvin(320),
    );
    sim
}

/// Uniform oxygen at 1 kg and 300 K inside a 300 K border: nothing ever moves, so
/// every chunk is skipped from the second tick on.
pub fn settled(width: usize, height: usize) -> Sim {
    let mut sim = bordered(width, height, Temp::from_kelvin(300));
    sim.fill_rect(
        1,
        1,
        width - 1,
        height - 1,
        builtin::OXYGEN,
        Mass::from_kg(1),
        Temp::from_kelvin(300),
    );
    sim
}

/// A dense oxygen pocket (20 kg per cell) in the middle of a vacuum: exercises
/// expansion into vacuum and the mass floor.
pub fn pressure_burst(width: usize, height: usize) -> Sim {
    let mut sim = bordered(width, height, Temp::from_kelvin(300));
    let (bw, bh) = ((width / 8).max(1), (height / 8).max(1));
    let (x0, y0) = ((width - bw) / 2, (height - bh) / 2);
    sim.fill_rect(
        x0,
        y0,
        (x0 + bw).min(width - 1),
        (y0 + bh).min(height - 1),
        builtin::OXYGEN,
        Mass::from_kg(20),
        Temp::from_kelvin(300),
    );
    sim
}

/// Light sandstone (100 kg per cell) at 280 K with a 1200 K block in the middle:
/// solid-to-solid conduction only.
pub fn hot_rock(width: usize, height: usize) -> Sim {
    let mut sim = bordered(width, height, Temp::from_kelvin(280));
    sim.fill_rect(
        1,
        1,
        width - 1,
        height - 1,
        builtin::SANDSTONE,
        Mass::from_kg(100),
        Temp::from_kelvin(280),
    );
    let (bw, bh) = ((width / 4).max(1), (height / 4).max(1));
    let (x0, y0) = ((width - bw) / 2, (height - bh) / 2);
    sim.fill_rect(
        x0,
        y0,
        (x0 + bw).min(width - 1),
        (y0 + bh).min(height - 1),
        builtin::SANDSTONE,
        Mass::from_kg(100),
        Temp::from_kelvin(1200),
    );
    sim
}

/// Every interior cell holds oxygen or carbon dioxide at a seeded random mass between
/// 0.5 kg and 2 kg and a temperature between 280 K and 320 K: every chunk stays active
/// in every pass for a long time, which makes this the worst-case tick benchmark.
pub fn noise(width: usize, height: usize, seed: u64) -> Sim {
    let mut sim = bordered(width, height, Temp::from_kelvin(300));
    let mut rng = Pcg32::new(seed, 7);
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let element = if rng.below(2) == 0 {
                builtin::OXYGEN
            } else {
                builtin::CARBON_DIOXIDE
            };
            let mass = Mass::from_grams(500 + rng.below(1501) as u64);
            let temp = Temp::from_millikelvin(280_000 + rng.below(40_001) as i64);
            sim.set_cell(x, y, element, mass, temp);
        }
    }
    sim
}

/// Oxygen at 1 kg everywhere with a block of water (one eighth of the world, 1,000 kg
/// per cell) hanging in the upper half: it falls, spreads along the floor, and pushes
/// the air out of its way.
pub fn pool(width: usize, height: usize) -> Sim {
    let mut sim = bordered(width, height, Temp::from_kelvin(300));
    sim.fill_rect(
        1,
        1,
        width - 1,
        height - 1,
        builtin::OXYGEN,
        Mass::from_kg(1),
        Temp::from_kelvin(300),
    );
    let (bw, bh) = ((width / 8).max(1), (height / 8).max(1));
    let x0 = (width - bw) / 2;
    let y0 = (height * 3 / 4).min(height - 1 - bh).max(1);
    sim.fill_rect(
        x0,
        y0,
        (x0 + bw).min(width - 1),
        (y0 + bh).min(height - 1),
        builtin::WATER,
        Mass::from_kg(1000),
        Temp::from_kelvin(290),
    );
    sim
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_builds_and_unknown_names_do_not() {
        for name in NAMES {
            assert!(by_name(name, 8, 8).is_some(), "{name}");
        }
        assert!(by_name("nope", 8, 8).is_none());
        assert!(by_name("settled", 2, 8).is_none());
    }
}
