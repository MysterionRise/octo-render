struct Uniforms {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    light_dir: vec4<f32>,
    eye_pos: vec4<f32>,
    base_color: vec4<f32>,
    specular_params: vec4<f32>,    // [specular_strength, shininess, ambient_strength, has_texture]
    light_view_proj: mat4x4<f32>,
    shadow_params: vec4<f32>,      // [shadow_enabled, shadow_bias, shadow_strength, _pad]
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

@group(1) @binding(0)
var diffuse_texture: texture_2d<f32>;
@group(1) @binding(1)
var diffuse_sampler: sampler;

@group(2) @binding(0)
var shadow_map: texture_depth_2d;
@group(2) @binding(1)
var shadow_sampler: sampler_comparison;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) texcoord: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) world_position: vec3<f32>,
    @location(2) texcoord: vec2<f32>,
    @location(3) light_space_pos: vec4<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    let world_pos = uniforms.model * vec4<f32>(input.position, 1.0);
    output.clip_position = uniforms.view_proj * world_pos;
    output.world_normal = (uniforms.model * vec4<f32>(input.normal, 0.0)).xyz;
    output.world_position = world_pos.xyz;
    output.texcoord = input.texcoord;
    output.light_space_pos = uniforms.light_view_proj * world_pos;
    return output;
}

fn compute_shadow(light_space_pos: vec4<f32>, bias: f32) -> f32 {
    // Perspective divide
    let proj = light_space_pos.xyz / light_space_pos.w;

    // Convert from [-1,1] to [0,1] for UV, and flip Y
    let uv = vec2<f32>(proj.x * 0.5 + 0.5, 1.0 - (proj.y * 0.5 + 0.5));

    // Out of shadow map bounds → fully lit
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || proj.z < 0.0 || proj.z > 1.0) {
        return 1.0;
    }

    let depth = proj.z - bias;

    // 3x3 PCF
    let texel_size = 1.0 / 2048.0;
    var shadow = 0.0;
    for (var x = -1; x <= 1; x++) {
        for (var y = -1; y <= 1; y++) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel_size;
            shadow += textureSampleCompare(shadow_map, shadow_sampler, uv + offset, depth);
        }
    }
    return shadow / 9.0;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let normal = normalize(input.world_normal);
    let light = normalize(uniforms.light_dir.xyz);

    let specular_strength = uniforms.specular_params.x;
    let shininess = uniforms.specular_params.y;
    let ambient_strength = uniforms.specular_params.z;
    let has_texture = uniforms.specular_params.w;

    let shadow_enabled = uniforms.shadow_params.x;
    let shadow_bias = uniforms.shadow_params.y;
    let shadow_strength = uniforms.shadow_params.z;

    // Determine surface color: texture or base_color
    var color: vec3<f32>;
    if (has_texture > 0.5) {
        color = textureSample(diffuse_texture, diffuse_sampler, input.texcoord).rgb;
    } else {
        color = uniforms.base_color.rgb;
    }

    // Shadow
    var shadow_factor = 1.0;
    if (shadow_enabled > 0.5) {
        let raw_shadow = compute_shadow(input.light_space_pos, shadow_bias);
        shadow_factor = mix(1.0 - shadow_strength, 1.0, raw_shadow);
    }

    // Ambient
    let ambient = ambient_strength * color;

    // Diffuse (Lambertian)
    let ndotl = max(dot(normal, light), 0.0);
    let diffuse = ndotl * color;

    // Specular (Blinn-Phong)
    let view_dir = normalize(uniforms.eye_pos.xyz - input.world_position);
    let half_dir = normalize(light + view_dir);
    let spec = pow(max(dot(normal, half_dir), 0.0), shininess);
    let specular = specular_strength * spec * vec3<f32>(1.0, 1.0, 1.0);

    // Shadow affects diffuse and specular, not ambient
    let result = ambient + (diffuse + specular) * shadow_factor;
    return vec4<f32>(result, 1.0);
}
