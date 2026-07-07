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
| Line drawing batch | 2.8124 ms | 100 random lines on a 10000x10000 image |
| Z-buffered triangle | 663.53 us | Single 512px triangle into a 512x512 image |
| Full frame flat | 1.1599 ms | `resources/teapot.obj`, 128x128 |
| Full frame Gouraud | 2.7501 ms | `resources/teapot.obj`, 128x128 |
| Full frame shadow | 4.9272 ms | `resources/teapot.obj`, 128x128 |
| Clipping test, unclipped | 285.87 us | one-triangle deterministic model, 256x256 |
| Clipping test, near-plane clipped | 298.14 us | same model, clipped by near plane |
| Texture nearest | 1.1168 ms | `resources/textured-quad.obj`, 256x256 |
| Texture linear | 1.7330 ms | same scene with bilinear filtering |
| Shadow no filter | 6.5043 ms | `resources/teapot.obj`, 256x256, 256px shadow map |
| Shadow PCF | 6.7547 ms | same scene with 3x3 PCF |

`resources/teapot.obj` contains 6,320 triangles after loading.

## Derived Throughput

| Full-frame mode | Resolution | Mean frame time | Approx. triangles/sec |
| --- | ---: | ---: | ---: |
| Flat | 128x128 | 1.1599 ms | 5.45M |
| Gouraud | 128x128 | 2.7501 ms | 2.30M |
| Shadow | 128x128 | 4.9272 ms | 1.28M |
| Flat | 512x512 | 2.2749 ms | 2.78M |
| Flat | 1024x1024 | 5.0397 ms | 1.25M |
| Shadow, no filter | 256x256 | 6.5043 ms | 0.97M |
| Shadow, PCF | 256x256 | 6.7547 ms | 0.94M |

## Resolution Scaling

| Mode | Resolution | Mean |
| --- | ---: | ---: |
| Flat | 128x128 | 1.1077 ms |
| Flat | 512x512 | 2.2749 ms |
| Flat | 1024x1024 | 5.0397 ms |

## Interpreting The Numbers

The renderer is pixel-work dominated. A large triangle can cost more than many tiny triangles because the CPU path loops over covered pixels, computes barycentric coverage, performs depth testing, and runs fragment shading directly in Rust.

The clipping path adds a small amount of CPU work on the tiny benchmark scene: 285.87 us unclipped versus 298.14 us when the triangle crosses the near plane. That cost buys real clipped-triangle emission with preserved varyings.

Linear texture filtering is visibly more expensive than nearest sampling because every texture lookup samples and blends four texels. PCF shadowing is also more expensive than a single compare, but the measured 256px teapot case keeps the additional cost modest.

## Optimization Notes

- Rasterization currently scans each triangle's screen-space bounding box; tiled rasterization or scanline edge walking would reduce wasted coverage checks.
- Fragment shading is scalar CPU code; SIMD or parallel tiling would be the next serious performance step.
- Shadow and SSAO are useful portfolio passes, not production-quality real-time effects.
- The benchmark suite favors reproducible evidence over peak tuning.
