# Benchmark Results

Benchmarks were collected with Criterion using:

```bash
cargo bench
```

## Environment

- Date: 2026-07-07
- OS: macOS 15.7.4, build 24G517
- Architecture: arm64
- CPU model: unavailable in the sandbox (`sysctl` permission denied)
- Rust: `rustc 1.92.0 (ded5c06cf 2025-12-08)`
- Cargo: `cargo 1.92.0 (344c4567c 2025-10-21)`
- Build profile: Criterion bench profile, optimized

## Criterion Summary

| Benchmark | Mean | Notes |
| --- | ---: | --- |
| Line drawing batch | 2.6906 ms | 100 random lines on a 10000x10000 image |
| Z-buffered triangle | 1.1791 ms | Single 512px triangle into a 512x512 image |
| Full frame flat | 3.3940 ms | `resources/teapot.obj`, 128x128 |
| Full frame Gouraud | 7.6778 ms | `resources/teapot.obj`, 128x128 |
| Full frame shadow | 15.508 ms | `resources/teapot.obj`, 128x128 |
| Clipping test, unclipped | 345.21 us | one-triangle deterministic model, 256x256 |
| Clipping test, near-plane clipped | 359.84 us | same model, clipped by near plane |
| Texture nearest | 1.6530 ms | `resources/textured-quad.obj`, 256x256 |
| Texture linear | 2.6382 ms | same scene with bilinear filtering |
| Texture mipmap | 2.6238 ms | same scene with generated mip levels and simplified LOD |
| Blend off | 1.6596 ms | `resources/textured-quad.obj`, 256x256 |
| Blend alpha | 2.0479 ms | same scene with source-over alpha blending |
| Blend additive | 1.9702 ms | same scene with additive blending |
| Shadow no filter | 16.702 ms | `resources/teapot.obj`, 256x256, 256px shadow map |
| Shadow PCF | 16.680 ms | same scene with 3x3 PCF; difference is within run noise |

`resources/teapot.obj` contains 6,320 triangles after loading.

## Derived Throughput

| Full-frame mode | Resolution | Mean frame time | Approx. triangles/sec |
| --- | ---: | ---: | ---: |
| Flat | 128x128 | 3.3940 ms | 1.86M |
| Gouraud | 128x128 | 7.6778 ms | 0.82M |
| Shadow | 128x128 | 15.508 ms | 0.41M |
| Flat | 512x512 | 4.7246 ms | 1.34M |
| Flat | 1024x1024 | 7.9883 ms | 0.79M |
| Shadow, no filter | 256x256 | 16.702 ms | 0.38M |
| Shadow, PCF | 256x256 | 16.680 ms | 0.38M |

## Resolution Scaling

| Mode | Resolution | Mean |
| --- | ---: | ---: |
| Flat | 128x128 | 3.4609 ms |
| Flat | 512x512 | 4.7246 ms |
| Flat | 1024x1024 | 7.9883 ms |

## Measured Optimization

The frustum-clipping and top-left-rasterization pass initially made the per-pixel loop slower because the rasterizer computed edge inclusion and then recomputed barycentric coordinates with dot products. The follow-up optimization reuses one precomputed triangle coverage object for both top-left inclusion and barycentric output.

| Benchmark | Before optimization | After optimization | Result |
| --- | ---: | ---: | --- |
| Z-buffered triangle | 1.6019 ms | 1.1791 ms | 26.1% faster |
| Full frame flat, 128x128 | 3.5233 ms | 3.3940 ms | 3.7% faster |
| Flat, 512x512 | 4.9940 ms | 4.7246 ms | 5.4% faster |
| Flat, 1024x1024 | 8.8056 ms | 7.9883 ms | 9.3% faster |

## Interpreting The Numbers

The renderer is pixel-work dominated. A large triangle can cost more than many tiny triangles because the CPU path loops over covered pixels, computes deterministic top-left coverage, performs depth testing, and runs fragment shading directly in Rust.

The clipping path adds a small amount of CPU work on the tiny benchmark scene: 345.21 us unclipped versus 359.84 us when the triangle crosses the near plane. That cost buys clipped-triangle emission with preserved varyings.

Linear texture filtering is visibly more expensive than nearest sampling because every texture lookup samples and blends four texels. Mipmap sampling has similar cost in this small textured-quad scene because the selected mip level quickly reduces texel footprint. Alpha and additive blending are more expensive than replacement writes because they read the destination color and perform per-channel math.

The full-frame numbers are slower than the older near-plane-only renderer because they now include six-plane clipping, deterministic top-left raster behavior, blend-capable color writes, and material/sampler state. This is the right tradeoff for the CTO portfolio goal: correctness and defensible evidence over screenshot-only speed.

## Optimization Notes

- Rasterization currently scans each triangle's screen-space bounding box; tiled rasterization or scanline edge walking would reduce wasted coverage checks.
- The top-left coverage path now reuses edge-function results for barycentric output; further wins require changing the traversal strategy, not just shaving helper calls.
- Fragment shading is scalar CPU code; SIMD or parallel tiling would be the next serious performance step.
- Shadow and SSAO are useful portfolio passes, not production-quality real-time effects.
- The benchmark suite favors reproducible evidence over peak tuning.
