//! The base pack loads, its elements are exactly the builtin table, and bad packs are
//! errors with messages.

use colony_content::{
    base, base_dir, load_pack, parse_buildings, parse_elements, reload_compatible, ContentError,
    BASE_BUILDINGS, BASE_ELEMENTS,
};
use colony_sim::{builtin, BuildingKind, ElementTable, Mass, Phase};

#[test]
fn base_pack_loads_and_elements_equal_builtin() {
    let pack = base();
    let builtin_table = ElementTable::builtin();
    assert_eq!(pack.elements.defs(), builtin_table.defs());
    for a in [Phase::Vacuum, Phase::Gas, Phase::Liquid, Phase::Solid] {
        for b in [Phase::Vacuum, Phase::Gas, Phase::Liquid, Phase::Solid] {
            assert_eq!(pack.elements.pair_mult(a, b), builtin_table.pair_mult(a, b));
        }
    }
    assert_eq!(
        colony_sim::save::table_hash(&pack.elements),
        colony_sim::save::table_hash(&builtin_table),
        "saves made with the builtin table load with the base pack"
    );
    let b = &pack.buildings;
    assert_eq!(b.len(), 17);
    assert_eq!(b.by_name("ladder"), Some(0));
    let gen = b.get(b.by_name("oxygen_generator").unwrap());
    assert_eq!(gen.size, (2, 2));
    assert_eq!(gen.cost, Some((builtin::SANDSTONE, Mass::from_kg(400))));
    match &gen.kind {
        BuildingKind::OxygenGenerator {
            feedstock,
            feed_per_tick,
            output,
            joules_per_tick,
        } => {
            assert_eq!(*feedstock, builtin::ICE);
            assert_eq!(*feed_per_tick, Mass::from_grams(50));
            assert_eq!(*output, builtin::OXYGEN);
            assert_eq!(*joules_per_tick, colony_sim::fixed::q16(25, 0));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(gen.power_per_tick, colony_sim::fixed::q16(-12, 0));
    assert!(b.get(b.by_name("wire").unwrap()).kind.is_wire());
    assert!(b.get(b.by_name("gas_vent").unwrap()).kind.is_pipe_cell());
    assert!(b.get(b.by_name("not_gate").unwrap()).kind.is_gate());
    if let Some(dir) = base_dir() {
        let from_disk = load_pack(&dir).unwrap();
        assert_eq!(from_disk.buildings, pack.buildings);
        assert_eq!(from_disk.elements.defs(), pack.elements.defs());
    }
}

#[test]
fn unknown_or_wrong_phase_references_are_errors() {
    let elements = parse_elements(BASE_ELEMENTS).unwrap();
    let bad_cost = BASE_BUILDINGS.replacen("(\"sandstone\", 50.0)", "(\"granite\", 50.0)", 1);
    assert_eq!(
        parse_buildings(&bad_cost, &elements).unwrap_err(),
        ContentError::UnknownElement {
            subject: "building \"ladder\"".to_string(),
            name: "granite".to_string()
        }
    );
    let gas_cost = BASE_BUILDINGS.replacen("(\"sandstone\", 50.0)", "(\"oxygen\", 50.0)", 1);
    assert!(matches!(
        parse_buildings(&gas_cost, &elements).unwrap_err(),
        ContentError::WrongPhase {
            wanted: Phase::Solid,
            ..
        }
    ));
    let bad_output = BASE_BUILDINGS.replacen("output: \"oxygen\"", "output: \"water\"", 1);
    assert!(matches!(
        parse_buildings(&bad_output, &elements).unwrap_err(),
        ContentError::WrongPhase {
            wanted: Phase::Gas,
            ..
        }
    ));
    let dup = BASE_BUILDINGS.replacen("id: \"door\"", "id: \"ladder\"", 1);
    assert_eq!(
        parse_buildings(&dup, &elements).unwrap_err(),
        ContentError::DuplicateId("ladder".to_string())
    );
    let flat = BASE_BUILDINGS.replacen("size: (1, 2)", "size: (1, 0)", 1);
    assert!(parse_buildings(&flat, &elements)
        .unwrap_err()
        .to_string()
        .contains("at least 1x1"));
    let syntax = parse_buildings("[(id: \"x\"", &elements).unwrap_err();
    assert!(matches!(syntax, ContentError::Parse { .. }));
    assert!(syntax.to_string().starts_with("buildings.ron:"));
}

#[test]
fn element_rules_are_enforced() {
    let no_vacuum = BASE_ELEMENTS.replacen("state: Vacuum", "state: Gas", 1);
    assert!(parse_elements(&no_vacuum)
        .unwrap_err()
        .to_string()
        .contains("vacuum"));
    let bad_link = BASE_ELEMENTS.replacen("solid: Some(\"ice\")", "solid: Some(\"water\")", 1);
    assert!(matches!(
        parse_elements(&bad_link).unwrap_err(),
        ContentError::WrongPhase {
            wanted: Phase::Solid,
            ..
        }
    ));
    let inverted = BASE_ELEMENTS.replacen("boil_k: Some(373.15)", "boil_k: Some(73.15)", 1);
    assert!(parse_elements(&inverted)
        .unwrap_err()
        .to_string()
        .contains("freeze_k"));
    let colour = BASE_ELEMENTS.replacen("\"#78c8ff\"", "\"blue\"", 1);
    assert!(parse_elements(&colour)
        .unwrap_err()
        .to_string()
        .contains("#rrggbb"));
}

#[test]
fn reload_keeps_indices_and_rejects_reorder() {
    let elements = parse_elements(BASE_ELEMENTS).unwrap();
    let old = parse_buildings(BASE_BUILDINGS, &elements).unwrap();
    let appended = format!(
        "{}\n",
        BASE_BUILDINGS.trim_end().trim_end_matches(']')
    ) + "    (id: \"big_heater\", size: (2, 1), kind: Heater(watts: 2000.0), colour: \"#ff0000\", power_w: -200.0),\n]\n";
    let new = parse_buildings(&appended, &elements).unwrap();
    assert!(reload_compatible(&old, &new).is_ok());
    assert_eq!(new.by_name("big_heater"), Some(17));
    assert_eq!(new.get(17).build_ticks, 20, "the default");
    for (i, d) in old.defs().iter().enumerate() {
        assert_eq!(new.get(i as u16), d);
    }
    let reordered = BASE_BUILDINGS
        .replacen("id: \"ladder\"", "id: \"door_\"", 1)
        .replacen("id: \"door\"", "id: \"ladder\"", 1)
        .replacen("id: \"door_\"", "id: \"door\"", 1);
    let new = parse_buildings(&reordered, &elements).unwrap();
    assert!(reload_compatible(&old, &new).is_err());
    let shorter = parse_buildings("[]", &elements).unwrap();
    assert!(reload_compatible(&old, &shorter).is_err());
    assert!(
        reload_compatible(&shorter, &old).is_ok(),
        "growing from nothing is fine"
    );
}
