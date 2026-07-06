use wgpu::util::DeviceExt;

pub struct GpuTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
}

impl GpuTexture {
    /// Create a 1x1 white texture (used as fallback for untextured models).
    pub fn white_1x1(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self::from_rgba(device, queue, &[255, 255, 255, 255], 1, 1, "White 1x1")
    }

    /// Create a texture from raw RGBA8 pixel data.
    pub fn from_rgba(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        data: &[u8],
        width: u32,
        height: u32,
        label: &str,
    ) -> Self {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let texture = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            data,
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(&format!("{label} Sampler")),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        GpuTexture {
            texture,
            view,
            sampler,
        }
    }

    /// Load a texture from a file on disk (PNG, TGA, JPG, etc.).
    pub fn from_file(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        path: &str,
    ) -> Result<Self, String> {
        let img = image::open(path)
            .map_err(|e| format!("Failed to load texture '{}': {}", path, e))?
            .to_rgba8();
        let (width, height) = img.dimensions();
        Ok(Self::from_rgba(device, queue, &img, width, height, path))
    }
}
