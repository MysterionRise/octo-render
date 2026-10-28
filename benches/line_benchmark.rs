use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use glam::Vec3;
use image::{ImageBuffer, Rgb, RgbImage};
use rand::{rngs::StdRng, Rng, SeedableRng};
use renderer::{
    build_mipmaps, draw_line, render_model, triangle_zbuf, BlendMode, FaceVertex, Material, Model,
    RenderMode, RenderOptions, RenderState, ShadowFilter, ShadowOptions, TextureFilter,
    TextureSampler, TextureWrap, DEFAULT_LIGHT_DIR,
};

type Pairs = (i32, i32, i32, i32);

fn criterion_benchmark(c: &mut Criterion) {
    bench_line_drawing(c);
    bench_triangle_rasterization(c);
    bench_full_model_render(c);
    bench_clipped_model_render(c);
    bench_texture_filter_variants(c);
    bench_blend_variants(c);
    bench_shadow_filter_variants(c);
}

fn bench_line_drawing(c: &mut Criterion) {
    // Seeded so every run (and every machine) draws the same 100 lines. The
    // 2048x2048 target keeps the fixture at ~12 MB instead of ~300 MB.
    let mut rng = StdRng::seed_from_u64(0x0c70);
    let test_size = 100usize;
    let from: i32 = 0;
    let to: i32 = 2048;
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
    let mut group = c.benchmark_group("software renderer full frame 128");
    group.sample_size(10);

    for mode in [RenderMode::Flat, RenderMode::Gouraud, RenderMode::Shadow] {
        let options = RenderOptions {
            width: 128,
            height: 128,
            mode,
            camera_eye: None,
            light_dir: DEFAULT_LIGHT_DIR,
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

    let mut resolution_group = c.benchmark_group("software renderer resolution scaling");
    resolution_group.sample_size(10);
    for resolution in [128, 512, 1024] {
        let options = RenderOptions {
            width: resolution,
            height: resolution,
            mode: RenderMode::Flat,
            camera_eye: None,
            light_dir: DEFAULT_LIGHT_DIR,
            debug_outputs: false,
            ..RenderOptions::default()
        };
        resolution_group.bench_function(format!("flat/{resolution}x{resolution}"), |b| {
            b.iter(|| {
                let output = render_model(black_box(&model), black_box(&options));
                black_box(output.image);
            })
        });
    }
    resolution_group.finish();
}

fn bench_clipped_model_render(c: &mut Criterion) {
    let model = clipping_test_model();
    let mut group = c.benchmark_group("software renderer clipping");
    group.sample_size(10);

    for (name, near_plane) in [("unclipped", 0.01), ("near-plane-clipped", 1.8)] {
        let options = RenderOptions {
            width: 256,
            height: 256,
            mode: RenderMode::Gouraud,
            camera_eye: Some(Vec3::new(0.0, 0.0, 3.0)),
            light_dir: Vec3::new(0.0, 0.0, 1.0),
            near_plane,
            ..RenderOptions::default()
        };
        group.bench_function(name, |b| {
            b.iter(|| {
                let output = render_model(black_box(&model), black_box(&options));
                black_box(output.image);
            })
        });
    }

    group.finish();
}

fn bench_texture_filter_variants(c: &mut Criterion) {
    let model = Model::from_file("resources/textured-quad.obj");
    let texture = image::open("resources/checker.ppm").unwrap().to_rgb8();
    let mut group = c.benchmark_group("software renderer texture filtering");
    group.sample_size(10);

    let mipmaps = build_mipmaps(&texture);
    for filter in [
        TextureFilter::Nearest,
        TextureFilter::Linear,
        TextureFilter::Mipmap,
    ] {
        let options = RenderOptions {
            width: 256,
            height: 256,
            mode: RenderMode::Texture,
            texture: Some(texture.clone()),
            texture_mipmaps: if filter == TextureFilter::Mipmap {
                mipmaps.clone()
            } else {
                Vec::new()
            },
            texture_sampler: TextureSampler {
                wrap: TextureWrap::Repeat,
                filter,
            },
            ..RenderOptions::default()
        };
        group.bench_function(filter.as_str(), |b| {
            b.iter(|| {
                let output = render_model(black_box(&model), black_box(&options));
                black_box(output.image);
            })
        });
    }

    group.finish();
}

fn bench_blend_variants(c: &mut Criterion) {
    let model = Model::from_file("resources/textured-quad.obj");
    let texture = image::open("resources/checker.ppm").unwrap().to_rgb8();
    let mut group = c.benchmark_group("software renderer blending");
    group.sample_size(10);

    for blend_mode in [BlendMode::Off, BlendMode::Alpha, BlendMode::Additive] {
        let options = RenderOptions {
            width: 256,
            height: 256,
            mode: RenderMode::Texture,
            texture: Some(texture.clone()),
            material: Material {
                alpha: 0.55,
                ..Material::default()
            },
            render_state: RenderState {
                blend_mode,
                depth: renderer::DepthState {
                    test_enabled: false,
                    write_enabled: false,
                },
                ..RenderState::default()
            },
            ..RenderOptions::default()
        };
        group.bench_function(blend_mode.as_str(), |b| {
            b.iter(|| {
                let output = render_model(black_box(&model), black_box(&options));
                black_box(output.image);
            })
        });
    }

    group.finish();
}

fn bench_shadow_filter_variants(c: &mut Criterion) {
    let model = Model::from_file("resources/teapot.obj");
    let mut group = c.benchmark_group("software renderer shadow filtering");
    group.sample_size(10);

    for filter in [ShadowFilter::None, ShadowFilter::Pcf] {
        let options = RenderOptions {
            width: 256,
            height: 256,
            mode: RenderMode::Shadow,
            shadow: ShadowOptions {
                size: 256,
                bias: 4.0,
                filter,
            },
            ..RenderOptions::default()
        };
        group.bench_function(filter.as_str(), |b| {
            b.iter(|| {
                let output = render_model(black_box(&model), black_box(&options));
                black_box(output.image);
            })
        });
    }

    group.finish();
}

fn clipping_test_model() -> Model {
    Model {
        vertices: vec![
            Vec3::new(-0.8, -0.8, -0.8),
            Vec3::new(0.8, -0.8, -0.8),
            Vec3::new(0.0, 0.8, 1.2),
        ],
        texcoords: Vec::new(),
        normals: vec![Vec3::new(0.0, 0.0, 1.0)],
        faces: vec![[
            FaceVertex {
                v: 0,
                vt: None,
                vn: Some(0),
            },
            FaceVertex {
                v: 1,
                vt: None,
                vn: Some(0),
            },
            FaceVertex {
                v: 2,
                vt: None,
                vn: Some(0),
            },
        ]],
        materials: Vec::new(),
        face_materials: vec![None],
    }
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
