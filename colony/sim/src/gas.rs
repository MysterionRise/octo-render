//! Gas passes: buoyancy sorting and flux.
//!
//! One gas per cell; pressure is mass. Both passes are gathers over the old buffer and
//! every exchange is computed identically by its two endpoints, so the world total is
//! exactly conserved (section 6.5 of the design document).
//!
//! The sort pass swaps a denser gas below a lighter one under mutual proposal, and
//! records [`FLAG_PRESSED`] on gas cells that touch a different gas with more than
//! twice their regional mass. The flux pass exchanges mass between same-element
//! neighbours, expands gas into vacuum, merges sub-floor residue, and evacuates pressed
//! cells into their own gas region. Every rule either moves mass toward equal pressure
//! within one gas or removes a density inversion, so a closed world settles and stays
//! settled.
//!
//! Each rule works on a [`Stencil`], the cell and its four neighbours loaded once; a
//! rule that has to look at a neighbour's neighbours loads that neighbour's stencil.

use crate::element::{ElementTable, Phase};
use crate::fixed::{scale_mag, Mass, ONE};
use crate::world::{Cell, CellOut, Dir, Grid, Stencil, FLAG_PRESSED};

/// Fraction of a mass difference that crosses one cell boundary per tick (1/4 in
/// Q16.16). With four neighbours this bounds a cell's total outflow by its own mass,
/// so no cell ever goes negative.
pub const K_FLUX: i64 = ONE / 4;

/// A different gas with more than this many times a cell's regional mass presses the
/// cell, which then evacuates into its own region.
pub const PRESSURE_RATIO: u64 = 2;

/// A gas cell that takes part in exchanges (at or above the mass floor).
#[inline]
fn is_regular_gas(c: &Cell) -> bool {
    c.phase == Phase::Gas && c.mass >= Mass::FLOOR.0
}

#[inline]
fn is_pressed(c: &Cell) -> bool {
    c.flags & FLAG_PRESSED != 0
}

/// Whether a different regular gas presses a regular gas cell: its mass exceeds
/// [`PRESSURE_RATIO`] times the mean mass of the cell and its regular same-element
/// neighbours. Comparing against that regional mean rather than the cell alone is what
/// stops a freshly filled interface cell, which starts thin, from being pushed straight
/// back; the interface only moves while the regions themselves are out of balance.
fn pressed_now(st: &Stencil) -> bool {
    let c = &st.cell;
    if !is_regular_gas(c) {
        return false;
    }
    let mut sum = c.mass as u64;
    let mut count: u64 = 1;
    let mut heaviest_other: u64 = 0;
    for n in st.iter() {
        if !is_regular_gas(n) {
            continue;
        }
        if n.element == c.element {
            sum += n.mass as u64;
            count += 1;
        } else {
            heaviest_other = heaviest_other.max(n.mass as u64);
        }
    }
    heaviest_other * count > PRESSURE_RATIO * sum
}

/// Buoyancy proposal: sink below a lighter gas, otherwise rise above a denser one.
/// Each cell proposes at most one partner, so no cell is in two swaps, and each swap
/// removes one density inversion, so sorting terminates.
fn sort_proposal(st: &Stencil, table: &ElementTable) -> Option<Dir> {
    let c = &st.cell;
    if !is_regular_gas(c) {
        return None;
    }
    let density = table.density(c.element);
    if let Some(b) = st.get(Dir::Down) {
        if is_regular_gas(b) && table.density(b.element) < density {
            return Some(Dir::Down);
        }
    }
    if let Some(a) = st.get(Dir::Up) {
        if is_regular_gas(a) && table.density(a.element) > density {
            return Some(Dir::Up);
        }
    }
    None
}

/// Sort pass: a cell takes its partner's state when the two cells propose each other,
/// otherwise it keeps its own. Either way it records whether it is pressed, which the
/// flux pass reads; for a cell that just swapped, the flag describes the old occupant
/// and is one tick stale, which only delays an evacuation.
pub fn sort_cell(grid: &Grid, x: usize, y: usize) -> CellOut {
    let st = grid.stencil(x, y);
    let flags = if pressed_now(&st) { FLAG_PRESSED } else { 0 };
    if let Some(dir) = sort_proposal(&st, grid.table) {
        let p = st.get(dir).expect("a proposal names an existing neighbour");
        let partner = grid.stencil(p.x, p.y);
        if sort_proposal(&partner, grid.table) == Some(dir.opposite()) {
            return CellOut {
                element: p.element,
                mass: p.mass,
                temp: p.temp,
                flags,
                active: true,
            };
        }
    }
    CellOut {
        flags,
        ..CellOut::keep(&st.cell)
    }
}

/// The regular, unpressed, same-element neighbour with the most mass (ties by priority
/// order): where sub-floor residue and evacuations go. Pressed cells never receive, so
/// two pressed cells cannot evacuate into each other.
fn merge_target(st: &Stencil) -> Option<&Cell> {
    let mut best: Option<&Cell> = None;
    for n in st.iter() {
        if is_regular_gas(n)
            && !is_pressed(n)
            && n.element == st.cell.element
            && best.is_none_or(|b| n.mass > b.mass)
        {
            best = Some(n);
        }
    }
    best
}

/// Whether a pressed regular gas cell leaves this tick, and where its mass goes.
#[inline]
fn evacuation(st: &Stencil) -> Option<&Cell> {
    if is_regular_gas(&st.cell) && is_pressed(&st.cell) {
        merge_target(st)
    } else {
        None
    }
}

/// For a vacuum cell: the regular-gas neighbour that expands into it this tick, which
/// is the heaviest one that is not pressed, ties broken by priority order. Pressed
/// cells are skipped by their flag alone (one load) rather than by whether they really
/// evacuate, so every cell around the vacuum agrees on the winner without a deeper
/// stencil; a pressed cell that stays merely waits a tick before expanding.
fn vacuum_winner(st: &Stencil) -> Option<&Cell> {
    let mut best: Option<&Cell> = None;
    for n in st.iter() {
        if is_regular_gas(n) && !is_pressed(n) && best.is_none_or(|b| n.mass > b.mass) {
            best = Some(n);
        }
    }
    best
}

/// Temperature of a cell after `m_in` of mass at a mass-weighted energy `e_in`
/// (sum of mass × temperature) joins `m0` of mass at `t0`. Rounds to nearest.
fn mix_temp(m0: u64, t0: i32, m_in: u64, e_in: i128) -> i32 {
    let total = (m0 + m_in) as i128;
    debug_assert!(total > 0);
    let num = m0 as i128 * t0 as i128 + e_in;
    let rounded = 2 * num + total;
    // Realistic masses and temperatures fit a 64-bit division, which is several times
    // cheaper than the 128-bit one; the wide path keeps the result exact otherwise.
    match (i64::try_from(rounded), i64::try_from(2 * total)) {
        (Ok(r), Ok(t)) => r.div_euclid(t) as i32,
        _ => rounded.div_euclid(2 * total) as i32,
    }
}

#[inline]
fn is_me(target: Option<&Cell>, x: usize, y: usize) -> bool {
    target.is_some_and(|t| t.x == x && t.y == y)
}

/// Flux pass. Liquids and solids are untouched.
pub fn flux_cell(grid: &Grid, x: usize, y: usize) -> CellOut {
    let st = grid.stencil(x, y);
    let c = &st.cell;
    match c.phase {
        Phase::Vacuum => {
            if let Some(w) = vacuum_winner(&st) {
                let f = scale_mag(w.mass as u64, K_FLUX) as u32;
                if f > 0 {
                    return CellOut {
                        element: w.element,
                        mass: f,
                        temp: w.temp,
                        flags: 0,
                        active: true,
                    };
                }
            }
            CellOut::keep(c)
        }
        Phase::Gas if c.mass < Mass::FLOOR.0 => {
            if c.mass == 0 || merge_target(&st).is_some() {
                CellOut::VACUUM
            } else {
                CellOut::keep(c)
            }
        }
        Phase::Gas => {
            if is_pressed(c) && merge_target(&st).is_some() {
                return CellOut::VACUUM;
            }
            let mut out: u64 = 0;
            let mut inflow: u64 = 0;
            let mut energy_in: i128 = 0;
            let mut active = false;
            for n in st.iter() {
                if n.phase == Phase::Gas && n.element == c.element {
                    if n.mass >= Mass::FLOOR.0 {
                        if is_pressed(n) {
                            let around = grid.stencil(n.x, n.y);
                            if let Some(t) = evacuation(&around) {
                                // n leaves this tick; it exchanges only with its target.
                                if t.x == x && t.y == y {
                                    inflow += n.mass as u64;
                                    energy_in += n.mass as i128 * n.temp as i128;
                                    active = true;
                                }
                                continue;
                            }
                        }
                        if c.mass > n.mass {
                            let f = scale_mag((c.mass - n.mass) as u64, K_FLUX);
                            out += f;
                            active |= f > 0;
                        } else if n.mass > c.mass {
                            let f = scale_mag((n.mass - c.mass) as u64, K_FLUX);
                            inflow += f;
                            energy_in += f as i128 * n.temp as i128;
                            active |= f > 0;
                        }
                    } else if n.mass > 0 && is_me(merge_target(&grid.stencil(n.x, n.y)), x, y) {
                        inflow += n.mass as u64;
                        energy_in += n.mass as i128 * n.temp as i128;
                        active = true;
                    }
                } else if n.phase == Phase::Vacuum
                    && is_me(vacuum_winner(&grid.stencil(n.x, n.y)), x, y)
                {
                    let f = scale_mag(c.mass as u64, K_FLUX);
                    out += f;
                    active |= f > 0;
                }
            }
            debug_assert!(out <= c.mass as u64, "outflow exceeds the cell's mass");
            let remaining = c.mass as u64 - out;
            let mass = remaining + inflow;
            assert!(
                mass <= u32::MAX as u64,
                "cell mass overflow: keep content masses below 13,000 kg per cell"
            );
            if mass == 0 {
                return CellOut::VACUUM;
            }
            let temp = if inflow == 0 {
                c.temp
            } else {
                mix_temp(remaining, c.temp, inflow, energy_in)
            };
            CellOut {
                element: c.element,
                mass: mass as u32,
                temp,
                flags: c.flags,
                active,
            }
        }
        Phase::Liquid | Phase::Solid => CellOut::keep(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::{builtin, ElementTable};
    use crate::fixed::Temp;
    use crate::world::World;

    fn grid_world(width: usize, height: usize) -> (ElementTable, World) {
        (ElementTable::builtin(), World::new(width, height))
    }

    /// Runs the sort pass over a tiny world and writes the result back, so flux tests
    /// see the flags a real tick would.
    fn apply_sort(table: &ElementTable, world: &mut World) {
        let grid = Grid::new(world, table);
        let outs: Vec<CellOut> = (0..world.height)
            .flat_map(|y| (0..world.width).map(move |x| (x, y)))
            .map(|(x, y)| sort_cell(&grid, x, y))
            .collect();
        for (i, o) in outs.into_iter().enumerate() {
            world.cells.element[i] = o.element;
            world.cells.mass[i] = o.mass;
            world.cells.temp[i] = o.temp;
            world.cells.flags[i] = o.flags;
        }
    }

    #[test]
    fn equal_neighbours_exchange_nothing() {
        let (table, mut world) = grid_world(2, 1);
        for x in 0..2 {
            world.set(
                &table,
                x,
                0,
                builtin::OXYGEN,
                Mass::from_kg(1),
                Temp::from_kelvin(300),
            );
        }
        let grid = Grid::new(&world, &table);
        let out = flux_cell(&grid, 0, 0);
        assert_eq!(out, CellOut::keep(&grid.cell(0, 0)));
        assert!(!out.active);
    }

    #[test]
    fn quarter_of_the_difference_moves_and_both_ends_agree() {
        let (table, mut world) = grid_world(2, 1);
        world.set(
            &table,
            0,
            0,
            builtin::OXYGEN,
            Mass::from_kg(2),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            1,
            0,
            builtin::OXYGEN,
            Mass::from_kg(1),
            Temp::from_kelvin(300),
        );
        let grid = Grid::new(&world, &table);
        let a = flux_cell(&grid, 0, 0);
        let b = flux_cell(&grid, 1, 0);
        let quarter = Mass::from_kg(1).raw() / 4;
        assert_eq!(a.mass, Mass::from_kg(2).raw() - quarter);
        assert_eq!(b.mass, Mass::from_kg(1).raw() + quarter);
        assert!(a.active && b.active);
    }

    #[test]
    fn inflow_mixes_temperature_by_mass() {
        let (table, mut world) = grid_world(2, 1);
        world.set(
            &table,
            0,
            0,
            builtin::OXYGEN,
            Mass::from_kg(2),
            Temp::from_kelvin(400),
        );
        world.set(
            &table,
            1,
            0,
            builtin::OXYGEN,
            Mass::from_kg(1),
            Temp::from_kelvin(300),
        );
        let grid = Grid::new(&world, &table);
        let a = flux_cell(&grid, 0, 0);
        let b = flux_cell(&grid, 1, 0);
        assert_eq!(
            a.temp,
            Temp::from_kelvin(400).raw(),
            "outflow does not change the sender's temperature"
        );
        // 1 kg at 300 K receives 0.25 kg at 400 K: (300 + 100) / 1.25 = 320 K
        assert_eq!(b.temp, Temp::from_kelvin(320).raw());
    }

    #[test]
    fn vacuum_takes_a_quarter_from_its_heaviest_neighbour_only() {
        let (table, mut world) = grid_world(3, 1);
        world.set(
            &table,
            0,
            0,
            builtin::OXYGEN,
            Mass::from_kg(4),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            2,
            0,
            builtin::CARBON_DIOXIDE,
            Mass::from_kg(8),
            Temp::from_kelvin(350),
        );
        let grid = Grid::new(&world, &table);
        let left = flux_cell(&grid, 0, 0);
        let middle = flux_cell(&grid, 1, 0);
        let right = flux_cell(&grid, 2, 0);
        assert_eq!(middle.element, builtin::CARBON_DIOXIDE.0);
        assert_eq!(middle.mass, Mass::from_kg(2).raw());
        assert_eq!(middle.temp, Temp::from_kelvin(350).raw());
        assert_eq!(right.mass, Mass::from_kg(6).raw());
        assert_eq!(
            left.mass,
            Mass::from_kg(4).raw(),
            "the loser does not expand"
        );
        assert!(!left.active);
    }

    #[test]
    fn denser_gas_sinks_below_lighter_gas() {
        let (table, mut world) = grid_world(1, 3);
        world.set(
            &table,
            0,
            0,
            builtin::OXYGEN,
            Mass::from_kg(1),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            0,
            1,
            builtin::CARBON_DIOXIDE,
            Mass::from_kg(1),
            Temp::from_kelvin(310),
        );
        world.set(
            &table,
            0,
            2,
            builtin::OXYGEN,
            Mass::from_kg(1),
            Temp::from_kelvin(320),
        );
        let grid = Grid::new(&world, &table);
        let bottom = sort_cell(&grid, 0, 0);
        let middle = sort_cell(&grid, 0, 1);
        let top = sort_cell(&grid, 0, 2);
        assert_eq!(
            (bottom.element, bottom.temp),
            (builtin::CARBON_DIOXIDE.0, Temp::from_kelvin(310).raw())
        );
        assert_eq!(
            (middle.element, middle.temp),
            (builtin::OXYGEN.0, Temp::from_kelvin(300).raw())
        );
        assert!(bottom.active && middle.active);
        assert_eq!(
            top,
            CellOut::keep(&grid.cell(0, 2)),
            "the top cell has nothing denser above it"
        );
    }

    #[test]
    fn sorted_layers_do_not_swap_and_settle() {
        let (table, mut world) = grid_world(2, 2);
        for x in 0..2 {
            world.set(
                &table,
                x,
                0,
                builtin::CARBON_DIOXIDE,
                Mass::from_kg(1),
                Temp::from_kelvin(300),
            );
            world.set(
                &table,
                x,
                1,
                builtin::OXYGEN,
                Mass::from_kg(1),
                Temp::from_kelvin(300),
            );
        }
        let grid = Grid::new(&world, &table);
        for y in 0..2 {
            for x in 0..2 {
                let out = sort_cell(&grid, x, y);
                assert_eq!(out, CellOut::keep(&grid.cell(x, y)));
                assert!(!out.active);
            }
        }
    }

    #[test]
    fn pressed_gas_evacuates_into_its_own_region() {
        // 4 kg oxygen | 0.5 kg carbon dioxide | 0.5 kg carbon dioxide
        let (table, mut world) = grid_world(3, 1);
        world.set(
            &table,
            0,
            0,
            builtin::OXYGEN,
            Mass::from_kg(4),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            1,
            0,
            builtin::CARBON_DIOXIDE,
            Mass::from_grams(500),
            Temp::from_kelvin(320),
        );
        world.set(
            &table,
            2,
            0,
            builtin::CARBON_DIOXIDE,
            Mass::from_grams(500),
            Temp::from_kelvin(340),
        );
        apply_sort(&table, &mut world);
        assert_eq!(world.cells.flags, vec![0, FLAG_PRESSED, 0]);
        let grid = Grid::new(&world, &table);
        let left = flux_cell(&grid, 0, 0);
        let middle = flux_cell(&grid, 1, 0);
        let right = flux_cell(&grid, 2, 0);
        assert_eq!(middle, CellOut::VACUUM);
        assert_eq!(right.mass, Mass::from_grams(500).raw() * 2);
        assert_eq!(right.temp, Temp::from_kelvin(330).raw());
        assert_eq!(
            left,
            CellOut::keep(&grid.cell(0, 0)),
            "oxygen expands only once the cell is vacuum"
        );
    }

    #[test]
    fn pressed_gas_without_a_same_element_neighbour_stays_and_fluxes_normally() {
        let (table, mut world) = grid_world(2, 1);
        world.set(
            &table,
            0,
            0,
            builtin::OXYGEN,
            Mass::from_kg(4),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            1,
            0,
            builtin::CARBON_DIOXIDE,
            Mass::from_grams(500),
            Temp::from_kelvin(320),
        );
        apply_sort(&table, &mut world);
        assert_eq!(world.cells.flags[1], FLAG_PRESSED);
        let grid = Grid::new(&world, &table);
        assert_eq!(flux_cell(&grid, 1, 0), CellOut::keep(&grid.cell(1, 0)));
        assert_eq!(flux_cell(&grid, 0, 0), CellOut::keep(&grid.cell(0, 0)));
    }

    #[test]
    fn two_pressed_cells_cannot_evacuate_into_each_other() {
        // oxygen 4 kg | co2 0.5 | co2 0.5 | oxygen 4 kg: both middle cells are pressed
        // and neither has an unpressed target, so both stay.
        let (table, mut world) = grid_world(4, 1);
        world.set(
            &table,
            0,
            0,
            builtin::OXYGEN,
            Mass::from_kg(4),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            1,
            0,
            builtin::CARBON_DIOXIDE,
            Mass::from_grams(500),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            2,
            0,
            builtin::CARBON_DIOXIDE,
            Mass::from_grams(500),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            3,
            0,
            builtin::OXYGEN,
            Mass::from_kg(4),
            Temp::from_kelvin(300),
        );
        apply_sort(&table, &mut world);
        assert_eq!(world.cells.flags, vec![0, FLAG_PRESSED, FLAG_PRESSED, 0]);
        let grid = Grid::new(&world, &table);
        let total: u64 = (0..4).map(|x| flux_cell(&grid, x, 0).mass as u64).sum();
        assert_eq!(
            total,
            2 * Mass::from_kg(4).raw() as u64 + 2 * Mass::from_grams(500).raw() as u64
        );
        assert_eq!(flux_cell(&grid, 1, 0), CellOut::keep(&grid.cell(1, 0)));
    }

    #[test]
    fn vacuum_ignores_an_evacuating_neighbour() {
        // vacuum | co2 0.5 (pressed by the 4 kg oxygen) | oxygen 4 kg, with co2 above
        // the pressed cell as its evacuation target.
        let (table, mut world) = grid_world(3, 2);
        world.set(
            &table,
            1,
            0,
            builtin::CARBON_DIOXIDE,
            Mass::from_grams(500),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            2,
            0,
            builtin::OXYGEN,
            Mass::from_kg(4),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            1,
            1,
            builtin::CARBON_DIOXIDE,
            Mass::from_kg(1),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            0,
            1,
            builtin::SANDSTONE,
            Mass::from_kg(100),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            2,
            1,
            builtin::SANDSTONE,
            Mass::from_kg(100),
            Temp::from_kelvin(300),
        );
        apply_sort(&table, &mut world);
        let grid = Grid::new(&world, &table);
        assert_eq!(flux_cell(&grid, 1, 0), CellOut::VACUUM);
        let vacuum = flux_cell(&grid, 0, 0);
        assert_eq!(
            vacuum,
            CellOut::keep(&grid.cell(0, 0)),
            "nothing expands into the vacuum this tick"
        );
        let above = flux_cell(&grid, 1, 1);
        assert_eq!(
            above.mass,
            Mass::from_kg(1).raw() + Mass::from_grams(500).raw()
        );
    }

    #[test]
    fn a_thin_cell_backed_by_a_dense_region_is_not_pressed() {
        // oxygen 1.5 kg | oxygen 0.5 kg (fresh) | carbon dioxide 1.5 kg: the middle cell
        // alone is under a third of its neighbour, but its region averages 1 kg.
        let (table, mut world) = grid_world(3, 1);
        world.set(
            &table,
            0,
            0,
            builtin::OXYGEN,
            Mass::from_grams(1500),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            1,
            0,
            builtin::OXYGEN,
            Mass::from_grams(500),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            2,
            0,
            builtin::CARBON_DIOXIDE,
            Mass::from_grams(1500),
            Temp::from_kelvin(300),
        );
        apply_sort(&table, &mut world);
        assert_eq!(world.cells.flags, vec![0, 0, 0]);
    }

    #[test]
    fn sub_floor_gas_merges_into_its_largest_regular_neighbour() {
        let (table, mut world) = grid_world(3, 1);
        world.set(
            &table,
            0,
            0,
            builtin::OXYGEN,
            Mass::from_kg(1),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            1,
            0,
            builtin::OXYGEN,
            Mass(10),
            Temp::from_kelvin(500),
        );
        world.set(
            &table,
            2,
            0,
            builtin::OXYGEN,
            Mass::from_kg(2),
            Temp::from_kelvin(300),
        );
        let grid = Grid::new(&world, &table);
        let left = flux_cell(&grid, 0, 0);
        let middle = flux_cell(&grid, 1, 0);
        let right = flux_cell(&grid, 2, 0);
        assert_eq!(middle, CellOut::VACUUM);
        assert_eq!(left.mass, Mass::from_kg(1).raw());
        assert_eq!(right.mass, Mass::from_kg(2).raw() + 10);
        assert!(right.active && !left.active);
    }

    #[test]
    fn sub_floor_gas_without_a_regular_neighbour_stays() {
        let (table, mut world) = grid_world(2, 1);
        world.set(
            &table,
            0,
            0,
            builtin::OXYGEN,
            Mass(10),
            Temp::from_kelvin(300),
        );
        let grid = Grid::new(&world, &table);
        let out = flux_cell(&grid, 0, 0);
        assert_eq!(out, CellOut::keep(&grid.cell(0, 0)));
        assert_eq!(flux_cell(&grid, 1, 0), CellOut::keep(&grid.cell(1, 0)));
    }
}
