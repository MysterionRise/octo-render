//! Dual-grid autotiling for solids. The corner grid has one entry on every vertex
//! shared by four cells, (W+1)×(H+1) of them; each entry packs the solid material to
//! draw and a four-bit case saying which of the four cells around the vertex are solid.
//! The tile drawn around a vertex is then picked by the case, which is what gives smooth
//! outer and inner corners from 16 tiles per material.

use crate::snapshot::Snapshot;
use colony_sim::Phase;

/// Bit for each cell around a corner, in the order bottom-left, bottom-right,
/// top-left, top-right (y up).
pub const BL: u16 = 1;
pub const BR: u16 = 2;
pub const TL: u16 = 4;
pub const TR: u16 = 8;

/// The (W+1)×(H+1) corner values, row-major with row 0 at the bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CornerGrid {
    pub width: u32,
    pub height: u32,
    pub values: Vec<u16>,
}

/// Packs a corner value from the solid rank and the four-bit case.
#[inline]
pub fn pack(rank: u8, case: u16) -> u16 {
    ((rank as u16) << 4) | (case & 15)
}

impl CornerGrid {
    /// Corner grid for a `cells_w × cells_h` world, all empty.
    pub fn new(cells_w: u32, cells_h: u32) -> CornerGrid {
        let (width, height) = (cells_w + 1, cells_h + 1);
        CornerGrid {
            width,
            height,
            values: vec![0; (width * height) as usize],
        }
    }

    #[inline]
    pub fn index(&self, kx: u32, ky: u32) -> usize {
        (ky * self.width + kx) as usize
    }

    #[inline]
    pub fn get(&self, kx: u32, ky: u32) -> u16 {
        self.values[self.index(kx, ky)]
    }

    /// The value for corner `(kx, ky)` of the snapshot: cells outside the world clamp to
    /// the nearest cell inside (the kernel's edges are walls, so rock continues outward).
    /// The material is the highest-ranked solid among the solid cells.
    pub fn corner_value(snapshot: &Snapshot, solid_ranks: &[u8], kx: u32, ky: u32) -> u16 {
        let (w, h) = (snapshot.width, snapshot.height);
        let clamp_x = |x: i64| x.clamp(0, w as i64 - 1) as u32;
        let clamp_y = |y: i64| y.clamp(0, h as i64 - 1) as u32;
        let cells = [
            (clamp_x(kx as i64 - 1), clamp_y(ky as i64 - 1), BL),
            (clamp_x(kx as i64), clamp_y(ky as i64 - 1), BR),
            (clamp_x(kx as i64 - 1), clamp_y(ky as i64), TL),
            (clamp_x(kx as i64), clamp_y(ky as i64), TR),
        ];
        let mut case = 0u16;
        let mut rank = 0u8;
        for (x, y, bit) in cells {
            let [index, packed, _, _] = snapshot.texels[snapshot.index(x, y)];
            if Phase::from_bits(packed as u16 & 3) == Phase::Solid {
                case |= bit;
                rank = rank.max(solid_ranks[index as usize]);
            }
        }
        if case == 0 {
            0
        } else {
            pack(rank, case)
        }
    }

    /// Recomputes every corner.
    pub fn rebuild_all(&mut self, snapshot: &Snapshot, solid_ranks: &[u8]) {
        assert_eq!(
            (self.width, self.height),
            (snapshot.width + 1, snapshot.height + 1)
        );
        for ky in 0..self.height {
            for kx in 0..self.width {
                let i = self.index(kx, ky);
                self.values[i] = Self::corner_value(snapshot, solid_ranks, kx, ky);
            }
        }
    }

    /// Recomputes the corners `kx0..=kx1`, `ky0..=ky1` (inclusive, clamped to the grid).
    pub fn rebuild_rect(
        &mut self,
        snapshot: &Snapshot,
        solid_ranks: &[u8],
        kx0: u32,
        ky0: u32,
        kx1: u32,
        ky1: u32,
    ) {
        let (kx1, ky1) = (kx1.min(self.width - 1), ky1.min(self.height - 1));
        for ky in ky0..=ky1 {
            for kx in kx0..=kx1 {
                let i = self.index(kx, ky);
                self.values[i] = Self::corner_value(snapshot, solid_ranks, kx, ky);
            }
        }
    }

    /// The corner rectangle a dirty chunk rectangle (in cells, inclusive) affects: one
    /// corner beyond the cells on every side.
    pub fn affected_corners(cx0: u32, cy0: u32, cx1: u32, cy1: u32) -> (u32, u32, u32, u32) {
        (cx0, cy0, cx1 + 1, cy1 + 1)
    }

    pub fn bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::solid_ranks;
    use colony_sim::{builtin, scenario, ElementTable, Mass, Sim, Temp, TickStats};

    fn snapshot_of(sim: &Sim) -> Snapshot {
        let dirty = vec![true; sim.chunks().len()];
        let stats = TickStats {
            tick: 0,
            chunks_processed: 0,
            chunks_skipped: 0,
            chunks_processed_mass: 0,
        };
        Snapshot::from_sim(sim, &dirty, 0, stats, 0, 0)
    }

    #[test]
    fn bit_order_on_a_two_by_two_world() {
        let table = ElementTable::builtin();
        let ranks = solid_ranks(&table);
        let mut sim = Sim::new(table, 2, 2);
        sim.set_cell(
            1,
            1,
            builtin::SANDSTONE,
            Mass::from_kg(100),
            Temp::from_kelvin(300),
        );
        let snap = snapshot_of(&sim);
        // The centre corner (1, 1) sees the solid as its top-right cell.
        assert_eq!(CornerGrid::corner_value(&snap, &ranks, 1, 1), pack(0, TR));
        // Corner (2, 2) is past the solid cell: all four clamp to (1, 1) = solid.
        assert_eq!(
            CornerGrid::corner_value(&snap, &ranks, 2, 2),
            pack(0, BL | BR | TL | TR)
        );
        // Corner (2, 1): BL clamps to (1, 0) vacuum, TL = (1, 1) solid, BR/TR clamp to x=1.
        assert_eq!(
            CornerGrid::corner_value(&snap, &ranks, 2, 1),
            pack(0, TL | TR)
        );
        assert_eq!(CornerGrid::corner_value(&snap, &ranks, 0, 0), 0);
    }

    #[test]
    fn the_higher_ranked_solid_wins_a_corner() {
        let table = ElementTable::builtin();
        let ranks = solid_ranks(&table);
        let mut sim = Sim::new(table, 2, 1);
        sim.set_cell(
            0,
            0,
            builtin::SANDSTONE,
            Mass::from_kg(100),
            Temp::from_kelvin(300),
        );
        sim.set_cell(
            1,
            0,
            builtin::ICE,
            Mass::from_kg(100),
            Temp::from_kelvin(250),
        );
        let snap = snapshot_of(&sim);
        let v = CornerGrid::corner_value(&snap, &ranks, 1, 0);
        assert_eq!(v >> 4, ranks[builtin::ICE.index()] as u16);
        assert_eq!(v & 15, BL | BR | TL | TR, "y clamps to the single row");
    }

    #[test]
    fn rect_rebuild_equals_full_rebuild_after_edits() {
        let mut sim = scenario::cave(70, 50);
        let table = sim.table().clone();
        let ranks = solid_ranks(&table);
        let snap = snapshot_of(&sim);
        let mut grid = CornerGrid::new(70, 50);
        grid.rebuild_all(&snap, &ranks);
        // Edit a block of cells inside chunk (1, 1) and rebuild only its corners.
        sim.fill_rect(
            40,
            35,
            46,
            41,
            builtin::SANDSTONE,
            Mass::from_kg(100),
            Temp::from_kelvin(300),
        );
        sim.set_cell(
            42,
            37,
            builtin::OXYGEN,
            Mass::from_kg(1),
            Temp::from_kelvin(300),
        );
        let snap = snapshot_of(&sim);
        let (kx0, ky0, kx1, ky1) = CornerGrid::affected_corners(32, 32, 63, 49);
        grid.rebuild_rect(&snap, &ranks, kx0, ky0, kx1, ky1);
        let mut full = CornerGrid::new(70, 50);
        full.rebuild_all(&snap, &ranks);
        assert_eq!(grid, full);
        assert_eq!(grid.bytes().len(), 71 * 51 * 2);
    }
}
