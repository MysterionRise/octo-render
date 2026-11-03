//! Cell storage: flat, row-major structure-of-arrays buffers over the whole world.
//!
//! Row 0 is the bottom of the world, so "down" is `y - 1`. Neighbour access is
//! `i ± 1` and `i ± width`; chunks exist only for scheduling (see [`crate::chunk`]).

use crate::element::{ElementId, ElementTable, Phase};
use crate::fixed::{Mass, Temp};

/// Set on a regular gas cell that touches a different gas with more than twice its
/// mass. Written by the sort pass, read by the flux pass (pressure evacuation).
pub const FLAG_PRESSED: u8 = 1;

/// The hot per-cell arrays. Element 0 is vacuum, which always has zero mass, zero
/// temperature, and zero flags so that identical worlds hash identically.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellBuffers {
    pub element: Vec<u16>,
    pub mass: Vec<u32>,
    pub temp: Vec<i32>,
    pub flags: Vec<u8>,
}

impl CellBuffers {
    pub fn new(len: usize) -> CellBuffers {
        CellBuffers {
            element: vec![0; len],
            mass: vec![0; len],
            temp: vec![0; len],
            flags: vec![0; len],
        }
    }

    pub fn len(&self) -> usize {
        self.element.len()
    }

    pub fn is_empty(&self) -> bool {
        self.element.is_empty()
    }
}

/// A finite world of cells.
#[derive(Clone, Debug)]
pub struct World {
    pub width: usize,
    pub height: usize,
    pub cells: CellBuffers,
}

impl World {
    pub fn new(width: usize, height: usize) -> World {
        assert!(width > 0 && height > 0, "world dimensions must be positive");
        World {
            width,
            height,
            cells: CellBuffers::new(width * height),
        }
    }

    #[inline]
    pub fn index(&self, x: usize, y: usize) -> usize {
        debug_assert!(x < self.width && y < self.height);
        y * self.width + x
    }

    pub fn get(&self, x: usize, y: usize) -> (ElementId, Mass, Temp) {
        let i = self.index(x, y);
        (
            ElementId(self.cells.element[i]),
            Mass(self.cells.mass[i]),
            Temp(self.cells.temp[i]),
        )
    }

    /// Writes a cell and clears its flags. Vacuum, and any gas or liquid with zero
    /// mass, is normalised to element 0 with zero mass and zero temperature.
    pub fn set(
        &mut self,
        table: &ElementTable,
        x: usize,
        y: usize,
        element: ElementId,
        mass: Mass,
        temp: Temp,
    ) {
        assert!(
            table.contains(element),
            "element {element:?} is not in the table"
        );
        let i = self.index(x, y);
        let phase = element.phase();
        let empty = phase == Phase::Vacuum || (mass.0 == 0 && phase != Phase::Solid);
        if empty {
            self.cells.element[i] = 0;
            self.cells.mass[i] = 0;
            self.cells.temp[i] = 0;
        } else {
            self.cells.element[i] = element.0;
            self.cells.mass[i] = mass.0;
            self.cells.temp[i] = temp.0;
        }
        self.cells.flags[i] = 0;
    }
}

/// Neighbour directions in the kernel's fixed priority order: down, left, right, up.
/// Every rule that has to break a tie between neighbours walks this order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Dir {
    Down = 0,
    Left = 1,
    Right = 2,
    Up = 3,
}

pub const DIRS: [Dir; 4] = [Dir::Down, Dir::Left, Dir::Right, Dir::Up];

impl Dir {
    #[inline]
    pub const fn offset(self) -> (isize, isize) {
        match self {
            Dir::Down => (0, -1),
            Dir::Left => (-1, 0),
            Dir::Right => (1, 0),
            Dir::Up => (0, 1),
        }
    }

    /// Position in [`DIRS`] and in a [`Stencil`].
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    #[inline]
    pub const fn opposite(self) -> Dir {
        match self {
            Dir::Down => Dir::Up,
            Dir::Left => Dir::Right,
            Dir::Right => Dir::Left,
            Dir::Up => Dir::Down,
        }
    }
}

/// A cell and its four neighbours in priority order, loaded once per pass so the
/// rules share the loads instead of each re-reading the grid.
#[derive(Clone, Copy, Debug)]
pub struct Stencil {
    pub cell: Cell,
    pub near: [Option<Cell>; 4],
}

impl Stencil {
    #[inline]
    pub fn get(&self, dir: Dir) -> Option<&Cell> {
        self.near[dir.index()].as_ref()
    }

    /// The neighbours that exist, in priority order.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &Cell> {
        self.near.iter().flatten()
    }
}

/// A snapshot of one cell read from the old buffer during a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub x: usize,
    pub y: usize,
    pub element: u16,
    pub phase: Phase,
    pub mass: u32,
    pub temp: i32,
    pub flags: u8,
}

/// What a pass writes for one cell, plus whether the cell exchanged anything.
///
/// `active` means a non-zero flux, swap, or heat transfer touched the cell, not that
/// its value changed: a cell inside a steady gradient keeps its value while mass
/// streams through it, and its chunk must keep being processed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellOut {
    pub element: u16,
    pub mass: u32,
    pub temp: i32,
    pub flags: u8,
    pub active: bool,
}

impl CellOut {
    pub const VACUUM: CellOut = CellOut {
        element: 0,
        mass: 0,
        temp: 0,
        flags: 0,
        active: true,
    };

    #[inline]
    pub fn keep(cell: &Cell) -> CellOut {
        CellOut {
            element: cell.element,
            mass: cell.mass,
            temp: cell.temp,
            flags: cell.flags,
            active: false,
        }
    }
}

/// Read-only view of the old buffer for one pass.
///
/// Invariant: every array has exactly `width * height` entries. [`Grid::new`] is the
/// only constructor, which is what lets [`Grid::cell`] check the coordinates once and
/// then load the four arrays without per-array bounds checks.
#[derive(Clone, Copy)]
pub struct Grid<'a> {
    width: usize,
    height: usize,
    element: &'a [u16],
    mass: &'a [u32],
    temp: &'a [i32],
    flags: &'a [u8],
    pub table: &'a ElementTable,
}

impl<'a> Grid<'a> {
    pub fn new(world: &'a World, table: &'a ElementTable) -> Grid<'a> {
        let len = world.width * world.height;
        assert_eq!(world.cells.element.len(), len);
        assert_eq!(world.cells.mass.len(), len);
        assert_eq!(world.cells.temp.len(), len);
        assert_eq!(world.cells.flags.len(), len);
        Grid {
            width: world.width,
            height: world.height,
            element: &world.cells.element,
            mass: &world.cells.mass,
            temp: &world.cells.temp,
            flags: &world.cells.flags,
            table,
        }
    }

    #[inline]
    pub fn width(&self) -> usize {
        self.width
    }

    #[inline]
    pub fn height(&self) -> usize {
        self.height
    }

    pub fn element(&self) -> &'a [u16] {
        self.element
    }

    pub fn mass(&self) -> &'a [u32] {
        self.mass
    }

    pub fn temp(&self) -> &'a [i32] {
        self.temp
    }

    pub fn flags(&self) -> &'a [u8] {
        self.flags
    }

    /// The cell at `(x, y)`; panics outside the world.
    #[inline]
    pub fn cell(&self, x: usize, y: usize) -> Cell {
        assert!(
            x < self.width && y < self.height,
            "cell ({x}, {y}) is outside the world"
        );
        self.cell_in_bounds(x, y)
    }

    /// Loads a cell whose coordinates the caller has already checked.
    #[inline]
    fn cell_in_bounds(&self, x: usize, y: usize) -> Cell {
        debug_assert!(x < self.width && y < self.height);
        let i = y * self.width + x;
        // SAFETY: `i < width * height`, and the constructor checked that every array
        // has exactly `width * height` entries.
        let (element, mass, temp, flags) = unsafe {
            (
                *self.element.get_unchecked(i),
                *self.mass.get_unchecked(i),
                *self.temp.get_unchecked(i),
                *self.flags.get_unchecked(i),
            )
        };
        Cell {
            x,
            y,
            element,
            phase: ElementId(element).phase(),
            mass,
            temp,
            flags,
        }
    }

    /// The neighbour in `dir`, or `None` at the world edge. Edges are walls: there is
    /// no cell there, not a vacuum.
    #[inline]
    pub fn neighbour(&self, x: usize, y: usize, dir: Dir) -> Option<Cell> {
        let (dx, dy) = dir.offset();
        let nx = x.wrapping_add_signed(dx);
        let ny = y.wrapping_add_signed(dy);
        if nx < self.width && ny < self.height {
            Some(self.cell_in_bounds(nx, ny))
        } else {
            None
        }
    }

    /// The cell and its four neighbours; panics outside the world.
    #[inline]
    pub fn stencil(&self, x: usize, y: usize) -> Stencil {
        let cell = self.cell(x, y);
        let near = if x > 0 && y > 0 && x + 1 < self.width && y + 1 < self.height {
            [
                Some(self.cell_in_bounds(x, y - 1)),
                Some(self.cell_in_bounds(x - 1, y)),
                Some(self.cell_in_bounds(x + 1, y)),
                Some(self.cell_in_bounds(x, y + 1)),
            ]
        } else {
            [
                self.neighbour(x, y, Dir::Down),
                self.neighbour(x, y, Dir::Left),
                self.neighbour(x, y, Dir::Right),
                self.neighbour(x, y, Dir::Up),
            ]
        };
        Stencil { cell, near }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::builtin;

    #[test]
    fn set_normalises_empty_cells_to_vacuum() {
        let table = ElementTable::builtin();
        let mut world = World::new(2, 2);
        world.set(
            &table,
            0,
            0,
            builtin::OXYGEN,
            Mass::ZERO,
            Temp::from_kelvin(300),
        );
        assert_eq!(world.get(0, 0), (ElementId::VACUUM, Mass::ZERO, Temp::ZERO));
        world.set(
            &table,
            1,
            1,
            builtin::VACUUM,
            Mass::from_kg(5),
            Temp::from_kelvin(300),
        );
        assert_eq!(world.get(1, 1), (ElementId::VACUUM, Mass::ZERO, Temp::ZERO));
        world.set(
            &table,
            1,
            0,
            builtin::SANDSTONE,
            Mass::ZERO,
            Temp::from_kelvin(300),
        );
        assert_eq!(world.get(1, 0).0, builtin::SANDSTONE);
    }

    #[test]
    #[should_panic(expected = "is not in the table")]
    fn set_rejects_unknown_elements() {
        let table = ElementTable::builtin();
        let mut world = World::new(1, 1);
        world.set(
            &table,
            0,
            0,
            ElementId::new(Phase::Gas, 7),
            Mass::from_kg(1),
            Temp::ZERO,
        );
    }

    #[test]
    #[should_panic(expected = "outside the world")]
    fn cell_outside_the_world_panics() {
        let table = ElementTable::builtin();
        let world = World::new(3, 3);
        Grid::new(&world, &table).cell(3, 0);
    }

    #[test]
    fn stencil_matches_neighbour_lookups_everywhere() {
        let table = ElementTable::builtin();
        let mut world = World::new(4, 3);
        for y in 0..3 {
            for x in 0..4 {
                world.set(
                    &table,
                    x,
                    y,
                    builtin::OXYGEN,
                    Mass::from_grams(100 + (x * 3 + y) as u64 * 10),
                    Temp::from_kelvin(300),
                );
            }
        }
        let grid = Grid::new(&world, &table);
        for y in 0..3 {
            for x in 0..4 {
                let st = grid.stencil(x, y);
                assert_eq!(st.cell, grid.cell(x, y));
                for dir in DIRS {
                    assert_eq!(
                        st.get(dir).copied(),
                        grid.neighbour(x, y, dir),
                        "({x}, {y}) {dir:?}"
                    );
                    assert_eq!(dir.opposite().opposite(), dir);
                }
            }
        }
    }

    #[test]
    fn edges_have_no_neighbours_and_down_is_minus_y() {
        let table = ElementTable::builtin();
        let world = World::new(3, 3);
        let grid = Grid::new(&world, &table);
        assert!(grid.neighbour(0, 0, Dir::Down).is_none());
        assert!(grid.neighbour(0, 0, Dir::Left).is_none());
        assert_eq!(
            grid.neighbour(1, 1, Dir::Down).map(|c| (c.x, c.y)),
            Some((1, 0))
        );
        assert_eq!(
            grid.neighbour(1, 1, Dir::Up).map(|c| (c.x, c.y)),
            Some((1, 2))
        );
        assert_eq!(grid.neighbour(2, 2, Dir::Right), None);
    }
}
