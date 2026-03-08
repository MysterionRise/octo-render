use glam::Vec3;
use image::{ImageBuffer, RgbImage};
use image::imageops::flip_vertical_in_place;
use renderer::{
    apply_ssao, compute_ssao, lookat, projection, triangle_shader,
    viewport, DepthShader, Model, Shader, ShadowShader,
};

fn main() {
    let model = Model::from_file("resources/pumpkin_tall_10k.obj");
    let width = 1024u32;
    let height = 1024u32;
    let depth = 255.0;

    let light_dir = Vec3::new(1.0, 1.0, -1.0).normalize();

    // Camera setup
    let eye = Vec3::new(0.0, 0.0, 200.0);
    let center = Vec3::ZERO;
    let up = Vec3::Y;

    let view = lookat(eye, center, up);
    let proj = projection(-1.0 / (eye - center).length());
    let vp = viewport(
        width as f32 / 8.0,
        height as f32 / 8.0,
        width as f32 * 3.0 / 4.0,
        height as f32 * 3.0 / 4.0,
        depth,
    );
    let model_view = view;
    let transform = vp * proj * view;

    // --- Pass 1: Shadow map from light's POV ---
    let light_eye = light_dir * 200.0;
    let light_view = lookat(light_eye, center, up);
    let light_proj = projection(-1.0 / (light_eye - center).length());
    let light_vp = viewport(
        width as f32 / 8.0,
        height as f32 / 8.0,
        width as f32 * 3.0 / 4.0,
        height as f32 * 3.0 / 4.0,
        depth,
    );
    let light_transform = light_vp * light_proj * light_view;

    let mut shadow_image: RgbImage = ImageBuffer::new(width, height);
    let mut shadow_zbuffer = vec![f32::NEG_INFINITY; (width * height) as usize];

    {
        let mut depth_shader = DepthShader {
            model: &model,
            transform: light_transform,
        };

        for face_idx in 0..model.faces.len() {
            let screen_coords: [Vec3; 3] = std::array::from_fn(|j| depth_shader.vertex(face_idx, j));
            triangle_shader(&screen_coords, &depth_shader, &mut shadow_zbuffer, &mut shadow_image);
        }
    }

    // --- Pass 2: Main render with shadows ---
    let mut image: RgbImage = ImageBuffer::new(width, height);
    let mut zbuffer = vec![f32::NEG_INFINITY; (width * height) as usize];

    {
        let mut shader = ShadowShader::new(
            &model,
            transform,
            model_view,
            light_transform,
            light_dir,
            &shadow_zbuffer,
            width,
            height,
        );

        for face_idx in 0..model.faces.len() {
            let screen_coords: [Vec3; 3] = std::array::from_fn(|j| shader.vertex(face_idx, j));
            triangle_shader(&screen_coords, &shader, &mut zbuffer, &mut image);
        }
    }

    // --- Pass 3: Screen-space ambient occlusion ---
    let ao = compute_ssao(&zbuffer, width, height, 5, 8);
    apply_ssao(&mut image, &ao, width, height);

    flip_vertical_in_place(&mut image);
    image.save("output.tga").unwrap();
}
