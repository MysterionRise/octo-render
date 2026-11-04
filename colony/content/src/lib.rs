//! `colony-content`: content packs for the colony engine (design document, section 7).
//!
//! A pack is a directory with `elements.ron` and `buildings.ron` in human units. This
//! crate parses them with `serde`, validates every cross-reference, converts the values
//! to the kernel's fixed-point units, and hands back the tables `colony-sim` consumes.
//! The base pack is embedded so a game runs without a content directory, and a test
//! pins its elements to `ElementTable::builtin()` so saves stay valid either way.

pub mod convert;
pub mod load;
pub mod spec;

pub use load::{
    buildings_from_specs, elements_from_specs, load_pack, parse_buildings, parse_elements,
    reload_compatible, ContentError, Pack,
};
pub use spec::{BuildingSpec, ElementSpec, KindSpec, State};

/// The embedded base pack.
pub const BASE_ELEMENTS: &str = include_str!("../base/elements.ron");
pub const BASE_BUILDINGS: &str = include_str!("../base/buildings.ron");

/// The base pack, parsed from the embedded text.
pub fn base() -> Pack {
    let elements = parse_elements(BASE_ELEMENTS).expect("the embedded base elements parse");
    let buildings =
        parse_buildings(BASE_BUILDINGS, &elements).expect("the embedded base buildings parse");
    Pack {
        elements,
        buildings,
    }
}

/// The directory of the base pack in a source checkout, if it exists next to the
/// workspace (`colony/content/base`), for hot reload during development.
pub fn base_dir() -> Option<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("base");
    dir.join("buildings.ron").is_file().then_some(dir)
}
