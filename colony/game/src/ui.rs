//! The egui inspector: timings, sim controls, overlays, the tool selector and build
//! menu, colonists, jobs and networks, the cell, colonist and building probe, save,
//! load, and content reload.

use colony_render::snapshot::{building_state_label, state_label};
use colony_render::{BuildingPose, CellProbe, ColonistPose, Overlay, PixelCamera, Snapshot};
use colony_sim::agent::NO_JOB;
use colony_sim::{scenario, BuildingTable, ElementTable, NO_BUILDING};

use crate::input::Tool;

pub struct UiState {
    pub paused: bool,
    pub speed: u32,
    pub scenario: String,
    pub seed: u64,
    pub overlay: Overlay,
    pub tool: Tool,
    /// The building the place tool puts down.
    pub selected_def: u16,
    /// The building table the window knows (extended by reloads).
    pub building_table: BuildingTable,
    /// Whether a content directory is watched for `buildings.ron` changes.
    pub watching_content: bool,
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
    /// Re-read `buildings.ron` from the content directory now.
    Reload,
}

/// The probe under the cursor: cell coordinates, its values, the colonist and the
/// building there.
pub type Probe = Option<(
    (usize, usize),
    CellProbe,
    Option<ColonistPose>,
    Option<BuildingPose>,
)>;

pub fn draw(
    ctx: &egui::Context,
    state: &mut UiState,
    snapshot: Option<&Snapshot>,
    table: &ElementTable,
    camera: &PixelCamera,
    probe: Probe,
) -> Vec<UiAction> {
    let mut actions = Vec::new();
    egui::SidePanel::left("inspector").default_width(280.0).show(ctx, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
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
            ui.horizontal_wrapped(|ui| {
                for overlay in Overlay::ALL {
                    ui.selectable_value(&mut state.overlay, overlay, overlay.label());
                }
            });
            ui.separator();

            ui.heading("Tool");
            ui.horizontal_wrapped(|ui| {
                for tool in Tool::ALL {
                    ui.selectable_value(&mut state.tool, tool, tool.label());
                }
            });
            if state.tool == Tool::Place {
                ui.label("Build menu (bottom-left cell goes under the cursor):");
                for (i, def) in state.building_table.defs().iter().enumerate() {
                    let cost = match def.cost {
                        Some((e, m)) => format!(", {} kg {}", m.to_grams() / 1000, table.get(e).name),
                        None => String::new(),
                    };
                    let text = format!("{} {}x{}{}", def.name, def.size.0, def.size.1, cost);
                    ui.selectable_value(&mut state.selected_def, i as u16, text);
                }
            }
            ui.separator();

            ui.heading("Colonists");
            if let Some(s) = snapshot {
                let j = s.stats.jobs;
                ui.label(format!("{} colonists, {} items", s.stats.colonists, s.items.len()));
                ui.label(format!("jobs: {} open, {} reserved, {} done, {} cancelled", j.open, j.reserved, j.done, j.cancelled));
                ui.label(format!("{} path solves this tick", s.stats.path_solves));
            }
            ui.separator();

            ui.heading("Buildings and networks");
            if let Some(s) = snapshot {
                let n = s.stats.networks;
                let built = s.buildings.iter().filter(|b| b.state == 2).count();
                ui.label(format!("{} buildings, {} built", s.stats.buildings, built));
                ui.label(format!(
                    "{} power components: {} J generated, {} J consumed, {} brown-outs",
                    n.power_components,
                    n.generated >> 16,
                    n.consumed >> 16,
                    n.brownouts
                ));
                ui.label(format!("{} pipe runs, {} packets; {} signal networks", n.pipe_networks, n.packets, n.signal_networks));
            }
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
                if state.watching_content && ui.button("Reload buildings.ron").clicked() {
                    actions.push(UiAction::Reload);
                }
            });
            if state.watching_content {
                ui.small("buildings.ron is watched; saving it reloads the table (append only)");
            }
            if !state.status.is_empty() {
                ui.label(&state.status);
            }
            ui.separator();

            ui.heading("Probe");
            match probe {
                Some(((x, y), p, colonist, building)) => {
                    ui.label(format!("cell ({x}, {y}), chunk ({}, {})", p.chunk.0, p.chunk.1));
                    ui.label(table.get(p.element).name.as_str());
                    ui.label(format!("{} g", p.mass.to_grams()));
                    let mk = p.temp.to_millikelvin();
                    ui.label(format!("{}.{:03} K", mk.div_euclid(1000), mk.rem_euclid(1000)));
                    ui.label(format!("flags {:#04b}", p.flags));
                    if let Some(c) = colonist {
                        let job = if c.job == NO_JOB { "no job".to_string() } else { format!("job {}", c.job) };
                        let carrying = match c.carrying {
                            Some(e) => format!(", carrying {}", table.get(table.id(e as usize)).name),
                            None => String::new(),
                        };
                        ui.label(format!("colonist {}: {}, {job}{carrying}", c.id, state_label(c.state)));
                    }
                    if let Some(b) = building {
                        let name = if state.building_table.contains(b.def) {
                            state.building_table.get(b.def).name.as_str()
                        } else {
                            "?"
                        };
                        let power = if b.component == NO_BUILDING {
                            "off the grid".to_string()
                        } else if b.powered {
                            format!("powered (component {})", b.component)
                        } else {
                            format!("brown-out (component {})", b.component)
                        };
                        ui.label(format!(
                            "{name} {}x{} at ({}, {}): {}, {}, {}{}{}",
                            b.w,
                            b.h,
                            b.x,
                            b.y,
                            building_state_label(b.state),
                            if b.enabled { "enabled" } else { "disabled" },
                            power,
                            if b.blocked { ", blocked" } else { "" },
                            if b.signal { ", signal on" } else { "" }
                        ));
                        if let Some((e, grams)) = b.stored {
                            ui.label(format!("holds {} g {}", grams, table.get(table.id(e as usize)).name));
                        }
                        if b.charge_x256 > 0 {
                            ui.label(format!("charge {}%", b.charge_x256 as u32 * 100 / 255));
                        }
                    }
                }
                None => {
                    ui.label("hover a cell");
                }
            }
            ui.separator();
            ui.small("WASD/arrows pan, +/- or wheel zoom, space pause, 1/2/3 speed,\nQ/E/B/X/T/C/N/P/O pick a tool, left click applies it, right click pours water, Esc quits");
        });
    });
    actions
}
