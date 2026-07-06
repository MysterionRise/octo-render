// Shadow map pass: vertex-only, depth-only render from light POV.

struct ShadowUniforms {
    light_view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
}

@group(0) @binding(0)
var<uniform> uniforms: ShadowUniforms;

@vertex
fn vs_main(@location(0) position: vec3<f32>) -> @builtin(position) vec4<f32> {
    return uniforms.light_view_proj * uniforms.model * vec4<f32>(position, 1.0);
}
