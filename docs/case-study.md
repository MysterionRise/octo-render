# Octo Render Case Study

## Problem

The goal of `octo-render` is to demonstrate graphics fundamentals by implementing the core shape of an OpenGL-style raster pipeline directly on the CPU in Rust. The portfolio value is not that this replaces OpenGL. The value is that the project makes normally hidden GPU pipeline stages explicit, testable, measurable, and explainable.

The CPU renderer is the primary proof. The `wgpu` viewer is a separate interactive comparison layer.

## Implemented Pipeline

The renderer loads OBJ meshes and MTL materials, normalizes model bounds, applies model/view/projection transforms, clips triangles against the view frustum, maps to the viewport, rasterizes triangles with a top-left barycentric rule, applies render state, runs fragment-style shading, writes a z-buffer, and optionally runs shadow mapping and SSAO/debug passes.

The current renderer supports:

- wireframe, flat, Gouraud, texture, normal-map, normal-debug, shadow, and SSAO modes
- OBJ positions, UVs, normals, polygon triangulation, negative indices, `mtllib`, and `usemtl`
- MTL material values, diffuse texture maps, normal-map keys, and CLI overrides
- perspective-correct interpolation for shader varyings
- six-plane clipped-triangle emission that preserves original-face barycentrics
- deterministic top-left triangle fill behavior
- culling, front-face winding, depth test/write, fill mode, scissor, clear state, color-write state, and blending
- repeat/clamp wrapping and nearest/linear/mipmap filtering
- material controls for base color, ambient, diffuse, specular, shininess, and alpha
- Blinn-Phong specular driven by the same light vector as diffuse shading and the shadow map
- configurable shadow-map size, bias, and none/PCF filtering

## Hardest Technical Issues

The most important correctness issue was clipping. A simple reject-only near-plane guard avoids crashes but is not a real raster pipeline. The current implementation clips triangles against the frustum and carries each clipped vertex's original barycentric coordinates. The rasterizer computes barycentrics in the emitted clipped triangle, remaps them back to the source triangle, then applies perspective-correct interpolation before fragment shading.

That approach keeps the existing shader trait small while preserving UVs, normals, and world-space positions through clipping.

The second important issue was raster determinism. The top-left fill rule means adjacent triangles share edges without double-filling or leaving cracks, which turns a vague "draw triangle" helper into a more defensible rasterizer.

The third issue was lighting consistency, found by auditing the gallery rather than by a failing test. Face normals were computed inward, which Lambert shading hid because the light vector was flipped too. The shadow-map camera used the unflipped vector, so it sat opposite the light that shaded the model, and the shadow render was mostly dark with a stray bright crescent. The same audit found that specular was never evaluated (`--specular`, `--shininess`, and MTL `Ks`/`Ns` were silently ignored), and that unlit fragments were discarded before the depth write, leaving holes behind back-lit surfaces. The fix defines one outward-normal, toward-the-light convention, adds Blinn-Phong specular, and gives the shadow pass its own fixed raster state. Tests now pin each behaviour.

The fourth important issue was evidence. The project now has deterministic golden-image tests, debug outputs, gallery renders, and Criterion benchmarks instead of relying on screenshots alone.

## Correctness Strategy

The test suite covers parser behavior, MTL loading, rasterization, top-left shared-edge behavior, depth state, culling, front-face winding, texture wrapping/filtering/mipmaps, blending, scissor/color-write state, frustum clipping, specular highlights, unlit depth writes, shadow casting and PCF, shadow-pass state isolation, debug outputs, SSAO determinism, and golden-image output.

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

- frustum clipping follows this renderer's projection convention rather than full OpenGL conformance
- mipmap LOD is simplified and does not use shader derivatives
- no stencil buffer, MSAA, cubemaps, instancing, or shader language
- blending supports alpha/additive modes, not arbitrary blend equations
- shadow mapping uses fixed bias and simple PCF
- the bundled meshes have no `vn` normals, so Gouraud shading uses face normals and looks faceted
- SSAO is deterministic and useful for pipeline demonstration, not physically based
- CPU performance is educational and measurable, not production real-time

## Next Work

The highest-value next steps are one measured optimization pass (band-parallel rasterization behind a `--threads` flag, verified byte-identical to the sequential path), smooth vertex-normal generation for meshes without `vn`, richer material assets, and a headless `wgpu` screenshot path for a side-by-side CPU/GPU comparison.
