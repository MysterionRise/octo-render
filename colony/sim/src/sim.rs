//! The simulation: world buffers, chunk scheduling, and the tick.
//!
//! A tick runs three gather passes (buoyancy sort, flux, heat). Each pass reads the
//! current buffer, writes the back buffer, then the two are swapped. Chunk activity is
//! tracked separately for the mass passes (sort and flux, which never read
//! temperature) and for the heat pass (which reads everything), so a solid region
//! that is only conducting heat costs nothing in the mass passes, and a chunk whose
//! 3×3 neighbourhood had no relevant activity last tick is copied instead of computed.

use crate::chunk::{ChunkGrid, CHUNK};
use crate::element::{ElementId, ElementTable};
use crate::fixed::{Mass, Temp};
use crate::hash::Fnv1a;
use crate::heat::capacity;
use crate::world::{CellBuffers, CellOut, Grid, World};
use crate::{fluid, heat};

const ACTIVE_EXCHANGE: u8 = 1;
const ACTIVE_PHASE: u8 = 2;

/// What one tick did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickStats {
    /// The tick number that was just completed (first tick is 0).
    pub tick: u64,
    /// Chunks visited by at least one pass (the heat pass visits a superset of the
    /// chunks the mass passes visit).
    pub chunks_processed: usize,
    pub chunks_skipped: usize,
    /// Chunks visited by the sort and flux passes.
    pub chunks_processed_mass: usize,
}

/// Which activity set a pass reads and feeds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PassKind {
    Mass,
    Heat,
}

/// A running simulation.
#[derive(Clone, Debug)]
pub struct Sim {
    table: ElementTable,
    world: World,
    back: CellBuffers,
    chunks: ChunkGrid,
    /// Chunks where the mass passes exchanged something during the previous tick.
    active_mass: Vec<bool>,
    /// Chunks where the heat pass exchanged something during the previous tick.
    active_heat: Vec<bool>,
    process_mass: Vec<bool>,
    process_heat: Vec<bool>,
    /// Per chunk, bit 0: the last pass exchanged something; bit 1: an element changed phase.
    pass_activity: Vec<u8>,
    tick_activity_mass: Vec<bool>,
    tick_activity_heat: Vec<bool>,
    scratch: Vec<bool>,
    tick: u64,
    parallel: bool,
}

impl Sim {
    /// An all-vacuum world. Every chunk starts active so the first tick visits it.
    pub fn new(table: ElementTable, width: usize, height: usize) -> Sim {
        let world = World::new(width, height);
        let chunks = ChunkGrid::new(width, height);
        let n = chunks.len();
        Sim {
            back: CellBuffers::new(world.cells.len()),
            table,
            world,
            chunks,
            active_mass: vec![true; n],
            active_heat: vec![true; n],
            process_mass: vec![false; n],
            process_heat: vec![false; n],
            pass_activity: vec![0; n],
            tick_activity_mass: vec![false; n],
            tick_activity_heat: vec![false; n],
            scratch: vec![false; n],
            tick: 0,
            parallel: cfg!(feature = "parallel"),
        }
    }

    pub fn table(&self) -> &ElementTable {
        &self.table
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn chunks(&self) -> ChunkGrid {
        self.chunks
    }

    pub fn tick_count(&self) -> u64 {
        self.tick
    }

    /// Whether passes run on the rayon pool. Results are identical either way.
    pub fn set_parallel(&mut self, parallel: bool) {
        self.parallel = parallel && cfg!(feature = "parallel");
    }

    pub fn is_parallel(&self) -> bool {
        self.parallel
    }

    /// Writes a cell and marks its chunk active so the next tick visits it.
    pub fn set_cell(&mut self, x: usize, y: usize, element: ElementId, mass: Mass, temp: Temp) {
        self.world.set(&self.table, x, y, element, mass, temp);
        let idx = self.chunks.index(x / CHUNK, y / CHUNK);
        self.active_mass[idx] = true;
        self.active_heat[idx] = true;
    }

    /// Fills the half-open rectangle `[x0, x1) × [y0, y1)`.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_rect(
        &mut self,
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        element: ElementId,
        mass: Mass,
        temp: Temp,
    ) {
        for y in y0..y1 {
            for x in x0..x1 {
                self.set_cell(x, y, element, mass, temp);
            }
        }
    }

    /// Runs one tick: buoyancy sort (which also records pressure flags), flux, heat.
    pub fn step(&mut self) -> TickStats {
        self.begin_tick();
        self.pass_sort();
        self.pass_flux();
        self.pass_heat();
        self.end_tick()
    }

    /// First phase of a tick: decides which chunks each pass visits. The phases are
    /// public so a runner can time them; call them in order or use [`Sim::step`].
    pub fn begin_tick(&mut self) {
        self.chunks
            .process_from_active(&self.active_mass, &mut self.process_mass);
        for ((s, m), h) in self
            .scratch
            .iter_mut()
            .zip(&self.active_mass)
            .zip(&self.active_heat)
        {
            *s = *m | *h;
        }
        self.chunks
            .process_from_active(&self.scratch, &mut self.process_heat);
        self.tick_activity_mass.fill(false);
        self.tick_activity_heat.fill(false);
    }

    pub fn pass_sort(&mut self) {
        self.run_pass(PassKind::Mass, fluid::sort_cell);
    }

    pub fn pass_flux(&mut self) {
        self.run_pass(PassKind::Mass, fluid::flux_cell);
    }

    pub fn pass_heat(&mut self) {
        self.run_pass(PassKind::Heat, heat::conduct_cell);
    }

    /// Last phase of a tick: publishes activity for the next tick and counts the tick.
    pub fn end_tick(&mut self) -> TickStats {
        let processed = self.process_heat.iter().filter(|p| **p).count();
        let processed_mass = self.process_mass.iter().filter(|p| **p).count();
        std::mem::swap(&mut self.active_mass, &mut self.tick_activity_mass);
        std::mem::swap(&mut self.active_heat, &mut self.tick_activity_heat);
        let stats = TickStats {
            tick: self.tick,
            chunks_processed: processed,
            chunks_skipped: self.chunks.len() - processed,
            chunks_processed_mass: processed_mass,
        };
        self.tick += 1;
        stats
    }

    fn run_pass<F>(&mut self, kind: PassKind, f: F)
    where
        F: Fn(&Grid, usize, usize) -> CellOut + Sync,
    {
        let Sim {
            table,
            world,
            back,
            chunks,
            process_mass,
            process_heat,
            pass_activity,
            tick_activity_mass,
            tick_activity_heat,
            parallel,
            ..
        } = self;
        let process = match kind {
            PassKind::Mass => &*process_mass,
            PassKind::Heat => &*process_heat,
        };
        let grid = Grid::new(world, table);
        sweep(&grid, *chunks, process, back, pass_activity, *parallel, &f);
        for (i, p) in pass_activity.iter().enumerate() {
            let exchanged = p & ACTIVE_EXCHANGE != 0;
            let changed = p & ACTIVE_PHASE != 0;
            match kind {
                PassKind::Mass => tick_activity_mass[i] |= exchanged | changed,
                PassKind::Heat => tick_activity_heat[i] |= exchanged | changed,
            }
            if changed {
                tick_activity_mass[i] = true;
                tick_activity_heat[i] = true;
            }
        }
        std::mem::swap(&mut world.cells, back);
    }

    /// Sum of all cell masses in raw units. Exactly constant across ticks.
    pub fn total_mass(&self) -> u64 {
        self.world.cells.mass.iter().map(|m| *m as u64).sum()
    }

    /// Sum of heat capacity × temperature over all cells, in Q32.32 joules.
    pub fn total_energy(&self) -> i128 {
        let grid = Grid::new(&self.world, &self.table);
        let mut total: i128 = 0;
        for y in 0..self.world.height {
            for x in 0..self.world.width {
                let c = grid.cell(x, y);
                total += capacity(&grid, &c) as i128 * c.temp as i128;
            }
        }
        total
    }

    /// Sum of heat capacities, the per-tick bound on energy-ledger drift (Q16.16 J/K).
    pub fn total_capacity(&self) -> i128 {
        let grid = Grid::new(&self.world, &self.table);
        let mut total: i128 = 0;
        for y in 0..self.world.height {
            for x in 0..self.world.width {
                total += capacity(&grid, &grid.cell(x, y)) as i128;
            }
        }
        total
    }

    /// FNV-1a over the tick number and every cell array, for replay and golden tests.
    pub fn state_hash(&self) -> u64 {
        let mut h = Fnv1a::new();
        h.write_u64(self.tick);
        h.write_u64(self.world.width as u64);
        h.write_u64(self.world.height as u64);
        h.write_u16s(&self.world.cells.element);
        h.write_u32s(&self.world.cells.mass);
        h.write_i32s(&self.world.cells.temp);
        h.write_bytes(&self.world.cells.flags);
        h.finish()
    }
}

/// One band's slices of the write buffers plus its chunks' activity flags.
struct BandOut<'a> {
    el: &'a mut [u16],
    mass: &'a mut [u32],
    temp: &'a mut [i32],
    flags: &'a mut [u8],
    act: &'a mut [u8],
}

/// Runs `f` over every processed cell, copies skipped chunks, and records per-chunk
/// activity. Bands are chunk rows, so each band owns its chunks' activity flags.
#[allow(clippy::too_many_arguments)]
fn sweep<F>(
    grid: &Grid,
    chunks: ChunkGrid,
    process: &[bool],
    out: &mut CellBuffers,
    activity: &mut [u8],
    parallel: bool,
    f: &F,
) where
    F: Fn(&Grid, usize, usize) -> CellOut + Sync,
{
    let band_cells = CHUNK * grid.width();
    let cols = chunks.cols;
    #[cfg(feature = "parallel")]
    if parallel {
        use rayon::prelude::*;
        out.element
            .par_chunks_mut(band_cells)
            .zip(out.mass.par_chunks_mut(band_cells))
            .zip(out.temp.par_chunks_mut(band_cells))
            .zip(out.flags.par_chunks_mut(band_cells))
            .zip(activity.par_chunks_mut(cols))
            .enumerate()
            .for_each(|(band, ((((el, mass), temp), flags), act))| {
                let target = BandOut {
                    el,
                    mass,
                    temp,
                    flags,
                    act,
                };
                sweep_band(grid, chunks, process, band, target, f)
            });
        return;
    }
    let _ = parallel;
    for (band, ((((el, mass), temp), flags), act)) in out
        .element
        .chunks_mut(band_cells)
        .zip(out.mass.chunks_mut(band_cells))
        .zip(out.temp.chunks_mut(band_cells))
        .zip(out.flags.chunks_mut(band_cells))
        .zip(activity.chunks_mut(cols))
        .enumerate()
    {
        let target = BandOut {
            el,
            mass,
            temp,
            flags,
            act,
        };
        sweep_band(grid, chunks, process, band, target, f);
    }
}

fn sweep_band<F>(grid: &Grid, chunks: ChunkGrid, process: &[bool], band: usize, out: BandOut, f: &F)
where
    F: Fn(&Grid, usize, usize) -> CellOut,
{
    let BandOut {
        el,
        mass,
        temp,
        flags,
        act,
    } = out;
    let w = grid.width();
    let y0 = band * CHUNK;
    let y1 = (y0 + CHUNK).min(grid.height());
    let base = y0 * w;
    for cx in 0..chunks.cols {
        let x0 = cx * CHUNK;
        let x1 = (x0 + CHUNK).min(w);
        let run = process[chunks.index(cx, band)];
        let mut active: u8 = 0;
        for y in y0..y1 {
            let row = y * w;
            if run {
                for x in x0..x1 {
                    let o = f(grid, x, y);
                    let i = row + x - base;
                    el[i] = o.element;
                    mass[i] = o.mass;
                    temp[i] = o.temp;
                    flags[i] = o.flags;
                    active |= ((o.active as u8) * ACTIVE_EXCHANGE)
                        | ((o.phase_changed as u8) * ACTIVE_PHASE);
                }
            } else {
                let (s, e) = (row + x0 - base, row + x1 - base);
                el[s..e].copy_from_slice(&grid.element()[s + base..e + base]);
                mass[s..e].copy_from_slice(&grid.mass()[s + base..e + base]);
                temp[s..e].copy_from_slice(&grid.temp()[s + base..e + base]);
                flags[s..e].copy_from_slice(&grid.flags()[s + base..e + base]);
            }
        }
        act[cx] = active;
    }
}
