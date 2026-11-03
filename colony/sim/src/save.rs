//! Snapshots: a versioned little-endian byte layout of the whole cell state, plus a
//! trailing state hash so a corrupted or edited file is rejected rather than loaded.
//!
//! Layout: magic `CSIM`, format version `u32`, element-table hash `u64`, width `u64`,
//! height `u64`, tick `u64`, then the element (`u16`), mass (`u32`), temperature
//! (`i32`), and flags (`u8`) arrays, then the state hash `u64`. Chunk activity is not
//! saved: a loaded world visits every chunk on its first tick, which produces the same
//! values as skipping would have.

use crate::element::ElementTable;
use crate::hash::Fnv1a;
use crate::sim::Sim;
use crate::world::{CellBuffers, World};

const MAGIC: &[u8; 4] = b"CSIM";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
pub enum SaveError {
    BadMagic,
    UnsupportedVersion(u32),
    TableMismatch,
    Truncated,
    HashMismatch,
    BadDimensions,
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

    fn u32(&mut self) -> Result<u32, SaveError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, SaveError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

/// Serialises the cell state and tick.
pub fn encode(sim: &Sim) -> Vec<u8> {
    let world = sim.world();
    let n = world.cells.len();
    let mut out = Vec::with_capacity(4 + 4 + 8 * 4 + n * 11 + 8);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&table_hash(sim.table()).to_le_bytes());
    out.extend_from_slice(&(world.width as u64).to_le_bytes());
    out.extend_from_slice(&(world.height as u64).to_le_bytes());
    out.extend_from_slice(&sim.tick_count().to_le_bytes());
    for v in &world.cells.element {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in &world.cells.mass {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in &world.cells.temp {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&world.cells.flags);
    out.extend_from_slice(&sim.state_hash().to_le_bytes());
    out
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
        *v = u16::from_le_bytes(r.take(2)?.try_into().unwrap());
    }
    for v in cells.mass.iter_mut() {
        *v = u32::from_le_bytes(r.take(4)?.try_into().unwrap());
    }
    for v in cells.temp.iter_mut() {
        *v = i32::from_le_bytes(r.take(4)?.try_into().unwrap());
    }
    cells.flags.copy_from_slice(r.take(n)?);
    let expected = r.u64()?;
    for &element in &cells.element {
        if !table.contains(crate::element::ElementId(element)) {
            return Err(SaveError::TableMismatch);
        }
    }
    let world = World {
        width,
        height,
        cells,
    };
    let sim = Sim::from_parts(table, world, tick);
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
        bad[4] = 9;
        assert_eq!(
            decode(ElementTable::builtin(), &bad).unwrap_err(),
            SaveError::UnsupportedVersion(9)
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
