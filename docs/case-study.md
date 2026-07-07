# Octo Render Case Study

## Problem

The goal of `octo-render` is to demonstrate graphics fundamentals by implementing the core shape of an OpenGL-style raster pipeline directly on the CPU in Rust. The portfolio value is not that this replaces OpenGL. The value is that the project makes normally hidden GPU pipeline stages explicit, testable, measurable, and explainable.

The CPU renderer is the primary proof. The `wgpu` viewer is a separate interactive comparison layer.

## Implemented Pipeline

The renderer loads OBJ meshes, normalizes model bounds, applies model/view/projection transforms, clips near-plane-crossing triangles, maps to the viewport, rasterizes triangles with barycentric coordinates, applies render state, runs fragment-style shading, writes a z-buffer, and optionally runs shadow mapping and SSAO/debug passes.

The current renderer supports:

- wireframe, flat, Gouraud, texture, normal-map, normal-debug, shadow, and SSAO modes
- OBJ positions, UVs, normals, polygon triangulation, and negative indices
- perspective-correct interpolation for shader varyings
- near-plane clipped-triangle emission that preserves original-face barycentrics
- culling, front-face winding, depth test/write, fill mode, scissor, clear state, and color-write state
- repeat/clamp wrapping and nearest/linear filtering
- material controls for base color, ambient, diffuse, specular, and shininess
- configurable shadow-map size, bias, and none/PCF filtering

## Hardest Technical Issues

The most important correctness issue was clipping. A simple reject-only near-plane guard avoids crashes but is not a real raster pipeline. The current implementation clips triangles crossing the near plane and carries each clipped vertex's original barycentric coordinates. The rasterizer computes barycentrics in the emitted clipped triangle, remaps them back to the source triangle, then applies perspective-correct interpolation before fragment shading.

That approach keeps the existing shader trait small while preserving UVs, normals, and world-space positions through clipping.

The second important issue was evidence. The project now has deterministic golden-image tests, debug outputs, gallery renders, and Criterion benchmarks instead of relying on screenshots alone.

## Correctness Strategy

The test suite covers parser behavior, rasterization, depth state, culling, front-face winding, texture wrapping/filtering, scissor/color-write state, near-plane clipping, debug outputs, SSAO determinism, and golden-image output.

The renderer intentionally keeps several outputs deterministic so regressions are visible in tests:

- fixed camera/light defaults
- fixed model normalization
- fixed software shadow-map and AO settings
- tolerance-based golden image comparison

## Performance Results

Criterion benchmarks cover line drawing, z-buffered triangle rasterization, full-model render time, clipped vs unclipped rendering, texture filter variants, shadow filter variants, and resolution scaling.

The README keeps only a short performance table. Detailed methodology and current numbers live in [benchmarks.md](benchmarks.md).

## Limitations

The renderer is still intentionally smaller than OpenGL:

- near-plane clipping is implemented; full six-plane homogeneous clipping is future work
- texture filtering has nearest and bilinear modes, but no mipmaps
- no stencil buffer, blending, MSAA, cubemaps, instancing, or shader language
- shadow mapping uses fixed bias and simple PCF
- SSAO is deterministic and useful for pipeline demonstration, not physically based
- CPU performance is educational and measurable, not production real-time

## Next Work

The highest-value next steps are full frustum clipping, exact top-left rasterization rules, blending, mipmaps, MTL material loading, and a short recorded demo comparing CPU renders with the `wgpu` viewer.
