// Colonists and items: one quad per instance from a storage buffer, expanded in the
// vertex shader (six vertices per instance, no vertex buffer). Positions are world
// pixels mapped with the same integer camera as `cells.wgsl`, so every quad edge lands on
// a screen pixel boundary and `colony_render::sprites::stamp_sprites` is its exact CPU
// twin; keep the two in step.

struct ViewUniforms {
    origin: vec2<i32>,    // bottom-left of the view in world pixels, y up
    viewport: vec2<u32>,  // physical pixels
    world: vec2<u32>,     // cells
    zoom: u32,
    tile_px: u32,
    overlay: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
};

struct SpriteInstance {
    pos: vec2<i32>,   // bottom-left in world pixels
    size: vec2<u32>,  // world pixels
    rgba: u32,        // r | g << 8 | b << 16 | a << 24
    layer: u32,
    pad0: u32,
    pad1: u32,
};

@group(0) @binding(0) var<uniform> view: ViewUniforms;
@group(0) @binding(1) var<storage, read> sprites: array<SpriteInstance>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) colour: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let s = sprites[vertex_index / 6u];
    let corner = vertex_index % 6u;
    // Two triangles: (0,0) (1,0) (0,1) and (1,0) (1,1) (0,1).
    let cx = select(0u, 1u, corner == 1u || corner == 3u || corner == 4u);
    let cy = select(0u, 1u, corner == 2u || corner == 4u || corner == 5u);
    let wx = s.pos.x + i32(cx * s.size.x);
    let wy = s.pos.y + i32(cy * s.size.y);
    let zoom = i32(view.zoom);
    let sx = f32((wx - view.origin.x) * zoom);
    let sy = f32(i32(view.viewport.y) - (wy - view.origin.y) * zoom);
    var output: VertexOutput;
    output.position = vec4<f32>(
        sx / f32(view.viewport.x) * 2.0 - 1.0,
        1.0 - sy / f32(view.viewport.y) * 2.0,
        0.0,
        1.0,
    );
    let c = s.rgba;
    output.colour = vec4<f32>(
        f32(c & 255u),
        f32((c >> 8u) & 255u),
        f32((c >> 16u) & 255u),
        f32(c >> 24u),
    ) / 255.0;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return input.colour;
}
