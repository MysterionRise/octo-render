#!/usr/bin/env bash
# Regenerate every image in renders/ (the README gallery and its debug maps)
# with the CPU software renderer. Run from anywhere; output lands in the repo.
#
#   scripts/render-gallery.sh
#
# Each command below is the exact recipe for one gallery image, so a visual
# change in a PR can be reproduced and reviewed.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release --bin app
app=./target/release/app

render() {
    "$app" "$@" | tail -n 1
}

size=(--width 768 --height 768)
teapot=(--model resources/teapot.obj "${size[@]}")
quad=(--model resources/textured-quad.obj "${size[@]}")
material_quad=(--model resources/material-quad.obj "${size[@]}" --light 0,0,1)
# A key light from the upper left makes the body cast a visible shadow on the spout.
key_light=(--light=-1,1,1)
shiny=(--ambient 0.08 --specular 0.5 --shininess 48)

# Hero: shadow map with PCF, SSAO, and Blinn-Phong specular together.
render --model resources/teapot.obj --width 1024 --height 768 --shader ssao \
    "${key_light[@]}" "${shiny[@]}" --shadow-filter pcf \
    --output renders/hero.png --debug-dir renders/debug/hero

# Pipeline stages, all under the default light.
render "${teapot[@]}" --shader wireframe --output renders/wireframe-teapot.png
render "${teapot[@]}" --shader flat --output renders/flat-teapot.png
render "${teapot[@]}" --shader gouraud --output renders/gouraud-teapot.png
render "${teapot[@]}" --shader gouraud "${shiny[@]}" --output renders/specular-teapot.png
render "${teapot[@]}" --shader normal-debug --output renders/normals-teapot.png

# Textures and normal mapping (4x4 PPM textures, magnified).
render "${quad[@]}" --shader texture --texture resources/checker.ppm \
    --output renders/textured-quad.png
render "${quad[@]}" --shader normal-map --texture resources/checker.ppm \
    --normal-map resources/normal-checker.ppm --texture-filter linear \
    --light 0,0,1 --ambient 0.05 --output renders/normal-map-quad.png

# Shadows and ambient occlusion, with debug maps.
render "${teapot[@]}" --shader shadow "${key_light[@]}" \
    --output renders/shadow-teapot.png --debug-dir renders/debug/teapot-shadow
render "${teapot[@]}" --shader shadow "${key_light[@]}" --shadow-filter pcf \
    --output renders/shadow-pcf-teapot.png --debug-dir renders/debug/teapot-shadow-pcf
render "${teapot[@]}" --shader ssao "${key_light[@]}" \
    --output renders/ssao-teapot.png --debug-dir renders/debug/teapot-ssao

# Clipping: the camera sits beside the spout, so the near plane slices the
# spout open and its interior shows through the cut.
render "${teapot[@]}" --shader gouraud --camera 1.3,0.55,0.3 \
    --output renders/frustum-clipped-teapot.png

# Blending, sampling, and MTL-driven materials on the material quad.
render "${material_quad[@]}" --shader texture --blend alpha --alpha 0.45 \
    --depth-test off --depth-write off --output renders/blend-alpha-quad.png
render "${material_quad[@]}" --shader texture --texture-filter mipmap \
    --output renders/mipmap-quad.png
render "${material_quad[@]}" --shader texture --output renders/material-quad.png
