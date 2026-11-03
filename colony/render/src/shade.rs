//! The CPU twin of `shaders/cells.wgsl`: [`shade_pixel`] computes exactly what the
//! fragment shader computes for one screen pixel, and [`cpu_reference`] renders a whole
//! view with it. Tests hold the shader to this function.

use crate::atlas::Atlas;
use crate::camera::{floor_div, PixelCamera};
use crate::corners::CornerGrid;
use crate::palette::Luts;
use crate::snapshot::{Snapshot, MASS_SHADE_MAX};
use crate::Overlay;
use colony_sim::Phase;
use image::{Rgba, RgbaImage};

/// Colour of screen pixels outside the world.
pub const VOID: [u8; 3] = [16, 16, 20];

/// Everything a pixel needs besides the camera.
pub struct Scene<'a> {
    pub snapshot: &'a Snapshot,
    pub corners: &'a CornerGrid,
    pub luts: &'a Luts,
    pub atlas: &'a Atlas,
}

/// The colour of one screen pixel (`sy` counts down from the top).
pub fn shade_pixel(
    scene: &Scene,
    camera: &PixelCamera,
    overlay: Overlay,
    sx: u32,
    sy: u32,
) -> [u8; 3] {
    let snap = scene.snapshot;
    let (px, py) = camera.screen_to_world((sx, sy));
    let t = camera.tile_px() as i64;
    let (cx, cy) = (floor_div(px, t), floor_div(py, t));
    if cx < 0 || cy < 0 || cx >= snap.width as i64 || cy >= snap.height as i64 {
        return VOID;
    }
    let (sub_x, sub_y) = (px - cx * t, py - cy * t);
    let _ = sub_x;
    let [e, packed, m, temp] = snap.texels[snap.index(cx as u32, cy as u32)];
    let base = scene.luts.element_colour(e);
    let phase = Phase::from_bits(packed as u16 & 3);
    let mut rgb = match phase {
        Phase::Vacuum => base,
        Phase::Gas => base.map(|c| ((c as u32 * (64 + m as u32)) >> 8).min(255) as u8),
        Phase::Liquid => {
            let level = (m as i64 * t) / MASS_SHADE_MAX as i64;
            if sub_y < level {
                base
            } else {
                base.map(|c| c >> 2)
            }
        }
        Phase::Solid => {
            let h = t / 2;
            let (kx, ky) = (floor_div(px + h, t), floor_div(py + h, t));
            let v = scene.corners.get(kx as u32, ky as u32);
            let (rank, case) = ((v >> 4) as u32, (v & 15) as u32);
            let (ox, oy) = ((px + h - kx * t) as u32, (py + h - ky * t) as u32);
            let tp = scene.atlas.tile_px;
            let [r, g, b, _] = scene.atlas.texel(case * tp + ox, rank * tp + (tp - 1 - oy));
            [r, g, b]
        }
    };
    if overlay != Overlay::None && e != 0 {
        let key = match overlay {
            Overlay::Pressure => {
                if phase == Phase::Gas {
                    m
                } else {
                    0
                }
            }
            _ => temp,
        };
        rgb = scene.luts.lookup(overlay as u32, key);
    }
    rgb
}

/// Renders the camera's whole viewport on the CPU.
pub fn cpu_reference(scene: &Scene, camera: &PixelCamera, overlay: Overlay) -> RgbaImage {
    let (w, h) = camera.viewport();
    let mut img = RgbaImage::new(w, h);
    for sy in 0..h {
        for sx in 0..w {
            let [r, g, b] = shade_pixel(scene, camera, overlay, sx, sy);
            img.put_pixel(sx, sy, Rgba([r, g, b, 255]));
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::solid_ranks;
    use crate::TILE_PX;
    use colony_sim::{builtin, scenario, Mass, Sim, Temp, TickStats};

    fn snapshot_of(sim: &Sim) -> Snapshot {
        let dirty = vec![true; sim.chunks().len()];
        let stats = TickStats::fresh(0, 0);
        Snapshot::from_sim(sim, &dirty, 0, stats, 0, 0)
    }

    struct Owned {
        snapshot: Snapshot,
        corners: CornerGrid,
        luts: Luts,
        atlas: Atlas,
    }

    fn build(sim: &Sim) -> Owned {
        let table = sim.table();
        let snapshot = snapshot_of(sim);
        let mut corners = CornerGrid::new(snapshot.width, snapshot.height);
        corners.rebuild_all(&snapshot, &solid_ranks(table));
        Owned {
            snapshot,
            corners,
            luts: Luts::from_table(table),
            atlas: Atlas::build(table, TILE_PX),
        }
    }

    fn scene(o: &Owned) -> Scene<'_> {
        Scene {
            snapshot: &o.snapshot,
            corners: &o.corners,
            luts: &o.luts,
            atlas: &o.atlas,
        }
    }

    /// Each screen pixel at zoom 1 with the camera at the origin is a world pixel; the
    /// cell is `world / TILE_PX`, and the headless image is one pixel per cell.
    #[test]
    fn gas_and_vacuum_colours_match_the_headless_renderer_for_every_cell() {
        let mut sim = scenario::gas_mixing(32, 24);
        for _ in 0..30 {
            sim.step();
        }
        let o = build(&sim);
        let cam = PixelCamera::new((32, 24), TILE_PX, (32 * TILE_PX, 24 * TILE_PX), 1);
        let headless = colony_headless::render_elements(&sim);
        let s = scene(&o);
        for cy in 0..24u32 {
            for cx in 0..32u32 {
                let (_, packed, _, _) = {
                    let t = o.snapshot.texels[o.snapshot.index(cx, cy)];
                    (t[0], t[1], t[2], t[3])
                };
                if Phase::from_bits(packed as u16 & 3) == Phase::Solid {
                    continue;
                }
                let sx = cx * TILE_PX + 3;
                let sy = (23 - cy) * TILE_PX + 3;
                let got = shade_pixel(&s, &cam, Overlay::None, sx, sy);
                let want = headless.get_pixel(cx, 23 - cy).0;
                assert_eq!(got, want, "cell ({cx}, {cy})");
            }
        }
    }

    #[test]
    fn temperature_overlay_matches_the_headless_temperature_render() {
        let mut sim = scenario::hot_rock(16, 16);
        sim.set_cell(
            3,
            3,
            builtin::OXYGEN,
            Mass::from_kg(1),
            Temp::from_kelvin(320),
        );
        sim.step();
        let o = build(&sim);
        let cam = PixelCamera::new((16, 16), TILE_PX, (16 * TILE_PX, 16 * TILE_PX), 1);
        let headless = colony_headless::render_temperature(&sim);
        let s = scene(&o);
        for cy in 0..16u32 {
            for cx in 0..16u32 {
                let got = shade_pixel(
                    &s,
                    &cam,
                    Overlay::Temperature,
                    cx * TILE_PX,
                    (15 - cy) * TILE_PX,
                );
                let want = headless.get_pixel(cx, 15 - cy).0;
                assert_eq!(got, want, "cell ({cx}, {cy})");
            }
        }
    }

    #[test]
    fn liquid_cells_fill_from_the_bottom_to_their_level() {
        let table = colony_sim::ElementTable::builtin();
        let mut sim = Sim::new(table, 1, 1);
        sim.set_cell(
            0,
            0,
            builtin::WATER,
            Mass::from_kg(500),
            Temp::from_kelvin(290),
        );
        let o = build(&sim);
        let cam = PixelCamera::new((1, 1), TILE_PX, (TILE_PX, TILE_PX), 1);
        let s = scene(&o);
        let full = o.luts.element_colour(builtin::WATER.index() as u8);
        let dim = full.map(|c| c >> 2);
        // Half full: the bottom 4 of 8 rows are water colour, the top 4 are dim.
        for sy in 0..TILE_PX {
            let got = shade_pixel(&s, &cam, Overlay::None, 0, sy);
            let world_y = TILE_PX - 1 - sy;
            assert_eq!(got, if world_y < 4 { full } else { dim }, "row {sy}");
        }
    }

    #[test]
    fn solids_read_the_atlas_and_outside_is_void() {
        let table = colony_sim::ElementTable::builtin();
        let mut sim = Sim::new(table, 2, 2);
        sim.fill_rect(
            0,
            0,
            2,
            2,
            builtin::SANDSTONE,
            Mass::from_kg(100),
            Temp::from_kelvin(300),
        );
        let o = build(&sim);
        // A viewport wider than the world: the world is centred and the rest is void.
        let cam = PixelCamera::new((2, 2), TILE_PX, (40, 40), 1);
        let s = scene(&o);
        assert_eq!(shade_pixel(&s, &cam, Overlay::None, 0, 0), VOID);
        assert_eq!(shade_pixel(&s, &cam, Overlay::None, 39, 39), VOID);
        let centre = shade_pixel(&s, &cam, Overlay::None, 20, 20);
        assert_ne!(centre, VOID);
        assert_ne!(centre, [0, 0, 0]);
        // Fully solid world: every world pixel is opaque atlas colour near sandstone.
        for sy in 12..28 {
            for sx in 12..28 {
                let c = shade_pixel(&s, &cam, Overlay::None, sx, sy);
                assert!(c[0] > 60 && c[1] > 50, "({sx}, {sy}) = {c:?}");
            }
        }
        let img = cpu_reference(&s, &cam, Overlay::None);
        assert_eq!(img.dimensions(), (40, 40));
    }
}
