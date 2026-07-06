use bytemuck::{Pod, Zeroable};
use glam::Mat4;
use octo_gpu::GpuTexture;
use wgpu::util::DeviceExt;

use crate::camera::CameraUniforms;
use crate::framebuffer::Framebuffer;
use crate::mesh::{GpuMesh, Vertex};
use crate::shadow::ShadowPass;

/// All uniforms for the forward pass, packed into one buffer.
/// NOTE: The shader transforms normals using the model matrix directly, which is
/// only correct for uniform scaling. If non-uniform scaling is needed, add a
/// normal_matrix (inverse-transpose of model) to the uniform layout.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ForwardUniforms {
    pub view_proj: [[f32; 4]; 4],
    pub model: [[f32; 4]; 4],
    pub light_dir: [f32; 4],       // w unused
    pub eye_pos: [f32; 4],         // w unused
    pub base_color: [f32; 4],      // RGBA base color
    pub specular_params: [f32; 4], // [specular_strength, shininess, ambient_strength, has_texture]
    pub light_view_proj: [[f32; 4]; 4],
    pub shadow_params: [f32; 4], // [shadow_enabled, shadow_bias, shadow_strength, _pad]
}

/// Material parameters for the Blinn-Phong lighting model.
pub struct MaterialParams {
    pub base_color: [f32; 4],
    pub specular_strength: f32,
    pub shininess: f32,
    pub ambient_strength: f32,
    pub has_texture: bool,
}

#[allow(clippy::derivable_impls)]
impl Default for MaterialParams {
    fn default() -> Self {
        MaterialParams {
            base_color: [0.8, 0.6, 0.4, 1.0],
            specular_strength: 0.5,
            shininess: 32.0,
            ambient_strength: 0.1,
            has_texture: false,
        }
    }
}

/// Shadow parameters for the forward pass.
pub struct ShadowParams {
    pub light_view_proj: Mat4,
    pub enabled: bool,
    pub bias: f32,
    pub strength: f32,
}

impl Default for ShadowParams {
    fn default() -> Self {
        ShadowParams {
            light_view_proj: Mat4::IDENTITY,
            enabled: false,
            bias: 0.005,
            strength: 0.5,
        }
    }
}

impl ForwardUniforms {
    pub fn new(
        camera: &CameraUniforms,
        model: Mat4,
        light_dir: [f32; 3],
        material: &MaterialParams,
        shadow: &ShadowParams,
    ) -> Self {
        ForwardUniforms {
            view_proj: camera.view_proj,
            model: model.to_cols_array_2d(),
            light_dir: [light_dir[0], light_dir[1], light_dir[2], 0.0],
            eye_pos: camera.eye_pos,
            base_color: material.base_color,
            specular_params: [
                material.specular_strength,
                material.shininess,
                material.ambient_strength,
                if material.has_texture { 1.0 } else { 0.0 },
            ],
            light_view_proj: shadow.light_view_proj.to_cols_array_2d(),
            shadow_params: [
                if shadow.enabled { 1.0 } else { 0.0 },
                shadow.bias,
                shadow.strength,
                0.0,
            ],
        }
    }
}

pub struct ForwardPass {
    pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    default_texture_bind_group: wgpu::BindGroup,
    _shadow_bind_group_layout: wgpu::BindGroupLayout,
    shadow_bind_group: wgpu::BindGroup,
}

impl ForwardPass {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, shadow_pass: &ShadowPass) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Forward Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../../shaders/gouraud.wgsl").into()),
        });

        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Forward Uniforms"),
            contents: bytemuck::bytes_of(&ForwardUniforms {
                view_proj: Mat4::IDENTITY.to_cols_array_2d(),
                model: Mat4::IDENTITY.to_cols_array_2d(),
                light_dir: [0.0, 1.0, 0.0, 0.0],
                eye_pos: [0.0, 0.0, 5.0, 0.0],
                base_color: [0.8, 0.6, 0.4, 1.0],
                specular_params: [0.5, 32.0, 0.1, 0.0],
                light_view_proj: Mat4::IDENTITY.to_cols_array_2d(),
                shadow_params: [0.0, 0.005, 0.5, 0.0],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // Bind group 0: uniforms
        let uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Forward Uniform Layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Forward Bind Group"),
            layout: &uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        // Bind group 1: texture + sampler
        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Forward Texture Layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        // Default white 1x1 texture
        let default_texture = GpuTexture::white_1x1(device, queue);
        let default_texture_bind_group = Self::create_texture_bind_group_inner(
            device,
            &texture_bind_group_layout,
            &default_texture,
        );

        // Bind group 2: shadow map (depth texture + comparison sampler)
        let shadow_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Forward Shadow Layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                        count: None,
                    },
                ],
            });

        let shadow_bind_group =
            Self::create_shadow_bind_group_inner(device, &shadow_bind_group_layout, shadow_pass);

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Forward Pipeline Layout"),
            bind_group_layouts: &[
                &uniform_bind_group_layout,
                &texture_bind_group_layout,
                &shadow_bind_group_layout,
            ],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Forward Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: Framebuffer::COLOR_FORMAT,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: Framebuffer::DEPTH_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        ForwardPass {
            pipeline,
            uniform_buffer,
            uniform_bind_group,
            texture_bind_group_layout,
            default_texture_bind_group,
            _shadow_bind_group_layout: shadow_bind_group_layout,
            shadow_bind_group,
        }
    }

    fn create_texture_bind_group_inner(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        texture: &GpuTexture,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Texture Bind Group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&texture.sampler),
                },
            ],
        })
    }

    fn create_shadow_bind_group_inner(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        shadow_pass: &ShadowPass,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Shadow Bind Group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&shadow_pass.depth_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&shadow_pass.comparison_sampler),
                },
            ],
        })
    }

    /// Create a bind group for a custom texture, using this pass's layout.
    pub fn create_texture_bind_group(
        &self,
        device: &wgpu::Device,
        texture: &GpuTexture,
    ) -> wgpu::BindGroup {
        Self::create_texture_bind_group_inner(device, &self.texture_bind_group_layout, texture)
    }

    /// Render scene into the framebuffer (HDR color + depth).
    pub fn render(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        framebuffer: &Framebuffer,
        queue: &wgpu::Queue,
        uniforms: &ForwardUniforms,
        meshes: &[&GpuMesh],
        texture_bind_group: Option<&wgpu::BindGroup>,
    ) {
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(uniforms));

        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Forward Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &framebuffer.color_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.1,
                        g: 0.1,
                        b: 0.15,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &framebuffer.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        render_pass.set_pipeline(&self.pipeline);
        render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
        render_pass.set_bind_group(
            1,
            texture_bind_group.unwrap_or(&self.default_texture_bind_group),
            &[],
        );
        render_pass.set_bind_group(2, &self.shadow_bind_group, &[]);

        for mesh in meshes {
            render_pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            render_pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        }
    }
}
