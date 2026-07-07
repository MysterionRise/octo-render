# Profiling Notes

The goal of profiling `octo-render` is to make the performance story defensible: benchmark first, identify the hot path, then optimize one thing at a time. The CPU renderer is intentionally scalar and educational, so the useful question is not "is this faster than a GPU?" It is "where does the software pipeline spend time, and what evidence supports that?"

## Current Evidence

The reproducible baseline is Criterion:

```bash
cargo bench
```

Detailed benchmark numbers live in [benchmarks.md](benchmarks.md). The benchmark suite covers:

- line drawing
- z-buffered triangle rasterization
- full-frame flat, Gouraud, and shadow renders
- clipped versus unclipped triangles
- nearest, linear, and mipmap texture sampling
- shadow no-filter versus PCF
- alpha/additive blending
- resolution scaling

## Hotspots

The current hotspots are the expected software rasterizer hotspots:

- triangle bounding-box scans
- edge/barycentric coverage tests for every candidate pixel
- perspective-correct varying interpolation
- z-buffer reads/writes
- texture sampling, especially bilinear and mipmap paths
- shadow-map lookups, especially PCF
- SSAO post-processing when enabled

The first measured optimization in this pass targeted the per-pixel coverage loop. The initial top-left implementation computed edge inclusion and then recomputed barycentrics separately. Reusing one precomputed edge-function coverage object improved the z-buffered triangle benchmark from `1.6019 ms` to `1.1791 ms` and improved the 1024x1024 flat render from `8.8056 ms` to `7.9883 ms`.

The code to inspect first is:

- `triangle_shader_perspective_with_state` for coverage, depth, perspective interpolation, and fragment dispatch
- `sample_texture_with_mipmaps` for sampler cost
- `blend_fragment` for color write state
- `render_shadow_map` and `ShadowShader` for shadow pass cost
- `compute_ssao` for post-process cost

## Instruments And Flamegraph Workflow

On macOS, use Instruments Time Profiler for call-tree confirmation:

```bash
cargo build --release --bin app
xcrun xctrace record \
  --template "Time Profiler" \
  --output target/profiles/octo-render.trace \
  --launch ./target/release/app -- \
  --model resources/teapot.obj \
  --shader shadow \
  --width 1024 \
  --height 1024 \
  --output /tmp/octo-render-profile.png
```

If `cargo-flamegraph` is installed locally, a portable Linux-style workflow is:

```bash
cargo flamegraph --bin app -- \
  --model resources/teapot.obj \
  --shader shadow \
  --width 1024 \
  --height 1024 \
  --output /tmp/octo-render-profile.png
```

No profiler artifact is committed because `.trace` and flamegraph output are machine-specific. The committed evidence is the reproducible Criterion suite plus this profiling playbook.

## Interpretation

The benchmarks show the same shape a software renderer should show:

- resolution increases cost because more pixels are visited
- bilinear and mipmap sampling cost more than nearest sampling
- PCF costs more than a single shadow-map compare
- clipping adds geometry work but is small compared with covered-pixel work for normal scenes
- top-left raster correctness costs more than the older looser coverage path, so the published numbers now favor correctness over peak speed

That is useful CTO-facing evidence because the implementation is not a black box. The performance story matches the architecture.

## Measured Improvement Path

Do not optimize by guessing. The next measured optimization should be one of:

- replace bounding-box barycentric scans with scanline edge walking
- tile the framebuffer and parallelize independent tiles
- cache per-triangle edge deltas and reciprocal area more aggressively
- split shadow and texture paths so simple flat/Gouraud shaders do less per-pixel branching
- SIMD-pack color and depth operations where the code remains understandable

The best first optimization is tiled parallel rasterization because it targets the dominant pixel loop while preserving the current architecture and tests.

## Reporting Format

When publishing new performance numbers, record:

- date
- OS and CPU
- Rust and Cargo versions
- build profile
- model and triangle count
- resolution
- shader mode
- texture/shadow/filter state
- frame time and approximate triangles/sec
