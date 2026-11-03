//! What the sim thread hands to the renderer after a tick: one packed texel per cell,
//! the exact values for the probe, and which chunks may have changed.

use colony_sim::chunk::{ChunkGrid, CHUNK};
use colony_sim::{ElementId, ElementTable, Mass, Phase, Sim, Temp, TickStats};

/// Largest quantised mass value, so `64 + m` is the headless brightness scale.
pub const MASS_SHADE_MAX: u8 = 192;

/// One cell's packed texel: element table index, phase and flags, quantised mass,
/// quantised temperature.
pub type Texel = [u8; 4];

#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Bumped on restart or load; a renderer re-uploads everything when it changes.
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub tick: u64,
    /// Row-major, row 0 is the bottom of the world.
    pub texels: Vec<Texel>,
    /// Exact raw masses and temperatures for the probe.
    pub mass: Vec<u32>,
    pub temp: Vec<i32>,
    pub chunk_cols: u32,
    pub chunk_rows: u32,
    /// Per chunk in [`ChunkGrid`] order: the chunk may differ from the last snapshot.
    pub dirty: Vec<bool>,
    pub stats: TickStats,
    /// Time the last step took on the sim thread.
    pub step_micros: u32,
    /// Steps per second times 100, smoothed on the sim thread.
    pub ticks_per_s_x100: u32,
}

/// Exact values of one cell, read from a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellProbe {
    pub element: ElementId,
    pub mass: Mass,
    pub temp: Temp,
    pub flags: u8,
    pub chunk: (u32, u32),
}

/// `0..=192` proportional to `mass / full`, saturating: the mapping `colony-headless`
/// uses for brightness.
pub fn quantise_mass(mass: u32, full: u32) -> u8 {
    let m = mass as u64;
    let f = full.max(1) as u64;
    ((m.min(f) * MASS_SHADE_MAX as u64) / f) as u8
}

/// 250 K to 350 K mapped to `0..=255`, saturating: the mapping `colony-headless` uses.
pub fn quantise_temp(temp: i32) -> u8 {
    let mk = Temp(temp).to_millikelvin().clamp(250_000, 350_000) - 250_000;
    (mk * 255 / 100_000) as u8
}

/// The mass that reads as "full" for an element: 2 kg for gases, `max_mass` (or
/// 1,000 kg if unlimited) for liquids, 2,000 kg for solids.
pub fn full_mass(table: &ElementTable, element: u16) -> u32 {
    match table.phase(element) {
        Phase::Vacuum => 1,
        Phase::Gas => Mass::from_kg(2).raw(),
        Phase::Liquid => {
            let max = table.max_mass(element);
            if max == u32::MAX {
                Mass::from_kg(1000).raw()
            } else {
                max
            }
        }
        Phase::Solid => Mass::from_kg(2000).raw(),
    }
}

/// Packs one cell.
pub fn texel(table: &ElementTable, element: u16, mass: u32, temp: i32, flags: u8) -> Texel {
    let id = ElementId(element);
    debug_assert!(id.index() < 256, "renderer supports 256 elements");
    debug_assert!(flags < 64, "renderer packs at most six flag bits");
    [
        id.index() as u8,
        (id.phase() as u8) | (flags << 2),
        quantise_mass(mass, full_mass(table, element)),
        quantise_temp(temp),
    ]
}

impl Snapshot {
    /// Builds a snapshot from the current state. `dirty` has one entry per chunk.
    pub fn from_sim(
        sim: &Sim,
        dirty: &[bool],
        generation: u64,
        stats: TickStats,
        step_micros: u32,
        ticks_per_s_x100: u32,
    ) -> Snapshot {
        let table = sim.table();
        assert!(table.len() <= 256, "renderer supports 256 elements");
        let world = sim.world();
        let chunks = sim.chunks();
        assert_eq!(dirty.len(), chunks.len());
        let cells = &world.cells;
        let texels = (0..cells.len())
            .map(|i| {
                texel(
                    table,
                    cells.element[i],
                    cells.mass[i],
                    cells.temp[i],
                    cells.flags[i],
                )
            })
            .collect();
        Snapshot {
            generation,
            width: world.width as u32,
            height: world.height as u32,
            tick: sim.tick_count(),
            texels,
            mass: cells.mass.clone(),
            temp: cells.temp.clone(),
            chunk_cols: chunks.cols as u32,
            chunk_rows: chunks.rows as u32,
            dirty: dirty.to_vec(),
            stats,
            step_micros,
            ticks_per_s_x100,
        }
    }

    #[inline]
    pub fn index(&self, x: u32, y: u32) -> usize {
        (y * self.width + x) as usize
    }

    pub fn chunk_grid(&self) -> ChunkGrid {
        ChunkGrid {
            cols: self.chunk_cols as usize,
            rows: self.chunk_rows as usize,
        }
    }

    /// Exact values of a cell, or `None` outside the world.
    pub fn probe(&self, table: &ElementTable, x: u32, y: u32) -> Option<CellProbe> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = self.index(x, y);
        let [index, packed, _, _] = self.texels[i];
        let phase = Phase::from_bits(packed as u16 & 3);
        let element = if index == 0 {
            ElementId::VACUUM
        } else {
            table.id(index as usize)
        };
        debug_assert_eq!(element.phase(), phase);
        Some(CellProbe {
            element,
            mass: Mass(self.mass[i]),
            temp: Temp(self.temp[i]),
            flags: packed >> 2,
            chunk: (x / CHUNK as u32, y / CHUNK as u32),
        })
    }

    /// Whether any chunk is dirty.
    pub fn any_dirty(&self) -> bool {
        self.dirty.iter().any(|d| *d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use colony_sim::{builtin, scenario, FLAG_PRESSED};

    #[test]
    fn texel_packs_index_phase_flags_and_quantised_values() {
        let table = ElementTable::builtin();
        let t = texel(
            &table,
            builtin::WATER.0,
            Mass::from_kg(500).raw(),
            Temp::from_kelvin(300).raw(),
            FLAG_PRESSED,
        );
        assert_eq!(t[0], 3);
        assert_eq!(t[1], Phase::Liquid as u8 | (FLAG_PRESSED << 2));
        assert_eq!(t[2], 96, "half of max_mass");
        assert_eq!(t[3], 127);
        let g = texel(
            &table,
            builtin::OXYGEN.0,
            Mass::from_kg(1).raw(),
            Temp::from_kelvin(250).raw(),
            0,
        );
        assert_eq!(g, [1, Phase::Gas as u8, 96, 0]);
        assert_eq!(texel(&table, 0, 0, 0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn quantisation_saturates_and_is_monotonic() {
        assert_eq!(quantise_mass(0, 100), 0);
        assert_eq!(quantise_mass(100, 100), MASS_SHADE_MAX);
        assert_eq!(quantise_mass(1_000_000, 100), MASS_SHADE_MAX);
        let mut last = 0;
        for m in 0..=100 {
            let q = quantise_mass(m, 100);
            assert!(q >= last);
            last = q;
        }
        assert_eq!(quantise_temp(Temp::from_kelvin(200).raw()), 0);
        assert_eq!(quantise_temp(Temp::from_kelvin(350).raw()), 255);
        assert_eq!(quantise_temp(Temp::from_kelvin(400).raw()), 255);
        assert!(
            quantise_temp(Temp::from_kelvin(300).raw())
                < quantise_temp(Temp::from_kelvin(301).raw())
        );
    }

    #[test]
    fn probe_equals_world_get_for_every_cell() {
        let mut sim = scenario::cave(64, 48);
        let s = sim.step();
        let dirty = sim.processed_chunks().to_vec();
        let snap = Snapshot::from_sim(&sim, &dirty, 1, s, 0, 0);
        let table = sim.table();
        let world = sim.world();
        for y in 0..48u32 {
            for x in 0..64u32 {
                let probe = snap.probe(table, x, y).unwrap();
                let (el, mass, temp) = world.get(x as usize, y as usize);
                assert_eq!(
                    (probe.element, probe.mass, probe.temp),
                    (el, mass, temp),
                    "({x}, {y})"
                );
                assert_eq!(
                    probe.flags,
                    world.cells.flags[world.index(x as usize, y as usize)]
                );
                assert_eq!(probe.chunk, (x / 32, y / 32));
            }
        }
        assert!(snap.probe(table, 64, 0).is_none());
        assert_eq!(snap.dirty, dirty);
        assert_eq!((snap.chunk_cols, snap.chunk_rows), (2, 2));
        assert_eq!(snap.tick, 1);
    }

    #[test]
    fn dirty_mirrors_the_kernel_after_a_quiet_tick() {
        let mut sim = scenario::settled(64, 64);
        sim.step();
        let s = sim.step();
        let snap = Snapshot::from_sim(&sim, sim.processed_chunks(), 0, s, 0, 0);
        assert!(!snap.any_dirty());
    }
}
