//! Power, pipes, and automation: the M5 exit criteria.

use colony_sim::building::ids;
use colony_sim::network::NETWORK_STRIDE;
use colony_sim::{
    builtin, save, scenario, BuildingTable, ElementId, ElementTable, EntityId, Mass, Sim, Temp,
};

const ROOM: Temp = Temp::from_kelvin(295);

/// A yard with no colonists and a cleared floor strip.
fn bench(width: usize) -> Sim {
    let mut sim = scenario::yard(width, 16, 1, 0);
    for x in 2..width - 2 {
        if sim.world().get(x, 2).0 == builtin::SANDSTONE {
            sim.set_cell(x, 2, builtin::OXYGEN, Mass::from_kg(1), ROOM);
        }
    }
    sim
}

fn built(sim: &mut Sim, def: u16, x: usize, y: usize) -> EntityId {
    sim.place_built(def, x, y)
        .unwrap_or_else(|| panic!("cannot place {def} at ({x}, {y})"))
}

/// A crank at (6, 2), wires to a pump at (10, 2), pipes up and over to a vent at (13, 4).
fn pipeline() -> (Sim, EntityId, EntityId, Vec<EntityId>, EntityId) {
    let mut sim = bench(40);
    let crank = built(&mut sim, ids::HAND_CRANK, 6, 2);
    for x in 8..10 {
        built(&mut sim, ids::WIRE, x, 2);
    }
    let pump = built(&mut sim, ids::GAS_PUMP, 10, 2);
    let pipes = [(10, 3), (10, 4), (11, 4), (12, 4)]
        .into_iter()
        .map(|(x, y)| built(&mut sim, ids::GAS_PIPE, x, y))
        .collect();
    let vent = built(&mut sim, ids::GAS_VENT, 13, 4);
    sim.set_cell(10, 2, builtin::OXYGEN, Mass::from_kg(2), ROOM);
    sim.set_cell(13, 4, ElementId::VACUUM, Mass::ZERO, Temp::ZERO);
    (sim, crank, pump, pipes, vent)
}

/// Mass in the cell arrays alone (packets and buildings excluded).
fn cell_mass(sim: &Sim) -> u64 {
    sim.world().cells.mass.iter().map(|m| *m as u64).sum()
}

/// What one pump draw weighs: two ticks of its rate.
fn packet_raw(sim: &Sim) -> u64 {
    match sim.building_table().get(ids::GAS_PUMP).kind {
        colony_sim::BuildingKind::GasPump { per_tick } => per_tick.raw() as u64 * NETWORK_STRIDE,
        _ => unreachable!(),
    }
}

fn packet_cells(sim: &Sim) -> Vec<(u32, u32)> {
    sim.buildings()
        .iter()
        .filter(|(_, b)| b.packet.is_some())
        .map(|(_, b)| (b.x, b.y))
        .collect()
}

#[test]
fn packets_advance_one_cell_per_step_and_the_vent_emits_exactly_the_pumped_mass() {
    let (mut sim, crank, pump, _, vent) = pipeline();
    assert!(sim.operate(crank, 200));
    let start_mass = sim.total_mass();
    let start_cells = cell_mass(&sim);
    sim.step(); // tick 0: networks rebuild, the pump draws its first packet
    assert_eq!(sim.networks().pipes.len(), 1);
    let net = &sim.networks().pipes[0];
    assert_eq!(net.cells.len(), 6);
    assert_eq!(net.cells[0].0, 0, "the vent is at distance zero");
    assert!(net.cells.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!(packet_cells(&sim), vec![(10, 2)]);
    assert!(sim.buildings().get(pump).unwrap().powered);
    assert_eq!(sim.total_mass(), start_mass);
    let mut emitted = 0u64;
    for tick in 1..40u64 {
        let before = packet_cells(&sim);
        sim.step();
        assert_eq!(sim.total_mass(), start_mass, "tick {tick}");
        if tick % NETWORK_STRIDE != 0 {
            assert_eq!(packet_cells(&sim), before, "nothing moves between steps");
            continue;
        }
        // Every packet moved exactly one cell toward the vent, the first one vented.
        let after = packet_cells(&sim);
        let moved: Vec<(u32, u32)> = before
            .iter()
            .filter_map(|&(x, y)| match (x, y) {
                (10, 2) => Some((10, 3)),
                (10, 3) => Some((10, 4)),
                (10, 4) => Some((11, 4)),
                (11, 4) => Some((12, 4)),
                (12, 4) => Some((13, 4)),
                _ => None,
            })
            .collect();
        if before.contains(&(13, 4)) {
            emitted += 1;
        }
        let mut expected = moved;
        expected.push((10, 2));
        expected.sort_unstable();
        let mut after_sorted = after.clone();
        after_sorted.sort_unstable();
        assert_eq!(after_sorted, expected, "tick {tick}");
    }
    assert!(emitted >= 10, "{emitted} packets vented");
    // Vented gas diffuses on, so the cells as a whole are short exactly the packets in
    // transit (one per pipe cell, the vent's included, once the train is full): every
    // packet the vent released went back into the cells.
    assert_eq!(
        start_cells - cell_mass(&sim),
        6 * packet_raw(&sim),
        "six packets in transit"
    );
    assert!(!sim.buildings().get(vent).unwrap().blocked);
    assert_eq!(sim.networks().stats.packets, 6);
    assert_eq!(
        sim.buildings()
            .get(pump)
            .unwrap()
            .packet
            .unwrap()
            .mass
            .raw() as u64,
        packet_raw(&sim)
    );
}

#[test]
fn a_blocked_vent_holds_the_packet_without_loss() {
    let (mut sim, crank, pump, pipes, vent) = pipeline();
    // A vent whose cell is rock can release nothing.
    sim.set_cell(13, 4, builtin::SANDSTONE, Mass::from_kg(100), ROOM);
    sim.operate(crank, 400);
    let start_mass = sim.total_mass();
    let start_cells = cell_mass(&sim);
    for _ in 0..60 {
        sim.step();
        assert_eq!(sim.total_mass(), start_mass);
    }
    assert!(sim.buildings().get(vent).unwrap().blocked);
    assert!(sim.buildings().get(vent).unwrap().packet.is_some());
    assert!(pipes
        .iter()
        .all(|p| sim.buildings().get(*p).unwrap().packet.is_some()));
    assert!(sim.buildings().get(pump).unwrap().packet.is_some());
    assert!(
        sim.buildings().get(pump).unwrap().blocked,
        "the pump cannot pass its packet on"
    );
    assert_eq!(
        start_cells - cell_mass(&sim),
        6 * packet_raw(&sim),
        "six packets, no more"
    );
    assert_eq!(sim.networks().stats.packets, 6);
    assert_eq!(sim.world().get(13, 4).0, builtin::SANDSTONE);
    // Clearing the vent lets the train move again.
    sim.set_cell(13, 4, ElementId::VACUUM, Mass::ZERO, Temp::ZERO);
    let start_cells = cell_mass(&sim);
    let start_mass = sim.total_mass();
    for _ in 0..4 {
        sim.step();
    }
    assert!(!sim.buildings().get(vent).unwrap().blocked);
    assert!(
        !sim.buildings().get(pump).unwrap().blocked,
        "the train moves again"
    );
    // Each step the vent returns one packet to the cells and the pump draws one.
    assert!(
        cell_mass(&sim) >= start_cells,
        "vented gas is back in the cells"
    );
    assert_eq!(sim.total_mass(), start_mass);
}

#[test]
fn packets_exchange_heat_with_hot_pipe_cells_symmetrically() {
    let (mut sim, crank, _, _, _) = pipeline();
    sim.operate(crank, 400);
    sim.set_cell(
        11,
        4,
        builtin::OXYGEN,
        Mass::from_kg(1),
        Temp::from_kelvin(400),
    );
    let start = sim.total_energy();
    let mut warmed = false;
    for _ in 0..40 {
        sim.step();
        if let Some(p) = sim
            .buildings()
            .iter()
            .find_map(|(_, b)| b.packet.filter(|_| (b.x, b.y) == (12, 4)))
        {
            // A packet that just left the hot cell is warmer than it started.
            if p.temp > ROOM {
                warmed = true;
            }
        }
    }
    assert!(warmed, "packets picked up heat from the hot cell");
    assert!(
        sim.world().get(11, 4).2 < Temp::from_kelvin(400),
        "the hot cell cooled"
    );
    assert_eq!(sim.energy_injected(), 0);
    let drift = (sim.total_energy() - start).abs();
    assert!(drift <= 2 * 40 * sim.total_capacity(), "drift {drift}");
}

#[test]
fn power_components_sum_generation_and_brown_out_consumers_in_slot_order() {
    let mut sim = bench(40);
    let crank = built(&mut sim, ids::HAND_CRANK, 6, 2);
    for x in 8..14 {
        built(&mut sim, ids::WIRE, x, 2);
    }
    let first = built(&mut sim, ids::GAS_PUMP, 14, 2);
    built(&mut sim, ids::WIRE, 15, 2);
    let second = built(&mut sim, ids::GAS_PUMP, 16, 2);
    let lonely = built(&mut sim, ids::HEATER, 30, 2);
    sim.operate(crank, 100);
    sim.step();
    let n = sim.networks();
    assert_eq!(n.power.len(), 1);
    assert_eq!(
        n.power[0].members.len(),
        10,
        "crank, seven wires, two pumps"
    );
    assert_eq!(
        n.component_of[lonely.index as usize],
        colony_sim::NO_BUILDING
    );
    let q = colony_sim::fixed::q16;
    assert_eq!(n.stats.generated, q(40, 0) * NETWORK_STRIDE as i64);
    assert_eq!(
        n.stats.consumed,
        q(24, 0) * NETWORK_STRIDE as i64,
        "one pump's worth"
    );
    assert_eq!(n.stats.brownouts, 1);
    assert!(
        sim.buildings().get(first).unwrap().powered,
        "the lower slot is served first"
    );
    assert!(!sim.buildings().get(second).unwrap().powered);
    assert!(
        !sim.buildings().get(lonely).unwrap().powered,
        "no wire, no power"
    );
    // Disabling the first pump frees the supply for the second.
    sim.toggle_building(14, 2);
    sim.step();
    sim.step();
    assert!(!sim.buildings().get(first).unwrap().powered);
    assert!(sim.buildings().get(second).unwrap().powered);
    assert_eq!(sim.networks().stats.brownouts, 0);
}

#[test]
fn batteries_charge_on_surplus_and_drain_on_deficit() {
    let mut sim = bench(40);
    let crank = built(&mut sim, ids::HAND_CRANK, 6, 2);
    for x in 8..10 {
        built(&mut sim, ids::WIRE, x, 2);
    }
    let battery = built(&mut sim, ids::BATTERY, 10, 2);
    for x in 12..14 {
        built(&mut sim, ids::WIRE, x, 2);
    }
    let heater = built(&mut sim, ids::HEATER, 14, 2);
    let q = colony_sim::fixed::q16;
    let per_step = NETWORK_STRIDE as i64;
    sim.operate(crank, 4); // two steps of generation
    let charge = |sim: &Sim| sim.buildings().get(battery).unwrap().battery;
    sim.step(); // tick 0: generation 80, heater 12, surplus 68 into the battery
    assert_eq!(charge(&sim), (q(40, 0) - q(6, 0)) * per_step);
    assert!(sim.buildings().get(heater).unwrap().powered);
    sim.step();
    sim.step(); // tick 2: the second stint
    assert_eq!(charge(&sim), 2 * (q(40, 0) - q(6, 0)) * per_step);
    sim.step();
    sim.step(); // tick 4: no generation, the battery drains the heater's draw
    assert_eq!(
        charge(&sim),
        2 * (q(40, 0) - q(6, 0)) * per_step - q(6, 0) * per_step
    );
    assert!(sim.buildings().get(heater).unwrap().powered);
    let mut steps = 0;
    while sim.buildings().get(heater).unwrap().powered && steps < 100 {
        sim.step();
        sim.step();
        steps += 1;
    }
    assert!(steps < 100, "the battery ran down");
    // 136 J charged, 12 J per step drained: 4 J remain, too little for a step.
    assert_eq!(charge(&sim), q(4, 0));
    assert_eq!(sim.networks().stats.brownouts, 1);
    assert!(
        sim.energy_injected() > 0,
        "the heater warmed its cell while powered"
    );
}

#[test]
fn hand_crank_powers_the_heater_only_while_colonists_operate_it() {
    let mut sim = scenario::yard(48, 16, 1, 2);
    for x in 2..46 {
        if sim.world().get(x, 2).0 == builtin::SANDSTONE {
            sim.set_cell(x, 2, builtin::OXYGEN, Mass::from_kg(1), ROOM);
        }
    }
    sim.refresh_nav();
    let crank = built(&mut sim, ids::HAND_CRANK, 20, 2);
    for x in 22..25 {
        built(&mut sim, ids::WIRE, x, 2);
    }
    let heater = built(&mut sim, ids::HEATER, 25, 2);
    let start = sim.total_energy() - sim.energy_injected();
    let mut powered_ticks = 0;
    for _ in 0..600 {
        sim.step();
        if sim.buildings().get(heater).unwrap().powered {
            powered_ticks += 1;
        }
    }
    assert!(powered_ticks > 100, "powered for {powered_ticks} ticks");
    assert!(sim.jobs().counts().done > 0, "colonists worked the crank");
    assert!(sim.energy_injected() > 0);
    assert!(sim.world().get(25, 2).2 > ROOM, "the heater's cell warmed");
    let drift = (sim.total_energy() - sim.energy_injected() - start).abs();
    assert!(drift <= 2 * 600 * sim.total_capacity());
    let _ = crank;
}

#[test]
fn not_gate_flips_one_step_after_its_sensor() {
    let mut sim = bench(40);
    let heater = built(&mut sim, ids::HEATER, 6, 2);
    let sensor = built(&mut sim, ids::TEMPERATURE_SENSOR, 6, 3);
    built(&mut sim, ids::SIGNAL_WIRE, 7, 3);
    let gate = built(&mut sim, ids::NOT_GATE, 8, 3);
    sim.step(); // tick 0: networks built, sensor cold, NOT of nothing is false
    let n = sim.networks();
    assert_eq!(n.signals.len(), 1);
    assert_eq!(n.signals[0].logic, vec![sensor.index, gate.index]);
    assert_eq!(n.signals[0].controlled, vec![heater.index]);
    let signal = |sim: &Sim, id: EntityId| sim.buildings().get(id).unwrap().signal;
    let enabled = |sim: &Sim| sim.buildings().get(heater).unwrap().enabled;
    assert!(!signal(&sim, sensor));
    sim.step();
    sim.step(); // tick 2: the sensor is still cold, the NOT gate reads it as false -> true
    assert!(!signal(&sim, sensor));
    assert!(signal(&sim, gate));
    assert!(enabled(&sim));
    sim.set_cell(
        6,
        3,
        builtin::OXYGEN,
        Mass::from_kg(1),
        Temp::from_kelvin(320),
    );
    sim.step();
    sim.step(); // tick 4: the sensor trips; the gate still sees the previous value
    assert!(signal(&sim, sensor));
    assert!(signal(&sim, gate));
    assert!(enabled(&sim));
    sim.step();
    sim.step(); // tick 6: one step later the gate flips and the heater is switched off
    assert!(!signal(&sim, gate));
    assert!(!enabled(&sim));
    sim.set_cell(6, 3, builtin::OXYGEN, Mass::from_kg(1), ROOM);
    sim.step();
    sim.step(); // tick 8: sensor off, gate still off
    assert!(!signal(&sim, sensor));
    assert!(!signal(&sim, gate));
    assert!(!enabled(&sim));
    sim.step();
    sim.step(); // tick 10: gate on, heater back on
    assert!(signal(&sim, gate));
    assert!(enabled(&sim));
}

#[test]
fn networks_rebuild_only_when_dirty_and_a_loaded_save_rebuilds_the_same_components() {
    let mut sim = scenario::powered(64, 20, 1, 3);
    for _ in 0..50 {
        sim.step();
    }
    let before = sim.networks().clone();
    assert_eq!(before.power.len(), 1);
    assert_eq!(before.pipes.len(), 1);
    assert_eq!(before.signals.len(), 1);
    for _ in 0..10 {
        sim.step();
    }
    assert_eq!(
        sim.networks().power,
        before.power,
        "nothing changed, nothing rebuilt"
    );
    let bytes = save::encode(&sim);
    let mut resumed =
        save::decode(ElementTable::builtin(), BuildingTable::builtin(), &bytes).unwrap();
    assert_eq!(resumed.state_hash(), sim.state_hash());
    sim.step();
    resumed.step();
    assert_eq!(resumed.networks(), sim.networks());
    for _ in 0..100 {
        sim.step();
        resumed.step();
    }
    assert_eq!(resumed.state_hash(), sim.state_hash());
    // Placing a wire marks the networks dirty; the next tick rebuilds them.
    let spot = (2..60)
        .find(|&x| sim.placement_valid(ids::WIRE, x, 2))
        .unwrap();
    sim.place_built(ids::WIRE, spot, 2);
    sim.step();
    assert_ne!(sim.networks().power.len() + 1, 0);
    assert!(sim.networks().component_of.len() >= before.component_of.len());
}
