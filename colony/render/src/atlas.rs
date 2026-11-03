//! A placeholder tile atlas drawn by code, so the renderer needs no art files: for each
//! solid material, the 16 dual-grid cases, one tile of `TILE_PX × TILE_PX` pixels each.
//! Within a tile the four quadrants map to the four cells around a corner; a quadrant is
//! opaque exactly when its case bit is set, so a pixel inside a solid cell always lands on
//! an opaque texel.

use crate::corners::{BL, BR, TL, TR};
use colony_sim::{ElementTable, Phase};
use image::{Rgba, RgbaImage};

#[derive(Clone, Debug)]
pub struct Atlas {
    pub tile_px: u32,
    pub rows: u32,
    pub image: RgbaImage,
}

/// Pixel rectangle `(x, y, w, h)` of a tile in the atlas image (y down).
pub fn tile_rect(tile_px: u32, rank: u8, case: u16) -> (u32, u32, u32, u32) {
    (
        (case & 15) as u32 * tile_px,
        rank as u32 * tile_px,
        tile_px,
        tile_px,
    )
}

fn hash(x: u32, y: u32) -> u32 {
    let mut h = x.wrapping_mul(0x9e37_79b9) ^ y.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^ (h >> 16)
}

fn scale(c: [u8; 3], num: u32, den: u32) -> [u8; 3] {
    c.map(|v| ((v as u32 * num) / den).min(255) as u8)
}

impl Atlas {
    /// Builds the atlas for every solid element of the table, in `solid_ranks` order.
    pub fn build(table: &ElementTable, tile_px: u32) -> Atlas {
        let solids: Vec<[u8; 3]> = table
            .defs()
            .iter()
            .filter(|d| d.phase == Phase::Solid)
            .map(|d| d.colour)
            .collect();
        let rows = solids.len().max(1) as u32;
        let mut image = RgbaImage::new(16 * tile_px, rows * tile_px);
        let half = tile_px / 2;
        for (rank, colour) in solids.iter().enumerate() {
            for case in 0..16u16 {
                let (x0, y0, _, _) = tile_rect(tile_px, rank as u8, case);
                for py in 0..tile_px {
                    for px in 0..tile_px {
                        // Image y is down: the top half of the tile is the top cells.
                        let upper = py < half;
                        let right = px >= half;
                        let bit = match (upper, right) {
                            (false, false) => BL,
                            (false, true) => BR,
                            (true, false) => TL,
                            (true, true) => TR,
                        };
                        let pixel = if case & bit == 0 {
                            Rgba([0, 0, 0, 0])
                        } else {
                            let qx = if right { px - half } else { px };
                            let qy = if upper { py } else { py - half };
                            // Quadrant boundaries are the cell boundaries (tiles are
                            // centred on cell corners), so a rim is drawn only where the
                            // neighbouring quadrant across the tile's middle is unset:
                            // darker on the sides and bottom, brighter on top.
                            let left_open = right && case & (bit >> 1) == 0;
                            let right_open = !right && case & (bit << 1) == 0;
                            let up_open = !upper && case & (bit << 2) == 0;
                            let down_open = upper && case & (bit >> 2) == 0;
                            let mut c = *colour;
                            let rim = (qx == 0 && left_open)
                                || (qx == half - 1 && right_open)
                                || (qy == half - 1 && down_open);
                            if rim {
                                c = scale(c, 6, 10);
                            } else if qy == 0 && up_open {
                                c = scale(c, 112, 100);
                            }
                            let dither = (hash(x0 + px, y0 + py) % 17) as i32 - 8;
                            c = c.map(|v| (v as i32 + dither).clamp(0, 255) as u8);
                            Rgba([c[0], c[1], c[2], 255])
                        };
                        image.put_pixel(x0 + px, y0 + py, pixel);
                    }
                }
            }
        }
        Atlas {
            tile_px,
            rows,
            image,
        }
    }

    pub fn texel(&self, x: u32, y: u32) -> [u8; 4] {
        self.image.get_pixel(x, y).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_and_tile_rects() {
        let atlas = Atlas::build(&ElementTable::builtin(), 8);
        assert_eq!(atlas.image.dimensions(), (128, 16));
        assert_eq!(tile_rect(8, 1, 5), (40, 8, 8, 8));
    }

    #[test]
    fn a_fully_set_tile_has_no_rims() {
        let atlas = Atlas::build(&ElementTable::builtin(), 8);
        let (x0, y0, _, _) = tile_rect(8, 0, 15);
        let colour = ElementTable::builtin()
            .get(colony_sim::builtin::SANDSTONE)
            .colour;
        for py in 0..8 {
            for px in 0..8 {
                let [r, g, b, a] = atlas.texel(x0 + px, y0 + py);
                assert_eq!(a, 255);
                for (v, c) in [(r, colour[0]), (g, colour[1]), (b, colour[2])] {
                    assert!(
                        (v as i32 - c as i32).abs() <= 8,
                        "only dither at ({px}, {py}): {v} vs {c}"
                    );
                }
            }
        }
    }

    #[test]
    fn set_quadrants_are_opaque_and_unset_ones_transparent() {
        let atlas = Atlas::build(&ElementTable::builtin(), 8);
        for rank in 0..2u8 {
            for case in 0..16u16 {
                let (x0, y0, _, _) = tile_rect(8, rank, case);
                for py in 0..8 {
                    for px in 0..8 {
                        let bit = match (py < 4, px >= 4) {
                            (false, false) => BL,
                            (false, true) => BR,
                            (true, false) => TL,
                            (true, true) => TR,
                        };
                        let a = atlas.texel(x0 + px, y0 + py)[3];
                        assert_eq!(
                            a == 255,
                            case & bit != 0,
                            "rank {rank} case {case} px ({px}, {py})"
                        );
                    }
                }
            }
        }
    }
}
