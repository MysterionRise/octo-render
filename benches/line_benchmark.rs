use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use glam::Vec3;
use image::{ImageBuffer, Rgb, RgbImage};
use rand::Rng;
use renderer::{draw_line, render_model, triangle_zbuf, Model, RenderMode, RenderOptions};

type Pairs = (i32, i32, i32, i32);

fn criterion_benchmark(c: &mut Criterion) {
    bench_line_drawing(c);
    bench_triangle_rasterization(c);
    bench_full_model_render(c);
}

fn bench_line_drawing(c: &mut Criterion) {
    let mut rng = rand::thread_rng();
    let test_size = 100usize;
    let from: i32 = 0;
    let to: i32 = 10000;
    let test_pairs = (0..test_size)
        .map(|_| {
            (
                rng.gen_range(from..to),
                rng.gen_range(from..to),
                rng.gen_range(from..to),
                rng.gen_range(from..to),
            )
        })
        .collect::<Vec<Pairs>>();

    let tga_red = Rgb([255, 0, 0]);
    let mut image: RgbImage = ImageBuffer::new(to as u32, to as u32);
    let mut group = c.benchmark_group("drawing line");

    group.bench_with_input("drawing line", &test_pairs, |b, t| {
        b.iter(|| {
            for (x1, y1, x2, y2) in t.iter() {
                draw_line(*x1, *y1, *x2, *y2, &mut image, tga_red);
            }
        })
    });

    group.finish();
}

fn bench_triangle_rasterization(c: &mut Criterion) {
    let red = Rgb([255, 0, 0]);
    c.bench_function("triangle/zbuffered 512px", |b| {
        b.iter_batched(
            || {
                (
                    ImageBuffer::from_pixel(512, 512, Rgb([0, 0, 0])),
                    vec![f32::NEG_INFINITY; 512 * 512],
                )
            },
            |(mut image, mut zbuffer)| {
                triangle_zbuf(
                    Vec3::new(32.0, 32.0, 0.2),
                    Vec3::new(480.0, 64.0, 0.2),
                    Vec3::new(240.0, 480.0, 0.2),
                    &mut zbuffer,
                    &mut image,
                    red,
                );
                black_box(image);
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_full_model_render(c: &mut Criterion) {
    let model = Model::from_file("resources/teapot.obj");
    let mut group = c.benchmark_group("software renderer full frame");
    group.sample_size(10);

    for mode in [RenderMode::Flat, RenderMode::Gouraud, RenderMode::Shadow] {
        let options = RenderOptions {
            width: 128,
            height: 128,
            mode,
            camera_eye: None,
            light_dir: Vec3::new(1.0, 1.0, -1.0),
            debug_outputs: false,
            ..RenderOptions::default()
        };
        group.bench_function(mode.as_str(), |b| {
            b.iter(|| {
                let output = render_model(black_box(&model), black_box(&options));
                black_box(output.image);
            })
        });
    }

    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
