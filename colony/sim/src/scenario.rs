//! Built-in scenarios for tests, benches, and the headless runner. Each returns a
//! fresh [`Sim`] on the builtin element table. Worlds must be at least 3×3 because
//! every scenario keeps a one-cell sandstone border.

use crate::element::{builtin, ElementTable};
use crate::fixed::{Mass, Temp};
use crate::rng::Pcg32;
use crate::sim::Sim;

/// Names accepted by [`by_name`].
pub const NAMES: [&str; 9] = [
    "gas-mixing",
    "settled",
    "pressure-burst",
    "hot-rock",
    "noise",
    "pool",
    "cave",
    "crew",
    "yard",
];

/// Builds a scenario by name with seed 1; see [`by_name_seeded`].
pub fn by_name(name: &str, width: usize, height: usize) -> Option<Sim> {
    by_name_seeded(name, width, height, 1)
}

/// Builds a scenario by name, or `None` for an unknown name or a world under 3×3.
/// The seed only matters for `noise` and `cave`.
pub fn by_name_seeded(name: &str, width: usize, height: usize, seed: u64) -> Option<Sim> {
    if width < 3 || height < 3 {
        return None;
    }
    match name {
        "gas-mixing" => Some(gas_mixing(width, height)),
        "settled" => Some(settled(width, height)),
        "pressure-burst" => Some(pressure_burst(width, height)),
        "hot-rock" => Some(hot_rock(width, height)),
        "noise" => Some(noise(width, height, seed)),
        "pool" => Some(pool(width, height)),
        "cave" => Some(cave_with_colonists(width, height, seed, 4)),
        "crew" => Some(crew(width, height, seed)),
        "yard" => Some(yard(width, height, seed, 4)),
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

/// A seeded cave world without colonists; see [`crate::worldgen::cave`].
pub fn cave(width: usize, height: usize) -> Sim {
    crate::worldgen::cave(width, height, 1)
}

/// A seeded cave with `n` colonists in its largest cavern, on the standing cells of
/// that cavern nearest the world centre.
pub fn cave_with_colonists(width: usize, height: usize, seed: u64, n: usize) -> Sim {
    let mut sim = crate::worldgen::cave(width, height, seed);
    sim.refresh_nav();
    let mask = sim.nav.largest_component();
    let (cx, cy) = (width as i64 / 2, height as i64 / 2);
    let mut spots: Vec<(u64, u32, u32)> = Vec::new();
    for y in 0..height as u32 {
        for x in 0..width as u32 {
            if mask[(y as usize) * width + x as usize] {
                let d = (x as i64 - cx).unsigned_abs() + (y as i64 - cy).unsigned_abs();
                spots.push((d, y, x));
            }
        }
    }
    spots.sort_unstable();
    for &(_, y, x) in spots.iter().take(n) {
        sim.spawn_colonist(x as usize, y as usize);
    }
    sim
}

/// A hall with a solid floor and ceiling, seeded pillars and climbable staircases,
/// `colonists` colonists spread along the floor, and up to `orders` dig orders on pillar
/// and stair cells, all reachable on foot.
pub fn hall(width: usize, height: usize, seed: u64, colonists: usize, orders: usize) -> Sim {
    let mut sim = Sim::new(ElementTable::builtin(), width, height);
    let rock = Mass::from_kg(1800);
    let temp = Temp::from_kelvin(295);
    sim.fill_rect(0, 0, width, height, builtin::OXYGEN, Mass::from_kg(1), temp);
    sim.fill_rect(0, 0, width, 2, builtin::SANDSTONE, rock, temp);
    sim.fill_rect(0, height - 1, width, height, builtin::SANDSTONE, rock, temp);
    sim.fill_rect(0, 0, 1, height, builtin::SANDSTONE, rock, temp);
    sim.fill_rect(width - 1, 0, width, height, builtin::SANDSTONE, rock, temp);
    let mut rng = Pcg32::new(seed, 11);
    let mut diggable: Vec<(u32, u32)> = Vec::new();
    // Pillars every three cells, two to six cells tall, with a four-step staircase in
    // place of two pillars every 24 cells. Only the two lowest pillar cells are ordered
    // dug: once the base is dug the cell above it stands on the floor, but nothing
    // above that does, and digging stairs would cut off the cells they lead to.
    let mut x = 3usize;
    while x + 8 < width - 1 {
        if x % 24 == 12 {
            for step in 0..4usize {
                for y in 2..(3 + step).min(height - 2) {
                    sim.set_cell(x + step, y, builtin::SANDSTONE, rock, temp);
                }
            }
            x += 6;
            continue;
        }
        let tall = 2 + rng.below(5) as usize;
        for y in 2..(2 + tall).min(height - 2) {
            sim.set_cell(x, y, builtin::SANDSTONE, rock, temp);
            if y <= 3 {
                diggable.push((x as u32, y as u32));
            }
        }
        x += 3;
    }
    sim.refresh_nav();
    let mut spots: Vec<(u32, u32)> = (1..width as u32 - 1)
        .filter(|&x| sim.nav().is_standing(x, 2))
        .map(|x| (x, 2))
        .collect();
    let stride = (spots.len() / colonists.max(1)).max(1);
    spots = spots.into_iter().step_by(stride).take(colonists).collect();
    for (x, y) in spots {
        sim.spawn_colonist(x as usize, y as usize);
    }
    let stride = (diggable.len() / orders.max(1)).max(1);
    for (x, y) in diggable.into_iter().step_by(stride).take(orders) {
        sim.order(
            crate::job::JobKind::Dig { x, y },
            crate::agent::DEFAULT_PRIORITY,
        );
    }
    sim
}

/// An open floor for building: a solid floor and ceiling, oxygen, one-cell boulders
/// every six cells (walkable over, diggable for material), one four-cell tower near the
/// right edge whose top only a ladder reaches, and `colonists` colonists spread along
/// the floor. No orders.
pub fn yard(width: usize, height: usize, seed: u64, colonists: usize) -> Sim {
    let mut sim = Sim::new(ElementTable::builtin(), width, height);
    let rock = Mass::from_kg(1800);
    let temp = Temp::from_kelvin(295);
    sim.fill_rect(0, 0, width, height, builtin::OXYGEN, Mass::from_kg(1), temp);
    sim.fill_rect(0, 0, width, 2, builtin::SANDSTONE, rock, temp);
    sim.fill_rect(0, height - 1, width, height, builtin::SANDSTONE, rock, temp);
    sim.fill_rect(0, 0, 1, height, builtin::SANDSTONE, rock, temp);
    sim.fill_rect(width - 1, 0, width, height, builtin::SANDSTONE, rock, temp);
    let mut rng = Pcg32::new(seed, 13);
    // Small worlds (tests of every scenario name) get just the floor.
    let tower = (width >= 16 && height >= 8).then_some(width - 6);
    let limit = tower.unwrap_or(width - 1);
    let mut x = 4usize;
    while x + 4 < limit {
        sim.set_cell(x, 2, builtin::SANDSTONE, rock, temp);
        x += 5 + rng.below(3) as usize;
    }
    if let Some(tower) = tower {
        for y in 2..6.min(height - 2) {
            sim.set_cell(tower, y, builtin::SANDSTONE, rock, temp);
        }
    }
    sim.refresh_nav();
    let spots: Vec<usize> = (1..limit)
        .filter(|&x| sim.nav().is_standing(x as u32, 2) && !sim.nav().is_solid(x as u32, 2))
        .collect();
    let stride = (spots.len() / colonists.max(1)).max(1);
    for x in spots.into_iter().step_by(stride).take(colonists) {
        sim.spawn_colonist(x, 2);
    }
    sim
}

/// The agents benchmark: a hall with 100 colonists and 400 dig orders.
pub fn crew(width: usize, height: usize, seed: u64) -> Sim {
    hall(width, height, seed, 100, 400)
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
    fn caves_get_colonists_and_the_crew_gets_orders() {
        let sim = cave_with_colonists(96, 64, 3, 5);
        assert_eq!(sim.colonists().len(), 5);
        for (_, c) in sim.colonists().iter() {
            assert!(sim.nav().is_standing(c.cell.0, c.cell.1));
        }
        let crew = crew(256, 64, 1);
        assert_eq!(crew.colonists().len(), 100);
        assert!(
            crew.jobs().counts().open >= 100,
            "orders: {:?}",
            crew.jobs().counts()
        );
    }

    #[test]
    fn every_name_builds_and_unknown_names_do_not() {
        for name in NAMES {
            assert!(by_name(name, 8, 8).is_some(), "{name}");
        }
        assert!(by_name("nope", 8, 8).is_none());
        assert!(by_name("settled", 2, 8).is_none());
    }
}
