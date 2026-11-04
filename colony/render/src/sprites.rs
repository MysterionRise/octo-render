//! Colonists and items over the cells: [`sprite_instances`] turns a snapshot into one
//! quad per sprite in world pixels, interpolating each colonist between the cell it left
//! and the cell it is entering; [`SpriteRenderer`] draws them from a storage buffer; and
//! [`stamp_sprites`] is the CPU twin of `shaders/sprites.wgsl`, which tests hold the
//! shader to.

use bytemuck::{Pod, Zeroable};
use colony_sim::agent::MOVE_TICKS;
use colony_sim::{BuildingKind, BuildingTable, NO_BUILDING};
use image::{Rgba, RgbaImage};

use crate::camera::PixelCamera;
use crate::palette::Luts;
use crate::snapshot::Snapshot;
use crate::Overlay;

pub const SHADER: &str = include_str!("../shaders/sprites.wgsl");

/// Draw order: items on the ground, buildings, packets, colonists, then badges.
pub const LAYER_ITEM: u32 = 0;
pub const LAYER_BUILDING: u32 = 1;
pub const LAYER_PACKET: u32 = 2;
pub const LAYER_COLONIST: u32 = 3;
pub const LAYER_BADGE: u32 = 4;

/// Overlay colours: powered, brown-out, unconnected, pipe holding a packet, empty pipe.
pub const POWERED: [u8; 3] = [72, 224, 96];
pub const BROWNOUT: [u8; 3] = [240, 64, 64];
pub const UNCONNECTED: [u8; 3] = [128, 128, 128];
pub const PIPE_FULL: [u8; 3] = [240, 240, 255];
pub const PIPE_EMPTY: [u8; 3] = [60, 80, 110];
pub const BLOCKED: [u8; 3] = [255, 40, 40];
pub const CHARGE: [u8; 3] = [120, 255, 120];

/// Colonist body colour by `AgentState::tag()`: idle, walking, fetching, working, stuck.
pub const STATE_COLOURS: [[u8; 3]; 5] = [
    [236, 226, 198],
    [120, 200, 255],
    [255, 196, 90],
    [140, 230, 120],
    [240, 80, 80],
];

/// Mirrors `SpriteInstance` in `sprites.wgsl` (32 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct SpriteInstance {
    /// Bottom-left corner in world pixels (y up).
    pub pos: [i32; 2],
    /// Size in world pixels.
    pub size: [u32; 2],
    /// `r | g << 8 | b << 16 | a << 24`.
    pub rgba: u32,
    pub layer: u32,
    pub _pad: [u32; 2],
}

impl SpriteInstance {
    pub fn new(pos: (i32, i32), size: (u32, u32), rgb: [u8; 3], layer: u32) -> SpriteInstance {
        SpriteInstance {
            pos: [pos.0, pos.1],
            size: [size.0, size.1],
            rgba: pack_rgba(rgb, 255),
            layer,
            _pad: [0; 2],
        }
    }

    pub fn rgb(&self) -> [u8; 3] {
        let c = self.rgba;
        [c as u8, (c >> 8) as u8, (c >> 16) as u8]
    }
}

pub fn pack_rgba(rgb: [u8; 3], a: u8) -> u32 {
    rgb[0] as u32 | (rgb[1] as u32) << 8 | (rgb[2] as u32) << 16 | (a as u32) << 24
}

/// A colonist's bottom-left world pixel, `t / (MOVE_TICKS × 256)` of the way from
/// `from` to `to`, where `t = progress × 256 + alpha_x256` (the fraction of the current
/// step period that has elapsed since the snapshot, `0..=256`), saturating at `to`.
pub fn lerp_cell_px(
    from: (u32, u32),
    to: (u32, u32),
    progress: u8,
    alpha_x256: u32,
    tile_px: u32,
) -> (i32, i32) {
    let span = MOVE_TICKS as i64 * 256;
    let t = (progress as i64 * 256 + alpha_x256.min(256) as i64).min(span);
    let axis = |a: u32, b: u32| -> i32 {
        let a = a as i64 * tile_px as i64;
        let b = b as i64 * tile_px as i64;
        (a + (b - a) * t / span) as i32
    };
    (axis(from.0, to.0), axis(from.1, to.1))
}

/// A building's quad colour: its definition's colour scaled by state (planned a
/// quarter, under construction half), halved again when a consumer is unpowered or the
/// building is switched off; the power and pipe overlays replace it outright.
pub fn building_colour(
    pose: &crate::snapshot::BuildingPose,
    def: &colony_sim::BuildingDef,
    has_packet: bool,
    overlay: Overlay,
) -> [u8; 3] {
    let on_wire = pose.component != NO_BUILDING;
    match overlay {
        Overlay::Power if def.kind.is_wire() || def.consumes_power() || def.power_per_tick > 0 => {
            return if !on_wire {
                UNCONNECTED
            } else if pose.powered {
                POWERED
            } else {
                BROWNOUT
            };
        }
        Overlay::Pipes if def.kind.is_pipe_cell() => {
            return if has_packet { PIPE_FULL } else { PIPE_EMPTY };
        }
        _ => {}
    }
    let scale = |c: [u8; 3], num: u32, den: u32| c.map(|v| (v as u32 * num / den) as u8);
    let mut c = match pose.state {
        0 => scale(def.colour, 1, 4),
        1 => scale(def.colour, 1, 2),
        _ => def.colour,
    };
    if pose.state == 2 && (!pose.enabled || (pose.consumes && !pose.powered)) {
        c = scale(c, 1, 2);
    }
    c
}

/// The sprites of a snapshot in draw order: ground items (half a cell, in the element's
/// colour), buildings (their footprint, see [`building_colour`], with a charge bar on
/// batteries and a red badge when blocked), packets (half a cell in the element's
/// colour), colonist bodies (three quarters of a cell wide, a cell tall, coloured by
/// state), and a badge on each carrying colonist. Carried items draw as badges only.
pub fn sprite_instances(
    snapshot: &Snapshot,
    luts: &Luts,
    defs: &BuildingTable,
    tile_px: u32,
    alpha_x256: u32,
    overlay: Overlay,
) -> Vec<SpriteInstance> {
    let t = tile_px as i32;
    let half = (tile_px / 2).max(1);
    let quarter = (tile_px / 4).max(1);
    let body_w = (tile_px * 3 / 4).max(1);
    let body_dx = (t - body_w as i32) / 2;
    let mut out = Vec::with_capacity(
        snapshot.items.len()
            + 2 * snapshot.buildings.len()
            + snapshot.packets.len()
            + 2 * snapshot.colonists.len(),
    );
    for item in snapshot.items.iter().filter(|i| !i.held) {
        let x = item.x as i32 * t + (t - half as i32) / 2;
        let y = item.y as i32 * t;
        out.push(SpriteInstance::new(
            (x, y),
            (half, half),
            luts.element_colour(item.element),
            LAYER_ITEM,
        ));
    }
    let mut badges = Vec::new();
    for b in &snapshot.buildings {
        if !defs.contains(b.def) {
            continue;
        }
        let def = defs.get(b.def);
        let has_packet = snapshot.packets.iter().any(|p| (p.x, p.y) == (b.x, b.y));
        let (px, py) = (b.x as i32 * t, b.y as i32 * t);
        let (w, h) = (b.w as u32 * tile_px, b.h as u32 * tile_px);
        out.push(SpriteInstance::new(
            (px, py),
            (w, h),
            building_colour(b, def, has_packet, overlay),
            LAYER_BUILDING,
        ));
        if matches!(def.kind, BuildingKind::Battery { .. }) && b.charge_x256 > 0 {
            let bar = (w * b.charge_x256 as u32 / 255).max(1);
            badges.push(SpriteInstance::new(
                (px, py),
                (bar, quarter),
                CHARGE,
                LAYER_BADGE,
            ));
        }
        if b.blocked && b.state == 2 {
            badges.push(SpriteInstance::new(
                (
                    px + w as i32 - quarter as i32,
                    py + h as i32 - quarter as i32,
                ),
                (quarter, quarter),
                BLOCKED,
                LAYER_BADGE,
            ));
        }
    }
    for p in &snapshot.packets {
        out.push(SpriteInstance::new(
            (
                p.x as i32 * t + (t - half as i32) / 2,
                p.y as i32 * t + (t - half as i32) / 2,
            ),
            (half, half),
            luts.element_colour(p.element),
            LAYER_PACKET,
        ));
    }
    for c in &snapshot.colonists {
        let (px, py) = lerp_cell_px(c.from, c.to, c.progress, alpha_x256, tile_px);
        let colour = STATE_COLOURS[(c.state as usize).min(STATE_COLOURS.len() - 1)];
        let body = (px + body_dx, py);
        out.push(SpriteInstance::new(
            body,
            (body_w, tile_px),
            colour,
            LAYER_COLONIST,
        ));
        if let Some(element) = c.carrying {
            badges.push(SpriteInstance::new(
                (
                    body.0 + body_w as i32 - quarter as i32,
                    body.1 + t - quarter as i32,
                ),
                (quarter, quarter),
                luts.element_colour(element),
                LAYER_BADGE,
            ));
        }
    }
    out.extend(badges);
    out
}

/// The screen rectangle `[x0, x1) × [y0, y1)` (y down) a sprite covers, before clipping.
pub fn screen_rect(camera: &PixelCamera, s: &SpriteInstance) -> (i64, i64, i64, i64) {
    let (ox, oy) = camera.origin();
    let z = camera.zoom() as i64;
    let vh = camera.viewport().1 as i64;
    let (x0, y0) = (s.pos[0] as i64, s.pos[1] as i64);
    let (x1, y1) = (x0 + s.size[0] as i64, y0 + s.size[1] as i64);
    (
        (x0 - ox) * z,
        vh - (y1 - oy) * z,
        (x1 - ox) * z,
        vh - (y0 - oy) * z,
    )
}

/// Paints the sprites over an image of the camera's viewport, in order: the CPU twin of
/// `sprites.wgsl` (opaque quads on pixel boundaries, later instances on top).
pub fn stamp_sprites(img: &mut RgbaImage, camera: &PixelCamera, sprites: &[SpriteInstance]) {
    let (vw, vh) = camera.viewport();
    for s in sprites {
        let (x0, y0, x1, y1) = screen_rect(camera, s);
        let [r, g, b] = s.rgb();
        for y in y0.max(0)..y1.min(vh as i64) {
            for x in x0.max(0)..x1.min(vw as i64) {
                img.put_pixel(x as u32, y as u32, Rgba([r, g, b, 255]));
            }
        }
    }
}

/// The GPU side: a pipeline over `sprites.wgsl`, the view uniforms shared with the cell
/// pass, and a storage buffer of instances grown on demand.
pub struct SpriteRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    capacity: usize,
    count: usize,
}

impl SpriteRenderer {
    pub fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        view_uniforms: &wgpu::Buffer,
    ) -> SpriteRenderer {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("colony sprites layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("colony sprites shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("colony sprites pipeline layout"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("colony sprites pipeline"),
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
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
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
        let capacity = 64;
        let buffer = Self::make_buffer(device, capacity);
        let bind_group = Self::make_bind_group(device, &layout, view_uniforms, &buffer);
        SpriteRenderer {
            pipeline,
            layout,
            bind_group,
            buffer,
            capacity,
            count: 0,
        }
    }

    fn make_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("colony sprites"),
            size: (capacity * std::mem::size_of::<SpriteInstance>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn make_bind_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        view_uniforms: &wgpu::Buffer,
        buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("colony sprites bind group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: view_uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buffer.as_entire_binding(),
                },
            ],
        })
    }

    /// Replaces the instances; the buffer grows (and the bind group is rebuilt) when
    /// they no longer fit.
    pub fn set_instances(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view_uniforms: &wgpu::Buffer,
        instances: &[SpriteInstance],
    ) {
        if instances.len() > self.capacity {
            self.capacity = instances.len().next_power_of_two();
            self.buffer = Self::make_buffer(device, self.capacity);
            self.bind_group =
                Self::make_bind_group(device, &self.layout, view_uniforms, &self.buffer);
        }
        if !instances.is_empty() {
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(instances));
        }
        self.count = instances.len();
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..(6 * self.count as u32), 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{ColonistPose, ItemPose};
    use crate::TILE_PX;
    use colony_sim::{scenario, ElementTable, TickStats};
    use naga::valid::{Capabilities, ValidationFlags, Validator};

    fn empty_snapshot() -> Snapshot {
        let sim = scenario::settled(16, 8);
        let n = sim.chunks().len();
        Snapshot::from_sim(&sim, &vec![true; n], 0, TickStats::fresh(n, 0), 0, 0)
    }

    #[test]
    fn shader_parses_validates_and_matches_the_struct_layout() {
        let module = naga::front::wgsl::parse_str(SHADER).expect("sprites.wgsl parses");
        Validator::new(ValidationFlags::all(), Capabilities::all())
            .validate(&module)
            .expect("sprites.wgsl validates");
        assert_eq!(std::mem::size_of::<SpriteInstance>(), 32);
        let (_, ty) = module
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some("SpriteInstance"))
            .expect("SpriteInstance struct");
        let naga::TypeInner::Struct { members, span } = &ty.inner else {
            panic!("not a struct");
        };
        assert_eq!(*span, 32);
        let offsets: Vec<u32> = members.iter().map(|m| m.offset).collect();
        assert_eq!(offsets, vec![0, 8, 16, 20, 24, 28]);
        let (_, view) = module
            .types
            .iter()
            .find(|(_, t)| t.name.as_deref() == Some("ViewUniforms"))
            .unwrap();
        let naga::TypeInner::Struct { span, .. } = &view.inner else {
            panic!("not a struct");
        };
        assert_eq!(*span, 48, "the view uniforms are shared with cells.wgsl");
    }

    #[test]
    fn colonists_interpolate_from_the_cell_they_left_and_saturate_on_arrival() {
        assert_eq!(lerp_cell_px((2, 3), (3, 3), 0, 0, 8), (16, 24));
        assert_eq!(lerp_cell_px((2, 3), (3, 3), 0, 128, 8), (18, 24));
        assert_eq!(lerp_cell_px((2, 3), (3, 3), 1, 0, 8), (20, 24));
        assert_eq!(lerp_cell_px((2, 3), (3, 3), 1, 256, 8), (24, 24));
        assert_eq!(lerp_cell_px((2, 3), (3, 3), MOVE_TICKS, 200, 8), (24, 24));
        assert_eq!(
            lerp_cell_px((5, 6), (4, 2), 1, 0, 8),
            (36, 32),
            "moving left and down"
        );
        assert_eq!(
            lerp_cell_px((5, 6), (4, 2), 1, 999, 8),
            (32, 16),
            "alpha clamps"
        );
    }

    #[test]
    fn instances_skip_carried_items_badge_carriers_and_colour_by_state() {
        let table = ElementTable::builtin();
        let luts = Luts::from_table(&table);
        let mut snap = empty_snapshot();
        snap.items = vec![
            ItemPose {
                id: 0,
                x: 1,
                y: 2,
                element: 4,
                held: false,
            },
            ItemPose {
                id: 1,
                x: 3,
                y: 2,
                element: 4,
                held: true,
            },
        ];
        snap.colonists = vec![
            ColonistPose {
                id: 0,
                from: (3, 2),
                to: (4, 2),
                progress: 1,
                state: 2,
                job: 7,
                carrying: Some(4),
            },
            ColonistPose {
                id: 1,
                from: (6, 2),
                to: (6, 2),
                progress: MOVE_TICKS,
                state: 4,
                job: u32::MAX,
                carrying: None,
            },
        ];
        let defs = BuildingTable::builtin();
        let out = sprite_instances(&snap, &luts, &defs, TILE_PX, 0, Overlay::None);
        assert_eq!(out.len(), 4, "one ground item, two bodies, one badge");
        assert_eq!(out[0].layer, LAYER_ITEM);
        assert_eq!((out[0].pos, out[0].size), ([10, 16], [4, 4]));
        assert_eq!(out[0].rgb(), luts.element_colour(4));
        assert_eq!(out[1].layer, LAYER_COLONIST);
        assert_eq!(
            (out[1].pos, out[1].size),
            ([29, 16], [6, 8]),
            "half way, 1 px inset"
        );
        assert_eq!(out[1].rgb(), STATE_COLOURS[2]);
        assert_eq!(out[2].pos, [49, 16]);
        assert_eq!(out[2].rgb(), STATE_COLOURS[4]);
        assert_eq!(out[3].layer, LAYER_BADGE);
        assert_eq!((out[3].pos, out[3].size), ([33, 22], [2, 2]));
        assert_eq!(out[3].rgb(), luts.element_colour(4));
        assert_eq!(pack_rgba([1, 2, 3], 255), 0xff03_0201);
        let later = sprite_instances(&snap, &luts, &defs, TILE_PX, 256, Overlay::None);
        assert_eq!(
            later[1].pos,
            [33, 16],
            "a full tick later the walker has arrived"
        );
        assert_eq!(later[2].pos, out[2].pos, "the stuck one does not move");
    }

    #[test]
    fn building_sprites_cover_footprints_and_tint_by_state_and_overlay() {
        use crate::snapshot::{BuildingPose, PacketPose};
        use colony_sim::building::ids;
        let table = ElementTable::builtin();
        let luts = Luts::from_table(&table);
        let defs = BuildingTable::builtin();
        let pose = |def: u16, x: u32, state: u8, powered: bool, component: u32| BuildingPose {
            id: 0,
            def,
            x,
            y: 2,
            w: defs.get(def).size.0,
            h: defs.get(def).size.1,
            state,
            enabled: true,
            powered,
            blocked: false,
            signal: false,
            consumes: defs.get(def).consumes_power(),
            charge_x256: 0,
            component,
            stored: None,
        };
        let mut snap = empty_snapshot();
        let mut battery = pose(ids::BATTERY, 8, 2, true, 0);
        battery.charge_x256 = 128;
        let mut blocked_vent = pose(ids::GAS_VENT, 12, 2, true, NO_BUILDING);
        blocked_vent.blocked = true;
        snap.buildings = vec![
            pose(ids::HAND_CRANK, 1, 2, true, 0),
            pose(ids::WIRE, 3, 2, true, 0),
            pose(ids::HEATER, 4, 0, false, NO_BUILDING),
            pose(ids::HEATER, 5, 2, false, 0),
            pose(ids::HEATER, 6, 2, false, NO_BUILDING),
            battery,
            pose(ids::GAS_PIPE, 11, 2, true, NO_BUILDING),
            blocked_vent,
        ];
        snap.packets = vec![PacketPose {
            x: 11,
            y: 2,
            element: 1,
        }];
        let out = sprite_instances(&snap, &luts, &defs, TILE_PX, 0, Overlay::None);
        // Eight buildings, a charge bar, a blocked badge, one packet.
        assert_eq!(out.len(), 11);
        let crank = &out[0];
        assert_eq!(
            (crank.pos, crank.size, crank.layer),
            ([8, 16], [16, 16], LAYER_BUILDING)
        );
        assert_eq!(crank.rgb(), defs.get(ids::HAND_CRANK).colour);
        let planned = &out[2];
        assert_eq!(planned.rgb(), defs.get(ids::HEATER).colour.map(|v| v / 4));
        let unpowered = &out[3];
        assert_eq!(unpowered.rgb(), defs.get(ids::HEATER).colour.map(|v| v / 2));
        let packet = out.iter().find(|s| s.layer == LAYER_PACKET).unwrap();
        assert_eq!((packet.pos, packet.size), ([90, 18], [4, 4]));
        assert_eq!(packet.rgb(), luts.element_colour(1));
        let bar = out
            .iter()
            .find(|s| s.layer == LAYER_BADGE && s.rgb() == CHARGE)
            .unwrap();
        assert_eq!(
            (bar.pos, bar.size),
            ([64, 16], [8, 2]),
            "half charged, half the width"
        );
        let badge = out.iter().find(|s| s.rgb() == BLOCKED).unwrap();
        assert_eq!((badge.pos, badge.size), ([102, 22], [2, 2]));
        assert!(
            out.windows(2).all(|w| w[0].layer <= w[1].layer),
            "draw order by layer"
        );

        let power = sprite_instances(&snap, &luts, &defs, TILE_PX, 0, Overlay::Power);
        assert_eq!(power[0].rgb(), POWERED, "a generator on a wire");
        assert_eq!(power[1].rgb(), POWERED, "the wire itself");
        assert_eq!(power[2].rgb(), UNCONNECTED, "planned and off the wire");
        assert_eq!(power[3].rgb(), BROWNOUT, "on the wire but unpowered");
        assert_eq!(power[4].rgb(), UNCONNECTED);
        assert_eq!(
            power[6].rgb(),
            defs.get(ids::GAS_PIPE).colour,
            "pipes keep their colour"
        );
        let pipes = sprite_instances(&snap, &luts, &defs, TILE_PX, 0, Overlay::Pipes);
        assert_eq!(pipes[6].rgb(), PIPE_FULL);
        assert_eq!(pipes[7].rgb(), PIPE_EMPTY);
        assert_eq!(pipes[0].rgb(), defs.get(ids::HAND_CRANK).colour);
    }

    #[test]
    fn stamp_paints_exactly_the_screen_rect_and_clips_at_the_viewport_edge() {
        let camera = PixelCamera::new((16, 8), TILE_PX, (64, 32), 2);
        let (ox, oy) = camera.origin();
        assert_eq!(
            (ox, oy),
            ((128 - 32) / 2, (64 - 16) / 2),
            "the view is centred"
        );
        let s = SpriteInstance::new((ox as i32 + 3, oy as i32 + 2), (4, 3), [9, 8, 7], 1);
        assert_eq!(screen_rect(&camera, &s), (6, 32 - 10, 14, 32 - 4));
        let mut img = RgbaImage::from_pixel(64, 32, Rgba([0, 0, 0, 255]));
        let edge = SpriteInstance::new((ox as i32 - 2, oy as i32 + 15), (4, 4), [1, 1, 1], 0);
        stamp_sprites(&mut img, &camera, &[s, edge]);
        let mut painted = 0;
        for (x, y, p) in img.enumerate_pixels() {
            let inside = (6..14).contains(&x) && (22..28).contains(&y);
            let edge_px = x < 4 && y < 2;
            assert_eq!(p.0 == [9, 8, 7, 255], inside, "({x}, {y})");
            assert_eq!(p.0 == [1, 1, 1, 255], edge_px, "({x}, {y})");
            painted += (inside || edge_px) as u32;
        }
        assert_eq!(painted, 8 * 6 + 4 * 2);
    }
}
