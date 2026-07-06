// Post-processing pass: ACES tone mapping, depth-based fog, color grading, vignette.

struct PostProcessParams {
    fog_color: vec4<f32>,   // RGB + _pad
    fog_params: vec4<f32>,  // [fog_enabled, fog_near, fog_far, fog_density]
    color_grade: vec4<f32>, // [exposure, contrast, saturation, vignette_strength]
    tint: vec4<f32>,        // RGB tint + _pad
}

@group(0) @binding(0)
var scene_texture: texture_2d<f32>;
@group(0) @binding(1)
var scene_sampler: sampler;
@group(0) @binding(2)
var depth_texture: texture_depth_2d;
@group(0) @binding(3)
var<uniform> params: PostProcessParams;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) texcoord: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    // Fullscreen triangle: 3 vertices cover the entire screen
    var output: VertexOutput;
    let u = f32(vertex_index & 1u) * 2.0;
    let v = f32(vertex_index >> 1u) * 2.0;
    output.position = vec4<f32>(u * 2.0 - 1.0, 1.0 - v * 2.0, 0.0, 1.0);
    output.texcoord = vec2<f32>(u, v);
    return output;
}

// ACES filmic tone mapping
fn aces_tonemap(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3(0.0), vec3(1.0));
}

// Linearize depth from [0,1] depth buffer to view-space distance
fn linearize_depth(d: f32, near: f32, far: f32) -> f32 {
    return near * far / (far - d * (far - near));
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    var color = textureSample(scene_texture, scene_sampler, input.texcoord).rgb;

    let exposure = params.color_grade.x;
    let contrast = params.color_grade.y;
    let saturation = params.color_grade.z;
    let vignette_strength = params.color_grade.w;

    let fog_enabled = params.fog_params.x;
    let fog_near = params.fog_params.y;
    let fog_far = params.fog_params.z;

    // Apply exposure
    color = color * exposure;

    // Apply color tint
    color = color * params.tint.rgb;

    // Depth-based fog
    if (fog_enabled > 0.5) {
        let dims = textureDimensions(depth_texture);
        let depth_uv = vec2<i32>(
            i32(input.texcoord.x * f32(dims.x)),
            i32(input.texcoord.y * f32(dims.y))
        );
        let raw_depth = textureLoad(depth_texture, depth_uv, 0);
        let linear_depth = linearize_depth(raw_depth, 1.0, 1000.0);
        let fog_factor = clamp((linear_depth - fog_near) / (fog_far - fog_near), 0.0, 1.0);
        color = mix(color, params.fog_color.rgb, fog_factor);
    }

    // ACES tone mapping (HDR → LDR)
    color = aces_tonemap(color);

    // Contrast adjustment (around 0.5 midpoint)
    color = clamp((color - 0.5) * contrast + 0.5, vec3(0.0), vec3(1.0));

    // Saturation adjustment
    let luminance = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    color = mix(vec3(luminance), color, saturation);

    // Vignette
    if (vignette_strength > 0.001) {
        let uv_centered = input.texcoord * 2.0 - 1.0;
        let dist = length(uv_centered);
        let vignette = 1.0 - smoothstep(0.5, 1.5, dist) * vignette_strength;
        color = color * vignette;
    }

    return vec4<f32>(color, 1.0);
}
