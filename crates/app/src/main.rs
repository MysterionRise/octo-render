use std::sync::Arc;

use glam::{Mat4, Vec3};
use octo_asset::load_obj;
use octo_gpu::GpuContext;
use octo_renderer::{ForwardPass, GpuMesh, OrbitCamera};
use octo_renderer::forward::ForwardUniforms;
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
    forward_pass: ForwardPass,
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

        let forward_pass =
            ForwardPass::new(&gpu.device, gpu.surface_format, size.width, size.height);

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
        let egui_renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            gpu.surface_format,
            None,
            1,
            false,
        );

        self.render = Some(RenderState {
            gpu,
            forward_pass,
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
                        .forward_pass
                        .resize(&render.gpu.device, new_size.width, new_size.height);
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
                                    .selectable_value(
                                        &mut self.current_model_idx,
                                        i,
                                        *name,
                                    )
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
                render
                    .gpu
                    .surface
                    .configure(&render.gpu.device, config);
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

        // Forward pass — Gouraud shading
        let raw_light = Vec3::new(self.light_dir[0], self.light_dir[1], self.light_dir[2]);
        let light = if raw_light.length_squared() < 1e-6 {
            Vec3::Y
        } else {
            raw_light.normalize()
        };
        let uniforms = ForwardUniforms::new(
            &self.camera.uniforms(),
            Mat4::IDENTITY,
            [light.x, light.y, light.z],
        );
        render
            .forward_pass
            .render(&mut encoder, &view, &render.gpu.queue, &uniforms, &[&render.mesh]);

        // egui render
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
