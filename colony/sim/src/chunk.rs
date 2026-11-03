//! Chunks are a scheduling unit only: 32×32 cells whose activity decides whether the
//! next tick has to visit them. Cell storage is flat; chunks never own cells.
//!
//! A chunk is processed when it, or any of its eight neighbours, had activity last
//! tick. Activity is "some non-zero exchange touched a cell", not "a value changed",
//! so a steady gradient stays active and a skipped chunk is guaranteed to have zero
//! flux across all of its borders (see `docs/colony-engine-design.md`, section 6.4).
//! Every stencil the passes use reaches at most two cells, and a tick runs three
//! passes, so the 32-cell chunk is wide enough for a skipped chunk's border cells to
//! stay unchanged throughout the tick.

/// Chunk edge length in cells.
pub const CHUNK: usize = 32;

/// The grid of chunks covering a world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkGrid {
    pub cols: usize,
    pub rows: usize,
}

impl ChunkGrid {
    pub fn new(width: usize, height: usize) -> ChunkGrid {
        ChunkGrid {
            cols: width.div_ceil(CHUNK),
            rows: height.div_ceil(CHUNK),
        }
    }

    pub fn len(&self) -> usize {
        self.cols * self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn index(&self, cx: usize, cy: usize) -> usize {
        cy * self.cols + cx
    }

    /// Marks every chunk whose 3×3 neighbourhood had activity.
    pub fn process_from_active(&self, active: &[bool], process: &mut [bool]) {
        debug_assert_eq!(active.len(), self.len());
        debug_assert_eq!(process.len(), self.len());
        for cy in 0..self.rows {
            for cx in 0..self.cols {
                let mut hit = false;
                let y0 = cy.saturating_sub(1);
                let y1 = (cy + 1).min(self.rows - 1);
                let x0 = cx.saturating_sub(1);
                let x1 = (cx + 1).min(self.cols - 1);
                for ny in y0..=y1 {
                    for nx in x0..=x1 {
                        hit |= active[self.index(nx, ny)];
                    }
                }
                process[self.index(cx, cy)] = hit;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_rounds_up_partial_chunks() {
        assert_eq!(ChunkGrid::new(64, 64), ChunkGrid { cols: 2, rows: 2 });
        assert_eq!(ChunkGrid::new(65, 1), ChunkGrid { cols: 3, rows: 1 });
    }

    #[test]
    fn activity_spreads_to_the_eight_neighbours_only() {
        let grid = ChunkGrid::new(5 * CHUNK, 5 * CHUNK);
        let mut active = vec![false; grid.len()];
        active[grid.index(2, 2)] = true;
        let mut process = vec![false; grid.len()];
        grid.process_from_active(&active, &mut process);
        let count = process.iter().filter(|p| **p).count();
        assert_eq!(count, 9);
        assert!(process[grid.index(1, 1)] && process[grid.index(3, 3)]);
        assert!(!process[grid.index(0, 0)] && !process[grid.index(4, 2)]);
    }
}
