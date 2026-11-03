// One fullscreen triangle; every lookup uses integer coordinates, so the output is
// pixel-exact at any integer zoom. `colony_render::shade::shade_pixel` is the CPU twin of
// `fs_main`; keep the two in step.

struct ViewUniforms {
    origin: vec2<i32>,    // bottom-left of the view in world pixels, y up
    viewport: vec2<u32>,  // physical pixels
    world: vec2<u32>,     // cells
    zoom: u32,
    tile_px: u32,
    overlay: u32,         // 0 none, 1 temperature, 2 pressure
    pad0: u32,
    pad1: u32,
    pad2: u32,
};

@group(0) @binding(0) var<uniform> view: ViewUniforms;
@group(0) @binding(1) var cells: texture_2d<u32>;    // e, phase|flags<<2, mass_q, temp_q
@group(0) @binding(2) var corners: texture_2d<u32>;  // rank<<4 | case
@group(0) @binding(3) var luts: texture_2d<u32>;     // row 0 element colour, 1 temperature, 2 pressure
@group(0) @binding(4) var atlas: texture_2d<u32>;    // 16 cases per row, one row per solid rank

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var output: VertexOutput;
    let u = f32(vertex_index & 1u) * 2.0;
    let v = f32(vertex_index >> 1u) * 2.0;
    output.position = vec4<f32>(u * 2.0 - 1.0, 1.0 - v * 2.0, 0.0, 1.0);
    return output;
}

fn floor_div(a: i32, b: i32) -> i32 {
    var n = a;
    if (a < 0) {
        n = a - (b - 1);
    }
    return n / b;
}

const VOID: vec3<u32> = vec3<u32>(16u, 16u, 20u);

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let sx = u32(input.position.x);
    let sy = u32(input.position.y);
    let px = view.origin.x + i32(sx / view.zoom);
    let py = view.origin.y + i32((view.viewport.y - 1u - sy) / view.zoom);
    let t = i32(view.tile_px);
    let cx = floor_div(px, t);
    let cy = floor_div(py, t);
    if (cx < 0 || cy < 0 || cx >= i32(view.world.x) || cy >= i32(view.world.y)) {
        return vec4<f32>(vec3<f32>(VOID) / 255.0, 1.0);
    }
    let sub_y = py - cy * t;
    let c = textureLoad(cells, vec2<i32>(cx, cy), 0);
    let e = c.x;
    let phase = c.y & 3u;
    let m = c.z;
    let temp = c.w;
    let base = textureLoad(luts, vec2<i32>(i32(e), 0), 0).rgb;
    var rgb = base;
    if (phase == 1u) {
        rgb = min((base * (64u + m)) >> vec3<u32>(8u), vec3<u32>(255u));
    } else if (phase == 2u) {
        let level = (i32(m) * t) / 192;
        if (sub_y >= level) {
            rgb = base >> vec3<u32>(2u);
        }
    } else if (phase == 3u) {
        let h = t / 2;
        let kx = floor_div(px + h, t);
        let ky = floor_div(py + h, t);
        let v = textureLoad(corners, vec2<i32>(kx, ky), 0).x;
        let rank = i32(v >> 4u);
        let tile_case = i32(v & 15u);
        let ox = px + h - kx * t;
        let oy = py + h - ky * t;
        rgb = textureLoad(atlas, vec2<i32>(tile_case * t + ox, rank * t + (t - 1 - oy)), 0).rgb;
    }
    if (view.overlay != 0u && e != 0u) {
        var key = temp;
        if (view.overlay == 2u) {
            key = select(0u, m, phase == 1u);
        }
        rgb = textureLoad(luts, vec2<i32>(i32(key), i32(view.overlay)), 0).rgb;
    }
    return vec4<f32>(vec3<f32>(rgb) / 255.0, 1.0);
}
