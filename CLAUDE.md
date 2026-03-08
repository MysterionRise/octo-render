# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

A software renderer in Rust, implementing a subset of OpenGL functionality following the [tinyrenderer](https://github.com/ssloy/tinyrenderer/wiki/) lessons (0-8). Renders Wavefront OBJ models to TGA images with perspective projection, Gouraud shading, shadow mapping, and ambient occlusion.

## Build & Run Commands

- **Build**: `cargo build --release`
- **Run**: `./target/release/app` (binary name is `app`, not `octo-render`)
- **Benchmarks**: `cargo bench` (uses criterion, benchmarks line drawing)
- **Check/lint**: `cargo clippy`
- **Run tests**: `cargo test`

## Architecture

The crate exposes both a library (`renderer`) and a binary (`app`):

- **`src/renderer.rs`** — Library crate (named `renderer`). Contains:
  - `Model` struct — Full OBJ parser supporting `v`, `vt`, `vn`, and `f v/vt/vn` formats
  - `read_waveform_obj_file()` — Legacy OBJ reader (backwards compat, wraps `Model`)
  - `draw_line()` — Bresenham's line algorithm
  - `triangle()`, `triangle_zbuf()` — Barycentric triangle rasterizer with z-buffer
  - `triangle_shader()` — Shader-based rasterizer
  - `viewport()`, `projection()`, `lookat()`, `transform_point()` — Matrix transforms
  - `Shader` trait — vertex/fragment shader interface
  - `GouraudShader` — Per-vertex Gouraud shading
  - `TextureShader` — Texture-mapped Gouraud shading
  - `NormalMapShader` — Tangent-space normal mapping with Darboux basis
  - `DepthShader` — Shadow map generation (depth-only pass)
  - `ShadowShader` — Gouraud shading + shadow map lookup
  - `compute_ssao()`, `apply_ssao()` — Screen-space ambient occlusion
- **`src/main.rs`** — Binary: 3-pass renderer (shadow map → main render → SSAO post-process)
- **`benches/line_benchmark.rs`** — Criterion benchmark for `draw_line`

Uses `image` (v0.25) for pixel buffers/TGA output and `glam` (v0.32) for vector/matrix math. OBJ models live in `resources/`.
