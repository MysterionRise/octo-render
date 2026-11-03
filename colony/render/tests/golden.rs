//! Pins the CPU reference render of a cave world pixel for pixel. The GPU test compares
//! its own output to the same reference, so this golden indirectly pins the shader too.
//! `UPDATE_GOLDENS=1 cargo test -p colony-render golden` re-blesses it; inspect first.

use std::path::PathBuf;

use colony_render::atlas::Atlas;
use colony_render::corners::CornerGrid;
use colony_render::palette::{solid_ranks, Luts};
use colony_render::shade::{cpu_reference, Scene};
use colony_render::{Overlay, PixelCamera, Snapshot, TILE_PX};
use colony_sim::scenario;

#[test]
fn cave_64_after_200_ticks_at_zoom_2_matches_golden() {
    let mut sim = scenario::cave(64, 64);
    let mut stats = sim.step();
    for _ in 0..199 {
        stats = sim.step();
    }
    let table = sim.table();
    let snapshot = Snapshot::from_sim(&sim, sim.processed_chunks(), 0, stats, 0, 0);
    let mut corners = CornerGrid::new(64, 64);
    corners.rebuild_all(&snapshot, &solid_ranks(table));
    let luts = Luts::from_table(table);
    let atlas = Atlas::build(table, TILE_PX);
    let scene = Scene {
        snapshot: &snapshot,
        corners: &corners,
        luts: &luts,
        atlas: &atlas,
    };
    let camera = PixelCamera::new((64, 64), TILE_PX, (256, 192), 2);
    let img = cpu_reference(&scene, &camera, Overlay::None);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/cave-64-200-z2.png");
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        img.save(&path).unwrap();
        return;
    }
    let golden = image::open(&path)
        .unwrap_or_else(|_| {
            panic!(
                "missing {}: run with UPDATE_GOLDENS=1 to create it",
                path.display()
            )
        })
        .to_rgba8();
    assert_eq!(golden.dimensions(), img.dimensions());
    assert!(golden.as_raw() == img.as_raw(), "CPU reference differs from the golden image; re-bless only for an intentional shading change");
    // The overlays must differ from the plain render somewhere and cover the same area.
    let heat = cpu_reference(&scene, &camera, Overlay::Temperature);
    let pressure = cpu_reference(&scene, &camera, Overlay::Pressure);
    assert_ne!(heat.as_raw(), img.as_raw());
    assert_ne!(pressure.as_raw(), heat.as_raw());
}
