//! Heat conduction between neighbouring cells (section 6.7 of the design document).
//!
//! `k = min(conduct_a, conduct_b) × pair_mult`, `q = k × ΔT`, clamped to a quarter of
//! the smaller heat capacity times ΔT so that four neighbours together can never push
//! a cell past their temperatures. Energy moves as the same integer at both ends;
//! converting it back to a temperature rounds per cell, which the energy-ledger test
//! bounds.

use crate::fixed::scale_mag;
use crate::world::{Cell, CellOut, Grid, DIRS};

/// Heat capacity of a cell in Q16.16 J/K.
#[inline]
pub fn capacity(grid: &Grid, cell: &Cell) -> i64 {
    (cell.mass as i64 * grid.table.shc(cell.element)) >> 16
}

/// Conduction pass for one cell.
pub fn conduct_cell(grid: &Grid, x: usize, y: usize) -> CellOut {
    let c = grid.cell(x, y);
    let cap_c = capacity(grid, &c);
    if cap_c == 0 {
        return CellOut::keep(&c);
    }
    let k_c = grid.table.conduct(c.element);
    let mut dq: i128 = 0;
    let mut active = false;
    for dir in DIRS {
        let Some(n) = grid.neighbour(x, y, dir) else {
            continue;
        };
        let cap_n = capacity(grid, &n);
        if cap_n == 0 {
            continue;
        }
        let k =
            (k_c.min(grid.table.conduct(n.element)) * grid.table.pair_mult(c.phase, n.phase)) >> 16;
        if k == 0 {
            continue;
        }
        let dt = n.temp as i64 - c.temp as i64;
        let mag = dt.unsigned_abs();
        let q = scale_mag(mag, k);
        let cap_min = cap_c.min(cap_n) as u64;
        let q_max = match mag.checked_mul(cap_min) {
            Some(product) => (product / 4) >> 16,
            None => ((mag as u128 * cap_min as u128 / 4) >> 16) as u64,
        };
        let q = q.min(q_max);
        if q != 0 {
            active = true;
            dq += if dt > 0 { q as i128 } else { -(q as i128) };
        }
    }
    if !active {
        return CellOut::keep(&c);
    }
    // The 64-bit division covers every realistic cell; the wide path keeps it exact.
    let delta = match i64::try_from(dq << 16) {
        Ok(wide) => (wide / cap_c) as i128,
        Err(_) => (dq << 16) / cap_c as i128,
    };
    let temp = (c.temp as i128 + delta).clamp(i32::MIN as i128, i32::MAX as i128) as i32;
    CellOut {
        element: c.element,
        mass: c.mass,
        temp,
        flags: c.flags,
        active,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::{builtin, ElementTable, Phase};
    use crate::fixed::{Mass, Temp, ONE};
    use crate::world::World;

    #[test]
    fn heat_flows_from_hot_to_cold_with_equal_magnitude() {
        let table = ElementTable::builtin();
        let mut world = World::new(2, 1);
        world.set(
            &table,
            0,
            0,
            builtin::SANDSTONE,
            Mass::from_kg(10),
            Temp::from_kelvin(400),
        );
        world.set(
            &table,
            1,
            0,
            builtin::SANDSTONE,
            Mass::from_kg(10),
            Temp::from_kelvin(300),
        );
        let grid = Grid::new(&world, &table);
        let hot = conduct_cell(&grid, 0, 0);
        let cold = conduct_cell(&grid, 1, 0);
        assert!(hot.temp < Temp::from_kelvin(400).raw());
        assert!(cold.temp > Temp::from_kelvin(300).raw());
        let lost = Temp::from_kelvin(400).raw() - hot.temp;
        let gained = cold.temp - Temp::from_kelvin(300).raw();
        assert_eq!(lost, gained, "equal masses move equal temperature");
        assert!(hot.active && cold.active);
    }

    #[test]
    fn vacuum_neither_conducts_nor_changes() {
        let table = ElementTable::builtin();
        let mut world = World::new(2, 1);
        world.set(
            &table,
            0,
            0,
            builtin::SANDSTONE,
            Mass::from_kg(10),
            Temp::from_kelvin(400),
        );
        let grid = Grid::new(&world, &table);
        assert_eq!(conduct_cell(&grid, 0, 0), CellOut::keep(&grid.cell(0, 0)));
        assert_eq!(conduct_cell(&grid, 1, 0), CellOut::keep(&grid.cell(1, 0)));
    }

    #[test]
    fn clamp_never_overshoots_with_four_hot_neighbours() {
        let mut table = ElementTable::builtin();
        // Make conduction huge so the clamp is what limits the transfer.
        table.set_pair_mult(Phase::Solid, Phase::Solid, 1_000_000 * ONE);
        let mut world = World::new(3, 3);
        for y in 0..3 {
            for x in 0..3 {
                world.set(
                    &table,
                    x,
                    y,
                    builtin::SANDSTONE,
                    Mass::from_kg(1),
                    Temp::from_kelvin(500),
                );
            }
        }
        world.set(
            &table,
            1,
            1,
            builtin::SANDSTONE,
            Mass::from_kg(1),
            Temp::from_kelvin(100),
        );
        let grid = Grid::new(&world, &table);
        let centre = conduct_cell(&grid, 1, 1);
        assert!(centre.temp <= Temp::from_kelvin(500).raw());
        assert!(centre.temp > Temp::from_kelvin(100).raw());
        let side = conduct_cell(&grid, 1, 0);
        assert!(
            side.temp >= Temp::from_kelvin(400).raw(),
            "a neighbour gives at most a quarter of the gap"
        );
    }
}
