//! Snapshots: a versioned little-endian byte layout of the whole state, plus a trailing
//! state hash so a corrupted or edited file is rejected rather than loaded.
//!
//! Layout (format 2): magic `CSIM`, format version `u32`, element-table hash `u64`,
//! width `u64`, height `u64`, tick `u64`, the element (`u16`), mass (`u32`),
//! temperature (`i32`), and flags (`u8`) arrays, then the entities (colonists, items,
//! the job board, written by [`encode_entities`]), then the state hash `u64`. Chunk
//! activity is not saved: a loaded world visits every chunk on its first tick, which
//! produces the same values as skipping would have. The same entity bytes feed
//! [`crate::Sim::state_hash`], so the hash can never cover less than the file.

use crate::agent::{AgentState, Colonist};
use crate::element::{ElementId, ElementTable};
use crate::entity::{Arena, EntityId};
use crate::fixed::{Mass, Temp};
use crate::hash::Fnv1a;
use crate::item::Item;
use crate::job::{Job, JobBoard, JobKind, JobState};
use crate::nav::{Goal, Path};
use crate::sim::Sim;
use crate::world::{CellBuffers, World};

const MAGIC: &[u8; 4] = b"CSIM";
pub const FORMAT_VERSION: u32 = 2;
/// Upper bound on any count read from a file.
const MAX_COUNT: u64 = 1 << 22;

#[derive(Debug, PartialEq, Eq)]
pub enum SaveError {
    BadMagic,
    UnsupportedVersion(u32),
    TableMismatch,
    Truncated,
    HashMismatch,
    BadDimensions,
    BadEntities,
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::BadMagic => write!(f, "not a colony-sim snapshot"),
            SaveError::UnsupportedVersion(v) => write!(f, "snapshot format {v} is not supported"),
            SaveError::TableMismatch => {
                write!(f, "snapshot was written with a different element table")
            }
            SaveError::Truncated => write!(f, "snapshot is truncated"),
            SaveError::HashMismatch => write!(f, "snapshot state hash does not match its contents"),
            SaveError::BadDimensions => write!(f, "snapshot dimensions are invalid"),
            SaveError::BadEntities => write!(f, "snapshot entity records are inconsistent"),
        }
    }
}

impl std::error::Error for SaveError {}

/// FNV-1a over everything in an element table that changes simulation results.
pub fn table_hash(table: &ElementTable) -> u64 {
    let mut h = Fnv1a::new();
    h.write_u64(table.len() as u64);
    for (i, def) in table.defs().iter().enumerate() {
        h.write_bytes(def.name.as_bytes());
        h.write_bytes(&[0, def.phase as u8]);
        h.write_u64(def.shc as u64);
        h.write_u64(def.conduct as u64);
        h.write_u64(def.density as u64);
        h.write_u32s(&[def.max_mass]);
        h.write_i32s(&[def.melt.unwrap_or(i32::MIN), def.boil.unwrap_or(i32::MIN)]);
        h.write_u16s(&[
            def.solid_of.map_or(u16::MAX, |e| e.0),
            def.liquid_of.map_or(u16::MAX, |e| e.0),
            def.gas_of.map_or(u16::MAX, |e| e.0),
        ]);
        h.write_u64(table.id(i).0 as u64);
    }
    h.finish()
}

// ---- writing ----

fn put_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_id(out: &mut Vec<u8>, id: EntityId) {
    put_u32(out, id.index);
    put_u32(out, id.generation);
}

fn put_cell(out: &mut Vec<u8>, c: (u32, u32)) {
    put_u32(out, c.0);
    put_u32(out, c.1);
}

fn put_path(out: &mut Vec<u8>, path: &Option<Path>) {
    match path {
        None => put_u8(out, 0),
        Some(p) => {
            put_u8(out, 1);
            put_u32(out, p.cells.len() as u32);
            for c in &p.cells {
                put_cell(out, *c);
            }
            put_u32(out, p.chunks.len() as u32);
            for c in &p.chunks {
                put_u32(out, *c);
            }
        }
    }
}

fn put_goal(out: &mut Vec<u8>, goal: Goal) {
    match goal {
        Goal::Cell { x, y } => {
            put_u8(out, 0);
            put_cell(out, (x, y));
        }
        Goal::Adjacent { x, y } => {
            put_u8(out, 1);
            put_cell(out, (x, y));
        }
    }
}

fn put_colonist(out: &mut Vec<u8>, c: &Colonist) {
    put_cell(out, c.cell);
    put_cell(out, c.from);
    put_u8(out, c.progress);
    put_u32(out, c.job);
    put_id(out, c.carrying);
    put_u32(out, c.seen_board);
    put_u8(out, c.state.tag());
    match &c.state {
        AgentState::Idle | AgentState::Stuck => {}
        AgentState::Walking { goal, path } => {
            put_goal(out, *goal);
            put_path(out, path);
        }
        AgentState::Fetching { item, path } => {
            put_id(out, *item);
            put_path(out, path);
        }
        AgentState::Working { ticks_left } => put_u8(out, *ticks_left),
    }
}

fn put_item(out: &mut Vec<u8>, it: &Item) {
    put_cell(out, (it.x, it.y));
    put_u16(out, it.element.0);
    put_u32(out, it.mass.0);
    put_i32(out, it.temp.0);
    put_id(out, it.holder);
    put_id(out, it.reserved_by);
}

fn put_arena<T>(out: &mut Vec<u8>, arena: &Arena<T>, put: impl Fn(&mut Vec<u8>, &T)) {
    put_u32(out, arena.slot_count() as u32);
    for i in 0..arena.slot_count() {
        let (generation, value) = arena.slot(i);
        put_u32(out, generation);
        match value {
            Some(v) => {
                put_u8(out, 1);
                put(out, v);
            }
            None => put_u8(out, 0),
        }
    }
    put_u32(out, arena.free_list().len() as u32);
    for f in arena.free_list() {
        put_u32(out, *f);
    }
}

fn put_jobs(out: &mut Vec<u8>, board: &JobBoard) {
    put_u32(out, board.jobs.len() as u32);
    for job in &board.jobs {
        put_u32(out, job.id);
        put_u8(out, job.kind.tag());
        put_cell(out, job.kind.target());
        put_u8(out, job.priority);
        match job.state {
            JobState::Open => put_u8(out, 0),
            JobState::Reserved(id) => {
                put_u8(out, 1);
                put_id(out, id);
            }
            JobState::Done => put_u8(out, 2),
            JobState::Cancelled => put_u8(out, 3),
        }
        put_u8(out, job.failures);
        put_u64(out, job.finished_tick);
    }
    put_u32(out, board.version);
    put_u32(out, board.next_id);
    put_u32(out, board.done_count);
    put_u32(out, board.cancelled_count);
}

/// Appends every entity of the simulation: colonists, items, and the job board.
pub fn encode_entities(sim: &Sim, out: &mut Vec<u8>) {
    put_arena(out, sim.colonists(), put_colonist);
    put_arena(out, sim.items(), put_item);
    put_jobs(out, sim.jobs());
}

/// Serialises the whole simulation.
pub fn encode(sim: &Sim) -> Vec<u8> {
    let world = sim.world();
    let n = world.cells.len();
    let mut out = Vec::with_capacity(4 + 4 + 8 * 4 + n * 11 + 8);
    out.extend_from_slice(MAGIC);
    put_u32(&mut out, FORMAT_VERSION);
    put_u64(&mut out, table_hash(sim.table()));
    put_u64(&mut out, world.width as u64);
    put_u64(&mut out, world.height as u64);
    put_u64(&mut out, sim.tick_count());
    for v in &world.cells.element {
        put_u16(&mut out, *v);
    }
    for v in &world.cells.mass {
        put_u32(&mut out, *v);
    }
    for v in &world.cells.temp {
        put_i32(&mut out, *v);
    }
    out.extend_from_slice(&world.cells.flags);
    encode_entities(sim, &mut out);
    put_u64(&mut out, sim.state_hash());
    out
}

// ---- reading ----

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SaveError> {
        let end = self.pos.checked_add(n).ok_or(SaveError::Truncated)?;
        let slice = self.bytes.get(self.pos..end).ok_or(SaveError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, SaveError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, SaveError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, SaveError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, SaveError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, SaveError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn count(&mut self) -> Result<usize, SaveError> {
        let n = self.u32()? as u64;
        if n > MAX_COUNT {
            return Err(SaveError::Truncated);
        }
        Ok(n as usize)
    }

    fn id(&mut self) -> Result<EntityId, SaveError> {
        Ok(EntityId {
            index: self.u32()?,
            generation: self.u32()?,
        })
    }

    fn cell(&mut self) -> Result<(u32, u32), SaveError> {
        Ok((self.u32()?, self.u32()?))
    }

    fn path(&mut self) -> Result<Option<Path>, SaveError> {
        if self.u8()? == 0 {
            return Ok(None);
        }
        let n = self.count()?;
        let mut cells = Vec::with_capacity(n);
        for _ in 0..n {
            cells.push(self.cell()?);
        }
        let m = self.count()?;
        let mut chunks = Vec::with_capacity(m);
        for _ in 0..m {
            chunks.push(self.u32()?);
        }
        Ok(Some(Path { cells, chunks }))
    }

    fn goal(&mut self) -> Result<Goal, SaveError> {
        let tag = self.u8()?;
        let (x, y) = self.cell()?;
        match tag {
            0 => Ok(Goal::Cell { x, y }),
            1 => Ok(Goal::Adjacent { x, y }),
            _ => Err(SaveError::BadEntities),
        }
    }

    fn colonist(&mut self) -> Result<Colonist, SaveError> {
        let cell = self.cell()?;
        let from = self.cell()?;
        let progress = self.u8()?;
        let job = self.u32()?;
        let carrying = self.id()?;
        let seen_board = self.u32()?;
        let state = match self.u8()? {
            0 => AgentState::Idle,
            1 => {
                let goal = self.goal()?;
                let path = self.path()?;
                AgentState::Walking { goal, path }
            }
            2 => {
                let item = self.id()?;
                let path = self.path()?;
                AgentState::Fetching { item, path }
            }
            3 => AgentState::Working {
                ticks_left: self.u8()?,
            },
            4 => AgentState::Stuck,
            _ => return Err(SaveError::BadEntities),
        };
        Ok(Colonist {
            cell,
            from,
            progress,
            state,
            job,
            carrying,
            seen_board,
        })
    }

    fn item(&mut self) -> Result<Item, SaveError> {
        let (x, y) = self.cell()?;
        let element = ElementId(self.u16()?);
        let mass = Mass(self.u32()?);
        let temp = Temp(self.i32()?);
        let holder = self.id()?;
        let reserved_by = self.id()?;
        Ok(Item {
            x,
            y,
            element,
            mass,
            temp,
            holder,
            reserved_by,
        })
    }

    fn arena<T>(
        &mut self,
        read: impl Fn(&mut Self) -> Result<T, SaveError>,
    ) -> Result<Arena<T>, SaveError> {
        let n = self.count()?;
        let mut slots = Vec::with_capacity(n);
        let mut generation = Vec::with_capacity(n);
        for _ in 0..n {
            generation.push(self.u32()?);
            slots.push(if self.u8()? == 1 {
                Some(read(self)?)
            } else {
                None
            });
        }
        let f = self.count()?;
        let mut free = Vec::with_capacity(f);
        for _ in 0..f {
            free.push(self.u32()?);
        }
        Arena::from_parts(slots, generation, free).ok_or(SaveError::BadEntities)
    }

    fn jobs(&mut self) -> Result<JobBoard, SaveError> {
        let n = self.count()?;
        let mut jobs = Vec::with_capacity(n);
        for _ in 0..n {
            let id = self.u32()?;
            let tag = self.u8()?;
            let (x, y) = self.cell()?;
            let kind = match tag {
                0 => JobKind::Dig { x, y },
                1 => JobKind::Build { x, y },
                _ => return Err(SaveError::BadEntities),
            };
            let priority = self.u8()?;
            let state = match self.u8()? {
                0 => JobState::Open,
                1 => JobState::Reserved(self.id()?),
                2 => JobState::Done,
                3 => JobState::Cancelled,
                _ => return Err(SaveError::BadEntities),
            };
            let failures = self.u8()?;
            let finished_tick = self.u64()?;
            jobs.push(Job {
                id,
                kind,
                priority,
                state,
                failures,
                finished_tick,
            });
        }
        let version = self.u32()?;
        let next_id = self.u32()?;
        let done_count = self.u32()?;
        let cancelled_count = self.u32()?;
        Ok(JobBoard {
            jobs,
            version,
            next_id,
            done_count,
            cancelled_count,
        })
    }
}

/// Rebuilds a simulation from [`encode`] output, against the same element table.
pub fn decode(table: ElementTable, bytes: &[u8]) -> Result<Sim, SaveError> {
    let mut r = Reader { bytes, pos: 0 };
    if r.take(4)? != MAGIC {
        return Err(SaveError::BadMagic);
    }
    let version = r.u32()?;
    if version != FORMAT_VERSION {
        return Err(SaveError::UnsupportedVersion(version));
    }
    if r.u64()? != table_hash(&table) {
        return Err(SaveError::TableMismatch);
    }
    let width = usize::try_from(r.u64()?).map_err(|_| SaveError::BadDimensions)?;
    let height = usize::try_from(r.u64()?).map_err(|_| SaveError::BadDimensions)?;
    let n = width
        .checked_mul(height)
        .filter(|n| *n > 0 && width <= 1 << 20 && height <= 1 << 20)
        .ok_or(SaveError::BadDimensions)?;
    let tick = r.u64()?;
    let mut cells = CellBuffers::new(n);
    for v in cells.element.iter_mut() {
        *v = r.u16()?;
    }
    for v in cells.mass.iter_mut() {
        *v = r.u32()?;
    }
    for v in cells.temp.iter_mut() {
        *v = r.i32()?;
    }
    cells.flags.copy_from_slice(r.take(n)?);
    let colonists = r.arena(|r| r.colonist())?;
    let items = r.arena(|r| r.item())?;
    let jobs = r.jobs()?;
    let expected = r.u64()?;
    for &element in &cells.element {
        if !table.contains(ElementId(element)) {
            return Err(SaveError::TableMismatch);
        }
    }
    for (_, c) in colonists.iter() {
        if c.cell.0 as usize >= width || c.cell.1 as usize >= height {
            return Err(SaveError::BadEntities);
        }
    }
    for (_, it) in items.iter() {
        if it.x as usize >= width || it.y as usize >= height || !table.contains(it.element) {
            return Err(SaveError::BadEntities);
        }
    }
    let world = World {
        width,
        height,
        cells,
    };
    let sim = Sim::from_parts_with(table, world, tick, colonists, items, jobs);
    if sim.state_hash() != expected {
        return Err(SaveError::HashMismatch);
    }
    Ok(sim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario;

    #[test]
    fn round_trip_restores_the_exact_state() {
        let mut sim = scenario::pool(40, 30);
        for _ in 0..50 {
            sim.step();
        }
        let bytes = encode(&sim);
        let restored = decode(ElementTable::builtin(), &bytes).unwrap();
        assert_eq!(restored.state_hash(), sim.state_hash());
        assert_eq!(restored.tick_count(), 50);
        assert_eq!(restored.world().cells, sim.world().cells);
    }

    #[test]
    fn rejects_bad_magic_versions_tables_truncation_and_edits() {
        let sim = scenario::settled(8, 8);
        let bytes = encode(&sim);
        let mut bad = bytes.clone();
        bad[0] = b'X';
        assert_eq!(
            decode(ElementTable::builtin(), &bad).unwrap_err(),
            SaveError::BadMagic
        );
        let mut bad = bytes.clone();
        bad[4] = 1;
        assert_eq!(
            decode(ElementTable::builtin(), &bad).unwrap_err(),
            SaveError::UnsupportedVersion(1)
        );
        let mut bad = bytes.clone();
        bad[8] ^= 1;
        assert_eq!(
            decode(ElementTable::builtin(), &bad).unwrap_err(),
            SaveError::TableMismatch
        );
        assert_eq!(
            decode(ElementTable::builtin(), &bytes[..bytes.len() - 3]).unwrap_err(),
            SaveError::Truncated
        );
        let mut bad = bytes.clone();
        let mass_start = 4 + 4 + 8 * 4 + 64 * 2;
        bad[mass_start + 4 * 20] ^= 1;
        assert_eq!(
            decode(ElementTable::builtin(), &bad).unwrap_err(),
            SaveError::HashMismatch
        );
    }

    #[test]
    fn table_hash_changes_with_any_physical_value() {
        let base = table_hash(&ElementTable::builtin());
        let mut defs = ElementTable::builtin().defs().to_vec();
        defs[1].conduct += 1;
        assert_ne!(table_hash(&ElementTable::new(defs)), base);
        let mut defs = ElementTable::builtin().defs().to_vec();
        defs[2].colour = [1, 2, 3];
        assert_eq!(
            table_hash(&ElementTable::new(defs)),
            base,
            "colour is not physical"
        );
    }
}
