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

/// A hall with walking, carrying colonists and debris on the ground: the GPU frame must
/// equal the CPU cell reference with the sprites stamped over it.
#[test]
fn gpu_render_with_colonists_equals_cpu_composite() {
    use colony_render::sprites::{sprite_instances, stamp_sprites};
    use colony_sim::agent::DEFAULT_PRIORITY;
    use colony_sim::JobKind;

    let Some((device, queue)) = request_device_headless() else {
        println!("skipped: no GPU adapter on this machine");
        return;
    };
    let mut sim = scenario::hall(64, 24, 1, 4, 0);
    let party: Vec<(u32, u32)> = sim.colonists().iter().map(|(_, c)| c.cell).collect();
    for &(cx, cy) in &party {
        if let Some(x) = (cx + 1..63).find(|&x| sim.nav().is_solid(x, cy)) {
            sim.order(JobKind::Dig { x, y: cy }, DEFAULT_PRIORITY);
        }
    }
    let mut stats = sim.step();
    for _ in 0..30 {
        stats = sim.step();
    }
    for &(cx, cy) in &party {
        sim.order(JobKind::Build { x: cx, y: cy + 1 }, DEFAULT_PRIORITY);
    }
    for _ in 0..12 {
        stats = sim.step();
    }
    let table = sim.table();
    let snapshot = Snapshot::from_sim(&sim, &vec![true; sim.chunks().len()], 0, stats, 0, 0);
    assert_eq!(snapshot.colonists.len(), 4);
    assert!(
        snapshot.colonists.iter().any(|c| c.from != c.to),
        "someone is mid-move"
    );
    let mut corners = CornerGrid::new(64, 24);
    corners.rebuild_all(&snapshot, &solid_ranks(table));
    let luts = Luts::from_table(table);
    let atlas = Atlas::build(table, TILE_PX);
    let scene = Scene {
        snapshot: &snapshot,
        corners: &corners,
        luts: &luts,
        atlas: &atlas,
    };
    let mut renderer = CellRenderer::new(&device, &queue, OFFSCREEN_FORMAT, table, (64, 24));
    renderer.upload(&queue, &snapshot);
    for (zoom, alpha) in [(1, 0), (3, 128), (8, 256)] {
        let camera = PixelCamera::new((64, 24), TILE_PX, (320, 160), zoom);
        let sprites = sprite_instances(&snapshot, &luts, TILE_PX, alpha);
        assert!(sprites.len() >= 4);
        renderer.set_sprites(&device, &queue, &sprites);
        renderer.set_view(&queue, &camera, Overlay::None);
        let gpu = render_to_image(&device, &queue, &renderer, camera.viewport());
        let mut cpu = cpu_reference(&scene, &camera, Overlay::None);
        stamp_sprites(&mut cpu, &camera, &sprites);
        assert!(
            gpu.as_raw() == cpu.as_raw(),
            "GPU and CPU composites differ at zoom {zoom}, alpha {alpha}"
        );
    }
    renderer.set_sprites(&device, &queue, &[]);
    let camera = PixelCamera::new((64, 24), TILE_PX, (320, 160), 2);
    renderer.set_view(&queue, &camera, Overlay::None);
    let gpu = render_to_image(&device, &queue, &renderer, camera.viewport());
    assert!(
        gpu.as_raw() == cpu_reference(&scene, &camera, Overlay::None).as_raw(),
        "no sprites leaves the cell render alone"
    );
}
