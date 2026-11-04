//! The simulation: world buffers, chunk scheduling, and the tick.
//!
//! A tick runs three gather passes (buoyancy sort, flux, heat). Each pass reads the
//! current buffer, writes the back buffer, then the two are swapped. Chunk activity is
//! tracked separately for the mass passes (sort and flux, which never read
//! temperature) and for the heat pass (which reads everything), so a solid region
//! that is only conducting heat costs nothing in the mass passes, and a chunk whose
//! 3×3 neighbourhood had no relevant activity last tick is copied instead of computed.

use crate::agent::{Colonist, DEFAULT_PRIORITY};
use crate::building::{Building, BuildingTable};
use crate::chunk::{ChunkGrid, CHUNK};
use crate::command::{Command, LogEntry};
use crate::element::{ElementId, ElementTable};
use crate::entity::Arena;
use crate::fixed::{Mass, Temp};
use crate::hash::Fnv1a;
use crate::heat::capacity;
use crate::item::Item;
use crate::job::{JobBoard, JobCounts, JobKind};
use crate::nav::NavGrid;
use crate::network::{NetworkStats, Networks};
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
    /// Path solves (assignment scans and route planning) the agents pass spent.
    pub path_solves: u32,
    pub jobs: JobCounts,
    pub colonists: u32,
    pub buildings: u32,
    pub networks: NetworkStats,
}

impl TickStats {
    /// Stats for a world that has not stepped yet: every chunk counts as processed.
    pub fn fresh(chunks: usize, tick: u64) -> TickStats {
        TickStats {
            tick,
            chunks_processed: chunks,
            chunks_skipped: 0,
            chunks_processed_mass: chunks,
            path_solves: 0,
            jobs: JobCounts::default(),
            colonists: 0,
            buildings: 0,
            networks: NetworkStats::default(),
        }
    }
}

/// Everything a simulation is rebuilt from (a loaded snapshot).
pub struct Parts {
    pub table: ElementTable,
    pub building_table: BuildingTable,
    pub world: World,
    pub tick: u64,
    pub colonists: Arena<Colonist>,
    pub items: Arena<Item>,
    pub jobs: JobBoard,
    pub buildings: Arena<Building>,
    /// Energy heaters and generators have injected so far (Q32.32 joules).
    pub energy_injected: i128,
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
    pub(crate) table: ElementTable,
    pub(crate) world: World,
    back: CellBuffers,
    chunks: ChunkGrid,
    /// Chunks where the mass passes exchanged something during the previous tick.
    active_mass: Vec<bool>,
    /// Chunks where the heat pass exchanged something during the previous tick.
    active_heat: Vec<bool>,
    process_mass: Vec<bool>,
    pub(crate) process_heat: Vec<bool>,
    /// Per chunk, bit 0: the last pass exchanged something; bit 1: an element changed phase.
    pass_activity: Vec<u8>,
    tick_activity_mass: Vec<bool>,
    tick_activity_heat: Vec<bool>,
    scratch: Vec<bool>,
    pub(crate) tick: u64,
    parallel: bool,
    pending: Vec<Command>,
    log: Vec<LogEntry>,
    record: bool,
    pub(crate) colonists: Arena<Colonist>,
    pub(crate) items: Arena<Item>,
    pub(crate) jobs: JobBoard,
    pub(crate) nav: NavGrid,
    last_path_solves: u32,
    pub(crate) building_table: BuildingTable,
    pub(crate) buildings: Arena<Building>,
    /// Per cell, the arena slot of the building occupying it, or `NO_BUILDING`.
    pub(crate) cell_building: Vec<u32>,
    /// Per cell, the `nav::SPECIAL_*` bits derived from buildings.
    pub(crate) special: Vec<u8>,
    /// Set when buildings were placed, removed, or finished, so networks rebuild.
    pub(crate) networks_dirty: bool,
    pub(crate) networks: Networks,
    pub(crate) energy_injected: i128,
}

impl Sim {
    /// An all-vacuum world with the builtin buildings. Every chunk starts active so the
    /// first tick visits it.
    pub fn new(table: ElementTable, width: usize, height: usize) -> Sim {
        Sim::new_with(table, BuildingTable::builtin(), width, height)
    }

    /// An all-vacuum world with the given building table.
    pub fn new_with(
        table: ElementTable,
        building_table: BuildingTable,
        width: usize,
        height: usize,
    ) -> Sim {
        Sim::from_parts_with(Parts {
            table,
            building_table,
            world: World::new(width, height),
            tick: 0,
            colonists: Arena::new(),
            items: Arena::new(),
            jobs: JobBoard::new(),
            buildings: Arena::new(),
            energy_injected: 0,
        })
    }

    /// Wraps existing cell state (a loaded snapshot) at a given tick with no entities and
    /// the builtin buildings.
    pub fn from_parts(table: ElementTable, world: World, tick: u64) -> Sim {
        Sim::from_parts_with(Parts {
            table,
            building_table: BuildingTable::builtin(),
            world,
            tick,
            colonists: Arena::new(),
            items: Arena::new(),
            jobs: JobBoard::new(),
            buildings: Arena::new(),
            energy_injected: 0,
        })
    }

    /// Wraps existing cell and entity state at a given tick. Every chunk starts active,
    /// which gives the same results as the activity the snapshot had; the occupancy map,
    /// walkability, and networks are derived from the entities.
    pub fn from_parts_with(parts: Parts) -> Sim {
        let Parts {
            table,
            building_table,
            world,
            tick,
            colonists,
            items,
            jobs,
            buildings,
            energy_injected,
        } = parts;
        let chunks = ChunkGrid::new(world.width, world.height);
        let n = chunks.len();
        let cells = world.cells.len();
        let mut sim = Sim {
            back: CellBuffers::new(cells),
            nav: NavGrid::new(world.width as u32, world.height as u32),
            table,
            world,
            chunks,
            active_mass: vec![true; n],
            active_heat: vec![true; n],
            process_mass: vec![true; n],
            process_heat: vec![true; n],
            pass_activity: vec![0; n],
            tick_activity_mass: vec![false; n],
            tick_activity_heat: vec![false; n],
            scratch: vec![false; n],
            tick,
            parallel: cfg!(feature = "parallel"),
            pending: Vec::new(),
            log: Vec::new(),
            record: true,
            colonists,
            items,
            jobs,
            last_path_solves: 0,
            building_table,
            buildings,
            cell_building: vec![crate::construct::NO_BUILDING; cells],
            special: vec![0; cells],
            networks_dirty: true,
            networks: Networks::default(),
            energy_injected,
        };
        sim.rebuild_occupancy();
        sim.nav
            .refresh_all(&sim.world.cells, &sim.table, &sim.special);
        // A tick boundary never carries walkability changes, so a loaded world must not either.
        sim.nav.clear_changed();
        sim
    }

    pub fn colonists(&self) -> &Arena<Colonist> {
        &self.colonists
    }

    pub fn items(&self) -> &Arena<Item> {
        &self.items
    }

    pub fn jobs(&self) -> &JobBoard {
        &self.jobs
    }

    pub fn nav(&self) -> &NavGrid {
        &self.nav
    }

    /// Mutable access for pathfinding queries, which use the grid's scratch arrays.
    pub fn nav_mut(&mut self) -> &mut NavGrid {
        &mut self.nav
    }

    /// Recomputes walkability from the current cells (scenario builders use it before
    /// the first tick; the agents pass keeps it fresh afterwards).
    pub fn refresh_nav(&mut self) {
        self.nav
            .refresh_all(&self.world.cells, &self.table, &self.special);
    }

    /// Queues a command for the start of the next tick. Commands apply in the order
    /// they were queued and are logged with that tick.
    pub fn queue(&mut self, command: Command) {
        self.pending.push(command);
    }

    /// Every command applied so far, with its tick.
    pub fn log(&self) -> &[LogEntry] {
        &self.log
    }

    /// Whether applied commands are kept in the log (on by default).
    pub fn set_recording(&mut self, record: bool) {
        self.record = record;
    }

    /// Replays a log: queues each entry at its tick while stepping to `until_tick`.
    /// Entries before the current tick are applied immediately.
    pub fn replay(&mut self, log: &[LogEntry], until_tick: u64) {
        let mut next = 0;
        while self.tick < until_tick {
            while next < log.len() && log[next].tick <= self.tick {
                self.queue(log[next].command);
                next += 1;
            }
            self.step();
        }
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::SetCell {
                x,
                y,
                element,
                mass,
                temp,
            } => self.set_cell(x, y, element, mass, temp),
            Command::FillRect {
                x0,
                y0,
                x1,
                y1,
                element,
                mass,
                temp,
            } => self.fill_rect(x0, y0, x1, y1, element, mass, temp),
            Command::Dig { x, y } => {
                self.order(
                    JobKind::Dig {
                        x: x as u32,
                        y: y as u32,
                    },
                    DEFAULT_PRIORITY,
                );
            }
            Command::Build { x, y } => {
                self.order(
                    JobKind::Build {
                        x: x as u32,
                        y: y as u32,
                    },
                    DEFAULT_PRIORITY,
                );
            }
            Command::CancelOrders { x, y } => {
                self.cancel_orders(x as u32, y as u32);
            }
            Command::SpawnColonist { x, y } => {
                self.spawn_colonist(x, y);
            }
            Command::SetPriority { x, y, priority } => {
                self.set_priority(x as u32, y as u32, priority);
            }
            Command::Place { def, x, y } => {
                self.place(def, x, y);
            }
            Command::Deconstruct { x, y } => {
                self.deconstruct(x, y);
            }
            Command::Toggle { x, y } => {
                self.toggle_building(x, y);
            }
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

    /// Writes a cell and marks its chunk active so the next tick visits it. Both the
    /// pending and the accumulating activity sets are marked, so the mark survives
    /// whether the write happens before the passes (a command) or after them (an
    /// agent's dig or build).
    pub fn set_cell(&mut self, x: usize, y: usize, element: ElementId, mass: Mass, temp: Temp) {
        self.world.set(&self.table, x, y, element, mass, temp);
        self.touch_chunk(x, y);
    }

    /// Marks the chunk of a cell active in both activity sets without writing the cell
    /// (a building was placed or removed there, so walkability must be refreshed).
    pub fn touch_chunk(&mut self, x: usize, y: usize) {
        let idx = self.chunks.index(x / CHUNK, y / CHUNK);
        self.active_mass[idx] = true;
        self.active_heat[idx] = true;
        self.tick_activity_mass[idx] = true;
        self.tick_activity_heat[idx] = true;
    }

    /// Adds `joules` (Q16.16, may be negative) to a cell's energy by changing its
    /// temperature. Returns the energy actually added (Q32.32, the temperature step
    /// times the capacity; zero when the cell has no heat capacity), which the caller
    /// accounts in `energy_injected`.
    pub fn add_heat(&mut self, x: usize, y: usize, joules: i64) -> i128 {
        let (element, mass, temp) = self.world.get(x, y);
        let cap = (mass.raw() as i64 * self.table.shc(element.0)) >> 16;
        if cap <= 0 {
            return 0;
        }
        let delta = ((joules as i128) << 16) / cap as i128;
        let new_temp =
            (temp.raw() as i128 + delta).clamp(i32::MIN as i128, i32::MAX as i128) as i32;
        if new_temp == temp.raw() {
            return 0;
        }
        self.world
            .set(&self.table, x, y, element, mass, Temp(new_temp));
        self.touch_chunk(x, y);
        (new_temp as i128 - temp.raw() as i128) * cap as i128
    }

    /// Energy of one cell (Q32.32 joules): capacity times temperature.
    pub fn cell_energy(&self, x: usize, y: usize) -> i128 {
        let (element, mass, temp) = self.world.get(x, y);
        let cap = (mass.raw() as i64 * self.table.shc(element.0)) >> 16;
        cap as i128 * temp.raw() as i128
    }

    /// The networks (power components, pipe runs, automation) as last rebuilt.
    pub fn networks(&self) -> &Networks {
        &self.networks
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

    /// Runs one tick: commands, buildings and networks, buoyancy sort (which also
    /// records pressure flags), flux, heat, agents.
    pub fn step(&mut self) -> TickStats {
        self.begin_tick();
        self.pass_buildings();
        self.pass_sort();
        self.pass_flux();
        self.pass_heat();
        self.pass_agents();
        self.end_tick()
    }

    /// First phase of a tick: decides which chunks each pass visits. The phases are
    /// public so a runner can time them; call them in order or use [`Sim::step`].
    pub fn begin_tick(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        for command in pending {
            self.apply(command);
            if self.record {
                self.log.push(LogEntry {
                    tick: self.tick,
                    command,
                });
            }
        }
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

    /// Buildings and networks: their effects are edits to the front buffer before the
    /// first gather, like commands. Sequential.
    pub fn pass_buildings(&mut self) {
        self.buildings_step();
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

    /// Colonists and items: walkability refresh, gravity, job assignment, movement,
    /// digging and building. Sequential, after the gather passes.
    pub fn pass_agents(&mut self) {
        self.last_path_solves = self.agents_step();
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
            path_solves: self.last_path_solves,
            jobs: self.jobs.counts(),
            colonists: self.colonists.len() as u32,
            buildings: self.buildings.len() as u32,
            networks: self.networks.stats,
        };
        self.jobs.prune(self.tick);
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

    /// Chunks visited by at least one pass during the last tick, in [`ChunkGrid`] order.
    /// A chunk outside this set is bit-identical to the previous tick (skipped chunks
    /// are copied), which is what a renderer needs to upload only what changed. Valid
    /// after [`Sim::step`]; before the first tick it names every chunk.
    pub fn processed_chunks(&self) -> &[bool] {
        &self.process_heat
    }

    /// Sum of all cell, item, building, and packet masses in raw units. Exactly
    /// constant across ticks.
    pub fn total_mass(&self) -> u64 {
        self.world.cells.mass.iter().map(|m| *m as u64).sum::<u64>()
            + self.item_mass()
            + self.building_mass()
    }

    /// Mass held by buildings: their material, their contents, and pipe packets.
    pub fn building_mass(&self) -> u64 {
        self.buildings
            .iter()
            .map(|(_, b)| {
                b.material.map_or(0, |(_, m, _)| m.raw() as u64)
                    + b.stored.map_or(0, |(_, m, _)| m.raw() as u64)
                    + b.packet.map_or(0, |p| p.mass.raw() as u64)
            })
            .sum()
    }

    /// `(mass, element, temperature)` of everything a building holds.
    fn building_contents(b: &Building) -> impl Iterator<Item = (u32, u16, i32)> {
        b.material
            .into_iter()
            .chain(b.stored)
            .map(|(e, m, t)| (m.raw(), e.0, t.raw()))
            .chain(b.packet.map(|p| (p.mass.raw(), p.element.0, p.temp.raw())))
    }

    /// Sum of item masses in raw units (on the ground and carried).
    pub fn item_mass(&self) -> u64 {
        self.items.iter().map(|(_, it)| it.mass.raw() as u64).sum()
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
        for (_, it) in self.items.iter() {
            let cap = (it.mass.raw() as i64 * self.table.shc(it.element.0)) >> 16;
            total += cap as i128 * it.temp.raw() as i128;
        }
        for (_, b) in self.buildings.iter() {
            for (mass, element, temp) in Sim::building_contents(b) {
                let cap = (mass as i64 * self.table.shc(element)) >> 16;
                total += cap as i128 * temp as i128;
            }
        }
        total
    }

    /// Energy heaters and generators have added so far (Q32.32 joules); the energy
    /// ledger is `total_energy − energy_injected`.
    pub fn energy_injected(&self) -> i128 {
        self.energy_injected
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
        for (_, it) in self.items.iter() {
            total += ((it.mass.raw() as i64 * self.table.shc(it.element.0)) >> 16) as i128;
        }
        for (_, b) in self.buildings.iter() {
            for (mass, element, _) in Sim::building_contents(b) {
                total += ((mass as i64 * self.table.shc(element)) >> 16) as i128;
            }
        }
        total
    }

    /// FNV-1a over the tick number and every cell array (no entities).
    pub fn cell_hash(&self) -> u64 {
        self.cell_hasher().finish()
    }

    fn cell_hasher(&self) -> Fnv1a {
        let mut h = Fnv1a::new();
        h.write_u64(self.tick);
        h.write_u64(self.world.width as u64);
        h.write_u64(self.world.height as u64);
        h.write_u16s(&self.world.cells.element);
        h.write_u32s(&self.world.cells.mass);
        h.write_i32s(&self.world.cells.temp);
        h.write_bytes(&self.world.cells.flags);
        h
    }

    /// FNV-1a over the cells and every entity (the same bytes a snapshot writes), for
    /// replay and golden tests.
    pub fn state_hash(&self) -> u64 {
        let mut h = self.cell_hasher();
        let mut entities = Vec::new();
        crate::save::encode_entities(self, &mut entities);
        h.write_bytes(&entities);
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
