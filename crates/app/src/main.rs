use std::sync::Arc;

use glam::{Mat4, Vec3};
use octo_asset::load_obj;
use octo_gpu::GpuContext;
use octo_renderer::blit::PostProcessParams;
use octo_renderer::forward::{ForwardUniforms, MaterialParams, ShadowParams};
use octo_renderer::shadow::ShadowUniforms;
use octo_renderer::{ForwardPass, Framebuffer, GpuMesh, OrbitCamera, PostProcessPass, ShadowPass};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

const MODELS: &[(&str, &str)] = &[
    ("Pumpkin", "resources/pumpkin_tall_10k.obj"),
    ("Teddy", "resources/teddy.obj"),
    ("Teapot", "resources/teapot.obj"),
    ("Cow", "resources/cow-nonormals.obj"),
];

struct RenderState {
    gpu: GpuContext,
    shadow_pass: ShadowPass,
    forward_pass: ForwardPass,
    framebuffer: Framebuffer,
    post_process: PostProcessPass,
    mesh: GpuMesh,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

struct App {
    window: Option<Arc<Window>>,
    render: Option<RenderState>,
    camera: OrbitCamera,
    // Input state
    mouse_pressed: bool,
    last_mouse_pos: Option<(f64, f64)>,
    // UI state
    light_dir: [f32; 3],
    current_model_idx: usize,
    model_changed: bool,
    // Material
    base_color: [f32; 3],
    shininess: f32,
    specular_strength: f32,
    ambient_strength: f32,
    // Shadows
    shadow_enabled: bool,
    shadow_bias: f32,
    shadow_strength: f32,
    // Post-processing
    exposure: f32,
    contrast: f32,
    saturation: f32,
    vignette: f32,
    fog_enabled: bool,
    fog_near: f32,
    fog_far: f32,
    fog_color: [f32; 3],
    tint: [f32; 3],
}

impl App {
    fn new() -> Self {
        App {
            window: None,
            render: None,
            camera: OrbitCamera::new(200.0, 16.0 / 9.0),
            mouse_pressed: false,
            last_mouse_pos: None,
            light_dir: [1.0, 1.0, -1.0],
            current_model_idx: 0,
            model_changed: false,
            base_color: [0.8, 0.6, 0.4],
            shininess: 32.0,
            specular_strength: 0.5,
            ambient_strength: 0.1,
            shadow_enabled: true,
            shadow_bias: 0.005,
            shadow_strength: 0.5,
            exposure: 1.0,
            contrast: 1.0,
            saturation: 1.0,
            vignette: 0.0,
            fog_enabled: false,
            fog_near: 50.0,
            fog_far: 500.0,
            fog_color: [0.6, 0.65, 0.7],
            tint: [1.0, 1.0, 1.0],
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = WindowAttributes::default()
            .with_title("Octo Render — GPU")
            .with_inner_size(LogicalSize::new(1280, 720));

        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        let gpu = pollster::block_on(GpuContext::new(window.clone()));

        let size = window.inner_size();
        self.camera.aspect = size.width as f32 / size.height as f32;

        let shadow_pass = ShadowPass::new(&gpu.device);
        let forward_pass = ForwardPass::new(&gpu.device, &gpu.queue, &shadow_pass);
        let framebuffer = Framebuffer::new(&gpu.device, size.width, size.height);
        let mut post_process = PostProcessPass::new(&gpu.device, gpu.surface_format);
        post_process.update_source(
            &gpu.device,
            &framebuffer.color_view,
            &framebuffer.depth_view,
        );

        // Load initial model
        let (_, path) = MODELS[self.current_model_idx];
        log::info!("Loading model: {}", path);
        let model = load_obj(path).expect("Failed to load initial OBJ model");
        let mesh = GpuMesh::from_model(&gpu.device, &model);

        // egui setup
        let egui_ctx = egui::Context::default();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let egui_renderer =
            egui_wgpu::Renderer::new(&gpu.device, gpu.surface_format, None, 1, false);

        self.render = Some(RenderState {
            gpu,
            shadow_pass,
            forward_pass,
            framebuffer,
            post_process,
            mesh,
            egui_ctx,
            egui_state,
            egui_renderer,
        });
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        // Let egui process events first
        if let Some(render) = &mut self.render {
            let window = self.window.as_ref().unwrap();
            let response = render.egui_state.on_window_event(window, &event);
            if response.consumed {
                return;
            }
        }

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(new_size) => {
                if let Some(render) = &mut self.render {
                    render.gpu.resize(new_size.width, new_size.height);
                    render
                        .framebuffer
                        .resize(&render.gpu.device, new_size.width, new_size.height);
                    render.post_process.update_source(
                        &render.gpu.device,
                        &render.framebuffer.color_view,
                        &render.framebuffer.depth_view,
                    );
                    self.camera.aspect = new_size.width as f32 / new_size.height as f32;
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left {
                    self.mouse_pressed = state == ElementState::Pressed;
                    if !self.mouse_pressed {
                        self.last_mouse_pos = None;
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if self.mouse_pressed {
                    if let Some((lx, ly)) = self.last_mouse_pos {
                        let dx = (position.x - lx) as f32;
                        let dy = (position.y - ly) as f32;
                        self.camera.rotate(dx, dy, 0.005);
                    }
                    self.last_mouse_pos = Some((position.x, position.y));
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32 * 0.1,
                };
                self.camera.zoom(scroll);
            }
            WindowEvent::RedrawRequested => {
                self.draw();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

impl App {
    fn draw(&mut self) {
        let render = match &mut self.render {
            Some(r) => r,
            None => return,
        };
        let window = self.window.as_ref().unwrap();

        // --- egui frame ---
        let raw_input = render.egui_state.take_egui_input(window);
        let egui_output = render.egui_ctx.run(raw_input, |ctx| {
            egui::SidePanel::left("controls")
                .default_width(200.0)
                .show(ctx, |ui| {
                    ui.heading("Octo Render");
                    ui.separator();

                    ui.label("Model:");
                    let current_name = MODELS[self.current_model_idx].0;
                    egui::ComboBox::from_id_salt("model_select")
                        .selected_text(current_name)
                        .show_ui(ui, |ui| {
                            for (i, (name, _)) in MODELS.iter().enumerate() {
                                if ui
                                    .selectable_value(&mut self.current_model_idx, i, *name)
                                    .changed()
                                {
                                    self.model_changed = true;
                                }
                            }
                        });

                    ui.separator();
                    ui.label("Light Direction:");
                    ui.add(egui::Slider::new(&mut self.light_dir[0], -1.0..=1.0).text("X"));
                    ui.add(egui::Slider::new(&mut self.light_dir[1], -1.0..=1.0).text("Y"));
                    ui.add(egui::Slider::new(&mut self.light_dir[2], -1.0..=1.0).text("Z"));

                    ui.separator();
                    ui.label("Material:");
                    ui.horizontal(|ui| {
                        ui.label("Color:");
                        ui.color_edit_button_rgb(&mut self.base_color);
                    });
                    ui.add(
                        egui::Slider::new(&mut self.shininess, 2.0..=256.0)
                            .logarithmic(true)
                            .text("Shininess"),
                    );
                    ui.add(
                        egui::Slider::new(&mut self.specular_strength, 0.0..=2.0).text("Specular"),
                    );
                    ui.add(
                        egui::Slider::new(&mut self.ambient_strength, 0.0..=1.0).text("Ambient"),
                    );

                    ui.separator();
                    ui.label("Shadows:");
                    ui.checkbox(&mut self.shadow_enabled, "Enable Shadows");
                    ui.add(egui::Slider::new(&mut self.shadow_bias, 0.0..=0.05).text("Bias"));
                    ui.add(
                        egui::Slider::new(&mut self.shadow_strength, 0.0..=1.0).text("Strength"),
                    );

                    ui.separator();
                    ui.label("Post-Processing:");
                    ui.add(
                        egui::Slider::new(&mut self.exposure, 0.1..=5.0)
                            .logarithmic(true)
                            .text("Exposure"),
                    );
                    ui.add(egui::Slider::new(&mut self.contrast, 0.5..=2.0).text("Contrast"));
                    ui.add(egui::Slider::new(&mut self.saturation, 0.0..=2.0).text("Saturation"));
                    ui.add(egui::Slider::new(&mut self.vignette, 0.0..=1.0).text("Vignette"));
                    ui.horizontal(|ui| {
                        ui.label("Tint:");
                        ui.color_edit_button_rgb(&mut self.tint);
                    });

                    ui.separator();
                    ui.label("Fog:");
                    ui.checkbox(&mut self.fog_enabled, "Enable Fog");
                    ui.add(egui::Slider::new(&mut self.fog_near, 1.0..=200.0).text("Near"));
                    ui.add(egui::Slider::new(&mut self.fog_far, 50.0..=1000.0).text("Far"));
                    ui.horizontal(|ui| {
                        ui.label("Color:");
                        ui.color_edit_button_rgb(&mut self.fog_color);
                    });

                    ui.separator();
                    ui.label(format!(
                        "Camera: yaw={:.1} pitch={:.1} r={:.0}",
                        self.camera.yaw.to_degrees(),
                        self.camera.pitch.to_degrees(),
                        self.camera.radius,
                    ));
                });
        });

        // Handle platform output (cursor changes, etc.)
        render
            .egui_state
            .handle_platform_output(window, egui_output.platform_output);

        let paint_jobs = render
            .egui_ctx
            .tessellate(egui_output.shapes, egui_output.pixels_per_point);

        // --- Model reload if changed ---
        if self.model_changed {
            let (_, path) = MODELS[self.current_model_idx];
            log::info!("Loading model: {}", path);
            match load_obj(path) {
                Ok(model) => {
                    render.mesh = GpuMesh::from_model(&render.gpu.device, &model);
                }
                Err(e) => {
                    log::error!("Failed to load model '{}': {}", path, e);
                }
            }
            self.model_changed = false;
        }

        // --- GPU rendering ---
        let surface_texture = match render.gpu.surface.get_current_texture() {
            Ok(tex) => tex,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                let config = &render.gpu.surface_config;
                render.gpu.surface.configure(&render.gpu.device, config);
                return;
            }
            Err(e) => {
                log::error!("Surface error: {:?}", e);
                return;
            }
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder =
            render
                .gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("Render Encoder"),
                });

        let raw_light = Vec3::new(self.light_dir[0], self.light_dir[1], self.light_dir[2]);
        let light = if raw_light.length_squared() < 1e-6 {
            Vec3::Y
        } else {
            raw_light.normalize()
        };

        // Compute light view-proj for shadow mapping
        let light_pos = light * 500.0;
        let light_view = Mat4::look_at_rh(light_pos, Vec3::ZERO, Vec3::Y);
        let light_proj = Mat4::orthographic_rh(-300.0, 300.0, -300.0, 300.0, 1.0, 1000.0);
        let light_view_proj = light_proj * light_view;

        // Shadow pass — depth-only from light POV
        let shadow_uniforms = ShadowUniforms {
            light_view_proj: light_view_proj.to_cols_array_2d(),
            model: Mat4::IDENTITY.to_cols_array_2d(),
        };
        render.shadow_pass.render(
            &mut encoder,
            &render.gpu.queue,
            &shadow_uniforms,
            &[&render.mesh],
        );

        // Forward pass — Blinn-Phong shading → framebuffer (HDR)
        let material = MaterialParams {
            base_color: [
                self.base_color[0],
                self.base_color[1],
                self.base_color[2],
                1.0,
            ],
            specular_strength: self.specular_strength,
            shininess: self.shininess,
            ambient_strength: self.ambient_strength,
            has_texture: false,
        };
        let shadow = ShadowParams {
            light_view_proj,
            enabled: self.shadow_enabled,
            bias: self.shadow_bias,
            strength: self.shadow_strength,
        };
        let uniforms = ForwardUniforms::new(
            &self.camera.uniforms(),
            Mat4::IDENTITY,
            [light.x, light.y, light.z],
            &material,
            &shadow,
        );
        render.forward_pass.render(
            &mut encoder,
            &render.framebuffer,
            &render.gpu.queue,
            &uniforms,
            &[&render.mesh],
            None,
        );

        // Post-process pass — framebuffer → swapchain (tone mapping, fog, color grading)
        let pp_params = PostProcessParams {
            fog_color: [self.fog_color[0], self.fog_color[1], self.fog_color[2], 1.0],
            fog_params: [
                if self.fog_enabled { 1.0 } else { 0.0 },
                self.fog_near,
                self.fog_far,
                1.0,
            ],
            color_grade: [self.exposure, self.contrast, self.saturation, self.vignette],
            tint: [self.tint[0], self.tint[1], self.tint[2], 1.0],
        };
        render
            .post_process
            .render(&mut encoder, &view, &render.gpu.queue, &pp_params);

        // egui render (on top of swapchain)
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [
                render.gpu.surface_config.width,
                render.gpu.surface_config.height,
            ],
            pixels_per_point: window.scale_factor() as f32,
        };

        for (id, image_delta) in &egui_output.textures_delta.set {
            render.egui_renderer.update_texture(
                &render.gpu.device,
                &render.gpu.queue,
                *id,
                image_delta,
            );
        }

        render.egui_renderer.update_buffers(
            &render.gpu.device,
            &render.gpu.queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );

        {
            let mut render_pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                })
                .forget_lifetime();
            render
                .egui_renderer
                .render(&mut render_pass, &paint_jobs, &screen_descriptor);
        }

        for id in &egui_output.textures_delta.free {
            render.egui_renderer.free_texture(id);
        }

        render.gpu.queue.submit(std::iter::once(encoder.finish()));
        surface_texture.present();
    }
}

fn main() {
    env_logger::init();
    let event_loop = EventLoop::new().unwrap();
    let mut app = App::new();
    event_loop.run_app(&mut app).unwrap();
}
