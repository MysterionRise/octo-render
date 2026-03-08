struct Uniforms {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    light_dir: vec4<f32>,
    eye_pos: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) world_position: vec3<f32>,
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    let world_pos = uniforms.model * vec4<f32>(input.position, 1.0);
    output.clip_position = uniforms.view_proj * world_pos;
    // NOTE: This assumes uniform scaling. For non-uniform scaling, normals should
    // be transformed by the inverse-transpose of the model matrix instead.
    output.world_normal = (uniforms.model * vec4<f32>(input.normal, 0.0)).xyz;
    output.world_position = world_pos.xyz;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let normal = normalize(input.world_normal);
    let light = normalize(uniforms.light_dir.xyz);

    // Gouraud-style N dot L
    let ndotl = max(dot(normal, light), 0.0);
    let ambient = 0.1;
    let intensity = ambient + ndotl * 0.9;

    return vec4<f32>(intensity, intensity, intensity, 1.0);
}
