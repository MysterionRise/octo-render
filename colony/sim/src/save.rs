//! Snapshots: a versioned little-endian byte layout of the whole state, plus a trailing
//! state hash so a corrupted or edited file is rejected rather than loaded.
//!
//! Layout (format 3): magic `CSIM`, format version `u32`, element-table hash `u64`,
//! width `u64`, height `u64`, tick `u64`, the element (`u16`), mass (`u32`),
//! temperature (`i32`), and flags (`u8`) arrays, then the entities (colonists, items,
//! the job board, the buildings, the injected energy, written by [`encode_entities`]),
//! then the state hash `u64`. Each building names its definition rather than indexing
//! it, so a save loads against any building table that still has those names, and the
//! hash does not depend on the table's order. Chunk
//! activity is not saved: a loaded world visits every chunk on its first tick, which
//! produces the same values as skipping would have. The same entity bytes feed
//! [`crate::Sim::state_hash`], so the hash can never cover less than the file.

use crate::agent::{AgentState, Colonist};
use crate::building::{Building, BuildingState, BuildingTable, Packet};
use crate::element::{ElementId, ElementTable};
use crate::entity::{Arena, EntityId};
use crate::fixed::{Mass, Temp};
use crate::hash::Fnv1a;
use crate::item::Item;
use crate::job::{Job, JobBoard, JobKind, JobState};
use crate::nav::{Goal, Path};
use crate::sim::{Parts, Sim};
use crate::world::{CellBuffers, World};

const MAGIC: &[u8; 4] = b"CSIM";
pub const FORMAT_VERSION: u32 = 3;
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
    /// The save names a building the current table does not have.
    UnknownBuilding(String),
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
            SaveError::UnknownBuilding(name) => {
                write!(
                    f,
                    "snapshot uses building {name:?}, which the building table lacks"
                )
            }
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
        Goal::Near { x, y, w, h } => {
            put_u8(out, 2);
            put_cell(out, (x, y));
            put_u8(out, w);
            put_u8(out, h);
        }
    }
}

fn put_kind(out: &mut Vec<u8>, kind: JobKind) {
    put_u8(out, kind.tag());
    match kind.cell() {
        Some(cell) => put_cell(out, cell),
        None => put_id(
            out,
            kind.building().expect("a job targets a cell or a building"),
        ),
    }
}

fn put_contents(out: &mut Vec<u8>, c: Option<(ElementId, Mass, Temp)>) {
    match c {
        None => put_u8(out, 0),
        Some((e, m, t)) => {
            put_u8(out, 1);
            put_u16(out, e.0);
            put_u32(out, m.0);
            put_i32(out, t.0);
        }
    }
}

fn put_building(out: &mut Vec<u8>, table: &BuildingTable, b: &Building) {
    let name = table.get(b.def).name.as_bytes();
    put_u16(out, name.len() as u16);
    out.extend_from_slice(name);
    put_cell(out, (b.x, b.y));
    put_u8(out, b.state.tag());
    put_contents(out, b.material);
    put_contents(out, b.stored);
    put_u64(out, b.battery as u64);
    let flags =
        (b.enabled as u8) | (b.powered as u8) << 1 | (b.signal as u8) << 2 | (b.blocked as u8) << 3;
    put_u8(out, flags);
    put_contents(out, b.packet.map(|p| (p.element, p.mass, p.temp)));
    put_u16(out, b.work);
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
        AgentState::Working { ticks_left } => put_u16(out, *ticks_left),
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
        put_kind(out, job.kind);
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

/// Appends every entity of the simulation: colonists, items, the job board, the
/// buildings (naming their definitions), and the injected energy.
pub fn encode_entities(sim: &Sim, out: &mut Vec<u8>) {
    put_arena(out, sim.colonists(), put_colonist);
    put_arena(out, sim.items(), put_item);
    put_jobs(out, sim.jobs());
    let table = sim.building_table();
    put_arena(out, sim.buildings(), |out, b| put_building(out, table, b));
    out.extend_from_slice(&sim.energy_injected().to_le_bytes());
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
            2 => {
                let w = self.u8()?;
                let h = self.u8()?;
                Ok(Goal::Near { x, y, w, h })
            }
            _ => Err(SaveError::BadEntities),
        }
    }

    fn kind(&mut self) -> Result<JobKind, SaveError> {
        let tag = self.u8()?;
        Ok(match tag {
            0 | 1 => {
                let (x, y) = self.cell()?;
                if tag == 0 {
                    JobKind::Dig { x, y }
                } else {
                    JobKind::Build { x, y }
                }
            }
            2..=5 => {
                let building = self.id()?;
                match tag {
                    2 => JobKind::Construct { building },
                    3 => JobKind::Deconstruct { building },
                    4 => JobKind::Deliver { building },
                    _ => JobKind::Operate { building },
                }
            }
            _ => return Err(SaveError::BadEntities),
        })
    }

    fn contents(&mut self) -> Result<Option<(ElementId, Mass, Temp)>, SaveError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some((
                ElementId(self.u16()?),
                Mass(self.u32()?),
                Temp(self.i32()?),
            ))),
            _ => Err(SaveError::BadEntities),
        }
    }

    /// A building, with its definition resolved by name in `table`.
    fn building(&mut self, table: &BuildingTable) -> Result<Building, SaveError> {
        let len = self.u16()? as usize;
        let name = std::str::from_utf8(self.take(len)?).map_err(|_| SaveError::BadEntities)?;
        let def = table
            .by_name(name)
            .ok_or_else(|| SaveError::UnknownBuilding(name.to_string()))?;
        let (x, y) = self.cell()?;
        let state = BuildingState::from_tag(self.u8()?).ok_or(SaveError::BadEntities)?;
        let material = self.contents()?;
        let stored = self.contents()?;
        let battery = self.u64()? as i64;
        let flags = self.u8()?;
        let packet = self.contents()?.map(|(element, mass, temp)| Packet {
            element,
            mass,
            temp,
        });
        let work = self.u16()?;
        Ok(Building {
            def,
            x,
            y,
            state,
            material,
            stored,
            battery,
            enabled: flags & 1 != 0,
            powered: flags & 2 != 0,
            signal: flags & 4 != 0,
            blocked: flags & 8 != 0,
            packet,
            work,
        })
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
                ticks_left: self.u16()?,
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
            let kind = self.kind()?;
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

/// Rebuilds a simulation from [`encode`] output, against the same element table and a
/// building table that has every building the save names (indices are remapped by name).
pub fn decode(
    table: ElementTable,
    building_table: BuildingTable,
    bytes: &[u8],
) -> Result<Sim, SaveError> {
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
    let buildings = r.arena(|r| r.building(&building_table))?;
    let mut injected = [0u8; 16];
    injected.copy_from_slice(r.take(16)?);
    let energy_injected = i128::from_le_bytes(injected);
    let expected = r.u64()?;
    for (_, b) in buildings.iter() {
        let size = building_table.get(b.def).size;
        if b.x as usize + size.0 as usize > width || b.y as usize + size.1 as usize > height {
            return Err(SaveError::BadEntities);
        }
        for (e, _, _) in b.material.into_iter().chain(b.stored) {
            if !table.contains(e) {
                return Err(SaveError::BadEntities);
            }
        }
        if b.packet.is_some_and(|p| !table.contains(p.element)) {
            return Err(SaveError::BadEntities);
        }
    }
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
    let sim = Sim::from_parts_with(Parts {
        table,
        building_table,
        world,
        tick,
        colonists,
        items,
        jobs,
        buildings,
        energy_injected,
    });
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
        let restored = decode(ElementTable::builtin(), BuildingTable::builtin(), &bytes).unwrap();
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
            decode(ElementTable::builtin(), BuildingTable::builtin(), &bad).unwrap_err(),
            SaveError::BadMagic
        );
        let mut bad = bytes.clone();
        bad[4] = 1;
        assert_eq!(
            decode(ElementTable::builtin(), BuildingTable::builtin(), &bad).unwrap_err(),
            SaveError::UnsupportedVersion(1)
        );
        let mut bad = bytes.clone();
        bad[8] ^= 1;
        assert_eq!(
            decode(ElementTable::builtin(), BuildingTable::builtin(), &bad).unwrap_err(),
            SaveError::TableMismatch
        );
        assert_eq!(
            decode(
                ElementTable::builtin(),
                BuildingTable::builtin(),
                &bytes[..bytes.len() - 3]
            )
            .unwrap_err(),
            SaveError::Truncated
        );
        let mut bad = bytes.clone();
        let mass_start = 4 + 4 + 8 * 4 + 64 * 2;
        bad[mass_start + 4 * 20] ^= 1;
        assert_eq!(
            decode(ElementTable::builtin(), BuildingTable::builtin(), &bad).unwrap_err(),
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
