//! The window: winit event loop, input, the frame loop (snapshot, upload, sprites,
//! draw, egui), and the content watcher that hot-reloads `buildings.ron`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use colony_render::host::STEP_PERIOD;
use colony_render::palette::Luts;
use colony_render::{
    sprite_instances, CellRenderer, HostEvent, HostRequest, PixelCamera, SimHost, Snapshot,
    SpriteInstance, TILE_PX,
};
use colony_sim::{scenario, BuildingTable};
use octo_gpu::{GpuContext, GpuContextOptions};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{Window, WindowAttributes, WindowId};

use crate::input::{self, command_for, drags, Action, Held, Tool};
use crate::ui::{self, UiAction, UiState};
use crate::{apply_content, Options};

/// World pixels per second of keyboard panning at zoom 1.
const PAN_SPEED: f32 = 480.0;
/// How often the content directory is checked for a changed `buildings.ron`.
const CONTENT_POLL: Duration = Duration::from_secs(1);
/// Placement preview colours.
const PREVIEW_OK: [u8; 3] = [90, 255, 130];
const PREVIEW_BAD: [u8; 3] = [255, 90, 90];

pub fn run(opts: Options) -> ExitCode {
    let event_loop = match EventLoop::new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot create an event loop (is a display available?): {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut app = App::new(opts);
    match event_loop.run_app(&mut app) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("event loop error: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Gfx {
    gpu: GpuContext,
    renderer: CellRenderer,
    luts: Luts,
    camera: PixelCamera,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

/// The content directory being watched and the last `buildings.ron` time seen.
struct ContentWatch {
    dir: PathBuf,
    modified: Option<SystemTime>,
    last_poll: Instant,
    /// A table sent to the host whose answer is still pending.
    pending: Option<BuildingTable>,
}

struct App {
    opts: Options,
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    host: Option<SimHost>,
    latest: Option<Arc<Snapshot>>,
    /// When `latest` arrived, for interpolating sprites between ticks.
    latest_at: Instant,
    uploaded: Option<Arc<Snapshot>>,
    ui: UiState,
    held: Held,
    cursor: Option<(u32, u32)>,
    left_down: bool,
    right_down: bool,
    /// The cell the held tool last acted on, so a drag acts once per cell.
    last_tool_cell: Option<(usize, usize)>,
    last_frame: Instant,
    pan_accum: (f32, f32),
    content: Option<ContentWatch>,
}

impl App {
    fn new(opts: Options) -> App {
        let content = opts
            .content
            .clone()
            .or_else(colony_content::base_dir)
            .map(|dir| ContentWatch {
                modified: buildings_mtime(&dir),
                dir,
                last_poll: Instant::now(),
                pending: None,
            });
        let ui = UiState {
            paused: false,
            speed: opts.speed,
            scenario: opts.scenario.clone(),
            seed: opts.seed,
            overlay: opts.overlay,
            tool: Tool::default(),
            selected_def: 0,
            building_table: BuildingTable::builtin(),
            watching_content: content.is_some(),
            status: String::new(),
            fps: 0.0,
            frame_ms: 0.0,
        };
        App {
            opts,
            window: None,
            gfx: None,
            host: None,
            latest: None,
            latest_at: Instant::now(),
            uploaded: None,
            ui,
            held: Held::default(),
            cursor: None,
            left_down: false,
            right_down: false,
            last_tool_cell: None,
            last_frame: Instant::now(),
            pan_accum: (0.0, 0.0),
            content,
        }
    }

    fn world(&self) -> (u32, u32) {
        (self.opts.width as u32, self.opts.height as u32)
    }

    /// Applies the current tool (left button) or pours water (right button) at the cursor.
    fn apply_tool(&mut self, button: MouseButton) {
        let (Some(gfx), Some(host), Some(cursor)) = (&self.gfx, &self.host, self.cursor) else {
            return;
        };
        let Some((x, y)) = gfx.camera.screen_to_cell(cursor) else {
            return;
        };
        if self.last_tool_cell == Some((x, y)) {
            return;
        }
        self.last_tool_cell = Some((x, y));
        let tool = match button {
            MouseButton::Left => self.ui.tool,
            MouseButton::Right => Tool::Pour,
            _ => return,
        };
        host.send(HostRequest::Command(command_for(
            tool,
            x,
            y,
            self.ui.selected_def,
        )));
    }

    /// How far into the current step period the sim thread is, in 1/256ths, for
    /// interpolating colonists between the snapshot's cells.
    fn step_alpha_x256(&self) -> u32 {
        let Some(s) = &self.latest else {
            return 0;
        };
        if self.ui.paused {
            return 0;
        }
        let period = if s.ticks_per_s_x100 > 0 {
            100.0 / s.ticks_per_s_x100 as f32
        } else {
            STEP_PERIOD.as_secs_f32() / self.ui.speed.max(1) as f32
        };
        let elapsed = self.latest_at.elapsed().as_secs_f32();
        ((elapsed / period.max(1e-3)).clamp(0.0, 1.0) * 256.0) as u32
    }

    /// The footprint of the selected building under the cursor, green when it can be
    /// placed there as far as the snapshot shows.
    fn placement_preview(&self) -> Option<SpriteInstance> {
        if self.ui.tool != Tool::Place {
            return None;
        }
        let (gfx, snapshot, cursor) = (self.gfx.as_ref()?, self.latest.as_ref()?, self.cursor?);
        let (x, y) = gfx.camera.screen_to_cell(cursor)?;
        if !self.ui.building_table.contains(self.ui.selected_def) {
            return None;
        }
        let def = self.ui.building_table.get(self.ui.selected_def);
        let ok = snapshot.placement_valid(def, x as u32, y as u32);
        let t = TILE_PX as i32;
        Some(SpriteInstance::new(
            (x as i32 * t, y as i32 * t),
            (def.size.0 as u32 * TILE_PX, def.size.1 as u32 * TILE_PX),
            if ok { PREVIEW_OK } else { PREVIEW_BAD },
            colony_render::sprites::LAYER_BADGE,
        ))
    }

    /// Reads `buildings.ron` from the watched directory and asks the host to install it.
    fn reload_buildings(&mut self) {
        let (Some(watch), Some(host)) = (&mut self.content, &self.host) else {
            self.ui.status = "no content directory to reload from".to_string();
            return;
        };
        let path = watch.dir.join("buildings.ron");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.ui.status = format!("{}: {e}", path.display());
                return;
            }
        };
        match colony_content::parse_buildings(&text, host.table()).and_then(|table| {
            colony_content::reload_compatible(&self.ui.building_table, &table).map(|_| table)
        }) {
            Ok(table) => {
                watch.pending = Some(table.clone());
                host.send(HostRequest::ReloadBuildings(table));
                self.ui.status = "reloading buildings.ron".to_string();
            }
            Err(e) => self.ui.status = format!("buildings.ron: {e}"),
        }
    }

    /// Once a second, checks whether `buildings.ron` changed on disk.
    fn poll_content(&mut self, now: Instant) {
        let changed = match &mut self.content {
            Some(watch) if now.duration_since(watch.last_poll) >= CONTENT_POLL => {
                watch.last_poll = now;
                let modified = buildings_mtime(&watch.dir);
                let changed = modified.is_some() && modified != watch.modified;
                watch.modified = modified;
                changed
            }
            _ => false,
        };
        if changed {
            self.reload_buildings();
        }
    }

    fn apply_ui_actions(&mut self, actions: Vec<UiAction>) {
        for action in actions {
            let Some(host) = &self.host else {
                return;
            };
            match action {
                UiAction::SetPaused(p) => {
                    self.ui.paused = p;
                    host.send(HostRequest::SetPaused(p));
                }
                UiAction::SetSpeed(s) => {
                    self.ui.speed = s;
                    host.send(HostRequest::SetSpeed(s));
                }
                UiAction::Restart => {
                    let (w, h) = self.world();
                    match scenario::by_name_seeded(
                        &self.ui.scenario,
                        w as usize,
                        h as usize,
                        self.ui.seed,
                    ) {
                        Some(mut sim) => {
                            if let Err(e) = sim.set_building_table(self.ui.building_table.clone()) {
                                self.ui.status = e;
                                continue;
                            }
                            host.send(HostRequest::Restart(Box::new(sim)));
                            self.ui.status = format!(
                                "restarted {} with seed {}",
                                self.ui.scenario, self.ui.seed
                            );
                        }
                        None => self.ui.status = format!("unknown scenario {}", self.ui.scenario),
                    }
                }
                UiAction::Save => host.send(HostRequest::Save("snapshot.csim".into())),
                UiAction::Load => match std::fs::read("snapshot.csim") {
                    Ok(bytes) => host.send(HostRequest::Load(bytes)),
                    Err(e) => self.ui.status = format!("snapshot.csim: {e}"),
                },
                UiAction::Reload => self.reload_buildings(),
            }
        }
    }

    fn handle_host_events(&mut self) {
        let Some(host) = &self.host else {
            return;
        };
        for event in host.events() {
            self.ui.status = match event {
                HostEvent::Saved(Ok(path)) => format!("saved {}", path.display()),
                HostEvent::Saved(Err(e)) => format!("save failed: {e}"),
                HostEvent::Loaded(Ok(tick)) => format!("loaded tick {tick}"),
                HostEvent::Loaded(Err(e)) => format!("load failed: {e}"),
                HostEvent::Restarted => "restarted".to_string(),
                HostEvent::Reloaded(Ok(n)) => {
                    if let Some(table) = self.content.as_mut().and_then(|w| w.pending.take()) {
                        self.ui.building_table = table;
                    }
                    format!("reloaded buildings.ron: {n} buildings")
                }
                HostEvent::Reloaded(Err(e)) => {
                    if let Some(w) = self.content.as_mut() {
                        w.pending = None;
                    }
                    format!("reload refused: {e}")
                }
            };
        }
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if self.gfx.is_none() || self.host.is_none() {
            return;
        }
        // Timing.
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32().max(1e-6);
        self.last_frame = now;
        let ms = dt * 1000.0;
        self.ui.frame_ms = if self.ui.frame_ms == 0.0 {
            ms
        } else {
            self.ui.frame_ms * 0.9 + ms * 0.1
        };
        self.ui.fps = 1000.0 / self.ui.frame_ms.max(0.001);

        // Host events, content changes, and the newest snapshot; a stalled sim leaves
        // the last one.
        self.handle_host_events();
        self.poll_content(now);
        if let Some(s) = self.host.as_ref().and_then(|h| h.take_latest()) {
            self.latest = Some(s);
            self.latest_at = now;
        }
        let alpha = self.step_alpha_x256();
        let preview = self.placement_preview();
        let gfx = self.gfx.as_mut().unwrap();
        let host = self.host.as_ref().unwrap();

        // A loaded snapshot may have another size: rebuild the renderer and camera.
        if let Some(s) = &self.latest {
            if (s.width, s.height) != gfx.renderer.world() {
                gfx.renderer = CellRenderer::new(
                    &gfx.gpu.device,
                    &gfx.gpu.queue,
                    gfx.gpu.surface_format,
                    host.table(),
                    (s.width, s.height),
                );
                gfx.camera = PixelCamera::new(
                    (s.width, s.height),
                    TILE_PX,
                    gfx.camera.viewport(),
                    gfx.camera.zoom(),
                );
                self.opts.width = s.width as usize;
                self.opts.height = s.height as usize;
                self.uploaded = None;
            }
        }

        // Keyboard panning.
        let (dx, dy) = self.held.pan_direction();
        if dx != 0 || dy != 0 {
            let speed = PAN_SPEED / gfx.camera.zoom() as f32;
            self.pan_accum.0 += dx as f32 * speed * dt;
            self.pan_accum.1 += dy as f32 * speed * dt;
            let (ix, iy) = (self.pan_accum.0.trunc(), self.pan_accum.1.trunc());
            self.pan_accum.0 -= ix;
            self.pan_accum.1 -= iy;
            gfx.camera.pan(ix as i64, iy as i64);
        }

        // Probe under the cursor, with the colonist and building there if any.
        let probe = self
            .cursor
            .and_then(|c| gfx.camera.screen_to_cell(c))
            .and_then(|(x, y)| {
                self.latest.as_ref().and_then(|s| {
                    s.probe(host.table(), x as u32, y as u32).map(|p| {
                        (
                            (x, y),
                            p,
                            s.colonist_at(x as u32, y as u32).copied(),
                            s.building_at(x as u32, y as u32).copied(),
                        )
                    })
                })
            });

        // egui.
        let raw_input = gfx.egui_state.take_egui_input(&window);
        let egui_ctx = gfx.egui_ctx.clone();
        let mut actions = Vec::new();
        let egui_output = {
            let ui_state = &mut self.ui;
            let latest = self.latest.as_deref();
            let camera = &gfx.camera;
            egui_ctx.run(raw_input, |ctx| {
                actions = ui::draw(ctx, ui_state, latest, host.table(), camera, probe);
            })
        };
        gfx.egui_state
            .handle_platform_output(&window, egui_output.platform_output);
        let paint_jobs = gfx
            .egui_ctx
            .tessellate(egui_output.shapes, egui_output.pixels_per_point);

        // Upload what changed, the sprites for this frame, and the view.
        if let Some(s) = &self.latest {
            if !self.uploaded.as_ref().is_some_and(|u| Arc::ptr_eq(u, s)) {
                gfx.renderer.upload(&gfx.gpu.queue, s);
                self.uploaded = Some(s.clone());
            }
            let mut sprites = sprite_instances(
                s,
                &gfx.luts,
                &self.ui.building_table,
                TILE_PX,
                alpha,
                self.ui.overlay,
            );
            sprites.extend(preview);
            gfx.renderer
                .set_sprites(&gfx.gpu.device, &gfx.gpu.queue, &sprites);
        }
        gfx.renderer
            .set_view(&gfx.gpu.queue, &gfx.camera, self.ui.overlay);

        // Draw.
        let surface_texture = match gfx.gpu.surface.get_current_texture() {
            Ok(t) => t,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                gfx.gpu
                    .surface
                    .configure(&gfx.gpu.device, &gfx.gpu.surface_config);
                return;
            }
            Err(e) => {
                log::error!("surface: {e}");
                return;
            }
        };
        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("colony frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("colony cells"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            gfx.renderer.draw(&mut pass);
        }
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [gfx.gpu.surface_config.width, gfx.gpu.surface_config.height],
            pixels_per_point: window.scale_factor() as f32,
        };
        for (id, delta) in &egui_output.textures_delta.set {
            gfx.egui_renderer
                .update_texture(&gfx.gpu.device, &gfx.gpu.queue, *id, delta);
        }
        gfx.egui_renderer.update_buffers(
            &gfx.gpu.device,
            &gfx.gpu.queue,
            &mut encoder,
            &paint_jobs,
            &screen,
        );
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui"),
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
            gfx.egui_renderer.render(&mut pass, &paint_jobs, &screen);
        }
        for id in &egui_output.textures_delta.free {
            gfx.egui_renderer.free_texture(id);
        }
        gfx.gpu.queue.submit(std::iter::once(encoder.finish()));
        surface_texture.present();

        self.apply_ui_actions(actions);
        if self.ui.status.len() > 200 {
            self.ui.status.truncate(200);
        }
        let _ = event_loop;
    }
}

/// The modification time of `buildings.ron` in a content directory.
fn buildings_mtime(dir: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(dir.join("buildings.ron"))
        .and_then(|m| m.modified())
        .ok()
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = WindowAttributes::default()
            .with_title("Colony")
            .with_inner_size(LogicalSize::new(1280, 720));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let gpu = pollster::block_on(GpuContext::new_with(
            window.clone(),
            GpuContextOptions {
                backends: wgpu::Backends::all(),
                prefer_srgb: false,
            },
        ));
        log::info!("surface format {:?}", gpu.surface_format);
        let (w, h) = self.world();
        let mut sim =
            scenario::by_name_seeded(&self.opts.scenario, w as usize, h as usize, self.opts.seed)
                .unwrap_or_else(|| {
                    eprintln!("unknown scenario {:?}; using cave", self.opts.scenario);
                    self.ui.scenario = "cave".to_string();
                    scenario::cave(w as usize, h as usize)
                });
        if let Some(dir) = self.content.as_ref().map(|c| c.dir.clone()) {
            match apply_content(&mut sim, &dir) {
                Ok(n) => self.ui.status = format!("{n} buildings from {}", dir.display()),
                Err(e) => {
                    eprintln!("{e}");
                    self.ui.status = e;
                }
            }
        }
        self.ui.building_table = sim.building_table().clone();
        let host = SimHost::spawn(sim, self.opts.speed, false);
        let renderer = CellRenderer::new(
            &gpu.device,
            &gpu.queue,
            gpu.surface_format,
            host.table(),
            (w, h),
        );
        let luts = Luts::from_table(host.table());
        let size = window.inner_size();
        let camera = PixelCamera::new((w, h), TILE_PX, (size.width, size.height), self.opts.zoom);
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
        self.gfx = Some(Gfx {
            gpu,
            renderer,
            luts,
            camera,
            egui_ctx,
            egui_state,
            egui_renderer,
        });
        self.host = Some(host);
        self.window = Some(window);
        self.last_frame = Instant::now();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let (consumed, wants_keyboard, over_ui) = match &mut self.gfx {
            Some(gfx) => {
                let response = gfx.egui_state.on_window_event(&window, &event);
                (
                    response.consumed,
                    gfx.egui_ctx.wants_keyboard_input(),
                    gfx.egui_ctx.is_pointer_over_area(),
                )
            }
            None => (false, false, false),
        };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gfx) = &mut self.gfx {
                    gfx.gpu.resize(size.width, size.height);
                    gfx.camera.set_viewport((size.width, size.height));
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state,
                        ..
                    },
                ..
            } => {
                if state == ElementState::Released {
                    self.held.release(code);
                } else if !consumed && !wants_keyboard {
                    self.held.press(code);
                    match input::map_key(code) {
                        Some(Action::TogglePause) => {
                            self.apply_ui_actions(vec![UiAction::SetPaused(!self.ui.paused)]);
                        }
                        Some(Action::Speed(s)) => {
                            self.apply_ui_actions(vec![UiAction::SetSpeed(s)])
                        }
                        Some(Action::ZoomIn) | Some(Action::ZoomOut) => {
                            if let Some(gfx) = &mut self.gfx {
                                let (vw, vh) = gfx.camera.viewport();
                                let steps = if input::map_key(code) == Some(Action::ZoomIn) {
                                    1
                                } else {
                                    -1
                                };
                                gfx.camera.zoom_by((vw / 2, vh / 2), steps);
                            }
                        }
                        Some(Action::Tool(tool)) => self.ui.tool = tool,
                        Some(Action::Quit) => event_loop.exit(),
                        _ => {}
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Some((position.x.max(0.0) as u32, position.y.max(0.0) as u32));
                if !over_ui {
                    if self.left_down && drags(self.ui.tool) {
                        self.apply_tool(MouseButton::Left);
                    } else if self.right_down {
                        self.apply_tool(MouseButton::Right);
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => self.cursor = None,
            WindowEvent::MouseInput { state, button, .. } => {
                let down = state == ElementState::Pressed;
                match button {
                    MouseButton::Left => self.left_down = down,
                    MouseButton::Right => self.right_down = down,
                    _ => {}
                }
                self.last_tool_cell = None;
                if down && !consumed && !over_ui {
                    self.apply_tool(button);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !consumed {
                    let y = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                    };
                    if y.abs() >= 0.5 {
                        if let Some(gfx) = &mut self.gfx {
                            let anchor = self.cursor.unwrap_or_else(|| {
                                let (vw, vh) = gfx.camera.viewport();
                                (vw / 2, vh / 2)
                            });
                            gfx.camera.zoom_by(anchor, if y > 0.0 { 1 } else { -1 });
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => self.draw(event_loop),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}
