//! Colour lookup tables the shader and the CPU reference share: element colours, the
//! temperature overlay, and the pressure overlay.

use colony_sim::{ElementTable, Phase};

/// A 256×3 RGBA8 table: row 0 element colour by table index, row 1 temperature, row 2
/// pressure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Luts {
    pub pixels: Vec<[u8; 4]>,
}

impl Luts {
    pub const WIDTH: u32 = 256;
    pub const ROWS: u32 = 3;

    pub fn from_table(table: &ElementTable) -> Luts {
        let mut pixels = vec![[0, 0, 0, 255]; (Self::WIDTH * Self::ROWS) as usize];
        for (i, def) in table.defs().iter().enumerate().take(256) {
            let [r, g, b] = def.colour;
            pixels[i] = [r, g, b, 255];
        }
        for v in 0..256usize {
            let [r, g, b] = temperature_colour(v as u8);
            pixels[256 + v] = [r, g, b, 255];
            let [r, g, b] = pressure_colour(v as u8);
            pixels[512 + v] = [r, g, b, 255];
        }
        Luts { pixels }
    }

    pub fn element_colour(&self, index: u8) -> [u8; 3] {
        let [r, g, b, _] = self.pixels[index as usize];
        [r, g, b]
    }

    pub fn lookup(&self, row: u32, key: u8) -> [u8; 3] {
        let [r, g, b, _] = self.pixels[(row * Self::WIDTH) as usize + key as usize];
        [r, g, b]
    }

    pub fn bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.pixels)
    }
}

/// The headless temperature ramp: blue at 250 K to red at 350 K.
pub fn temperature_colour(t: u8) -> [u8; 3] {
    [t, 60, 255 - t]
}

fn lerp(a: [u8; 3], b: [u8; 3], num: u32, den: u32) -> [u8; 3] {
    let mut out = [0u8; 3];
    for i in 0..3 {
        let (x, y) = (a[i] as u32, b[i] as u32);
        out[i] = ((x * (den - num) + y * num) / den) as u8;
    }
    out
}

/// Pressure ramp over the quantised mass: dark blue through green to red at 192 and
/// above; vacuum (0) is dark grey.
pub fn pressure_colour(m: u8) -> [u8; 3] {
    const LOW: [u8; 3] = [20, 40, 120];
    const MID: [u8; 3] = [40, 200, 80];
    const HIGH: [u8; 3] = [230, 60, 40];
    if m == 0 {
        return [40, 40, 40];
    }
    let k = m.min(192) as u32;
    if k < 96 {
        lerp(LOW, MID, k, 96)
    } else {
        lerp(MID, HIGH, k - 96, 96)
    }
}

/// For every table index, its rank among the solid elements in table order (0 for
/// non-solids, which never read a rank).
pub fn solid_ranks(table: &ElementTable) -> Vec<u8> {
    let mut ranks = vec![0u8; table.len()];
    let mut next = 0u8;
    for (i, def) in table.defs().iter().enumerate() {
        if def.phase == Phase::Solid {
            ranks[i] = next;
            next += 1;
        }
    }
    ranks
}

/// Number of solid elements in the table (rows of the atlas).
pub fn solid_count(table: &ElementTable) -> usize {
    table
        .defs()
        .iter()
        .filter(|d| d.phase == Phase::Solid)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use colony_sim::builtin;

    #[test]
    fn luts_hold_element_colours_and_overlay_ramps() {
        let table = ElementTable::builtin();
        let luts = Luts::from_table(&table);
        assert_eq!(luts.pixels.len(), 768);
        assert_eq!(
            luts.element_colour(builtin::OXYGEN.index() as u8),
            [120, 200, 255]
        );
        assert_eq!(luts.element_colour(0), [8, 8, 12]);
        assert_eq!(luts.lookup(1, 0), [0, 60, 255]);
        assert_eq!(luts.lookup(1, 255), [255, 60, 0]);
        assert_eq!(luts.lookup(2, 0), [40, 40, 40]);
        assert_eq!(luts.lookup(2, 96), [40, 200, 80]);
        assert_eq!(luts.lookup(2, 192), [230, 60, 40]);
        assert_eq!(luts.lookup(2, 255), [230, 60, 40]);
        assert_eq!(luts.bytes().len(), 768 * 4);
    }

    #[test]
    fn solid_ranks_count_solids_in_table_order() {
        let table = ElementTable::builtin();
        let ranks = solid_ranks(&table);
        assert_eq!(ranks[builtin::SANDSTONE.index()], 0);
        assert_eq!(ranks[builtin::ICE.index()], 1);
        assert_eq!(ranks[builtin::WATER.index()], 0);
        assert_eq!(solid_count(&table), 2);
    }
}
