//! Heat conduction between neighbouring cells (section 6.7 of the design document).
//!
//! `k = min(conduct_a, conduct_b) × pair_mult`, `q = k × ΔT`, clamped to a quarter of
//! the smaller heat capacity times ΔT so that four neighbours together can never push
//! a cell past their temperatures. Energy moves as the same integer at both ends;
//! converting it back to a temperature rounds per cell, which the energy-ledger test
//! bounds.

use crate::element::{ElementId, ElementTable, Phase};
use crate::fixed::{scale_mag, Temp};
use crate::world::{Cell, CellOut, Grid, Stencil};

/// Half-width of the band around a melting or boiling point inside which a cell keeps
/// its phase (3 K), so a cell sitting on a threshold does not flip every tick.
pub const HYSTERESIS: i32 = Temp::from_kelvin(3).raw();

/// The element a cell of `element` at `temp` turns into, if it crosses a threshold by
/// more than [`HYSTERESIS`]. Mass is unchanged and there is no latent heat, as in the
/// reference game.
pub fn phase_change(table: &ElementTable, element: u16, temp: i32) -> Option<u16> {
    let def = table.get(ElementId(element));
    let target = match def.phase {
        Phase::Solid => def
            .melt
            .filter(|m| temp > m + HYSTERESIS)
            .and(def.liquid_of),
        Phase::Liquid => {
            if def.boil.is_some_and(|b| temp > b + HYSTERESIS) {
                def.gas_of
            } else if def.melt.is_some_and(|m| temp < m - HYSTERESIS) {
                def.solid_of
            } else {
                None
            }
        }
        Phase::Gas => def
            .boil
            .filter(|b| temp < b - HYSTERESIS)
            .and(def.liquid_of),
        Phase::Vacuum => None,
    };
    target.map(|id| id.0).filter(|id| *id != element)
}

/// Heat capacity of a cell in Q16.16 J/K.
#[inline]
pub fn capacity(grid: &Grid, cell: &Cell) -> i64 {
    (cell.mass as i64 * grid.table.shc(cell.element)) >> 16
}

/// Conduction pass for one cell.
pub fn conduct_cell(grid: &Grid, x: usize, y: usize) -> CellOut {
    let st = grid.stencil(x, y);
    let c = &st.cell;
    let cap_c = capacity(grid, c);
    if cap_c == 0 {
        return CellOut::keep(c);
    }
    let conducted = conduct(grid, &st, cap_c);
    let temp = conducted.unwrap_or(c.temp);
    match phase_change(grid.table, c.element, temp) {
        Some(element) => CellOut {
            element,
            mass: c.mass,
            temp,
            flags: 0,
            active: true,
            phase_changed: true,
        },
        None => match conducted {
            Some(temp) => CellOut {
                element: c.element,
                mass: c.mass,
                temp,
                flags: c.flags,
                active: true,
                phase_changed: false,
            },
            None => CellOut::keep(c),
        },
    }
}

/// One side of an exchange: heat capacity (Q16.16 J/K), conductivity (Q16.16 J/(K·tick)),
/// phase, and temperature.
#[derive(Clone, Copy, Debug)]
pub struct Body {
    pub cap: i64,
    pub conduct: i64,
    pub phase: Phase,
    pub temp: i32,
}

/// The energy (Q16.16 J) that flows from the warmer of two bodies to the cooler one in
/// one exchange: `min(conductivities) × pair_mult × |ΔT|`, clamped to a quarter of the
/// smaller capacity times `|ΔT|`. Zero when either body has no capacity. The one rule
/// for cells, pipe packets, and anything else that conducts.
pub fn pair_flow(table: &ElementTable, a: Body, b: Body) -> u64 {
    if a.cap <= 0 || b.cap <= 0 {
        return 0;
    }
    let dt = (a.temp as i64 - b.temp as i64).unsigned_abs();
    if dt == 0 {
        return 0;
    }
    let k = (a.conduct.min(b.conduct) * table.pair_mult(a.phase, b.phase)) >> 16;
    if k == 0 {
        return 0;
    }
    let q = scale_mag(dt, k);
    let cap_min = a.cap.min(b.cap) as u64;
    let q_max = match dt.checked_mul(cap_min) {
        Some(product) => (product / 4) >> 16,
        None => ((dt as u128 * cap_min as u128 / 4) >> 16) as u64,
    };
    q.min(q_max)
}

/// The cell's temperature after conduction, or `None` when nothing moved.
fn conduct(grid: &Grid, st: &Stencil, cap_c: i64) -> Option<i32> {
    let c = &st.cell;
    let body_c = Body {
        cap: cap_c,
        conduct: grid.table.conduct(c.element),
        phase: c.phase,
        temp: c.temp,
    };
    let mut dq: i128 = 0;
    let mut active = false;
    for n in st.iter() {
        let dt = n.temp as i64 - c.temp as i64;
        if dt == 0 {
            continue;
        }
        let body_n = Body {
            cap: capacity(grid, n),
            conduct: grid.table.conduct(n.element),
            phase: n.phase,
            temp: n.temp,
        };
        let q = pair_flow(grid.table, body_c, body_n);
        if q != 0 {
            active = true;
            dq += if dt > 0 { q as i128 } else { -(q as i128) };
        }
    }
    if !active {
        return None;
    }
    // The 64-bit division covers every realistic cell; the wide path keeps it exact.
    let delta = match i64::try_from(dq << 16) {
        Ok(wide) => (wide / cap_c) as i128,
        Err(_) => (dq << 16) / cap_c as i128,
    };
    Some((c.temp as i128 + delta).clamp(i32::MIN as i128, i32::MAX as i128) as i32)
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
    fn water_freezes_boils_and_keeps_its_phase_inside_the_hysteresis_band() {
        let table = ElementTable::builtin();
        let freeze = Temp::from_millikelvin(273_150).raw();
        let boil = Temp::from_millikelvin(373_150).raw();
        assert_eq!(
            phase_change(&table, builtin::WATER.0, freeze - HYSTERESIS - 1),
            Some(builtin::ICE.0)
        );
        assert_eq!(
            phase_change(&table, builtin::WATER.0, freeze - HYSTERESIS),
            None
        );
        assert_eq!(
            phase_change(&table, builtin::WATER.0, boil + HYSTERESIS + 1),
            Some(builtin::STEAM.0)
        );
        assert_eq!(
            phase_change(&table, builtin::ICE.0, freeze + HYSTERESIS + 1),
            Some(builtin::WATER.0)
        );
        assert_eq!(
            phase_change(&table, builtin::ICE.0, freeze + HYSTERESIS),
            None
        );
        assert_eq!(
            phase_change(&table, builtin::STEAM.0, boil - HYSTERESIS - 1),
            Some(builtin::WATER.0)
        );
        assert_eq!(
            phase_change(&table, builtin::STEAM.0, Temp::from_kelvin(500).raw()),
            None
        );
        assert_eq!(
            phase_change(&table, builtin::SANDSTONE.0, Temp::from_kelvin(5000).raw()),
            None
        );
    }

    #[test]
    fn a_cold_water_cell_freezes_in_the_heat_pass_without_any_conduction() {
        let table = ElementTable::builtin();
        let mut world = World::new(1, 1);
        world.set(
            &table,
            0,
            0,
            builtin::WATER,
            Mass::from_kg(500),
            Temp::from_kelvin(250),
        );
        let grid = Grid::new(&world, &table);
        let out = conduct_cell(&grid, 0, 0);
        assert_eq!(
            (out.element, out.mass, out.temp),
            (
                builtin::ICE.0,
                Mass::from_kg(500).raw(),
                Temp::from_kelvin(250).raw()
            )
        );
        assert!(out.active && out.phase_changed);
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
