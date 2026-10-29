# Benchmark Results

The tables in the first half of this page are the July 2026 macOS baseline. The [correctness sprint re-measurement](#correctness-sprint-re-measurement-october-2026) below compares the code before and after the lighting fixes on one Linux machine.

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
| Line drawing batch | 2.6906 ms | 100 unseeded random lines on a 10000x10000 image (old fixture) |
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

## Correctness Sprint Re-measurement (October 2026)

The correctness sprint changed what several benchmarks do, so it was measured as a before/after pair on one machine, back to back. "Before" is commit `8316fbc`; "after" is the sprint.

- Date: 2026-10-27
- OS: Linux 6.18 (cloud VM)
- CPU: Intel Xeon @ 2.10 GHz, 4 cores (benchmarks are single-threaded)
- Rust: `rustc 1.97.0 (2d8144b78 2026-07-07)`, `cargo 1.97.0`
- Build profile: Criterion bench profile, optimized

| Benchmark | Before | After | Change | Cause |
| --- | ---: | ---: | ---: | --- |
| Line drawing batch | 4.8292 ms | 0.5178 ms | not comparable | fixture changed: seeded RNG, 2048x2048 target |
| Z-buffered triangle, 512px | 1.4894 ms | 1.4157 ms | -4.9% | code path unchanged; run-to-run noise |
| Full frame flat, 128x128 | 1.5339 ms | 2.7943 ms | +82.2% | every face is now clipped, rasterized, and depth-written |
| Full frame Gouraud, 128x128 | 3.5980 ms | 3.7267 ms | +3.6% | no lighting discards, offset by the cached normal matrix |
| Full frame shadow, 128x128 | 6.3349 ms | 6.3679 ms | +0.5% | same as Gouraud |
| Flat, 512x512 | 3.2304 ms | 5.6611 ms | +75.2% | same as flat 128 |
| Flat, 1024x1024 | 6.9152 ms | 11.374 ms | +64.5% | same as flat 128 |
| Clipping, unclipped | 354.3 us | 315.9 us | -10.8% | cached normal matrix |
| Clipping, near-plane clipped | 396.4 us | 328.5 us | -17.1% | cached normal matrix |
| Texture nearest | 1.8635 ms | 1.7306 ms | -7.1% | cached normal matrix |
| Texture linear | 3.0580 ms | 2.7793 ms | -9.1% | cached normal matrix |
| Texture mipmap | 2.9215 ms | 2.8867 ms | -1.2% | cached normal matrix |
| Blend off | 1.9038 ms | 1.8198 ms | -4.4% | cached normal matrix |
| Blend alpha | 2.3343 ms | 2.3085 ms | -1.1% | cached normal matrix |
| Blend additive | 2.2851 ms | 2.0923 ms | -8.4% | cached normal matrix |
| Shadow no filter, 256x256 | 8.0527 ms | 8.4207 ms | +4.6% | new default light, and unlit fragments now write depth |
| Shadow PCF, 256x256 | 8.7763 ms | 9.2559 ms | +5.5% | same as shadow no filter |

The flat regression is the honest cost of rendering every face. The old flat path skipped any face whose shading came out black before clipping it. Under the old inward-normal convention that was roughly half the teapot, and it is exactly what left holes behind back-lit surfaces. Gouraud and shadow modes discarded per fragment instead, after clipping and rasterization, so removing their discard costs little.

At this point back-face culling recovered only part of the flat regression (best of 15 CLI runs: -19% at 128x128, -30% at 1024x1024), because culling ran in screen space after every face had been clipped.

## Culling Before Clipping (October 2026)

The next change moved culling ahead of clipping (a clip-space winding test) and let triangles that lie inside every frustum plane skip the clipper entirely. That is nearly every triangle in a normal view, and the clipper allocated about nine `Vec`s per face. Output is bit-identical. Same machine and toolchain as above, measured back to back on 2026-10-29; "before" is commit `6a3f6be` (smooth normals), both runs use the same benchmark file.

| Benchmark | Before | After | Change |
| --- | ---: | ---: | ---: |
| Full frame flat, 128x128 | 2.7455 ms | 1.4350 ms | -47.7% |
| Full frame Gouraud, 128x128 | 3.6498 ms | 2.0826 ms | -42.9% |
| Full frame shadow, 128x128 | 6.8944 ms | 3.6268 ms | -47.4% |
| Flat, 512x512 | 5.5724 ms | 3.9073 ms | -29.9% |
| Flat, 1024x1024 | 11.611 ms | 10.022 ms | -13.7% |
| Culling: flat 128, `--cull off` | 2.7880 ms | 1.4010 ms | -49.7% |
| Culling: flat 128, `--cull back` | 2.2143 ms | 0.6661 ms | -69.9% |
| Culling: flat 1024, `--cull off` | 11.824 ms | 10.233 ms | -13.5% |
| Culling: flat 1024, `--cull back` | 7.1685 ms | 5.7522 ms | -19.8% |
| Clipping, unclipped | 305.7 us | 312.7 us | +2.3% (noise) |
| Clipping, near-plane clipped | 332.3 us | 330.4 us | -0.6% (noise) |
| Shadow no filter, 256x256 | 8.5523 ms | 5.5528 ms | -35.1% |
| Shadow PCF, 256x256 | 8.9442 ms | 5.9898 ms | -33.0% |

Most of the gain is the trivial-accept path, which is why `--cull off` improves as much as `--cull back`. Small frames gain the most because geometry dominates them; at 1024x1024 pixel work dominates. Flat mode at 128x128 is now faster than before the correctness sprint (1.53 ms on commit `8316fbc`), even though it rasterizes every face. The clipping benchmarks use a one-triangle scene, so they do not move. The default stays `--cull off` because the teapot is not a closed mesh.

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
