# Benchmark Results

Benchmarks were collected with Criterion using:

```bash
cargo bench
```

## Environment

- Date: 2026-07-06
- OS: macOS 15.7.4, build 24G517
- Architecture: arm64
- CPU model: unavailable in sandboxed environment
- Rust: `rustc 1.92.0 (ded5c06cf 2025-12-08)`
- Cargo: `cargo 1.92.0 (344c4567c 2025-10-21)`
- Build profile: Criterion bench profile, optimized

## Criterion Summary

| Benchmark | Mean | Notes |
| --- | ---: | --- |
| Line drawing batch | 2.434 ms | 100 random lines on a 10000x10000 image |
| Z-buffered triangle | 728.0 us | Single 512px triangle into a 512x512 image |
| Full frame flat | 1.307 ms | `resources/teapot.obj`, 128x128 |
| Full frame Gouraud | 2.365 ms | `resources/teapot.obj`, 128x128 |
| Full frame shadow | 4.131 ms | `resources/teapot.obj`, 128x128 |

`resources/teapot.obj` contains 6,320 triangles after loading.

## Derived Throughput

| Full-frame mode | Mean frame time | Approx. triangles/sec |
| --- | ---: | ---: |
| Flat | 1.307 ms | 4.84M |
| Gouraud | 2.365 ms | 2.67M |
| Shadow | 4.131 ms | 1.53M |

The full-frame benchmark resolution is intentionally small so Criterion can run enough iterations for stable measurements. The README gallery uses larger 768px and 1024x768 images to show visual quality.

## Interpreting The Numbers

The CPU renderer is pixel-work dominated. The same triangle count can cost more at a higher resolution because the rasterizer visits more covered pixels and runs more fragment work. Shadow and SSAO modes also perform extra passes, so their frame time is not directly comparable to flat shading.
