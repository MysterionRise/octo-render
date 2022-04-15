use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use renderer::{draw_line};
use rand::Rng;
use image::{Rgb, RgbImage, ImageBuffer};

type Pairs = (i32, i32, i32, i32);

fn criterion_benchmark(c: &mut Criterion) {
    let mut rng = rand::thread_rng();
    let test_size = 100usize;
    let from: i32 = 0;
    let to: i32 = 10000;
    let test_pairs = (0..test_size).map(|_| {
        (rng.gen_range(from..to), rng.gen_range(from..to), rng.gen_range(from..to), rng.gen_range(from..to))
    }).collect::<Vec<Pairs>>();

    let tga_red = Rgb([255, 0, 0]);
    let tga_white = Rgb([255, 255, 255]);
    let mut image: RgbImage = ImageBuffer::new(to as u32, to as u32);
    let mut group = c.benchmark_group("drawing line");

    group.bench_with_input(
        "drawing line",
        &test_pairs,
        |b, t| {
            b.iter(|| {
                for (x1, y1, x2, y2) in t.iter() {
                    draw_line(*x1, *y1, *x2, *y2, &mut image, tga_red);
                }
            })
        },
    );

    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);