# Tinyrenderer And Habr Comparison

`octo-render` started from the same educational direction as tinyrenderer: implement enough of the graphics pipeline in software to understand what OpenGL normally hides. The current repo is not a line-for-line port. It is a Rust implementation with extra CLI, tests, docs, benchmark evidence, gallery renders, and a separate `wgpu` viewer.

References:

- Habr article provided in planning: <https://habr.com/en/post/248153/>
- Tinyrenderer wiki: <https://github.com/ssloy/tinyrenderer/wiki/>

## Lesson Coverage

| Tinyrenderer/Habr topic | Repo status | Evidence |
| --- | --- | --- |
| Lesson 0: image output and project setup | Implemented | PNG/TGA-capable CLI output and gallery renders |
| Lesson 1: Bresenham line drawing | Implemented | `draw_line` plus line benchmark and tests |
| Lesson 2: triangle rasterization and culling | Implemented and extended | Barycentric rasterizer, top-left fill rule, cull state, front-face winding |
| Lesson 3: hidden-face removal / z-buffer | Implemented and extended | depth clear/test/write state and z-buffer tests |
| Lesson 4: perspective projection | Implemented | model/view/projection transforms and perspective-correct varyings |
| Lesson 5: moving camera | Implemented | CLI camera/light vectors and look-at transform |
| Lesson 6: shader abstraction | Implemented and extended | Rust `Shader` trait with flat, Gouraud, texture, normal debug, shadow modes |
| Lesson 6bis: tangent-space normal mapping | Implemented subset | normal-map mode with CLI or MTL normal-map texture |
| Lesson 7: shadow mapping | Implemented and extended | configurable shadow size, bias, no-filter/PCF, debug shadow map |
| Lesson 8: ambient occlusion | Implemented subset | deterministic SSAO post-process and AO debug map |
| Lesson 9: real OpenGL / GLSL app | Separated deliberately | `octo-app` is a `wgpu` viewer/comparison layer, not the CPU proof |

## Extended Beyond The Lessons

The current repo adds portfolio-grade engineering around the educational renderer:

- robust OBJ parsing with polygon triangulation and negative indices
- OBJ/MTL material loading with diffuse and normal-map references
- explicit OpenGL-like render state exposed through CLI flags
- six-plane frustum clipping with varying preservation
- deterministic top-left raster fill behavior
- texture wrap/filter controls including simplified mipmaps
- alpha and additive blending
- debug outputs for depth, normals, shadow map, and AO
- Criterion benchmarks for primitives and full-frame renders
- golden-image regression tests
- written architecture, benchmark, profiling, and case-study docs

## Not Implemented

These are outside the current claim:

- full OpenGL API compatibility
- GLSL/SPIR-V shader compilation
- command buffers, VAOs, VBOs, or uniform-buffer abstractions
- stencil buffer
- MSAA
- cubemaps
- instancing
- derivative-based shader functions
- gamma-correct color pipeline
- full material system with PBR or multiple texture formats

## Portfolio Framing

The honest claim is:

> A from-scratch Rust CPU software rasterizer implementing core OpenGL-style pipeline stages, with tests, benchmarks, debug outputs, and portfolio-quality evidence.

The claim is not:

> A replacement for OpenGL.

That distinction matters. A CTO or engineering lead should see direct ownership of the important graphics fundamentals without being asked to believe an overbroad compatibility claim.

