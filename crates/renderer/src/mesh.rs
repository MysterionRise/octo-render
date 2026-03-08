use bytemuck::{Pod, Zeroable};
use glam::Vec3;
use octo_core::Model;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
}

impl Vertex {
    const ATTRIBS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32x3,
    ];

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

pub struct GpuMesh {
    pub vertex_buffer: wgpu::Buffer,
    pub index_buffer: wgpu::Buffer,
    pub index_count: u32,
}

impl GpuMesh {
    /// Build GPU mesh from a Model. Computes per-face normals as fallback
    /// when OBJ normals are not available.
    pub fn from_model(device: &wgpu::Device, model: &Model) -> Self {
        let mut vertices = Vec::with_capacity(model.faces.len() * 3);
        let mut indices = Vec::with_capacity(model.faces.len() * 3);

        for face in &model.faces {
            let base = vertices.len() as u32;

            // Compute face normal as fallback
            let v0 = model.vertices[face[0].v];
            let v1 = model.vertices[face[1].v];
            let v2 = model.vertices[face[2].v];
            let face_normal = (v2 - v0).cross(v1 - v0).normalize();

            for fv in face {
                let position = model.vertices[fv.v];
                let normal = fv
                    .vn
                    .map(|ni| model.normals[ni])
                    .unwrap_or(face_normal);

                // Handle NaN normals from degenerate triangles
                let normal = if normal.is_nan() {
                    Vec3::Y
                } else {
                    normal
                };

                vertices.push(Vertex {
                    position: position.into(),
                    normal: normal.into(),
                });
            }

            indices.push(base);
            indices.push(base + 1);
            indices.push(base + 2);
        }

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Mesh Vertex Buffer"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });

        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Mesh Index Buffer"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        GpuMesh {
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
        }
    }
}
