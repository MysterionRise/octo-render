//! Buildings: the fixed-point table content loading produces, and the entities placed
//! from it. Footprints are rectangles anchored at their bottom-left cell. A building
//! never lives in the cell arrays; `Sim` keeps a per-cell occupancy map beside the
//! arenas, so the fluid and heat passes are untouched by buildings.

use crate::element::ElementId;
use crate::fixed::{Mass, Temp};
use crate::hash::Fnv1a;

/// What a building does. Rates are per tick (10 Hz) in the kernel's fixed-point units:
/// energies are Q16.16 joules, masses raw Q16.16 kilograms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildingKind {
    /// Colonists climb its cells.
    Ladder,
    /// Its cells become solid (built from its material) but colonists pass through.
    Door,
    /// Holds debris delivered by colonists.
    Storage {
        capacity: Mass,
    },
    /// Adds `joules_per_tick` to its anchor cell while powered.
    Heater {
        joules_per_tick: i64,
    },
    /// Turns delivered `feedstock` into `output` gas at its anchor cell, plus heat.
    OxygenGenerator {
        feedstock: ElementId,
        feed_per_tick: Mass,
        output: ElementId,
        joules_per_tick: i64,
    },
    /// Generates `joules_per_tick` while a colonist operates it.
    HandCrank {
        joules_per_tick: i64,
    },
    /// Stores up to `capacity` joules of its component's surplus.
    Battery {
        capacity: i64,
    },
    /// Joins buildings into a power component.
    Wire,
    /// Moves up to `per_tick` of the gas in its cell into a packet each network step.
    GasPump {
        per_tick: Mass,
    },
    /// Carries one packet.
    Pipe,
    /// Releases arriving packets into its cell.
    Vent,
    /// Signals when its cell is warmer than `above`.
    TemperatureSensor {
        above: Temp,
    },
    /// Signals when its cell holds more than `above` of gas.
    PressureSensor {
        above: Mass,
    },
    /// Joins sensors, gates, and the buildings they control.
    SignalWire,
    And,
    Or,
    Not,
}

impl BuildingKind {
    /// A short stable tag, for hashes and saves.
    pub fn tag(&self) -> u8 {
        match self {
            BuildingKind::Ladder => 0,
            BuildingKind::Door => 1,
            BuildingKind::Storage { .. } => 2,
            BuildingKind::Heater { .. } => 3,
            BuildingKind::OxygenGenerator { .. } => 4,
            BuildingKind::HandCrank { .. } => 5,
            BuildingKind::Battery { .. } => 6,
            BuildingKind::Wire => 7,
            BuildingKind::GasPump { .. } => 8,
            BuildingKind::Pipe => 9,
            BuildingKind::Vent => 10,
            BuildingKind::TemperatureSensor { .. } => 11,
            BuildingKind::PressureSensor { .. } => 12,
            BuildingKind::SignalWire => 13,
            BuildingKind::And => 14,
            BuildingKind::Or => 15,
            BuildingKind::Not => 16,
        }
    }

    /// Part of the power graph (wire) or attached to one.
    pub fn is_wire(&self) -> bool {
        matches!(self, BuildingKind::Wire)
    }

    pub fn is_pipe_cell(&self) -> bool {
        matches!(
            self,
            BuildingKind::Pipe | BuildingKind::GasPump { .. } | BuildingKind::Vent
        )
    }

    pub fn is_signal_cell(&self) -> bool {
        matches!(
            self,
            BuildingKind::SignalWire
                | BuildingKind::TemperatureSensor { .. }
                | BuildingKind::PressureSensor { .. }
                | BuildingKind::And
                | BuildingKind::Or
                | BuildingKind::Not
        )
    }

    pub fn is_gate(&self) -> bool {
        matches!(
            self,
            BuildingKind::And | BuildingKind::Or | BuildingKind::Not
        )
    }

    pub fn is_sensor(&self) -> bool {
        matches!(
            self,
            BuildingKind::TemperatureSensor { .. } | BuildingKind::PressureSensor { .. }
        )
    }
}

/// One building definition in kernel units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildingDef {
    pub name: String,
    /// Footprint in cells, at least 1×1.
    pub size: (u8, u8),
    pub kind: BuildingKind,
    /// The material a colonist must bring, if any.
    pub cost: Option<(ElementId, Mass)>,
    /// Ticks of work to construct.
    pub build_ticks: u16,
    /// Display colour.
    pub colour: [u8; 3],
    /// Q16.16 joules per tick: positive generates, negative consumes, zero needs no power.
    pub power_per_tick: i64,
}

impl BuildingDef {
    pub fn new(name: &str, size: (u8, u8), kind: BuildingKind) -> BuildingDef {
        BuildingDef {
            name: name.to_string(),
            size,
            kind,
            cost: None,
            build_ticks: 20,
            colour: [200, 200, 200],
            power_per_tick: 0,
        }
    }

    pub fn with_cost(mut self, element: ElementId, mass: Mass) -> BuildingDef {
        self.cost = Some((element, mass));
        self
    }

    pub fn with_build_ticks(mut self, ticks: u16) -> BuildingDef {
        self.build_ticks = ticks;
        self
    }

    pub fn with_colour(mut self, colour: [u8; 3]) -> BuildingDef {
        self.colour = colour;
        self
    }

    pub fn with_power(mut self, power_per_tick: i64) -> BuildingDef {
        self.power_per_tick = power_per_tick;
        self
    }

    pub fn cells(&self) -> u32 {
        self.size.0 as u32 * self.size.1 as u32
    }

    pub fn consumes_power(&self) -> bool {
        self.power_per_tick < 0
    }
}

/// The table of building definitions; indices are stable within a session and across
/// hot reloads, which may only append.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct BuildingTable {
    defs: Vec<BuildingDef>,
}

impl BuildingTable {
    /// Panics on duplicate names, empty footprints, or more than 65,535 entries.
    pub fn new(defs: Vec<BuildingDef>) -> BuildingTable {
        assert!(defs.len() < u16::MAX as usize, "too many buildings");
        for (i, d) in defs.iter().enumerate() {
            assert!(
                d.size.0 >= 1 && d.size.1 >= 1,
                "building {} has an empty footprint",
                d.name
            );
            assert!(
                !defs[..i].iter().any(|o| o.name == d.name),
                "duplicate building {}",
                d.name
            );
        }
        BuildingTable { defs }
    }

    pub fn empty() -> BuildingTable {
        BuildingTable::default()
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    pub fn defs(&self) -> &[BuildingDef] {
        &self.defs
    }

    pub fn get(&self, index: u16) -> &BuildingDef {
        &self.defs[index as usize]
    }

    pub fn contains(&self, index: u16) -> bool {
        (index as usize) < self.defs.len()
    }

    pub fn by_name(&self, name: &str) -> Option<u16> {
        self.defs
            .iter()
            .position(|d| d.name == name)
            .map(|i| i as u16)
    }

    /// Whether `self` keeps every entry of `old` at the same index with the same name
    /// (the hot-reload rule: append only).
    pub fn is_append_only_extension(&self, old: &BuildingTable) -> bool {
        self.defs.len() >= old.defs.len()
            && self
                .defs
                .iter()
                .zip(&old.defs)
                .all(|(n, o)| n.name == o.name)
    }

    /// A hash of every physical value, for logs and saves.
    pub fn hash(&self) -> u64 {
        let mut h = Fnv1a::new();
        h.write_u64(self.defs.len() as u64);
        for d in &self.defs {
            h.write_bytes(d.name.as_bytes());
            h.write_bytes(&[0, d.size.0, d.size.1, d.kind.tag()]);
            let (ce, cm) = d.cost.map_or((u16::MAX, 0), |(e, m)| (e.0, m.raw()));
            h.write_u16s(&[ce, d.build_ticks]);
            h.write_u32s(&[cm]);
            h.write_u64(d.power_per_tick as u64);
            match &d.kind {
                BuildingKind::Storage { capacity } => h.write_u32s(&[capacity.raw()]),
                BuildingKind::Heater { joules_per_tick }
                | BuildingKind::HandCrank { joules_per_tick } => {
                    h.write_u64(*joules_per_tick as u64)
                }
                BuildingKind::OxygenGenerator {
                    feedstock,
                    feed_per_tick,
                    output,
                    joules_per_tick,
                } => {
                    h.write_u16s(&[feedstock.0, output.0]);
                    h.write_u32s(&[feed_per_tick.raw()]);
                    h.write_u64(*joules_per_tick as u64);
                }
                BuildingKind::Battery { capacity } => h.write_u64(*capacity as u64),
                BuildingKind::GasPump { per_tick } => h.write_u32s(&[per_tick.raw()]),
                BuildingKind::TemperatureSensor { above } => h.write_i32s(&[above.raw()]),
                BuildingKind::PressureSensor { above } => h.write_u32s(&[above.raw()]),
                _ => {}
            }
        }
        h.finish()
    }
}

/// Indices of the [`BuildingTable::builtin`] entries.
pub mod ids {
    pub const LADDER: u16 = 0;
    pub const DOOR: u16 = 1;
    pub const STORAGE_BIN: u16 = 2;
    pub const HEATER: u16 = 3;
    pub const OXYGEN_GENERATOR: u16 = 4;
    pub const HAND_CRANK: u16 = 5;
    pub const BATTERY: u16 = 6;
    pub const WIRE: u16 = 7;
    pub const GAS_PUMP: u16 = 8;
    pub const GAS_PIPE: u16 = 9;
    pub const GAS_VENT: u16 = 10;
    pub const TEMPERATURE_SENSOR: u16 = 11;
    pub const PRESSURE_SENSOR: u16 = 12;
    pub const SIGNAL_WIRE: u16 = 13;
    pub const AND_GATE: u16 = 14;
    pub const OR_GATE: u16 = 15;
    pub const NOT_GATE: u16 = 16;
}

impl BuildingTable {
    /// The buildings of the base content pack, in kernel units, for scenarios, tests,
    /// and benches; `colony-content` pins its `base/buildings.ron` to this table.
    pub fn builtin() -> BuildingTable {
        use crate::element::builtin::{ICE, OXYGEN, SANDSTONE};
        use crate::fixed::q16;
        let cost = |kg: u32| (SANDSTONE, Mass::from_kg(kg));
        let simple = |name: &str,
                      size: (u8, u8),
                      kind: BuildingKind,
                      kg: u32,
                      ticks: u16,
                      colour: [u8; 3]| {
            let (e, m) = cost(kg);
            BuildingDef::new(name, size, kind)
                .with_cost(e, m)
                .with_build_ticks(ticks)
                .with_colour(colour)
        };
        BuildingTable::new(vec![
            simple(
                "ladder",
                (1, 1),
                BuildingKind::Ladder,
                50,
                20,
                [210, 180, 140],
            ),
            simple("door", (1, 2), BuildingKind::Door, 200, 30, [139, 111, 71]),
            simple(
                "storage_bin",
                (2, 2),
                BuildingKind::Storage {
                    capacity: Mass::from_kg(2000),
                },
                200,
                30,
                [176, 160, 112],
            ),
            simple(
                "heater",
                (1, 1),
                BuildingKind::Heater {
                    joules_per_tick: q16(50, 0),
                },
                100,
                30,
                [255, 127, 80],
            )
            .with_power(q16(-6, 0)),
            simple(
                "oxygen_generator",
                (2, 2),
                BuildingKind::OxygenGenerator {
                    feedstock: ICE,
                    feed_per_tick: Mass::from_grams(50),
                    output: OXYGEN,
                    joules_per_tick: q16(25, 0),
                },
                400,
                60,
                [95, 211, 200],
            )
            .with_power(q16(-12, 0)),
            simple(
                "hand_crank",
                (2, 2),
                BuildingKind::HandCrank {
                    joules_per_tick: q16(40, 0),
                },
                200,
                40,
                [201, 162, 39],
            )
            .with_power(q16(40, 0)),
            simple(
                "battery",
                (2, 2),
                BuildingKind::Battery {
                    capacity: q16(40_000, 0),
                },
                300,
                40,
                [127, 191, 63],
            ),
            simple("wire", (1, 1), BuildingKind::Wire, 25, 10, [224, 192, 64]),
            simple(
                "gas_pump",
                (1, 1),
                BuildingKind::GasPump {
                    per_tick: Mass::from_grams(50),
                },
                200,
                40,
                [79, 163, 240],
            )
            .with_power(q16(-24, 0)),
            simple(
                "gas_pipe",
                (1, 1),
                BuildingKind::Pipe,
                25,
                10,
                [111, 143, 176],
            ),
            simple(
                "gas_vent",
                (1, 1),
                BuildingKind::Vent,
                100,
                20,
                [159, 192, 224],
            ),
            simple(
                "temperature_sensor",
                (1, 1),
                BuildingKind::TemperatureSensor {
                    above: Temp::from_kelvin(300),
                },
                50,
                20,
                [224, 80, 80],
            ),
            simple(
                "pressure_sensor",
                (1, 1),
                BuildingKind::PressureSensor {
                    above: Mass::from_grams(1500),
                },
                50,
                20,
                [80, 160, 224],
            ),
            simple(
                "signal_wire",
                (1, 1),
                BuildingKind::SignalWire,
                10,
                10,
                [64, 224, 128],
            ),
            simple("and_gate", (1, 1), BuildingKind::And, 50, 20, [48, 192, 96]),
            simple("or_gate", (1, 1), BuildingKind::Or, 50, 20, [48, 160, 128]),
            simple("not_gate", (1, 1), BuildingKind::Not, 50, 20, [32, 128, 96]),
        ])
    }
}

/// A packet travelling through pipes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    pub element: ElementId,
    pub mass: Mass,
    pub temp: Temp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildingState {
    /// Placed, waiting for a colonist and its material.
    Planned,
    /// A colonist is working on it.
    UnderConstruction,
    Built,
}

impl BuildingState {
    pub fn tag(self) -> u8 {
        match self {
            BuildingState::Planned => 0,
            BuildingState::UnderConstruction => 1,
            BuildingState::Built => 2,
        }
    }

    pub fn from_tag(tag: u8) -> Option<BuildingState> {
        Some(match tag {
            0 => BuildingState::Planned,
            1 => BuildingState::UnderConstruction,
            2 => BuildingState::Built,
            _ => return None,
        })
    }
}

/// A placed building.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Building {
    pub def: u16,
    /// Bottom-left cell of the footprint.
    pub x: u32,
    pub y: u32,
    pub state: BuildingState,
    /// The material it was built from; returned on deconstruction.
    pub material: Option<(ElementId, Mass, Temp)>,
    /// Delivered contents (storage bins, generator feedstock).
    pub stored: Option<(ElementId, Mass, Temp)>,
    /// Battery charge, Q16.16 joules.
    pub battery: i64,
    pub enabled: bool,
    pub powered: bool,
    /// Automation output (sensors, gates) as of the last network step.
    pub signal: bool,
    /// The packet in this pipe cell, pump, or vent.
    pub packet: Option<Packet>,
    /// Could not act last tick (vent full, heater in vacuum, generator starved).
    pub blocked: bool,
    /// Operating ticks banked by colonists (hand cranks), spent by the power step.
    pub work: u16,
}

impl Building {
    pub fn new(def: u16, x: u32, y: u32) -> Building {
        Building {
            def,
            x,
            y,
            state: BuildingState::Planned,
            material: None,
            stored: None,
            battery: 0,
            enabled: true,
            powered: false,
            signal: false,
            packet: None,
            blocked: false,
            work: 0,
        }
    }

    /// The footprint cells in row-major order.
    pub fn footprint(&self, def: &BuildingDef) -> impl Iterator<Item = (u32, u32)> + '_ {
        let (w, h) = (def.size.0 as u32, def.size.1 as u32);
        let (x0, y0) = (self.x, self.y);
        (0..h).flat_map(move |dy| (0..w).map(move |dx| (x0 + dx, y0 + dy)))
    }

    pub fn contains(&self, def: &BuildingDef, x: u32, y: u32) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x + def.size.0 as u32
            && y < self.y + def.size.1 as u32
    }

    pub fn is_built(&self) -> bool {
        self.state == BuildingState::Built
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::builtin;

    fn table() -> BuildingTable {
        BuildingTable::new(vec![
            BuildingDef::new("ladder", (1, 1), BuildingKind::Ladder)
                .with_cost(builtin::SANDSTONE, Mass::from_kg(50)),
            BuildingDef::new(
                "heater",
                (1, 1),
                BuildingKind::Heater { joules_per_tick: 6 },
            )
            .with_power(-6),
        ])
    }

    #[test]
    fn table_lookups_and_the_append_only_rule() {
        let t = table();
        assert_eq!(t.by_name("heater"), Some(1));
        assert_eq!(t.by_name("pump"), None);
        assert!(t.get(1).consumes_power());
        let mut more = t.defs().to_vec();
        more.push(BuildingDef::new("wire", (1, 1), BuildingKind::Wire));
        let extended = BuildingTable::new(more.clone());
        assert!(extended.is_append_only_extension(&t));
        assert!(
            !t.is_append_only_extension(&extended),
            "shorter is not an extension"
        );
        more.swap(0, 1);
        assert!(!BuildingTable::new(more).is_append_only_extension(&t));
        assert_ne!(t.hash(), extended.hash());
        let mut changed = t.defs().to_vec();
        changed[1].kind = BuildingKind::Heater { joules_per_tick: 7 };
        assert_ne!(BuildingTable::new(changed).hash(), t.hash());
    }

    #[test]
    fn builtin_ids_name_their_entries() {
        let t = BuildingTable::builtin();
        assert_eq!(t.len(), 17);
        for (i, name) in [
            (ids::LADDER, "ladder"),
            (ids::DOOR, "door"),
            (ids::STORAGE_BIN, "storage_bin"),
            (ids::HEATER, "heater"),
            (ids::OXYGEN_GENERATOR, "oxygen_generator"),
            (ids::HAND_CRANK, "hand_crank"),
            (ids::BATTERY, "battery"),
            (ids::WIRE, "wire"),
            (ids::GAS_PUMP, "gas_pump"),
            (ids::GAS_PIPE, "gas_pipe"),
            (ids::GAS_VENT, "gas_vent"),
            (ids::TEMPERATURE_SENSOR, "temperature_sensor"),
            (ids::PRESSURE_SENSOR, "pressure_sensor"),
            (ids::SIGNAL_WIRE, "signal_wire"),
            (ids::AND_GATE, "and_gate"),
            (ids::OR_GATE, "or_gate"),
            (ids::NOT_GATE, "not_gate"),
        ] {
            assert_eq!(t.by_name(name), Some(i), "{name}");
        }
        assert!(t.get(ids::HEATER).consumes_power());
        assert!(!t.get(ids::HAND_CRANK).consumes_power());
    }

    #[test]
    #[should_panic(expected = "duplicate building")]
    fn duplicate_names_panic() {
        BuildingTable::new(vec![
            BuildingDef::new("a", (1, 1), BuildingKind::Wire),
            BuildingDef::new("a", (1, 1), BuildingKind::Pipe),
        ]);
    }

    #[test]
    fn footprints_are_row_major_from_the_anchor() {
        let def = BuildingDef::new("gen", (2, 2), BuildingKind::Wire);
        let b = Building::new(0, 5, 3);
        let cells: Vec<(u32, u32)> = b.footprint(&def).collect();
        assert_eq!(cells, vec![(5, 3), (6, 3), (5, 4), (6, 4)]);
        assert!(b.contains(&def, 6, 4));
        assert!(!b.contains(&def, 7, 4));
        assert_eq!(def.cells(), 4);
        assert_eq!(BuildingState::from_tag(2), Some(BuildingState::Built));
        assert_eq!(BuildingState::from_tag(3), None);
    }
}
