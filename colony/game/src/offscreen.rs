//! Screenshot and benchmark without a window. Both need a GPU adapter; on a machine
//! without one they say so and fail.

use std::process::ExitCode;
use std::time::Instant;

use colony_render::headless::{render_to_image, request_device_headless, OFFSCREEN_FORMAT};
use colony_render::{CellRenderer, PixelCamera, Snapshot, TILE_PX};
use colony_sim::{scenario, TickStats};

use crate::Options;

pub const FRAME_SIZE: (u32, u32) = (1920, 1080);

pub fn run(opts: &Options) -> ExitCode {
    let Some((device, queue)) = request_device_headless() else {
        eprintln!("no GPU adapter found: --screenshot and --benchmark need one");
        return ExitCode::FAILURE;
    };
    let Some(mut sim) =
        scenario::by_name_seeded(&opts.scenario, opts.width, opts.height, opts.seed)
    else {
        eprintln!(
            "unknown scenario {:?}; known: {}",
            opts.scenario,
            scenario::NAMES.join(", ")
        );
        return ExitCode::FAILURE;
    };
    let n = sim.chunks().len();
    let mut stats = TickStats {
        tick: 0,
        chunks_processed: n,
        chunks_skipped: 0,
        chunks_processed_mass: n,
    };
    for _ in 0..opts.ticks {
        stats = sim.step();
    }
    let snapshot = Snapshot::from_sim(&sim, &vec![true; n], 0, stats, 0, 0);
    let world = (opts.width as u32, opts.height as u32);
    let mut renderer = CellRenderer::new(&device, &queue, OFFSCREEN_FORMAT, sim.table(), world);
    renderer.upload(&queue, &snapshot);
    let camera = PixelCamera::new(world, TILE_PX, FRAME_SIZE, opts.zoom);
    renderer.set_view(&queue, &camera, opts.overlay);

    if let Some(path) = &opts.screenshot {
        let img = render_to_image(&device, &queue, &renderer, FRAME_SIZE);
        if let Err(e) = img.save(path) {
            eprintln!("{}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!(
            "wrote {} ({}x{}, {} after {} ticks, zoom {})",
            path.display(),
            FRAME_SIZE.0,
            FRAME_SIZE.1,
            opts.scenario,
            opts.ticks,
            opts.zoom
        );
    }
    if let Some(frames) = opts.benchmark {
        let frames = frames.max(1);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("colony benchmark target"),
            size: wgpu::Extent3d {
                width: FRAME_SIZE.0,
                height: FRAME_SIZE.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let draw = || {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("colony benchmark"),
            });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("colony benchmark pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                renderer.draw(&mut pass);
            }
            queue.submit(Some(encoder.finish()));
        };
        draw();
        let _ = device.poll(wgpu::Maintain::Wait);
        let t0 = Instant::now();
        for _ in 0..frames {
            draw();
        }
        let _ = device.poll(wgpu::Maintain::Wait);
        let per = t0.elapsed().as_secs_f64() * 1000.0 / frames as f64;
        println!(
            "{frames} frames at {}x{} zoom {}: {per:.3} ms per frame ({:.0} fps)",
            FRAME_SIZE.0,
            FRAME_SIZE.1,
            opts.zoom,
            1000.0 / per
        );
    }
    ExitCode::SUCCESS
}
