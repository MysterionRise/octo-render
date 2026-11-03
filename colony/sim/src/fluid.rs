//! Fluid passes: buoyancy sorting and flux, for gases and liquids.
//!
//! One element per cell; pressure is mass. Both passes are gathers over the old buffer
//! and every exchange is computed identically by its two endpoints, so the world total
//! is exactly conserved (sections 6.5 and 6.6 of the design document).
//!
//! The sort pass swaps a denser fluid below a lighter one under mutual proposal (a
//! liquid also falls into vacuum), and records [`FLAG_PRESSED`] on gas cells that touch
//! a different gas with more than twice their regional mass, or a liquid that wants to
//! spread into them. The flux pass exchanges mass between same-element neighbours
//! (gases by pressure, liquids by level and gravity up to their `max_mass`), expands
//! fluids into vacuum, merges sub-floor residue, and evacuates pressed gas cells into
//! their own gas region. Every rule either moves mass toward equilibrium within one
//! element or removes a density inversion, so a closed world settles and stays settled.
//!
//! Each rule works on a [`Stencil`], the cell and its four neighbours loaded once; a
//! rule that has to look at a neighbour's neighbours loads that neighbour's stencil.

use crate::element::{ElementTable, Phase};
use crate::fixed::{scale_mag, Mass, ONE};
use crate::world::{Cell, CellOut, Dir, Grid, Stencil, DIRS, FLAG_PRESSED};

/// Fraction of a mass difference that crosses one cell boundary per tick (1/4 in
/// Q16.16). With four neighbours this bounds a cell's total outflow by its own mass,
/// so no cell ever goes negative.
pub const K_FLUX: i64 = ONE / 4;

/// A different gas with more than this many times a cell's regional mass presses the
/// cell, which then evacuates into its own region.
pub const PRESSURE_RATIO: u64 = 2;

/// A liquid cell below this mass (10 g) merges into a neighbour, as in the reference game.
pub const LIQUID_FLOOR: Mass = Mass::from_grams(10);

/// A liquid spreads sideways only from this mass (80 g), so a spread never leaves
/// either cell under the floor.
pub const LIQUID_SPREAD_MIN: Mass = Mass::from_grams(80);

/// Fraction of its mass a liquid cell can send in each direction per tick (1/4 in
/// Q16.16): sideways a quarter of the level difference, down a quarter of the cell,
/// up a quarter of its excess. The four flows can never exceed the cell's mass.
pub const K_LIQUID: i64 = ONE / 4;

/// A gas cell that takes part in exchanges (at or above the mass floor).
#[inline]
fn is_regular_gas(c: &Cell) -> bool {
    c.phase == Phase::Gas && c.mass >= Mass::FLOOR.0
}

/// A liquid cell that takes part in exchanges (at or above its floor).
#[inline]
fn is_regular_liquid(c: &Cell) -> bool {
    c.phase == Phase::Liquid && c.mass >= LIQUID_FLOOR.0
}

#[inline]
fn is_fluid(c: &Cell) -> bool {
    is_regular_gas(c) || is_regular_liquid(c)
}

#[inline]
fn is_pressed(c: &Cell) -> bool {
    c.flags & FLAG_PRESSED != 0
}

/// Mass above a liquid's `max_mass`, which pushes upward. An overfill under the liquid
/// floor is tolerated: pushing it up would only create sub-floor residue that merges
/// straight back down, a churn that never settles.
#[inline]
fn excess(table: &ElementTable, c: &Cell) -> u64 {
    let over = (c.mass as u64).saturating_sub(table.max_mass(c.element) as u64);
    if over >= LIQUID_FLOOR.0 as u64 {
        over
    } else {
        0
    }
}

/// Whether a regular gas cell is pressed: a different regular gas next to it has more
/// than [`PRESSURE_RATIO`] times the mean mass of the cell and its regular same-element
/// neighbours, or a liquid beside it wants to spread into it, or the liquid below it is
/// over its `max_mass`. Comparing gases against the regional mean rather than the cell
/// alone is what stops a freshly filled interface cell, which starts thin, from being
/// pushed straight back; the interface only moves while the regions themselves are out
/// of balance.
fn pressed_now(st: &Stencil, table: &ElementTable) -> bool {
    let c = &st.cell;
    if !is_regular_gas(c) {
        return false;
    }
    let mut sum = c.mass as u64;
    let mut count: u64 = 1;
    let mut heaviest_other: u64 = 0;
    for (i, n) in st.near.iter().enumerate() {
        let Some(n) = n else {
            continue;
        };
        if is_regular_gas(n) {
            if n.element == c.element {
                sum += n.mass as u64;
                count += 1;
            } else {
                heaviest_other = heaviest_other.max(n.mass as u64);
            }
        } else if is_regular_liquid(n) {
            let dir = DIRS[i];
            if (dir == Dir::Left || dir == Dir::Right) && n.mass >= LIQUID_SPREAD_MIN.0 {
                return true;
            }
            if dir == Dir::Down && excess(table, n) > 0 {
                return true;
            }
        }
    }
    heaviest_other * count > PRESSURE_RATIO * sum
}

/// Buoyancy proposal: sink below a lighter fluid (a liquid also sinks into vacuum),
/// otherwise rise above a denser one; a vacuum proposes the liquid above it. Each cell
/// proposes at most one partner, so no cell is in two swaps, and each swap removes one
/// inversion, so sorting terminates.
fn sort_proposal(st: &Stencil, table: &ElementTable) -> Option<Dir> {
    let c = &st.cell;
    if c.phase == Phase::Vacuum {
        return st
            .get(Dir::Up)
            .filter(|a| is_regular_liquid(a))
            .map(|_| Dir::Up);
    }
    if !is_fluid(c) {
        return None;
    }
    let density = table.density(c.element);
    if let Some(b) = st.get(Dir::Down) {
        if (is_fluid(b) && table.density(b.element) < density)
            || (b.phase == Phase::Vacuum && is_regular_liquid(c))
        {
            return Some(Dir::Down);
        }
    }
    if let Some(a) = st.get(Dir::Up) {
        if is_fluid(a) && table.density(a.element) > density {
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
    let flags = if pressed_now(&st, grid.table) {
        FLAG_PRESSED
    } else {
        0
    };
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
                phase_changed: false,
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
        if is_fluid(n)
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

/// How much a fluid neighbour in direction `dir` (seen from the vacuum) would send into
/// a vacuum cell this tick: gases a quarter of their mass from any side; liquids a
/// quarter of their mass from beside it, or their excess from below it. A liquid above
/// a vacuum falls by swapping in the sort pass instead.
#[inline]
fn vacuum_offer(table: &ElementTable, n: &Cell, dir: Dir) -> u64 {
    if is_regular_gas(n) {
        if is_pressed(n) {
            0
        } else {
            scale_mag(n.mass as u64, K_FLUX)
        }
    } else if is_regular_liquid(n) {
        match dir {
            Dir::Left | Dir::Right if n.mass >= LIQUID_SPREAD_MIN.0 => {
                scale_mag(n.mass as u64, K_LIQUID)
            }
            Dir::Down => excess(table, n).min(scale_mag(n.mass as u64, K_LIQUID)),
            _ => 0,
        }
    } else {
        0
    }
}

/// For a vacuum cell: the neighbour that expands into it this tick and the mass it
/// sends: the heaviest fluid with a non-zero offer, ties broken by priority order.
/// Pressed gases are skipped by their flag alone (one load) rather than by whether they
/// really evacuate, so every cell around the vacuum agrees on the winner without a
/// deeper stencil; a pressed cell that stays merely waits a tick before expanding.
fn vacuum_winner<'a>(st: &'a Stencil, table: &ElementTable) -> Option<(&'a Cell, u64)> {
    let mut best: Option<(&Cell, u64)> = None;
    for (i, n) in st.near.iter().enumerate() {
        let Some(n) = n else {
            continue;
        };
        let offer = vacuum_offer(table, n, DIRS[i]);
        if offer > 0 && best.is_none_or(|(b, _)| n.mass > b.mass) {
            best = Some((n, offer));
        }
    }
    best
}

/// Temperature of a cell after `m_in` of mass at a mass-weighted energy `e_in`
/// (sum of mass × temperature) joins `m0` of mass at `t0`. Rounds to nearest.
pub(crate) fn mix_temp(m0: u64, t0: i32, m_in: u64, e_in: i128) -> i32 {
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

/// Mass that flows from a liquid cell `c` to a same-element liquid neighbour `n` in
/// direction `dir` (positive) or from `n` to `c` (negative), computed identically from
/// either end. Sideways: a quarter of the level difference, never above the receiver's
/// `max_mass`. Down: up to a quarter of the upper cell, as much as the lower one can
/// take. Up: only the lower cell's excess over `max_mass`, up to a quarter of it. The
/// four budgets add up to the cell's mass, so a cell never goes negative.
fn liquid_flow(table: &ElementTable, c: &Cell, n: &Cell, dir: Dir) -> i64 {
    let max = table.max_mass(c.element) as u64;
    let (mc, mn) = (c.mass as u64, n.mass as u64);
    let quarter = |m: u64| scale_mag(m, K_LIQUID);
    match dir {
        Dir::Left | Dir::Right => {
            if mc > mn {
                quarter(mc - mn).min(max.saturating_sub(mn)) as i64
            } else if mn > mc {
                -(quarter(mn - mc).min(max.saturating_sub(mc)) as i64)
            } else {
                0
            }
        }
        // n is below c: c pours down what fits, or n pushes its excess up.
        Dir::Down => {
            if mn < max {
                quarter(mc).min(max - mn) as i64
            } else {
                -((mn - max).min(quarter(mn)) as i64)
            }
        }
        // n is above c: mirror of the case above.
        Dir::Up => {
            if mc < max {
                -(quarter(mn).min(max - mc) as i64)
            } else {
                (mc - max).min(quarter(mc)) as i64
            }
        }
    }
}

/// Flux pass. Solids are untouched.
pub fn flux_cell(grid: &Grid, x: usize, y: usize) -> CellOut {
    let st = grid.stencil(x, y);
    let table = grid.table;
    let c = &st.cell;
    match c.phase {
        Phase::Vacuum => {
            if let Some((w, offer)) = vacuum_winner(&st, table) {
                debug_assert!(offer <= u32::MAX as u64);
                return CellOut {
                    element: w.element,
                    mass: offer as u32,
                    temp: w.temp,
                    flags: 0,
                    active: true,
                    phase_changed: false,
                };
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
        Phase::Liquid if c.mass < LIQUID_FLOOR.0 => {
            if c.mass == 0 || merge_target(&st).is_some() {
                CellOut::VACUUM
            } else {
                CellOut::keep(c)
            }
        }
        Phase::Gas | Phase::Liquid => {
            let liquid = c.phase == Phase::Liquid;
            if !liquid && is_pressed(c) && merge_target(&st).is_some() {
                return CellOut::VACUUM;
            }
            let floor = if liquid {
                LIQUID_FLOOR.0
            } else {
                Mass::FLOOR.0
            };
            let mut out: u64 = 0;
            let mut inflow: u64 = 0;
            let mut energy_in: i128 = 0;
            let mut active = false;
            for (i, n) in st.near.iter().enumerate() {
                let Some(n) = n else {
                    continue;
                };
                let dir = DIRS[i];
                if n.phase == c.phase && n.element == c.element {
                    if n.mass >= floor {
                        if !liquid && is_pressed(n) {
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
                        let flow = if liquid {
                            liquid_flow(table, c, n, dir)
                        } else if c.mass > n.mass {
                            scale_mag((c.mass - n.mass) as u64, K_FLUX) as i64
                        } else {
                            -(scale_mag((n.mass - c.mass) as u64, K_FLUX) as i64)
                        };
                        if flow > 0 {
                            out += flow as u64;
                            active = true;
                        } else if flow < 0 {
                            let f = flow.unsigned_abs();
                            inflow += f;
                            energy_in += f as i128 * n.temp as i128;
                            active = true;
                        }
                    } else if n.mass > 0 && is_me(merge_target(&grid.stencil(n.x, n.y)), x, y) {
                        inflow += n.mass as u64;
                        energy_in += n.mass as i128 * n.temp as i128;
                        active = true;
                    }
                } else if n.phase == Phase::Vacuum {
                    let around = grid.stencil(n.x, n.y);
                    if let Some((w, offer)) = vacuum_winner(&around, table) {
                        if w.x == x && w.y == y {
                            out += offer;
                            active = true;
                        }
                    }
                }
            }
            let remaining = (c.mass as u64)
                .checked_sub(out)
                .expect("a cell's outflow exceeded its mass: a flow rule broke its budget");
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
                phase_changed: false,
            }
        }
        Phase::Solid => CellOut::keep(c),
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

    #[test]
    fn liquid_falls_through_gas_and_into_vacuum() {
        let (table, mut world) = grid_world(1, 3);
        world.set(
            &table,
            0,
            1,
            builtin::OXYGEN,
            Mass::from_kg(1),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            0,
            2,
            builtin::WATER,
            Mass::from_kg(500),
            Temp::from_kelvin(290),
        );
        let grid = Grid::new(&world, &table);
        let middle = sort_cell(&grid, 0, 1);
        let top = sort_cell(&grid, 0, 2);
        assert_eq!(
            (middle.element, middle.mass),
            (builtin::WATER.0, Mass::from_kg(500).raw())
        );
        assert_eq!(
            (top.element, top.mass),
            (builtin::OXYGEN.0, Mass::from_kg(1).raw())
        );
        assert_eq!(
            sort_cell(&grid, 0, 0),
            CellOut::keep(&grid.cell(0, 0)),
            "vacuum below gas stays"
        );
        // Now water sits above the vacuum: they swap too.
        let mut world = World::new(1, 2);
        world.set(
            &table,
            0,
            1,
            builtin::WATER,
            Mass::from_kg(500),
            Temp::from_kelvin(290),
        );
        let grid = Grid::new(&world, &table);
        assert_eq!(sort_cell(&grid, 0, 0).element, builtin::WATER.0);
        assert_eq!(sort_cell(&grid, 0, 1), CellOut::VACUUM);
    }

    #[test]
    fn water_pours_down_until_the_lower_cell_is_full() {
        let (table, mut world) = grid_world(1, 2);
        world.set(
            &table,
            0,
            0,
            builtin::WATER,
            Mass::from_kg(300),
            Temp::from_kelvin(290),
        );
        world.set(
            &table,
            0,
            1,
            builtin::WATER,
            Mass::from_kg(900),
            Temp::from_kelvin(290),
        );
        let grid = Grid::new(&world, &table);
        let lower = flux_cell(&grid, 0, 0);
        let upper = flux_cell(&grid, 0, 1);
        assert_eq!(
            lower.mass,
            Mass::from_kg(525).raw(),
            "a quarter of the upper cell pours"
        );
        assert_eq!(upper.mass, Mass::from_kg(675).raw());
        assert!(lower.active && upper.active);
    }

    #[test]
    fn over_full_water_pushes_its_excess_up() {
        let (table, mut world) = grid_world(1, 2);
        world.set(
            &table,
            0,
            0,
            builtin::WATER,
            Mass::from_kg(1200),
            Temp::from_kelvin(290),
        );
        world.set(
            &table,
            0,
            1,
            builtin::WATER,
            Mass::from_kg(100),
            Temp::from_kelvin(290),
        );
        let grid = Grid::new(&world, &table);
        assert_eq!(flux_cell(&grid, 0, 0).mass, Mass::from_kg(1000).raw());
        assert_eq!(flux_cell(&grid, 0, 1).mass, Mass::from_kg(300).raw());
        let mut world = World::new(1, 2);
        world.set(
            &table,
            0,
            0,
            builtin::WATER,
            Mass::from_kg(2000),
            Temp::from_kelvin(290),
        );
        world.set(
            &table,
            0,
            1,
            builtin::WATER,
            Mass::from_kg(100),
            Temp::from_kelvin(290),
        );
        let grid = Grid::new(&world, &table);
        assert_eq!(
            flux_cell(&grid, 0, 0).mass,
            Mass::from_kg(1500).raw(),
            "at most a quarter pushes up per tick"
        );
    }

    #[test]
    fn water_levels_sideways_and_spreads_into_vacuum() {
        let (table, mut world) = grid_world(3, 1);
        world.set(
            &table,
            1,
            0,
            builtin::WATER,
            Mass::from_kg(800),
            Temp::from_kelvin(290),
        );
        world.set(
            &table,
            2,
            0,
            builtin::WATER,
            Mass::from_kg(400),
            Temp::from_kelvin(290),
        );
        let grid = Grid::new(&world, &table);
        let left = flux_cell(&grid, 0, 0);
        let middle = flux_cell(&grid, 1, 0);
        let right = flux_cell(&grid, 2, 0);
        // a quarter of 800 kg spreads into the vacuum, a quarter of the 400 kg difference levels right
        assert_eq!(
            (left.element, left.mass),
            (builtin::WATER.0, Mass::from_kg(200).raw())
        );
        assert_eq!(middle.mass, Mass::from_kg(500).raw());
        assert_eq!(right.mass, Mass::from_kg(500).raw());
    }

    #[test]
    fn gas_beside_water_is_pressed_and_evacuates() {
        let (table, mut world) = grid_world(3, 1);
        world.set(
            &table,
            0,
            0,
            builtin::WATER,
            Mass::from_kg(600),
            Temp::from_kelvin(290),
        );
        world.set(
            &table,
            1,
            0,
            builtin::OXYGEN,
            Mass::from_kg(1),
            Temp::from_kelvin(300),
        );
        world.set(
            &table,
            2,
            0,
            builtin::OXYGEN,
            Mass::from_kg(1),
            Temp::from_kelvin(300),
        );
        apply_sort(&table, &mut world);
        assert_eq!(world.cells.flags, vec![0, FLAG_PRESSED, 0]);
        let grid = Grid::new(&world, &table);
        assert_eq!(flux_cell(&grid, 1, 0), CellOut::VACUUM);
        assert_eq!(flux_cell(&grid, 2, 0).mass, Mass::from_kg(2).raw());
        assert_eq!(
            flux_cell(&grid, 0, 0),
            CellOut::keep(&grid.cell(0, 0)),
            "water waits for the vacuum"
        );
    }

    #[test]
    fn thin_water_does_not_spread_and_sub_floor_water_merges() {
        let (table, mut world) = grid_world(3, 1);
        world.set(
            &table,
            0,
            0,
            builtin::WATER,
            Mass::from_grams(30),
            Temp::from_kelvin(290),
        );
        world.set(
            &table,
            1,
            0,
            builtin::WATER,
            Mass::from_grams(5),
            Temp::from_kelvin(290),
        );
        let grid = Grid::new(&world, &table);
        assert_eq!(flux_cell(&grid, 1, 0), CellOut::VACUUM);
        assert_eq!(
            flux_cell(&grid, 0, 0).mass,
            Mass::from_grams(30).raw() + Mass::from_grams(5).raw()
        );
        assert_eq!(
            flux_cell(&grid, 2, 0),
            CellOut::keep(&grid.cell(2, 0)),
            "30 g is below the spread minimum"
        );
    }
}
