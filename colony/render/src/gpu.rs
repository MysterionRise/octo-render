//! The wgpu side: one pipeline, four integer textures, one uniform buffer, one draw.

use bytemuck::{Pod, Zeroable};
use colony_sim::ElementTable;
use octo_gpu::GpuBuffer;

use crate::atlas::Atlas;
use crate::corners::CornerGrid;
use crate::palette::{solid_ranks, Luts};
use crate::snapshot::Snapshot;
use crate::{Overlay, PixelCamera, CHUNK, TILE_PX};

pub const SHADER: &str = include_str!("../shaders/cells.wgsl");

/// Mirrors `ViewUniforms` in `cells.wgsl` (48 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct ViewUniforms {
    pub origin: [i32; 2],
    pub viewport: [u32; 2],
    pub world: [u32; 2],
    pub zoom: u32,
    pub tile_px: u32,
    pub overlay: u32,
    pub _pad: [u32; 3],
}

impl ViewUniforms {
    pub fn from_camera(
        camera: &PixelCamera,
        world_cells: (u32, u32),
        overlay: Overlay,
    ) -> ViewUniforms {
        let (ox, oy) = camera.origin();
        let (vw, vh) = camera.viewport();
        ViewUniforms {
            origin: [ox as i32, oy as i32],
            viewport: [vw, vh],
            world: [world_cells.0, world_cells.1],
            zoom: camera.zoom(),
            tile_px: camera.tile_px(),
            overlay: overlay as u32,
            _pad: [0; 3],
        }
    }
}

fn data_texture(
    device: &wgpu::Device,
    label: &str,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// Writes the `w × h` rectangle at `(x0, y0)` of a row-major CPU array of `row_width`
/// texels straight into the texture (`write_texture` needs no row alignment).
#[allow(clippy::too_many_arguments)]
fn write_rect(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    data: &[u8],
    bytes_per_pixel: u32,
    row_width: u32,
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
) {
    let offset = ((y0 * row_width + x0) * bytes_per_pixel) as usize;
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x: x0, y: y0, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        &data[offset..],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(row_width * bytes_per_pixel),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}

/// Draws the world from a snapshot.
pub struct CellRenderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniforms: GpuBuffer,
    cells: wgpu::Texture,
    corners: wgpu::Texture,
    world: (u32, u32),
    corner_grid: CornerGrid,
    solid_ranks: Vec<u8>,
    uploaded_generation: Option<u64>,
}

impl CellRenderer {
    /// Builds the pipeline for `target_format` and empty textures for a `world_cells` world.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
        table: &ElementTable,
        world_cells: (u32, u32),
    ) -> CellRenderer {
        let (w, h) = world_cells;
        let cells = data_texture(device, "colony cells", wgpu::TextureFormat::Rgba8Uint, w, h);
        let corners = data_texture(
            device,
            "colony corners",
            wgpu::TextureFormat::R16Uint,
            w + 1,
            h + 1,
        );
        let luts_cpu = Luts::from_table(table);
        let luts = data_texture(
            device,
            "colony luts",
            wgpu::TextureFormat::Rgba8Uint,
            Luts::WIDTH,
            Luts::ROWS,
        );
        write_rect(
            queue,
            &luts,
            luts_cpu.bytes(),
            4,
            Luts::WIDTH,
            0,
            0,
            Luts::WIDTH,
            Luts::ROWS,
        );
        let atlas_cpu = Atlas::build(table, TILE_PX);
        let (aw, ah) = atlas_cpu.image.dimensions();
        let atlas = data_texture(
            device,
            "colony atlas",
            wgpu::TextureFormat::Rgba8Uint,
            aw,
            ah,
        );
        write_rect(queue, &atlas, atlas_cpu.image.as_raw(), 4, aw, 0, 0, aw, ah);
        let uniforms = GpuBuffer::uniform(device, "colony view", &ViewUniforms::zeroed());

        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Uint,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("colony cells layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1),
                texture_entry(2),
                texture_entry(3),
                texture_entry(4),
            ],
        });
        let view_of = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("colony cells bind group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view_of(&cells)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view_of(&corners)),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&view_of(&luts)),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&view_of(&atlas)),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("colony cells shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("colony cells pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("colony cells pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        CellRenderer {
            pipeline,
            bind_group,
            uniforms,
            cells,
            corners,
            world: world_cells,
            corner_grid: CornerGrid::new(w, h),
            solid_ranks: solid_ranks(table),
            uploaded_generation: None,
        }
    }

    pub fn world(&self) -> (u32, u32) {
        self.world
    }

    /// Uploads what changed: everything when the generation changed (or on the first
    /// call), otherwise only the dirty chunks and the corners around them.
    pub fn upload(&mut self, queue: &wgpu::Queue, snapshot: &Snapshot) {
        assert_eq!(
            (snapshot.width, snapshot.height),
            self.world,
            "snapshot size must match the renderer's world"
        );
        let (w, h) = self.world;
        let texels: &[u8] = bytemuck::cast_slice(&snapshot.texels);
        if self.uploaded_generation != Some(snapshot.generation) {
            write_rect(queue, &self.cells, texels, 4, w, 0, 0, w, h);
            self.corner_grid.rebuild_all(snapshot, &self.solid_ranks);
            write_rect(
                queue,
                &self.corners,
                self.corner_grid.bytes(),
                2,
                w + 1,
                0,
                0,
                w + 1,
                h + 1,
            );
            self.uploaded_generation = Some(snapshot.generation);
            return;
        }
        let chunk = CHUNK as u32;
        for cy in 0..snapshot.chunk_rows {
            for cx in 0..snapshot.chunk_cols {
                if !snapshot.dirty[(cy * snapshot.chunk_cols + cx) as usize] {
                    continue;
                }
                let x0 = cx * chunk;
                let y0 = cy * chunk;
                let cw = (w - x0).min(chunk);
                let ch = (h - y0).min(chunk);
                write_rect(queue, &self.cells, texels, 4, w, x0, y0, cw, ch);
                let (kx0, ky0, kx1, ky1) =
                    CornerGrid::affected_corners(x0, y0, x0 + cw - 1, y0 + ch - 1);
                self.corner_grid
                    .rebuild_rect(snapshot, &self.solid_ranks, kx0, ky0, kx1, ky1);
                write_rect(
                    queue,
                    &self.corners,
                    self.corner_grid.bytes(),
                    2,
                    w + 1,
                    kx0,
                    ky0,
                    kx1 - kx0 + 1,
                    ky1 - ky0 + 1,
                );
            }
        }
    }

    pub fn set_view(&self, queue: &wgpu::Queue, camera: &PixelCamera, overlay: Overlay) {
        self.uniforms.write(
            queue,
            &ViewUniforms::from_camera(camera, self.world, overlay),
        );
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    pub fn corner_grid(&self) -> &CornerGrid {
        &self.corner_grid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use naga::valid::{Capabilities, ValidationFlags, Validator};

    #[test]
    fn shader_parses_and_validates() {
        let module = naga::front::wgsl::parse_str(SHADER).expect("cells.wgsl parses");
        Validator::new(ValidationFlags::all(), Capabilities::all())
            .validate(&module)
            .expect("cells.wgsl validates");
    }

    #[test]
    fn uniform_struct_matches_the_wgsl_layout() {
        assert_eq!(std::mem::size_of::<ViewUniforms>(), 48);
        let module = naga::front::wgsl::parse_str(SHADER).unwrap();
        let (_, ty) = module
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some("ViewUniforms"))
            .expect("ViewUniforms struct");
        let naga::TypeInner::Struct { members, span } = &ty.inner else {
            panic!("not a struct");
        };
        assert_eq!(*span, 48);
        let offsets: Vec<u32> = members.iter().map(|m| m.offset).collect();
        assert_eq!(offsets, vec![0, 8, 16, 24, 28, 32, 36, 40, 44]);
        let names: Vec<&str> = members.iter().map(|m| m.name.as_deref().unwrap()).collect();
        assert_eq!(
            names[..6],
            ["origin", "viewport", "world", "zoom", "tile_px", "overlay"]
        );
    }

    #[test]
    fn uniforms_follow_the_camera() {
        let cam = PixelCamera::new((64, 32), TILE_PX, (256, 128), 2);
        let u = ViewUniforms::from_camera(&cam, (64, 32), Overlay::Pressure);
        let (ox, oy) = cam.origin();
        assert_eq!(u.origin, [ox as i32, oy as i32]);
        assert_eq!(
            (u.viewport, u.world, u.zoom, u.tile_px, u.overlay),
            ([256, 128], [64, 32], 2, TILE_PX, 2)
        );
    }
}
