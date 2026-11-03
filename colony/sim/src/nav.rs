//! Walkability, regions, and pathfinding for colonists.
//!
//! A colonist stands on a *standing* cell: a passable cell whose cell below is solid (or
//! the world edge). Moves are sideways along standing cells, one-cell steps up (with
//! headroom) and down, and drops of up to [`MAX_FALL`] cells. Regions are the connected
//! components of standing cells inside each 16×16 block, numbered in row-major first-visit
//! order so that a loaded save rebuilds identical ids. Long paths run A* over the region
//! graph and then a local A* restricted to the corridor of regions; everything is integer
//! and iterates in a fixed order, so paths are deterministic.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::chunk::{ChunkGrid, CHUNK};
use crate::element::{ElementId, ElementTable, Phase};
use crate::fixed::Mass;
use crate::world::CellBuffers;

pub const BLOCK: usize = 16;
pub const REGIONS_PER_BLOCK: u32 = 128;
pub const LOCAL_NODE_CAP: usize = 4096;
pub const MAX_FALL: u32 = 4;
/// A liquid cell at or above this mass cannot be walked through.
pub const DEEP_LIQUID: u32 = Mass::from_kg(500).raw();
pub const NO_REGION: u32 = u32::MAX;

pub const SOLID: u8 = 1;
pub const PASSABLE: u8 = 2;
pub const STANDING: u8 = 4;

/// Where a path should end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Goal {
    /// Stand on this cell.
    Cell { x: u32, y: u32 },
    /// Stand on a 4-neighbour of this cell (to work on it).
    Adjacent { x: u32, y: u32 },
}

impl Goal {
    pub fn target(&self) -> (u32, u32) {
        match *self {
            Goal::Cell { x, y } | Goal::Adjacent { x, y } => (x, y),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionEdge {
    pub from: u32,
    pub to: u32,
    pub weight: u16,
}

/// A cell path; `cells` is stored reversed so `advance()` pops the next cell.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Path {
    pub cells: Vec<(u32, u32)>,
    /// Sorted chunk indices the path crosses, for cheap invalidation.
    pub chunks: Vec<u32>,
}

impl Path {
    pub fn advance(&mut self) -> Option<(u32, u32)> {
        self.cells.pop()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether any chunk the path crosses changed walkability.
    pub fn crosses_changed(&self, changed_chunks: &[bool]) -> bool {
        self.chunks.iter().any(|c| changed_chunks[*c as usize])
    }
}

#[derive(Clone, Debug)]
pub struct NavGrid {
    w: u32,
    h: u32,
    bcols: u32,
    brows: u32,
    chunks: ChunkGrid,
    flags: Vec<u8>,
    region: Vec<u32>,
    region_count: Vec<u8>,
    edges: Vec<Vec<RegionEdge>>,
    changed_chunks: Vec<bool>,
    stamp: u32,
    g: Vec<u32>,
    mark: Vec<u32>,
    parent: Vec<u32>,
    rg: Vec<u32>,
    rmark: Vec<u32>,
    rparent: Vec<u32>,
}

#[inline]
fn flags_of(table: &ElementTable, element: u16, mass: u32) -> (bool, bool) {
    let phase = ElementId(element).phase();
    let solid = phase == Phase::Solid;
    let passable = !solid && !(phase == Phase::Liquid && mass >= DEEP_LIQUID);
    let _ = table;
    (solid, passable)
}

impl NavGrid {
    pub fn new(w: u32, h: u32) -> NavGrid {
        let n = (w * h) as usize;
        let bcols = w.div_ceil(BLOCK as u32);
        let brows = h.div_ceil(BLOCK as u32);
        let blocks = (bcols * brows) as usize;
        let chunks = ChunkGrid::new(w as usize, h as usize);
        NavGrid {
            w,
            h,
            bcols,
            brows,
            chunks,
            flags: vec![0; n],
            region: vec![NO_REGION; n],
            region_count: vec![0; blocks],
            edges: vec![Vec::new(); blocks],
            changed_chunks: vec![false; chunks.len()],
            stamp: 0,
            g: vec![0; n],
            mark: vec![0; n],
            parent: vec![0; n],
            rg: vec![0; blocks * REGIONS_PER_BLOCK as usize],
            rmark: vec![0; blocks * REGIONS_PER_BLOCK as usize],
            rparent: vec![0; blocks * REGIONS_PER_BLOCK as usize],
        }
    }

    pub fn width(&self) -> u32 {
        self.w
    }

    pub fn height(&self) -> u32 {
        self.h
    }

    #[inline]
    fn idx(&self, x: u32, y: u32) -> usize {
        (y * self.w + x) as usize
    }

    #[inline]
    pub fn is_solid(&self, x: u32, y: u32) -> bool {
        self.flags[self.idx(x, y)] & SOLID != 0
    }

    #[inline]
    pub fn is_passable(&self, x: u32, y: u32) -> bool {
        self.flags[self.idx(x, y)] & PASSABLE != 0
    }

    #[inline]
    pub fn is_standing(&self, x: u32, y: u32) -> bool {
        self.flags[self.idx(x, y)] & STANDING != 0
    }

    pub fn region_of(&self, x: u32, y: u32) -> u32 {
        self.region[self.idx(x, y)]
    }

    pub fn block_of(&self, x: u32, y: u32) -> u32 {
        (y / BLOCK as u32) * self.bcols + x / BLOCK as u32
    }

    pub fn block_count(&self) -> usize {
        (self.bcols * self.brows) as usize
    }

    pub fn edges_of_block(&self, block: u32) -> &[RegionEdge] {
        &self.edges[block as usize]
    }

    pub fn changed_chunks(&self) -> &[bool] {
        &self.changed_chunks
    }

    pub fn clear_changed(&mut self) {
        self.changed_chunks.iter_mut().for_each(|c| *c = false);
    }

    pub fn flags(&self) -> &[u8] {
        &self.flags
    }

    pub fn regions(&self) -> &[u32] {
        &self.region
    }

    /// Recomputes the flags of one cell from the cell buffers; returns whether it changed.
    fn update_cell_flags(
        &mut self,
        cells: &CellBuffers,
        table: &ElementTable,
        x: u32,
        y: u32,
    ) -> bool {
        let i = self.idx(x, y);
        let (solid, passable) = flags_of(table, cells.element[i], cells.mass[i]);
        let below_solid = if y == 0 {
            true
        } else {
            let j = self.idx(x, y - 1);
            flags_of(table, cells.element[j], cells.mass[j]).0
        };
        let mut f = 0;
        if solid {
            f |= SOLID;
        }
        if passable {
            f |= PASSABLE;
            if below_solid {
                f |= STANDING;
            }
        }
        let changed = self.flags[i] != f;
        self.flags[i] = f;
        changed
    }

    /// Recomputes everything.
    pub fn refresh_all(&mut self, cells: &CellBuffers, table: &ElementTable) {
        for y in 0..self.h {
            for x in 0..self.w {
                self.update_cell_flags(cells, table, x, y);
            }
        }
        for b in 0..self.block_count() as u32 {
            self.rebuild_block(b);
        }
        for b in 0..self.block_count() as u32 {
            self.rebuild_edges(b);
        }
        self.changed_chunks.iter_mut().for_each(|c| *c = true);
    }

    /// Recomputes the flags of every processed chunk (plus the row above each, whose
    /// standing state depends on it), rebuilds the regions of blocks whose flags
    /// changed and the edges around them, and records which chunks changed.
    pub fn refresh(&mut self, cells: &CellBuffers, table: &ElementTable, processed: &[bool]) {
        let mut dirty_blocks = vec![false; self.block_count()];
        let chunk = CHUNK as u32;
        for cy in 0..self.chunks.rows as u32 {
            for cx in 0..self.chunks.cols as u32 {
                let c = self.chunks.index(cx as usize, cy as usize);
                if !processed[c] {
                    continue;
                }
                let x0 = cx * chunk;
                let x1 = (x0 + chunk).min(self.w);
                let y0 = cy * chunk;
                let y1 = (y0 + chunk + 1).min(self.h);
                let mut any = false;
                for y in y0..y1 {
                    for x in x0..x1 {
                        if self.update_cell_flags(cells, table, x, y) {
                            any = true;
                            dirty_blocks[self.block_of(x, y) as usize] = true;
                        }
                    }
                }
                if any {
                    self.changed_chunks[c] = true;
                }
            }
        }
        let mut edge_blocks = vec![false; self.block_count()];
        for b in (0..self.block_count()).filter(|&b| dirty_blocks[b]) {
            self.rebuild_block(b as u32);
            let (bx, by) = (b as u32 % self.bcols, b as u32 / self.bcols);
            for ny in by.saturating_sub(1)..=(by + 1).min(self.brows - 1) {
                for nx in bx.saturating_sub(1)..=(bx + 1).min(self.bcols - 1) {
                    edge_blocks[(ny * self.bcols + nx) as usize] = true;
                }
            }
        }
        for b in (0..self.block_count()).filter(|&b| edge_blocks[b]) {
            self.rebuild_edges(b as u32);
        }
    }

    fn block_cells(&self, block: u32) -> (u32, u32, u32, u32) {
        let bx = block % self.bcols;
        let by = block / self.bcols;
        let x0 = bx * BLOCK as u32;
        let y0 = by * BLOCK as u32;
        (
            x0,
            y0,
            (x0 + BLOCK as u32).min(self.w),
            (y0 + BLOCK as u32).min(self.h),
        )
    }

    /// Symmetric moves (sideways, step up, step down) from a standing cell, for region
    /// labelling. Appends `(x, y)` destinations.
    fn symmetric_moves(&self, x: u32, y: u32, out: &mut Vec<(u32, u32)>) {
        for nx in [x.wrapping_sub(1), x + 1] {
            if nx >= self.w {
                continue;
            }
            if self.is_standing(nx, y) {
                out.push((nx, y));
            }
            if y + 1 < self.h && self.is_passable(x, y + 1) && self.is_standing(nx, y + 1) {
                out.push((nx, y + 1));
            }
            if y > 0 && self.is_passable(nx, y) && self.is_standing(nx, y - 1) {
                out.push((nx, y - 1));
            }
        }
    }

    /// Every move from a standing cell with its cost: the symmetric ones and drops of
    /// two to [`MAX_FALL`] cells.
    pub fn moves(&self, x: u32, y: u32, out: &mut Vec<((u32, u32), u32)>) {
        out.clear();
        if !self.is_standing(x, y) {
            return;
        }
        for nx in [x.wrapping_sub(1), x + 1] {
            if nx >= self.w {
                continue;
            }
            if self.is_standing(nx, y) {
                out.push(((nx, y), 1));
            } else if self.is_passable(nx, y) {
                let mut k = 1;
                while k <= MAX_FALL && y >= k {
                    let ny = y - k;
                    if self.is_standing(nx, ny) {
                        out.push(((nx, ny), k));
                        break;
                    }
                    if !self.is_passable(nx, ny) {
                        break;
                    }
                    k += 1;
                }
            }
            if y + 1 < self.h && self.is_passable(x, y + 1) && self.is_standing(nx, y + 1) {
                out.push(((nx, y + 1), 1));
            }
        }
    }

    /// Whether `to` is a legal single move from `from` on the current flags.
    pub fn is_legal_move(&self, from: (u32, u32), to: (u32, u32)) -> bool {
        let mut moves = Vec::with_capacity(6);
        self.moves(from.0, from.1, &mut moves);
        moves.iter().any(|(dst, _)| *dst == to)
    }

    fn rebuild_block(&mut self, block: u32) {
        let (x0, y0, x1, y1) = self.block_cells(block);
        for y in y0..y1 {
            for x in x0..x1 {
                let i = self.idx(x, y);
                self.region[i] = NO_REGION;
            }
        }
        let mut count: u32 = 0;
        let mut queue: Vec<(u32, u32)> = Vec::new();
        let mut scratch = Vec::with_capacity(6);
        for y in y0..y1 {
            for x in x0..x1 {
                let i = self.idx(x, y);
                if !self.is_standing(x, y) || self.region[i] != NO_REGION {
                    continue;
                }
                let local = count.min(REGIONS_PER_BLOCK - 1);
                let id = block * REGIONS_PER_BLOCK + local;
                count += 1;
                self.region[i] = id;
                queue.clear();
                queue.push((x, y));
                let mut head = 0;
                while head < queue.len() {
                    let (cx, cy) = queue[head];
                    head += 1;
                    scratch.clear();
                    self.symmetric_moves(cx, cy, &mut scratch);
                    for &(nx, ny) in &scratch {
                        if nx < x0 || nx >= x1 || ny < y0 || ny >= y1 {
                            continue;
                        }
                        let j = self.idx(nx, ny);
                        if self.region[j] == NO_REGION {
                            self.region[j] = id;
                            queue.push((nx, ny));
                        }
                    }
                }
            }
        }
        self.region_count[block as usize] = count.min(REGIONS_PER_BLOCK) as u8;
    }

    fn rebuild_edges(&mut self, block: u32) {
        let (x0, y0, x1, y1) = self.block_cells(block);
        let (bx, by) = (block % self.bcols, block / self.bcols);
        let mut edges: Vec<RegionEdge> = Vec::new();
        let mut moves = Vec::with_capacity(6);
        for y in y0..y1 {
            for x in x0..x1 {
                if !self.is_standing(x, y) {
                    continue;
                }
                let from = self.region_of(x, y);
                self.moves(x, y, &mut moves);
                for &((nx, ny), cost) in &moves {
                    let nb = self.block_of(nx, ny);
                    if nb == block && cost < 2 {
                        continue;
                    }
                    let to = self.region_of(nx, ny);
                    if to == NO_REGION || to == from {
                        continue;
                    }
                    let (nbx, nby) = (nb % self.bcols, nb / self.bcols);
                    let dist = bx.abs_diff(nbx) + by.abs_diff(nby);
                    let weight = (16 * dist).max(1).min(u16::MAX as u32) as u16;
                    edges.push(RegionEdge { from, to, weight });
                }
            }
        }
        edges.sort_by_key(|e| (e.from, e.to, e.weight));
        edges.dedup_by(|a, b| a.from == b.from && a.to == b.to);
        self.edges[block as usize] = edges;
    }

    fn next_stamp(&mut self) -> u32 {
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.mark.iter_mut().for_each(|m| *m = 0);
            self.rmark.iter_mut().for_each(|m| *m = 0);
            self.stamp = 1;
        }
        self.stamp
    }

    fn region_block(&self, region: u32) -> u32 {
        region / REGIONS_PER_BLOCK
    }

    fn block_distance(&self, a: u32, b: u32) -> u32 {
        let (ax, ay) = (a % self.bcols, a / self.bcols);
        let (bx, by) = (b % self.bcols, b / self.bcols);
        ax.abs_diff(bx) + ay.abs_diff(by)
    }

    /// Dijkstra over regions from `start`, up to `cap` cost; leaves distances and parents
    /// in the region scratch under the current stamp. Returns the stamp.
    pub fn dijkstra_regions(&mut self, start: u32, cap: u32) -> u32 {
        let stamp = self.next_stamp();
        let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
        self.rg[start as usize] = 0;
        self.rmark[start as usize] = stamp;
        self.rparent[start as usize] = start;
        heap.push(Reverse((0, start)));
        while let Some(Reverse((g, r))) = heap.pop() {
            if g > self.rg[r as usize] || g > cap {
                continue;
            }
            let block = self.region_block(r) as usize;
            for e in &self.edges[block] {
                if e.from != r {
                    continue;
                }
                let ng = g + e.weight as u32;
                let t = e.to as usize;
                if self.rmark[t] != stamp || ng < self.rg[t] {
                    self.rmark[t] = stamp;
                    self.rg[t] = ng;
                    self.rparent[t] = r;
                    heap.push(Reverse((ng, e.to)));
                }
            }
        }
        stamp
    }

    /// Distance of a region after [`NavGrid::dijkstra_regions`], if reached.
    pub fn region_dist(&self, region: u32) -> Option<u32> {
        if self.rmark[region as usize] == self.stamp {
            Some(self.rg[region as usize])
        } else {
            None
        }
    }

    /// The regions on the way from the Dijkstra start to `region`, sorted.
    pub fn corridor_to(&self, region: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut r = region;
        loop {
            out.push(r);
            let p = self.rparent[r as usize];
            if p == r {
                break;
            }
            r = p;
        }
        out.sort_unstable();
        out
    }

    /// A* over regions to any of `goals`; returns the cost and the sorted corridor.
    pub fn region_path(&mut self, start: u32, goals: &[u32]) -> Option<(u32, Vec<u32>)> {
        if goals.is_empty() {
            return None;
        }
        if goals.contains(&start) {
            return Some((0, vec![start]));
        }
        let stamp = self.next_stamp();
        let goal_blocks: Vec<u32> = goals.iter().map(|g| self.region_block(*g)).collect();
        let h = |nav: &NavGrid, r: u32| -> u32 {
            let b = nav.region_block(r);
            16 * goal_blocks
                .iter()
                .map(|gb| nav.block_distance(b, *gb))
                .min()
                .unwrap_or(0)
        };
        let mut heap: BinaryHeap<Reverse<(u32, u32, u32)>> = BinaryHeap::new();
        self.rg[start as usize] = 0;
        self.rmark[start as usize] = stamp;
        self.rparent[start as usize] = start;
        heap.push(Reverse((h(self, start), 0, start)));
        while let Some(Reverse((_, g, r))) = heap.pop() {
            if g > self.rg[r as usize] {
                continue;
            }
            if goals.contains(&r) {
                return Some((g, self.corridor_to(r)));
            }
            let block = self.region_block(r) as usize;
            for i in 0..self.edges[block].len() {
                let e = self.edges[block][i];
                if e.from != r {
                    continue;
                }
                let ng = g + e.weight as u32;
                let t = e.to as usize;
                if self.rmark[t] != stamp || ng < self.rg[t] {
                    self.rmark[t] = stamp;
                    self.rg[t] = ng;
                    self.rparent[t] = r;
                    let f = ng + h(self, e.to);
                    heap.push(Reverse((f, ng, e.to)));
                }
            }
        }
        None
    }

    fn reaches(&self, goal: Goal, x: u32, y: u32) -> bool {
        match goal {
            Goal::Cell { x: gx, y: gy } => x == gx && y == gy,
            Goal::Adjacent { x: gx, y: gy } => {
                (x == gx && y.abs_diff(gy) == 1) || (y == gy && x.abs_diff(gx) == 1)
            }
        }
    }

    fn heuristic(&self, goal: Goal, x: u32, y: u32) -> u32 {
        let (gx, gy) = goal.target();
        let d = x.abs_diff(gx) + y.abs_diff(gy);
        match goal {
            Goal::Cell { .. } => d,
            Goal::Adjacent { .. } => d.saturating_sub(1),
        }
    }

    /// Local A* from a standing cell to the goal, through cells whose region is in the
    /// sorted `corridor` (or anywhere if it is empty), giving up at [`LOCAL_NODE_CAP`].
    pub fn local_path(&mut self, start: (u32, u32), goal: Goal, corridor: &[u32]) -> Option<Path> {
        if !self.is_standing(start.0, start.1) {
            return None;
        }
        let stamp = self.next_stamp();
        let si = self.idx(start.0, start.1);
        self.g[si] = 0;
        self.mark[si] = stamp;
        self.parent[si] = si as u32;
        let mut heap: BinaryHeap<Reverse<(u32, u32, u32)>> = BinaryHeap::new();
        heap.push(Reverse((
            self.heuristic(goal, start.0, start.1),
            0,
            si as u32,
        )));
        let mut expanded = 0usize;
        let mut moves = Vec::with_capacity(6);
        while let Some(Reverse((_, g, i))) = heap.pop() {
            let i = i as usize;
            if g > self.g[i] {
                continue;
            }
            let (x, y) = ((i as u32) % self.w, (i as u32) / self.w);
            if self.reaches(goal, x, y) {
                return Some(self.build_path(si, i));
            }
            expanded += 1;
            if expanded > LOCAL_NODE_CAP {
                return None;
            }
            self.moves(x, y, &mut moves);
            for &((nx, ny), cost) in &moves {
                let j = self.idx(nx, ny);
                if !corridor.is_empty() && corridor.binary_search(&self.region[j]).is_err() {
                    continue;
                }
                let ng = g + cost;
                if self.mark[j] != stamp || ng < self.g[j] {
                    self.mark[j] = stamp;
                    self.g[j] = ng;
                    self.parent[j] = i as u32;
                    heap.push(Reverse((ng + self.heuristic(goal, nx, ny), ng, j as u32)));
                }
            }
        }
        None
    }

    fn build_path(&self, start: usize, end: usize) -> Path {
        let mut cells = Vec::new();
        let mut chunks = Vec::new();
        let mut i = end;
        while i != start {
            let (x, y) = ((i as u32) % self.w, (i as u32) / self.w);
            cells.push((x, y));
            chunks.push(self.chunks.index(x as usize / CHUNK, y as usize / CHUNK) as u32);
            i = self.parent[i] as usize;
        }
        let (sx, sy) = ((start as u32) % self.w, (start as u32) / self.w);
        chunks.push(self.chunks.index(sx as usize / CHUNK, sy as usize / CHUNK) as u32);
        chunks.sort_unstable();
        chunks.dedup();
        Path { cells, chunks }
    }

    /// A per-cell mask of the standing cells reachable from the seed region whose
    /// reachable set holds the most standing cells (a benchmark helper: "the biggest
    /// cavern"). Regions already reached by an earlier seed are not re-seeded.
    pub fn largest_component(&mut self) -> Vec<bool> {
        let n_regions = self.block_count() * REGIONS_PER_BLOCK as usize;
        let mut cells_per_region = vec![0u32; n_regions];
        for r in &self.region {
            if *r != NO_REGION {
                cells_per_region[*r as usize] += 1;
            }
        }
        let mut visited = vec![false; n_regions];
        let mut best: Option<(u32, Vec<u32>)> = None;
        for seed in 0..n_regions as u32 {
            if cells_per_region[seed as usize] == 0 || visited[seed as usize] {
                continue;
            }
            let stamp = self.dijkstra_regions(seed, u32::MAX);
            let reached: Vec<u32> = (0..n_regions as u32)
                .filter(|r| self.rmark[*r as usize] == stamp)
                .collect();
            let size: u32 = reached.iter().map(|r| cells_per_region[*r as usize]).sum();
            for r in &reached {
                visited[*r as usize] = true;
            }
            if best.as_ref().is_none_or(|(bs, _)| size > *bs) {
                best = Some((size, reached));
            }
        }
        let mut in_best = vec![false; n_regions];
        if let Some((_, reached)) = best {
            for r in reached {
                in_best[r as usize] = true;
            }
        }
        self.region
            .iter()
            .map(|r| *r != NO_REGION && in_best[*r as usize])
            .collect()
    }

    /// Standing cells a goal can be reached from.
    pub fn goal_cells(&self, goal: Goal) -> Vec<(u32, u32)> {
        match goal {
            Goal::Cell { x, y } => {
                if x < self.w && y < self.h && self.is_standing(x, y) {
                    vec![(x, y)]
                } else {
                    Vec::new()
                }
            }
            Goal::Adjacent { x, y } => {
                let mut out = Vec::new();
                for (nx, ny) in [
                    (x, y.wrapping_sub(1)),
                    (x.wrapping_sub(1), y),
                    (x + 1, y),
                    (x, y + 1),
                ] {
                    if nx < self.w && ny < self.h && self.is_standing(nx, ny) {
                        out.push((nx, ny));
                    }
                }
                out
            }
        }
    }

    /// Sorted regions of the goal's standing cells.
    pub fn goal_regions(&self, goal: Goal) -> Vec<u32> {
        let mut r: Vec<u32> = self
            .goal_cells(goal)
            .iter()
            .map(|&(x, y)| self.region_of(x, y))
            .filter(|r| *r != NO_REGION)
            .collect();
        r.sort_unstable();
        r.dedup();
        r
    }

    /// Full pathfinding from a standing cell: region A* then corridor-restricted local
    /// A*; nearby goals skip the region stage.
    pub fn path_to(&mut self, start: (u32, u32), goal: Goal) -> Option<Path> {
        if !self.is_standing(start.0, start.1) {
            return None;
        }
        let goals = self.goal_regions(goal);
        if goals.is_empty() {
            return None;
        }
        let start_block = self.block_of(start.0, start.1);
        let (gx, gy) = goal.target();
        let goal_block = self.block_of(gx.min(self.w - 1), gy.min(self.h - 1));
        let near = {
            let (ax, ay) = (start_block % self.bcols, start_block / self.bcols);
            let (bx, by) = (goal_block % self.bcols, goal_block / self.bcols);
            ax.abs_diff(bx) <= 1 && ay.abs_diff(by) <= 1
        };
        if near {
            let (ax, ay) = (start_block % self.bcols, start_block / self.bcols);
            let mut corridor = Vec::new();
            for ny in ay.saturating_sub(1)..=(ay + 1).min(self.brows - 1) {
                for nx in ax.saturating_sub(1)..=(ax + 1).min(self.bcols - 1) {
                    let b = ny * self.bcols + nx;
                    let n = self.region_count[b as usize] as u32;
                    for local in 0..n {
                        corridor.push(b * REGIONS_PER_BLOCK + local);
                    }
                }
            }
            corridor.sort_unstable();
            if let Some(p) = self.local_path(start, goal, &corridor) {
                return Some(p);
            }
        }
        let start_region = self.region_of(start.0, start.1);
        let (_, corridor) = self.region_path(start_region, &goals)?;
        self.local_path(start, goal, &corridor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::builtin;
    use crate::fixed::Temp;
    use crate::scenario;
    use crate::sim::Sim;

    /// Builds a world from rows of characters, top row first: `#` sandstone, `.` oxygen,
    /// `~` deep water, `-` shallow water.
    fn world_from(rows: &[&str]) -> Sim {
        let h = rows.len();
        let w = rows[0].len();
        let mut sim = Sim::new(ElementTable::builtin(), w, h);
        for (r, row) in rows.iter().enumerate() {
            let y = h - 1 - r;
            for (x, ch) in row.chars().enumerate() {
                match ch {
                    '#' => sim.set_cell(
                        x,
                        y,
                        builtin::SANDSTONE,
                        Mass::from_kg(100),
                        Temp::from_kelvin(300),
                    ),
                    '.' => sim.set_cell(
                        x,
                        y,
                        builtin::OXYGEN,
                        Mass::from_kg(1),
                        Temp::from_kelvin(300),
                    ),
                    '~' => sim.set_cell(
                        x,
                        y,
                        builtin::WATER,
                        Mass::from_kg(600),
                        Temp::from_kelvin(290),
                    ),
                    '-' => sim.set_cell(
                        x,
                        y,
                        builtin::WATER,
                        Mass::from_kg(100),
                        Temp::from_kelvin(290),
                    ),
                    _ => panic!("unknown cell {ch}"),
                }
            }
        }
        sim
    }

    fn nav_of(sim: &Sim) -> NavGrid {
        let mut nav = NavGrid::new(sim.world().width as u32, sim.world().height as u32);
        nav.refresh_all(&sim.world().cells, sim.table());
        nav
    }

    #[test]
    fn flags_standing_drops_and_deep_liquid() {
        // y=5 ........
        // y=4 ..#.....
        // y=3 ..#..~..
        // y=2 ..#..~..
        // y=1 ##...~..
        // y=0 ########
        let sim = world_from(&[
            "........", "..#.....", "..#..~..", "..#..~..", "##...~..", "########",
        ]);
        let nav = nav_of(&sim);
        assert!(
            nav.is_standing(0, 2) && nav.is_standing(1, 2),
            "on the ledge"
        );
        assert!(nav.is_standing(2, 5), "top of the pillar");
        assert!(nav.is_standing(3, 1) && nav.is_standing(4, 1));
        assert!(
            !nav.is_passable(5, 1) && !nav.is_standing(5, 1),
            "deep water is impassable"
        );
        assert!(!nav.is_standing(6, 2), "mid-air");
        let mut moves = Vec::new();
        nav.moves(2, 5, &mut moves);
        assert!(
            moves.contains(&((1, 2), 3)),
            "a three-cell drop to the ledge: {moves:?}"
        );
        assert!(
            moves.contains(&((3, 1), 4)),
            "a four-cell drop to the floor: {moves:?}"
        );
        nav.moves(1, 2, &mut moves);
        assert!(moves.contains(&((0, 2), 1)));
        assert!(
            !moves.iter().any(|(d, _)| *d == (2, 5)),
            "no climbing a three-cell wall"
        );
        assert_eq!(
            moves,
            vec![((0, 2), 1)],
            "the pillar blocks the ledge on the right"
        );
    }

    #[test]
    fn shallow_water_is_walkable_and_regions_are_pure_functions_of_cells() {
        let sim = world_from(&["......", "..#...", "-#.#..", "######"]);
        let nav = nav_of(&sim);
        assert!(nav.is_standing(0, 1), "100 kg water is walkable");
        assert_eq!(nav.region_of(0, 1), 0, "first region in row-major order");
        let mut lazy = NavGrid::new(6, 4);
        lazy.refresh_all(&sim.world().cells, sim.table());
        assert_eq!(lazy.regions(), nav.regions());
        assert_eq!(lazy.flags(), nav.flags());
    }

    #[test]
    fn local_path_climbs_steps_and_drops() {
        // y=3 ......
        // y=2 ....#.
        // y=1 ...##.
        // y=0 ######
        let sim = world_from(&["......", "....#.", "...##.", "######"]);
        let mut nav = nav_of(&sim);
        let path = nav
            .path_to((0, 1), Goal::Cell { x: 4, y: 3 })
            .expect("a path up the stairs");
        let mut at = (0, 1);
        let mut p = path.clone();
        while let Some(next) = p.advance() {
            assert!(nav.is_legal_move(at, next), "{at:?} -> {next:?}");
            at = next;
        }
        assert_eq!(at, (4, 3));
        assert_eq!(path.len(), 4, "0,1 -> 1,1 -> 2,1 -> 3,2 -> 4,3");
        let down = nav
            .path_to((4, 3), Goal::Cell { x: 5, y: 1 })
            .expect("a drop");
        assert_eq!(down.len(), 1);
        assert!(
            nav.path_to((5, 1), Goal::Cell { x: 4, y: 3 }).is_none(),
            "a two-cell wall cannot be climbed"
        );
        let adjacent = nav
            .path_to((0, 1), Goal::Adjacent { x: 3, y: 1 })
            .expect("stand next to the step");
        assert_eq!(adjacent.cells.first().copied(), Some((2, 1)));
    }

    #[test]
    fn region_graph_connects_blocks_and_corridor_paths_match_region_paths() {
        let mut sim = scenario::cave(96, 64);
        for _ in 0..5 {
            sim.step();
        }
        let mut nav = nav_of(&sim);
        let standing: Vec<(u32, u32)> = (0..64)
            .flat_map(|y| (0..96).map(move |x| (x, y)))
            .filter(|&(x, y)| nav.is_standing(x, y))
            .collect();
        assert!(standing.len() > 100);
        let mut rng = crate::rng::Pcg32::new(3, 9);
        let mut reachable = 0;
        for _ in 0..200 {
            let a = standing[rng.below(standing.len() as u32) as usize];
            let b = standing[rng.below(standing.len() as u32) as usize];
            let ra = nav.region_of(a.0, a.1);
            let rb = nav.region_of(b.0, b.1);
            let region_ok = nav.region_path(ra, &[rb]).is_some();
            let path = nav.path_to(a, Goal::Cell { x: b.0, y: b.1 });
            assert_eq!(region_ok, path.is_some(), "{a:?} -> {b:?}");
            if let Some(p) = path {
                reachable += 1;
                let mut at = a;
                let mut p = p;
                while let Some(next) = p.advance() {
                    assert!(nav.is_legal_move(at, next), "{at:?} -> {next:?}");
                    at = next;
                }
                assert_eq!(at, b);
            }
        }
        assert!(reachable > 0);
    }

    #[test]
    fn lazy_refresh_after_edits_equals_a_full_rebuild() {
        let mut sim = scenario::cave(96, 64);
        let mut nav = nav_of(&sim);
        for _ in 0..30 {
            sim.step();
            nav.refresh(&sim.world().cells, sim.table(), sim.processed_chunks());
        }
        sim.fill_rect(
            40,
            20,
            50,
            26,
            builtin::SANDSTONE,
            Mass::from_kg(100),
            Temp::from_kelvin(300),
        );
        sim.fill_rect(
            20,
            30,
            30,
            33,
            builtin::OXYGEN,
            Mass::from_kg(1),
            Temp::from_kelvin(300),
        );
        sim.step();
        nav.refresh(&sim.world().cells, sim.table(), sim.processed_chunks());
        let full = nav_of(&sim);
        assert_eq!(nav.flags(), full.flags());
        assert_eq!(nav.regions(), full.regions());
        for b in 0..full.block_count() as u32 {
            assert_eq!(nav.edges_of_block(b), full.edges_of_block(b), "block {b}");
        }
        assert!(nav.changed_chunks().iter().any(|c| *c));
    }
}
