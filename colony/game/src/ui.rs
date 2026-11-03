//! The egui inspector: timings, sim controls, overlays, the cell probe, save and load.

use colony_render::{CellProbe, Overlay, PixelCamera, Snapshot};
use colony_sim::{scenario, ElementTable};

pub struct UiState {
    pub paused: bool,
    pub speed: u32,
    pub scenario: String,
    pub seed: u64,
    pub overlay: Overlay,
    pub status: String,
    pub fps: f32,
    pub frame_ms: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiAction {
    SetPaused(bool),
    SetSpeed(u32),
    Restart,
    Save,
    Load,
}

/// The probe under the cursor: cell coordinates and its values.
pub type Probe = Option<((usize, usize), CellProbe)>;

pub fn draw(
    ctx: &egui::Context,
    state: &mut UiState,
    snapshot: Option<&Snapshot>,
    table: &ElementTable,
    camera: &PixelCamera,
    probe: Probe,
) -> Vec<UiAction> {
    let mut actions = Vec::new();
    egui::SidePanel::left("inspector").default_width(260.0).show(ctx, |ui| {
        ui.heading("Colony");
        ui.label(format!("{:.0} fps, {:.2} ms per frame", state.fps, state.frame_ms));
        ui.label(format!("zoom {}, view origin ({}, {})", camera.zoom(), camera.origin().0, camera.origin().1));
        ui.separator();

        ui.heading("Simulation");
        if let Some(s) = snapshot {
            let total = (s.chunk_cols * s.chunk_rows).max(1) as f32;
            ui.label(format!("tick {}", s.tick));
            ui.label(format!("step {:.2} ms, {:.1} ticks/s", s.step_micros as f32 / 1000.0, s.ticks_per_s_x100 as f32 / 100.0));
            ui.label(format!(
                "chunks: {:.0}% any pass, {:.0}% mass passes",
                100.0 * s.stats.chunks_processed as f32 / total,
                100.0 * s.stats.chunks_processed_mass as f32 / total
            ));
        } else {
            ui.label("waiting for the first snapshot");
        }
        ui.horizontal(|ui| {
            let label = if state.paused { "Resume" } else { "Pause" };
            if ui.button(label).clicked() {
                actions.push(UiAction::SetPaused(!state.paused));
            }
            for speed in 1..=3u32 {
                if ui.selectable_label(state.speed == speed, format!("{speed}x")).clicked() && state.speed != speed {
                    actions.push(UiAction::SetSpeed(speed));
                }
            }
        });
        ui.separator();

        ui.heading("Overlay");
        ui.horizontal(|ui| {
            for overlay in Overlay::ALL {
                ui.selectable_value(&mut state.overlay, overlay, overlay.label());
            }
        });
        ui.separator();

        ui.heading("World");
        egui::ComboBox::from_id_salt("scenario").selected_text(state.scenario.clone()).show_ui(ui, |ui| {
            for name in scenario::NAMES {
                ui.selectable_value(&mut state.scenario, name.to_string(), name);
            }
        });
        ui.horizontal(|ui| {
            ui.label("seed");
            ui.add(egui::DragValue::new(&mut state.seed));
            if ui.button("Restart").clicked() {
                actions.push(UiAction::Restart);
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Save snapshot.csim").clicked() {
                actions.push(UiAction::Save);
            }
            if ui.button("Load").clicked() {
                actions.push(UiAction::Load);
            }
        });
        if !state.status.is_empty() {
            ui.label(&state.status);
        }
        ui.separator();

        ui.heading("Probe");
        match probe {
            Some(((x, y), p)) => {
                ui.label(format!("cell ({x}, {y}), chunk ({}, {})", p.chunk.0, p.chunk.1));
                ui.label(table.get(p.element).name.as_str());
                ui.label(format!("{} g", p.mass.to_grams()));
                let mk = p.temp.to_millikelvin();
                ui.label(format!("{}.{:03} K", mk.div_euclid(1000), mk.rem_euclid(1000)));
                ui.label(format!("flags {:#04b}", p.flags));
            }
            None => {
                ui.label("hover a cell");
            }
        }
        ui.separator();
        ui.small("WASD/arrows pan, +/- or wheel zoom, space pause, 1/2/3 speed,\nleft click digs, right click pours water, Esc quits");
    });
    actions
}
