# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A from-scratch Rust CPU software rasterizer that reimplements the core of an OpenGL-style pipeline. It follows the [tinyrenderer](https://github.com/ssloy/tinyrenderer/wiki/) lessons 0-8 and extends them with six-plane frustum clipping, a top-left fill rule, explicit render state, texture sampling with mipmaps, MTL materials, Blinn-Phong specular, shadow mapping with PCF, and SSAO. It renders Wavefront OBJ models to PNG (or any format the `image` crate writes, chosen by the output extension).

A separate `wgpu` viewer under `crates/` is an interactive GPU comparison layer. It is not the CPU proof and shares no code with the CPU renderer.

## Build & Run Commands

- **Build everything**: `cargo build --release --workspace`
- **CPU renderer CLI**: `cargo run --release --bin app -- --model resources/teapot.obj --shader shadow --output out.png` (the binary is `app`; `--help` lists every flag)
- **GPU viewer**: `cargo run --release -p octo-app` (needs a display and a Metal or Vulkan adapter)
- **Lint**: `cargo fmt --check` and `cargo clippy --workspace --all-targets` (CI runs both plus the tests)
- **Tests**: `cargo test --workspace`. CPU renderer tests live in the `#[cfg(test)] mod tests` at the bottom of `src/renderer.rs`; the CLI parser test is in `src/main.rs`. The `crates/*` GPU stack has no tests.
- **Benchmarks**: `cargo bench` (Criterion, `benches/line_benchmark.rs`: lines, triangles, full frames, clipping, texture filters, blending, shadow filters, resolution scaling)
- **Gallery**: `scripts/render-gallery.sh` regenerates every image in `renders/` with the exact commands used for the README.
- **Demo reel**: `scripts/make-demo.sh` renders `examples/demo.rs` (a 21 s, 9:16 storyboard of `render_scene` frames with 2×2 supersampling) and uses ffmpeg (libx264 + drawtext) to add captions, a progress bar, and an end card, writing `renders/demo/octo-render-demo.mp4` and a GIF under 8 MB. `--preview` renders 270×480 into `target/demo-preview/` in seconds. Frames go to `target/demo/`.
- **Golden images**: `UPDATE_GOLDENS=1 cargo test -p octo-render golden` rewrites `tests/golden/*.png` from the current output. Only do this for an intentional pixel change, and inspect the image diff before committing. `tests/golden/legacy-camera-flat-teapot-64.png` is a frozen render from the pre-October-2026 camera; it is compared directly and never re-blessed.

## Architecture

Workspace root package `octo-render`:

- **`src/renderer.rs`** — library crate `renderer`, a single large file:
  - OBJ/MTL loading: `Model::load` / `Model::load_with_options` (`v`/`vt`/`vn`, polygon fan triangulation, negative indices, `mtllib`/`usemtl`), `Model::generate_missing_normals` (welded, angle-weighted, crease-limited smooth normals for corners without `vn`), `ObjMaterial`, `Material`
  - Transforms: `viewport`, `projection`, `lookat`, `transform_point`
  - Raster: `draw_line` (Bresenham), `triangle_shader_perspective_with_state` and wrappers (barycentric coverage, top-left rule, perspective-correct varyings), `clip_varying_triangle_against_frustum` (six planes, preserves source barycentrics)
  - Render state: `RenderState`, `CullMode`, `FrontFace`, `DepthState`, `FillMode`, `ScissorRect`, `BlendMode`
  - `Shader` trait and built-ins: `GouraudShader`, `TextureShader`, `NormalMapShader`, `NormalDebugShader`, `DepthShader`, `ShadowShader`
  - Textures: `TextureSampler`, `sample_texture_with_mipmaps`, `build_mipmaps`
  - `render_scene(&[SceneObject], &RenderOptions, scene_radius) -> RenderOutput` orchestrates a frame: shadow-map depth pass over every object (fixed depth-only state) → main pass per object (each `SceneObject` has its own model-to-scene transform and material, sharing one z-buffer) → SSAO post-process → debug maps → vertical flip. `render_model(&Model, &RenderOptions)` is a scene of one object recentred and scaled to `SCENE_RADIUS` (`SceneObject::normalized`, `normalized_transform`).
- **`src/procedural.rs`** — `renderer::procedural`: meshes generated from math with analytic (or central-difference) unit normals, outward CCW winding, and bit-exact seams and poles: `grid_surface` (shared parametric builder), `ground_grid`, `ground_disk`, `uv_sphere`, `torus`, `torus_knot` (parallel-transport frame), `parametric`, `supershape` (`Superformula`), `twisted_torus` (`TwistedTorus`). Tests weld by exact position and check watertightness, winding, volumes, and normals.
- **`src/main.rs`** — `app` CLI: hand-rolled flag parser, material and texture resolution, writes the image plus optional debug PNGs
- **`benches/line_benchmark.rs`** — Criterion suite
- **`scripts/render-gallery.sh`** — reproducible gallery renders
- **`examples/demo.rs`** + **`scripts/make-demo.sh`** — the demo reel: storyboard segments (view, cast, eased camera orbit, light sweep), per-frame procedural meshes, parallel frame rendering, caption timeline, ffmpeg assembly

`crates/` (GPU viewer): `core` (mesh types), `asset` (its own OBJ loader), `gpu` (wgpu context, buffers, textures), `renderer` (shadow, forward, post-process passes), `app` (the `octo-app` binary, winit + egui), and `shaders/*.wgsl`.

## Conventions

- **Light direction**: `RenderOptions::light_dir` and `--light` give the direction *toward* the light in normalized scene space (default `DEFAULT_LIGHT_DIR`, 1,1,1). `render_model` rotates it into eye space once for shading, and renders the shadow map with an orthographic camera along it, so diffuse, specular, and shadows always agree. The shadow camera scales with the scene radius `r` (`shadow_camera`): half-extent `1.025r`, eye `2r` out, its own near/far planes, `0.5/r` NDC depth per unit; at `SCENE_RADIUS` (2) that is 2.05, 4, and ~32 depth units per scene unit. `--shadow-bias` is in depth units, so one unit is `r/63.75` scene units. `RenderOptions::camera_target` (default the origin) is where the camera looks; the light always aims at the origin. Built-in shaders take the eye-space light.
- **Normals** point outward. `Model::face_normal` assumes counter-clockwise winding (the OBJ convention) and only falls back to +Z for zero-area faces. `Model::load` generates smooth normals for corners without `vn` (crease angle `DEFAULT_CREASE_ANGLE_DEG`, 60°); flat mode always uses face normals.
- **Camera**: `build_transforms` uses `lookat` (eye at the view-space origin) and `perspective` (vertical FOV, aspect-correct, reversed-Z with an infinite far plane, `depth_scale = distance² / 4`), with a margin-free viewport. Default eye `DEFAULT_CAMERA_EYE` (0, 0.5, 6), FOV `DEFAULT_FOV_Y_DEGREES` (45). Near/far are view-space distances from the eye, applied by the clipper; each pass (camera, shadow) carries its own in `RenderTransforms`. The legacy `projection` helper is unused by `render_model`.
- **Depth**: larger z is closer. The z-buffer clears to `-inf` and the depth test passes when `z > current`.
- **No lighting discards**: unlit fragments shade to ambient (or black) and still write depth, so they occlude what is behind them.
- **Scene space**: models are recentred and scaled into a normalized scene before the camera is applied, so `--camera` and `--light` live in that space.

Dependencies: `image` 0.25 (PNG, PNM, TGA, and JPEG codecs only) and `glam` 0.32. OBJ models and textures live in `resources/`; write-ups live in `docs/`.

## Planned: Colony-Sim Engine

`docs/colony-engine-design.md` is the research, architecture, and roadmap for a purpose-built 2D colony-sim engine (an Oxygen Not Included-like cell simulation with pixel art) on a thin custom `wgpu` core that reuses `crates/gpu` and the `crates/app` wiring. It is design only: no engine code exists yet. When that work starts, the crates go under a new top-level `colony/` folder (placeholder names `colony-sim`, `colony-content`, `colony-headless`, `colony-render`, `colony-game`). The simulation kernel crate must not depend on `wgpu`, `winit`, or `glam`, and its state and tick code use fixed-point integers, never `f32`/`f64`; the doc's section 11 lists the determinism rules.
