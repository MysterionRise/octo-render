//! Renders a cave on the GPU and compares it byte for byte with the CPU reference and
//! the golden image. Skips (and passes) on machines without a GPU adapter, which is the
//! case in CI containers; on a developer machine it is the real test of the shader.

use std::path::PathBuf;

use colony_render::atlas::Atlas;
use colony_render::corners::CornerGrid;
use colony_render::headless::{render_to_image, request_device_headless, OFFSCREEN_FORMAT};
use colony_render::palette::{solid_ranks, Luts};
use colony_render::shade::{cpu_reference, Scene};
use colony_render::{CellRenderer, Overlay, PixelCamera, Snapshot, TILE_PX};
use colony_sim::scenario;

#[test]
fn gpu_render_equals_cpu_reference_and_golden() {
    let Some((device, queue)) = request_device_headless() else {
        println!("skipped: no GPU adapter on this machine");
        return;
    };
    let mut sim = scenario::cave(64, 64);
    let mut stats = sim.step();
    for _ in 0..199 {
        stats = sim.step();
    }
    let table = sim.table();
    let snapshot = Snapshot::from_sim(&sim, &vec![true; sim.chunks().len()], 0, stats, 0, 0);
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
    let mut renderer = CellRenderer::new(&device, &queue, OFFSCREEN_FORMAT, table, (64, 64));
    renderer.upload(&queue, &snapshot);
    assert_eq!(renderer.corner_grid(), &corners);
    for overlay in Overlay::ALL {
        renderer.set_view(&queue, &camera, overlay);
        let gpu = render_to_image(&device, &queue, &renderer, camera.viewport());
        let cpu = cpu_reference(&scene, &camera, overlay);
        assert!(
            gpu.as_raw() == cpu.as_raw(),
            "GPU and CPU renders differ for overlay {:?}",
            overlay
        );
        if overlay == Overlay::None {
            let path =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/cave-64-200-z2.png");
            let golden = image::open(&path).unwrap().to_rgba8();
            assert!(
                gpu.as_raw() == golden.as_raw(),
                "GPU render differs from the golden image"
            );
        }
    }
    // A second upload with one dirty chunk must leave the picture unchanged.
    let mut dirty = vec![false; sim.chunks().len()];
    dirty[3] = true;
    let again = Snapshot::from_sim(&sim, &dirty, 0, stats, 0, 0);
    renderer.upload(&queue, &again);
    renderer.set_view(&queue, &camera, Overlay::None);
    let gpu = render_to_image(&device, &queue, &renderer, camera.viewport());
    assert!(gpu.as_raw() == cpu_reference(&scene, &camera, Overlay::None).as_raw());
}
