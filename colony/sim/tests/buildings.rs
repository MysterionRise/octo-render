//! Buildings end to end: placement, construction from debris, deconstruction, doors,
//! ladders, saves, replay, and hot reload.

use colony_sim::agent::DEFAULT_PRIORITY;
use colony_sim::building::ids;
use colony_sim::{
    builtin, save, scenario, BuildingDef, BuildingKind, BuildingState, BuildingTable, Command,
    ElementTable, Goal, JobKind, JobState, Mass, Sim,
};

/// Steps until `done` holds or `limit` ticks pass; returns the ticks taken.
fn run_until(sim: &mut Sim, limit: usize, mut done: impl FnMut(&Sim) -> bool) -> usize {
    let mut ticks = 0;
    while !done(sim) && ticks < limit {
        sim.step();
        ticks += 1;
    }
    ticks
}

/// The first boulder to the right of the party's first colonist.
fn first_boulder(sim: &Sim) -> (u32, u32) {
    let (cx, cy) = sim.colonists().iter().next().unwrap().1.cell;
    let x = (cx + 1..sim.nav().width() - 1)
        .find(|&x| sim.nav().is_solid(x, cy))
        .expect("a boulder");
    (x, cy)
}

/// Digs the first boulder so the party has a sandstone item.
fn dig_one_cell(sim: &mut Sim) -> (u32, u32) {
    let (x, y) = first_boulder(sim);
    sim.order(JobKind::Dig { x, y }, DEFAULT_PRIORITY);
    let ticks = run_until(sim, 600, |s| !s.items().is_empty());
    assert!(ticks < 600, "the dig finished");
    (x, y)
}

/// A free spot for a `w × h` footprint on the floor, at least two cells from any
/// colonist and boulder.
fn floor_spot(sim: &Sim, w: u8, h: u8) -> (usize, usize) {
    floor_spot_from(sim, 2, w, h)
}

fn floor_spot_from(sim: &Sim, from: u32, w: u8, h: u8) -> (usize, usize) {
    let nav = sim.nav();
    let colonists: Vec<(u32, u32)> = sim.colonists().iter().map(|(_, c)| c.cell).collect();
    (from..nav.width() - 2 - w as u32)
        .find(|&x| {
            (0..w as u32 + 2).all(|dx| {
                let cx = x + dx - 1;
                (0..h as u32 + 1).all(|dy| !nav.is_solid(cx, 2 + dy))
            }) && colonists
                .iter()
                .all(|&(cx, _)| cx + 2 < x || cx > x + w as u32 + 1)
        })
        .map(|x| (x as usize, 2))
        .expect("a free floor spot")
}

#[test]
fn placed_generator_is_built_from_a_sandstone_item_and_the_remainder_is_dropped() {
    let mut sim = scenario::yard(48, 16, 1, 2);
    let start_mass = sim.total_mass();
    dig_one_cell(&mut sim);
    let dug = sim.items().iter().next().unwrap().1.mass;
    assert_eq!(dug, Mass::from_kg(1800));
    let (x, y) = floor_spot(&sim, 2, 2);
    assert!(sim.placement_valid(ids::OXYGEN_GENERATOR, x, y));
    assert!(
        !sim.placement_valid(ids::OXYGEN_GENERATOR, x, 0),
        "the floor is solid"
    );
    let id = sim.place(ids::OXYGEN_GENERATOR, x, y).expect("placed");
    assert_eq!(sim.building_at(x + 1, y + 1), Some(id));
    assert!(
        !sim.placement_valid(ids::WIRE, x, y),
        "a footprint cell is occupied"
    );
    assert_eq!(sim.jobs().counts().open, 1, "a construct job is open");
    assert_eq!(
        sim.buildings().get(id).unwrap().state,
        BuildingState::Planned
    );
    let mut mass_ok = true;
    let ticks = run_until(&mut sim, 1500, |s| {
        mass_ok &= s.total_mass() == start_mass;
        s.buildings().get(id).unwrap().is_built()
    });
    assert!(ticks < 1500, "the generator was built");
    assert!(
        mass_ok,
        "cells + items + buildings stayed constant every tick"
    );
    let b = sim.buildings().get(id).unwrap();
    assert_eq!(
        b.material.map(|(e, m, _)| (e, m)),
        Some((builtin::SANDSTONE, Mass::from_kg(400)))
    );
    assert_eq!(sim.building_mass(), Mass::from_kg(400).raw() as u64);
    assert_eq!(
        sim.item_mass(),
        (Mass::from_kg(1800).raw() - Mass::from_kg(400).raw()) as u64,
        "the remainder of the item was dropped"
    );
    assert!(sim.items().iter().all(|(_, it)| it.on_ground()));
    assert_eq!(sim.jobs().counts().done, 2, "the dig and the construction");
    assert!(sim.colonists().iter().all(|(_, c)| c.carrying.is_none()));
    assert_eq!(sim.total_mass(), start_mass);

    // Deconstruction gives the material back as an item.
    assert!(sim.deconstruct(x, y));
    let ticks = run_until(&mut sim, 1500, |s| s.buildings().get(id).is_none());
    assert!(ticks < 1500, "the generator was taken apart");
    assert_eq!(sim.item_mass(), Mass::from_kg(1800).raw() as u64);
    assert!(sim
        .cell_building()
        .iter()
        .all(|b| *b == colony_sim::NO_BUILDING));
    assert_eq!(sim.building_at(x, y), None);
    assert!(sim.placement_valid(ids::WIRE, x, y));
    assert_eq!(sim.total_mass(), start_mass);
}

#[test]
fn a_planned_building_is_removed_at_once_and_its_job_cancelled() {
    let mut sim = scenario::yard(48, 16, 1, 2);
    let (x, y) = floor_spot(&sim, 1, 1);
    let id = sim.place(ids::WIRE, x, y).unwrap();
    assert_eq!(sim.jobs().live_for_building(id).count(), 1);
    assert!(sim.deconstruct(x, y));
    assert!(sim.buildings().get(id).is_none());
    assert_eq!(sim.jobs().live_for_building(id).count(), 0);
    assert_eq!(sim.jobs().counts().cancelled, 1);
    assert!(!sim.deconstruct(x, y), "nothing there any more");
    assert!(
        sim.place(ids::WIRE, x, y).is_some(),
        "the cell is free again"
    );
    assert!(sim.toggle_building(x, y));
    assert!(!sim.buildings().iter().next().unwrap().1.enabled);
}

#[test]
fn a_door_is_solid_to_gas_and_passable_to_colonists() {
    let mut sim = scenario::yard(48, 16, 1, 2);
    let start_mass = sim.total_mass();
    dig_one_cell(&mut sim);
    let (x, y) = floor_spot(&sim, 1, 2);
    let id = sim.place(ids::DOOR, x, y).unwrap();
    let ticks = run_until(&mut sim, 1500, |s| {
        s.buildings().get(id).unwrap().is_built()
    });
    assert!(ticks < 1500, "the door was built");
    sim.step();
    let (xu, yu) = (x as u32, y as u32);
    for dy in 0..2 {
        let (el, _, _) = sim.world().get(x, y + dy);
        assert_eq!(el, builtin::SANDSTONE, "door cells are solid material");
        assert!(sim.nav().is_solid(xu, yu + dy as u32));
        assert!(sim.nav().is_passable(xu, yu + dy as u32));
    }
    let door_mass: u64 = (0..2)
        .map(|dy| sim.world().get(x, y + dy).1.raw() as u64)
        .sum();
    assert_eq!(door_mass, Mass::from_kg(200).raw() as u64);
    assert!(
        sim.nav().is_standing(xu, yu),
        "colonists stand in the doorway"
    );
    let path = sim
        .nav_mut()
        .path_to((xu - 1, yu), Goal::Cell { x: xu + 1, y: yu })
        .expect("a path through the door");
    assert!(path.cells.contains(&(xu, yu)));
    assert_eq!(sim.total_mass(), start_mass);
    // Gas on one side cannot reach the other through the door cells.
    sim.set_cell(
        x - 1,
        y,
        builtin::OXYGEN,
        Mass::from_kg(3),
        colony_sim::Temp::from_kelvin(295),
    );
    let start_mass = sim.total_mass();
    for _ in 0..40 {
        sim.step();
    }
    for dy in 0..2 {
        assert_eq!(sim.world().get(x, y + dy).0, builtin::SANDSTONE);
    }
    assert_eq!(sim.total_mass(), start_mass);
    // Taking it apart returns the cells to vacuum and the material to an item.
    sim.deconstruct(x, y);
    let ticks = run_until(&mut sim, 1500, |s| s.buildings().get(id).is_none());
    assert!(ticks < 1500);
    for dy in 0..2 {
        assert_ne!(
            sim.world().get(x, y + dy).0.phase(),
            colony_sim::Phase::Solid
        );
    }
    assert_eq!(sim.item_mass(), Mass::from_kg(1800).raw() as u64);
    assert_eq!(sim.total_mass(), start_mass);
}

#[test]
fn colonists_climb_a_ladder_to_the_tower_top() {
    let mut sim = scenario::yard(64, 16, 1, 2);
    dig_one_cell(&mut sim);
    // The yard's tower: four cells tall, its left column free.
    let nav = sim.nav();
    let px = 64 - 6;
    let top = 2 + (2..14).take_while(|&y| nav.is_solid(px, y)).count() as u32;
    assert_eq!(top, 6);
    assert!((2..top).all(|y| !nav.is_solid(px - 1, y)));
    let party: Vec<(u32, u32)> = sim.colonists().iter().map(|(_, c)| c.cell).collect();
    assert!(
        sim.nav_mut()
            .path_to(party[0], Goal::Cell { x: px, y: top })
            .is_none(),
        "the tower top is unreachable without a ladder"
    );
    let ladders: Vec<_> = (2..top)
        .map(|y| {
            sim.place(ids::LADDER, px as usize - 1, y as usize)
                .expect("ladder placed")
        })
        .collect();
    let ticks = run_until(&mut sim, 4000, |s| {
        ladders
            .iter()
            .all(|id| s.buildings().get(*id).unwrap().is_built())
    });
    assert!(ticks < 4000, "every ladder was built");
    sim.step();
    for y in 2..top {
        assert!(sim.nav().is_ladder(px - 1, y));
        assert!(sim.nav().is_standing(px - 1, y));
    }
    let path = sim
        .nav_mut()
        .path_to(party[0], Goal::Cell { x: px, y: top })
        .expect("the ladder reaches the tower top");
    assert!(
        path.cells.contains(&(px - 1, top - 1)),
        "the path climbs the ladder"
    );
    // A colonist actually goes up and digs the top cell.
    sim.order(JobKind::Dig { x: px, y: top - 1 }, DEFAULT_PRIORITY);
    let ticks = run_until(&mut sim, 2000, |s| {
        s.world().get(px as usize, top as usize - 1).0.phase() != colony_sim::Phase::Solid
    });
    assert!(ticks < 2000, "the top of the tower was dug from the ladder");
}

#[test]
fn save_format_3_round_trips_buildings_and_continues_identically() {
    let mut sim = scenario::yard(64, 20, 2, 3);
    dig_one_cell(&mut sim);
    let (x, y) = floor_spot(&sim, 2, 2);
    let gen = sim.place(ids::OXYGEN_GENERATOR, x, y).unwrap();
    let (lx, ly) = floor_spot(&sim, 1, 1);
    sim.place(ids::LADDER, lx + 3, ly).unwrap();
    for _ in 0..120 {
        sim.step();
    }
    let bytes = save::encode(&sim);
    let mut resumed =
        save::decode(ElementTable::builtin(), BuildingTable::builtin(), &bytes).unwrap();
    assert_eq!(resumed.state_hash(), sim.state_hash());
    assert_eq!(resumed.buildings(), sim.buildings());
    assert_eq!(resumed.cell_building(), sim.cell_building());
    assert_eq!(resumed.special(), sim.special());
    assert_eq!(resumed.nav().flags(), sim.nav().flags());
    for _ in 0..200 {
        sim.step();
        resumed.step();
        assert_eq!(resumed.state_hash(), sim.state_hash());
    }
    assert!(sim.buildings().get(gen).unwrap().is_built());
    // Loading against a table that lost a name fails cleanly; an extended one works.
    let mut fewer = BuildingTable::builtin().defs().to_vec();
    fewer.retain(|d| d.name != "ladder");
    assert_eq!(
        save::decode(ElementTable::builtin(), BuildingTable::new(fewer), &bytes).unwrap_err(),
        save::SaveError::UnknownBuilding("ladder".to_string())
    );
    let mut more = BuildingTable::builtin().defs().to_vec();
    more.insert(0, BuildingDef::new("fan", (1, 1), BuildingKind::Wire));
    let remapped = save::decode(ElementTable::builtin(), BuildingTable::new(more), &bytes).unwrap();
    let def = remapped.buildings().get(gen).unwrap().def;
    assert_eq!(remapped.building_table().get(def).name, "oxygen_generator");
    // Older formats are rejected.
    let mut old = bytes.clone();
    old[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(
        save::decode(ElementTable::builtin(), BuildingTable::builtin(), &old).unwrap_err(),
        save::SaveError::UnsupportedVersion(2)
    );
}

#[test]
fn a_logged_run_with_placements_replays_to_the_same_hash() {
    let build = || scenario::yard(64, 20, 3, 3);
    let mut sim = build();
    let (px, py) = first_boulder(&sim);
    sim.queue(Command::Dig {
        x: px as usize,
        y: py as usize,
    });
    let (x, y) = floor_spot(&sim, 2, 2);
    sim.queue(Command::Place {
        def: ids::HAND_CRANK,
        x,
        y,
    });
    sim.queue(Command::Place {
        def: ids::WIRE,
        x: x + 2,
        y,
    });
    sim.queue(Command::Toggle { x: x + 2, y });
    for _ in 0..150 {
        sim.step();
    }
    sim.queue(Command::Deconstruct { x: x + 2, y });
    for _ in 0..150 {
        sim.step();
    }
    let log = sim.log().to_vec();
    assert_eq!(log.len(), 5);
    let mut replayed = build();
    replayed.replay(&log, sim.tick_count());
    assert_eq!(replayed.state_hash(), sim.state_hash());
    assert!(
        sim.buildings().iter().any(|(_, b)| b.is_built()),
        "the hand crank was built"
    );
    let decoded = colony_sim::command::decode_log(&colony_sim::command::encode_log(&log)).unwrap();
    assert_eq!(decoded, log);
}

#[test]
fn hot_reload_appends_a_building_and_rejects_a_reorder() {
    let mut sim = scenario::yard(48, 16, 1, 2);
    let (x, y) = floor_spot(&sim, 1, 1);
    let id = sim.place(ids::WIRE, x, y).unwrap();
    let mut more = BuildingTable::builtin().defs().to_vec();
    more.push(
        BuildingDef::new(
            "big_heater",
            (2, 1),
            BuildingKind::Heater { joules_per_tick: 1 },
        )
        .with_power(-1),
    );
    let extended = BuildingTable::new(more.clone());
    assert!(sim.set_building_table(extended.clone()).is_ok());
    assert_eq!(sim.building_table().len(), 18);
    assert_eq!(
        sim.building_table()
            .get(sim.buildings().get(id).unwrap().def)
            .name,
        "wire"
    );
    let (hx, hy) = floor_spot_from(&sim, x as u32 + 2, 2, 1);
    assert!(sim.placement_valid(17, hx, hy));
    more.swap(0, 1);
    assert!(sim.set_building_table(BuildingTable::new(more)).is_err());
    assert!(
        sim.set_building_table(BuildingTable::builtin()).is_err(),
        "shorter"
    );
    assert_eq!(sim.building_table(), &extended);
    assert_eq!(
        sim.jobs().get(0).map(|j| j.state),
        Some(JobState::Open),
        "the wire's construct job is untouched"
    );
}
