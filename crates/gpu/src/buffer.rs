use bytemuck::Pod;
use wgpu::util::DeviceExt;

/// Typed wrapper around a wgpu::Buffer.
pub struct GpuBuffer {
    pub buffer: wgpu::Buffer,
    pub size: u64,
}

impl GpuBuffer {
    /// Create a vertex buffer from a slice of Pod data.
    pub fn vertex<T: Pod>(device: &wgpu::Device, label: &str, data: &[T]) -> Self {
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(data),
            usage: wgpu::BufferUsages::VERTEX,
        });
        GpuBuffer {
            size: std::mem::size_of_val(data) as u64,
            buffer,
        }
    }

    /// Create an index buffer from a slice of u32 indices.
    pub fn index(device: &wgpu::Device, label: &str, data: &[u32]) -> Self {
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(data),
            usage: wgpu::BufferUsages::INDEX,
        });
        GpuBuffer {
            size: std::mem::size_of_val(data) as u64,
            buffer,
        }
    }

    /// Create a uniform buffer from Pod data.
    pub fn uniform<T: Pod>(device: &wgpu::Device, label: &str, data: &T) -> Self {
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::bytes_of(data),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        GpuBuffer {
            size: std::mem::size_of::<T>() as u64,
            buffer,
        }
    }

    /// Upload new data to this buffer.
    pub fn write<T: Pod>(&self, queue: &wgpu::Queue, data: &T) {
        queue.write_buffer(&self.buffer, 0, bytemuck::bytes_of(data));
    }
}
