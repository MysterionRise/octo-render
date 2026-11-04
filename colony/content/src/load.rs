//! Parsing, validation, and table building. Every cross-reference is checked here so a
//! bad pack is an error with a message, never a kernel panic.

use std::fmt;
use std::path::Path;

use colony_sim::{
    BuildingDef, BuildingKind, BuildingTable, ElementDef, ElementId, ElementTable, Phase,
};

use crate::convert;
use crate::spec::{BuildingSpec, ElementSpec, KindSpec, State};

#[derive(Debug, Clone, PartialEq)]
pub enum ContentError {
    /// A file could not be read.
    Io(String),
    /// RON syntax or type error, with the file.
    Parse {
        file: String,
        message: String,
    },
    DuplicateId(String),
    /// `subject` names `name`, which is not an element.
    UnknownElement {
        subject: String,
        name: String,
    },
    /// `subject` names `name`, which has the wrong phase.
    WrongPhase {
        subject: String,
        name: String,
        wanted: Phase,
    },
    /// Any other rule violation.
    Invalid(String),
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContentError::Io(e) => write!(f, "{e}"),
            ContentError::Parse { file, message } => write!(f, "{file}: {message}"),
            ContentError::DuplicateId(id) => write!(f, "duplicate id {id:?}"),
            ContentError::UnknownElement { subject, name } => {
                write!(f, "{subject} names unknown element {name:?}")
            }
            ContentError::WrongPhase {
                subject,
                name,
                wanted,
            } => write!(f, "{subject} names {name:?}, which is not a {wanted:?}"),
            ContentError::Invalid(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ContentError {}

/// A parsed and validated pack: both tables, ready for the kernel.
#[derive(Clone, Debug)]
pub struct Pack {
    pub elements: ElementTable,
    pub buildings: BuildingTable,
}

fn phase_of(state: State) -> Phase {
    match state {
        State::Vacuum => Phase::Vacuum,
        State::Gas => Phase::Gas,
        State::Liquid => Phase::Liquid,
        State::Solid => Phase::Solid,
    }
}

fn check_unique(ids: impl Iterator<Item = String>) -> Result<(), ContentError> {
    let mut seen: Vec<String> = Vec::new();
    for id in ids {
        if seen.contains(&id) {
            return Err(ContentError::DuplicateId(id));
        }
        seen.push(id);
    }
    Ok(())
}

fn finite_non_negative(subject: &str, field: &str, v: f64) -> Result<(), ContentError> {
    if !v.is_finite() || v < 0.0 {
        return Err(ContentError::Invalid(format!(
            "{subject}: {field} must be finite and non-negative, got {v}"
        )));
    }
    Ok(())
}

/// Parses the RON text of `elements.ron` and builds the element table.
pub fn parse_elements(text: &str) -> Result<ElementTable, ContentError> {
    let specs: Vec<ElementSpec> = ron::from_str(text).map_err(|e| ContentError::Parse {
        file: "elements.ron".to_string(),
        message: e.to_string(),
    })?;
    elements_from_specs(&specs)
}

/// Builds the element table from parsed records.
pub fn elements_from_specs(specs: &[ElementSpec]) -> Result<ElementTable, ContentError> {
    if specs.is_empty() {
        return Err(ContentError::Invalid("no elements".to_string()));
    }
    check_unique(specs.iter().map(|s| s.id.clone()))?;
    if specs[0].state != State::Vacuum {
        return Err(ContentError::Invalid(
            "the first element must be the vacuum".to_string(),
        ));
    }
    if specs[1..].iter().any(|s| s.state == State::Vacuum) {
        return Err(ContentError::Invalid(
            "only the first element may be a vacuum".to_string(),
        ));
    }
    if specs.len() > 1 << 14 {
        return Err(ContentError::Invalid(
            "more than 16384 elements".to_string(),
        ));
    }
    let ids: Vec<ElementId> = specs
        .iter()
        .enumerate()
        .map(|(i, s)| ElementId::new(phase_of(s.state), i as u16))
        .collect();
    let lookup = |subject: &str, name: &str, wanted: Phase| -> Result<ElementId, ContentError> {
        let i = specs.iter().position(|s| s.id == name).ok_or_else(|| {
            ContentError::UnknownElement {
                subject: subject.to_string(),
                name: name.to_string(),
            }
        })?;
        if ids[i].phase() != wanted {
            return Err(ContentError::WrongPhase {
                subject: subject.to_string(),
                name: name.to_string(),
                wanted,
            });
        }
        Ok(ids[i])
    };
    let mut defs = Vec::with_capacity(specs.len());
    for (i, s) in specs.iter().enumerate() {
        let subject = format!("element {:?}", s.id);
        finite_non_negative(&subject, "shc_kj_per_kg_k", s.shc_kj_per_kg_k)?;
        finite_non_negative(&subject, "conductivity_w_per_m_k", s.conductivity_w_per_m_k)?;
        finite_non_negative(&subject, "density_kg_per_m3", s.density_kg_per_m3)?;
        if i == 0 && (s.shc_kj_per_kg_k != 0.0 || s.conductivity_w_per_m_k != 0.0) {
            return Err(ContentError::Invalid(
                "the vacuum must have zero heat capacity and conductivity".to_string(),
            ));
        }
        let colour = convert::colour(&s.colour).ok_or_else(|| {
            ContentError::Invalid(format!("{subject}: colour {:?} is not #rrggbb", s.colour))
        })?;
        let mut def = ElementDef::new(
            &s.id,
            phase_of(s.state),
            convert::shc(s.shc_kj_per_kg_k),
            convert::conductivity(s.conductivity_w_per_m_k),
            convert::density(s.density_kg_per_m3),
            colour,
        );
        if let Some(kg) = s.max_mass_kg {
            finite_non_negative(&subject, "max_mass_kg", kg)?;
            def = def.with_max_mass(convert::mass_kg(kg).raw());
        }
        let links = s.solid.is_some() || s.liquid.is_some() || s.gas.is_some();
        let thresholds = s.freeze_k.is_some() || s.boil_k.is_some();
        if links || thresholds {
            if let (Some(f), Some(b)) = (s.freeze_k, s.boil_k) {
                if !(f < b) {
                    return Err(ContentError::Invalid(format!(
                        "{subject}: freeze_k must be below boil_k"
                    )));
                }
            }
            let solid = s
                .solid
                .as_deref()
                .map(|n| lookup(&subject, n, Phase::Solid))
                .transpose()?;
            let liquid = s
                .liquid
                .as_deref()
                .map(|n| lookup(&subject, n, Phase::Liquid))
                .transpose()?;
            let gas = s
                .gas
                .as_deref()
                .map(|n| lookup(&subject, n, Phase::Gas))
                .transpose()?;
            def = def.with_phases(
                s.freeze_k.map(|k| convert::kelvin(k).raw()),
                s.boil_k.map(|k| convert::kelvin(k).raw()),
                solid,
                liquid,
                gas,
            );
        }
        defs.push(def);
    }
    Ok(ElementTable::new(defs))
}

/// Parses the RON text of `buildings.ron` against an element table.
pub fn parse_buildings(text: &str, elements: &ElementTable) -> Result<BuildingTable, ContentError> {
    let specs: Vec<BuildingSpec> = ron::from_str(text).map_err(|e| ContentError::Parse {
        file: "buildings.ron".to_string(),
        message: e.to_string(),
    })?;
    buildings_from_specs(&specs, elements)
}

/// Builds the building table from parsed records.
pub fn buildings_from_specs(
    specs: &[BuildingSpec],
    elements: &ElementTable,
) -> Result<BuildingTable, ContentError> {
    check_unique(specs.iter().map(|s| s.id.clone()))?;
    if specs.len() >= u16::MAX as usize {
        return Err(ContentError::Invalid(
            "more than 65534 buildings".to_string(),
        ));
    }
    let element =
        |subject: &str, name: &str, wanted: Option<Phase>| -> Result<ElementId, ContentError> {
            let id = elements
                .id_by_name(name)
                .ok_or_else(|| ContentError::UnknownElement {
                    subject: subject.to_string(),
                    name: name.to_string(),
                })?;
            if let Some(wanted) = wanted {
                if id.phase() != wanted {
                    return Err(ContentError::WrongPhase {
                        subject: subject.to_string(),
                        name: name.to_string(),
                        wanted,
                    });
                }
            }
            Ok(id)
        };
    let mut defs = Vec::with_capacity(specs.len());
    for s in specs {
        let subject = format!("building {:?}", s.id);
        if s.size.0 == 0 || s.size.1 == 0 {
            return Err(ContentError::Invalid(format!(
                "{subject}: size must be at least 1x1"
            )));
        }
        if !s.power_w.is_finite() {
            return Err(ContentError::Invalid(format!(
                "{subject}: power_w must be finite"
            )));
        }
        let kind = match &s.kind {
            KindSpec::Ladder => BuildingKind::Ladder,
            KindSpec::Door => BuildingKind::Door,
            KindSpec::Storage { capacity_kg } => {
                finite_non_negative(&subject, "capacity_kg", *capacity_kg)?;
                BuildingKind::Storage {
                    capacity: convert::mass_kg(*capacity_kg),
                }
            }
            KindSpec::Heater { watts } => {
                finite_non_negative(&subject, "watts", *watts)?;
                BuildingKind::Heater {
                    joules_per_tick: convert::watts(*watts),
                }
            }
            KindSpec::OxygenGenerator {
                feedstock,
                feedstock_kg_per_s,
                output,
                heat_w,
            } => {
                finite_non_negative(&subject, "feedstock_kg_per_s", *feedstock_kg_per_s)?;
                finite_non_negative(&subject, "heat_w", *heat_w)?;
                BuildingKind::OxygenGenerator {
                    feedstock: element(&subject, feedstock, None)?,
                    feed_per_tick: convert::mass_per_tick(*feedstock_kg_per_s),
                    output: element(&subject, output, Some(Phase::Gas))?,
                    joules_per_tick: convert::watts(*heat_w),
                }
            }
            KindSpec::HandCrank { watts } => {
                finite_non_negative(&subject, "watts", *watts)?;
                BuildingKind::HandCrank {
                    joules_per_tick: convert::watts(*watts),
                }
            }
            KindSpec::Battery { capacity_kj } => {
                finite_non_negative(&subject, "capacity_kj", *capacity_kj)?;
                BuildingKind::Battery {
                    capacity: convert::kilojoules(*capacity_kj),
                }
            }
            KindSpec::Wire => BuildingKind::Wire,
            KindSpec::GasPump { kg_per_s } => {
                finite_non_negative(&subject, "kg_per_s", *kg_per_s)?;
                BuildingKind::GasPump {
                    per_tick: convert::mass_per_tick(*kg_per_s),
                }
            }
            KindSpec::Pipe => BuildingKind::Pipe,
            KindSpec::Vent => BuildingKind::Vent,
            KindSpec::TemperatureSensor { above_k } => {
                if !above_k.is_finite() {
                    return Err(ContentError::Invalid(format!(
                        "{subject}: above_k must be finite"
                    )));
                }
                BuildingKind::TemperatureSensor {
                    above: convert::kelvin(*above_k),
                }
            }
            KindSpec::PressureSensor { above_kg } => {
                finite_non_negative(&subject, "above_kg", *above_kg)?;
                BuildingKind::PressureSensor {
                    above: convert::mass_kg(*above_kg),
                }
            }
            KindSpec::SignalWire => BuildingKind::SignalWire,
            KindSpec::And => BuildingKind::And,
            KindSpec::Or => BuildingKind::Or,
            KindSpec::Not => BuildingKind::Not,
        };
        let colour = convert::colour(&s.colour).ok_or_else(|| {
            ContentError::Invalid(format!("{subject}: colour {:?} is not #rrggbb", s.colour))
        })?;
        let mut def = BuildingDef::new(&s.id, s.size, kind)
            .with_build_ticks(s.build_ticks)
            .with_colour(colour)
            .with_power(convert::watts(s.power_w));
        if let Some((name, kg)) = &s.cost {
            finite_non_negative(&subject, "cost", *kg)?;
            def = def.with_cost(
                element(&subject, name, Some(Phase::Solid))?,
                convert::mass_kg(*kg),
            );
        }
        defs.push(def);
    }
    Ok(BuildingTable::new(defs))
}

/// The hot-reload rule: every existing building keeps its index and name.
pub fn reload_compatible(old: &BuildingTable, new: &BuildingTable) -> Result<(), ContentError> {
    if new.is_append_only_extension(old) {
        Ok(())
    } else {
        Err(ContentError::Invalid(
            "a reload may only append buildings; existing entries must keep their order and names"
                .to_string(),
        ))
    }
}

/// Loads `elements.ron` and `buildings.ron` from a pack directory.
pub fn load_pack(dir: &Path) -> Result<Pack, ContentError> {
    let read = |name: &str| -> Result<String, ContentError> {
        let path = dir.join(name);
        std::fs::read_to_string(&path)
            .map_err(|e| ContentError::Io(format!("{}: {e}", path.display())))
    };
    let elements = parse_elements(&read("elements.ron")?)?;
    let buildings = parse_buildings(&read("buildings.ron")?, &elements)?;
    Ok(Pack {
        elements,
        buildings,
    })
}
