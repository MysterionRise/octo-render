//! The element table: what a cell can hold and how it conducts heat.
//!
//! Units are fixed point (see [`crate::fixed`]). Specific heat capacity is Q16.16
//! J/(kg·K); conductive transfer is Q16.16 J per kelvin per tick, which already folds
//! in the tick length and the one-metre cell, so content loading does that conversion
//! once and the kernel never multiplies by a time step. Density is Q16.16 kg/m³ and
//! only orders gases for buoyancy.
//!
//! An [`ElementId`] packs the phase into its top two bits, so a pass can tell a gas
//! from a solid with a shift instead of a table lookup per neighbour.

use crate::fixed::{q16, ONE};

/// The phase of an element, which decides which passes touch a cell.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum Phase {
    Vacuum = 0,
    Gas = 1,
    Liquid = 2,
    Solid = 3,
}

impl Phase {
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The phase encoded in the low two bits of `bits`.
    #[inline]
    pub const fn from_bits(bits: u16) -> Phase {
        match bits & 3 {
            0 => Phase::Vacuum,
            1 => Phase::Gas,
            2 => Phase::Liquid,
            _ => Phase::Solid,
        }
    }
}

/// Bit position of the phase inside an [`ElementId`].
pub const PHASE_SHIFT: u32 = 14;
/// Mask of the table index inside an [`ElementId`].
pub const INDEX_MASK: u16 = (1 << PHASE_SHIFT) - 1;

/// A packed element reference: phase in bits 14–15, table index in bits 0–13.
/// Element 0 is always vacuum.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct ElementId(pub u16);

impl ElementId {
    pub const VACUUM: ElementId = ElementId(0);

    pub const fn new(phase: Phase, index: u16) -> ElementId {
        assert!(index <= INDEX_MASK, "element index does not fit in 14 bits");
        ElementId(((phase as u16) << PHASE_SHIFT) | index)
    }

    #[inline]
    pub const fn index(self) -> usize {
        (self.0 & INDEX_MASK) as usize
    }

    #[inline]
    pub const fn phase(self) -> Phase {
        Phase::from_bits(self.0 >> PHASE_SHIFT)
    }
}

/// One element definition. Content loading builds these from human-unit data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementDef {
    pub name: String,
    pub phase: Phase,
    /// Specific heat capacity, Q16.16 J/(kg·K).
    pub shc: i64,
    /// Conductive transfer per kelvin of difference per tick, Q16.16 J/(K·tick).
    pub conduct: i64,
    /// Density, Q16.16 kg/m³; orders gases for buoyancy.
    pub density: i64,
    /// Display colour for debug renders.
    pub colour: [u8; 3],
}

impl ElementDef {
    pub fn new(
        name: &str,
        phase: Phase,
        shc: i64,
        conduct: i64,
        density: i64,
        colour: [u8; 3],
    ) -> ElementDef {
        ElementDef {
            name: name.to_string(),
            phase,
            shc,
            conduct,
            density,
            colour,
        }
    }
}

/// The table of elements plus per-phase-pair conduction multipliers.
#[derive(Clone, Debug)]
pub struct ElementTable {
    defs: Vec<ElementDef>,
    shc: Vec<i64>,
    conduct: Vec<i64>,
    density: Vec<i64>,
    /// Symmetric Q16.16 multiplier applied to conduction between two phases.
    pair_mult: [[i64; 4]; 4],
}

impl ElementTable {
    /// Builds a table. Element 0 must be the vacuum: phase `Vacuum`, zero heat
    /// capacity, zero conduction.
    pub fn new(defs: Vec<ElementDef>) -> ElementTable {
        assert!(!defs.is_empty(), "element table needs at least the vacuum");
        let vacuum = &defs[0];
        assert!(
            vacuum.phase == Phase::Vacuum && vacuum.shc == 0 && vacuum.conduct == 0,
            "element 0 must be the vacuum with no heat capacity or conduction"
        );
        assert!(
            defs.len() <= INDEX_MASK as usize + 1,
            "too many elements for 14-bit indices"
        );
        for def in &defs {
            assert!(
                def.shc >= 0 && def.conduct >= 0 && def.density >= 0,
                "{}: negative physical values",
                def.name
            );
        }
        let shc = defs.iter().map(|d| d.shc).collect();
        let conduct = defs.iter().map(|d| d.conduct).collect();
        let density = defs.iter().map(|d| d.density).collect();
        ElementTable {
            defs,
            shc,
            conduct,
            density,
            pair_mult: [[ONE; 4]; 4],
        }
    }

    /// Sets the conduction multiplier between two phases (applied in both directions).
    pub fn set_pair_mult(&mut self, a: Phase, b: Phase, mult: i64) {
        assert!(mult >= 0, "pair multipliers are non-negative");
        self.pair_mult[a.index()][b.index()] = mult;
        self.pair_mult[b.index()][a.index()] = mult;
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// The packed id of the element at `index`.
    pub fn id(&self, index: usize) -> ElementId {
        ElementId::new(self.defs[index].phase, index as u16)
    }

    /// Whether `id` names an element of this table with the right phase bits.
    pub fn contains(&self, id: ElementId) -> bool {
        self.defs
            .get(id.index())
            .is_some_and(|d| d.phase == id.phase())
    }

    pub fn get(&self, id: ElementId) -> &ElementDef {
        &self.defs[id.index()]
    }

    pub fn defs(&self) -> &[ElementDef] {
        &self.defs
    }

    pub fn id_by_name(&self, name: &str) -> Option<ElementId> {
        self.defs
            .iter()
            .position(|d| d.name == name)
            .map(|i| self.id(i))
    }

    /// The phase of a packed element id; no table access.
    #[inline]
    pub fn phase(&self, element: u16) -> Phase {
        ElementId(element).phase()
    }

    #[inline]
    pub fn shc(&self, element: u16) -> i64 {
        self.shc[ElementId(element).index()]
    }

    #[inline]
    pub fn conduct(&self, element: u16) -> i64 {
        self.conduct[ElementId(element).index()]
    }

    #[inline]
    pub fn density(&self, element: u16) -> i64 {
        self.density[ElementId(element).index()]
    }

    #[inline]
    pub fn pair_mult(&self, a: Phase, b: Phase) -> i64 {
        self.pair_mult[a.index()][b.index()]
    }

    /// A small physically sourced table used by scenarios, tests, and benches:
    /// vacuum, oxygen, carbon dioxide, water, sandstone. Conduction is the real
    /// conductivity in W/(m·K) times the 0.1 s tick; densities are at room conditions.
    pub fn builtin() -> ElementTable {
        ElementTable::new(vec![
            ElementDef::new("vacuum", Phase::Vacuum, 0, 0, 0, [8, 8, 12]),
            ElementDef::new(
                "oxygen",
                Phase::Gas,
                q16(1005, 0),
                q16(0, 2_400),
                q16(1, 429_000),
                [120, 200, 255],
            ),
            ElementDef::new(
                "carbon_dioxide",
                Phase::Gas,
                q16(846, 0),
                q16(0, 1_460),
                q16(1, 977_000),
                [200, 120, 90],
            ),
            ElementDef::new(
                "water",
                Phase::Liquid,
                q16(4179, 0),
                q16(0, 60_900),
                q16(1000, 0),
                [60, 110, 220],
            ),
            ElementDef::new(
                "sandstone",
                Phase::Solid,
                q16(830, 0),
                q16(0, 290_000),
                q16(2323, 0),
                [160, 140, 100],
            ),
        ])
    }
}

/// Ids of the [`ElementTable::builtin`] entries.
pub mod builtin {
    use super::{ElementId, Phase};

    pub const VACUUM: ElementId = ElementId::VACUUM;
    pub const OXYGEN: ElementId = ElementId::new(Phase::Gas, 1);
    pub const CARBON_DIOXIDE: ElementId = ElementId::new(Phase::Gas, 2);
    pub const WATER: ElementId = ElementId::new(Phase::Liquid, 3);
    pub const SANDSTONE: ElementId = ElementId::new(Phase::Solid, 4);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_pack_phase_and_index() {
        let id = ElementId::new(Phase::Solid, 4);
        assert_eq!(id.index(), 4);
        assert_eq!(id.phase(), Phase::Solid);
        assert_eq!(ElementId::VACUUM.phase(), Phase::Vacuum);
        assert_eq!(
            Phase::from_bits(ElementId::new(Phase::Liquid, 3).0 >> PHASE_SHIFT),
            Phase::Liquid
        );
    }

    #[test]
    fn builtin_table_is_consistent() {
        let table = ElementTable::builtin();
        assert_eq!(table.len(), 5);
        assert_eq!(table.id_by_name("oxygen"), Some(builtin::OXYGEN));
        assert_eq!(table.id(4), builtin::SANDSTONE);
        assert!(table.contains(builtin::WATER));
        assert!(
            !table.contains(ElementId::new(Phase::Gas, 4)),
            "wrong phase bits"
        );
        assert!(
            !table.contains(ElementId::new(Phase::Gas, 9)),
            "unknown index"
        );
        assert_eq!(table.phase(builtin::SANDSTONE.0), Phase::Solid);
        assert!(table.density(builtin::CARBON_DIOXIDE.0) > table.density(builtin::OXYGEN.0));
        assert_eq!(table.pair_mult(Phase::Gas, Phase::Solid), ONE);
    }

    #[test]
    fn pair_multiplier_is_symmetric() {
        let mut table = ElementTable::builtin();
        table.set_pair_mult(Phase::Gas, Phase::Solid, 25 * ONE);
        assert_eq!(table.pair_mult(Phase::Solid, Phase::Gas), 25 * ONE);
        assert_eq!(table.pair_mult(Phase::Gas, Phase::Gas), ONE);
    }

    #[test]
    #[should_panic(expected = "element 0 must be the vacuum")]
    fn rejects_table_without_vacuum_first() {
        ElementTable::new(vec![ElementDef::new(
            "oxygen",
            Phase::Gas,
            ONE,
            0,
            ONE,
            [0; 3],
        )]);
    }
}
