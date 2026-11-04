//! Kernel invariants the design document promises: exact mass conservation, bounded
//! energy drift, bit-identical runs, parallel equals sequential, and chunk skipping.

use colony_sim::{builtin, save, scenario, BuildingTable, Command, ElementTable, Mass, Sim, Temp};

fn run(sim: &mut Sim, ticks: usize) -> Vec<u64> {
    (0..ticks)
        .map(|_| {
            sim.step();
            sim.state_hash()
        })
        .collect()
}

#[test]
fn mass_is_conserved_exactly_in_every_scenario() {
    for name in scenario::NAMES {
        let mut sim = scenario::by_name(name, 70, 45).unwrap();
        let start = sim.total_mass();
        for tick in 0..300 {
            sim.step();
            assert_eq!(
                sim.total_mass(),
                start,
                "{name}: mass changed at tick {tick}"
            );
        }
    }
}

#[test]
fn energy_drift_stays_within_the_rounding_bound() {
    for name in scenario::NAMES {
        let mut sim = scenario::by_name(name, 70, 45).unwrap();
        let start = sim.total_energy() - sim.energy_injected();
        let ticks = 300;
        for _ in 0..ticks {
            sim.step();
        }
        // Each cell can lose at most one raw temperature unit per exchange to
        // truncation, in the heat pass and again in the flux mixing: two LSBs × its
        // capacity per tick, summed over the world. Heaters and generators inject
        // energy on purpose; the ledger subtracts what they recorded.
        let bound = 2 * ticks as i128 * sim.total_capacity();
        let drift = (sim.total_energy() - sim.energy_injected() - start).abs();
        assert!(
            drift <= bound,
            "{name}: energy drift {drift} exceeds bound {bound}"
        );
    }
}

#[test]
fn identical_inputs_give_identical_hashes() {
    let mut a = scenario::gas_mixing(96, 64);
    let mut b = scenario::gas_mixing(96, 64);
    assert_eq!(run(&mut a, 200), run(&mut b, 200));
}

#[test]
fn hash_is_sensitive_to_one_cell() {
    let mut a = scenario::gas_mixing(40, 40);
    let mut b = scenario::gas_mixing(40, 40);
    b.set_cell(
        20,
        20,
        builtin::OXYGEN,
        Mass::from_kg(3),
        Temp::from_kelvin(280),
    );
    assert_ne!(run(&mut a, 5), run(&mut b, 5));
}

#[cfg(feature = "parallel")]
#[test]
fn parallel_matches_sequential_bit_for_bit() {
    for name in scenario::NAMES {
        let mut seq = scenario::by_name(name, 130, 97).unwrap();
        seq.set_parallel(false);
        let mut par = scenario::by_name(name, 130, 97).unwrap();
        par.set_parallel(true);
        assert!(par.is_parallel());
        assert_eq!(run(&mut seq, 60), run(&mut par, 60), "{name}");
    }
}

#[test]
fn settled_world_skips_every_chunk_after_the_first_tick() {
    let mut sim = scenario::settled(128, 128);
    let first = sim.step();
    assert_eq!(first.chunks_skipped, 0, "the first tick visits every chunk");
    let second = sim.step();
    assert_eq!(second.chunks_processed, 0);
    assert_eq!(second.chunks_processed_mass, 0);
    assert_eq!(second.chunks_skipped, sim.chunks().len());
    assert!(sim.chunks().len() >= 16);
}

#[test]
fn solid_world_skips_the_mass_passes_while_heat_still_runs() {
    let mut sim = scenario::hot_rock(128, 128);
    sim.step();
    let stats = sim.step();
    assert_eq!(stats.chunks_processed_mass, 0, "solids never move");
    assert!(stats.chunks_processed > 0, "the hot block keeps conducting");
}

#[test]
fn skipped_chunks_do_not_leak_mass_next_to_active_ones() {
    // A 5-chunk-wide world where only the far-left chunk column has a disturbance:
    // chunks on the right settle and are skipped while their neighbours keep running.
    let mut sim = scenario::settled(5 * 32, 2 * 32);
    sim.set_cell(
        3,
        3,
        builtin::OXYGEN,
        Mass::from_kg(6),
        Temp::from_kelvin(400),
    );
    let start = sim.total_mass();
    let mut saw_partial = false;
    for _ in 0..400 {
        let stats = sim.step();
        saw_partial |= stats.chunks_skipped > 0 && stats.chunks_processed > 0;
        assert_eq!(sim.total_mass(), start);
    }
    assert!(
        saw_partial,
        "expected a tick with both processed and skipped chunks"
    );
}

#[test]
fn pocket_expands_into_vacuum_without_creating_mass() {
    let mut sim = scenario::pressure_burst(48, 48);
    let start = sim.total_mass();
    let vacuum_before = sim
        .world()
        .cells
        .element
        .iter()
        .filter(|e| **e == 0)
        .count();
    for _ in 0..200 {
        sim.step();
    }
    let vacuum_after = sim
        .world()
        .cells
        .element
        .iter()
        .filter(|e| **e == 0)
        .count();
    assert!(vacuum_after < vacuum_before);
    assert_eq!(sim.total_mass(), start);
    let max = sim.world().cells.mass.iter().copied().max().unwrap();
    assert!(max <= Mass::from_kg(1800).raw());
}

#[test]
fn hot_rock_edge_cools_and_its_surroundings_warm() {
    // The block spans x 12..20 at y 16; conduction at 100 kg per cell is slow, so the
    // front only moves a cell or two in 2,000 ticks and the centre stays untouched.
    let mut sim = scenario::hot_rock(33, 33);
    let edge_before = sim.world().get(12, 16).2;
    let outside_before = sim.world().get(11, 16).2;
    let centre_before = sim.world().get(16, 16).2;
    let energy_before = sim.total_energy();
    for _ in 0..2000 {
        sim.step();
    }
    assert!(
        sim.world().get(12, 16).2 < edge_before,
        "block edge did not cool"
    );
    assert!(
        sim.world().get(11, 16).2 > outside_before,
        "surroundings did not warm"
    );
    assert!(sim.world().get(16, 16).2 <= centre_before);
    assert!(sim.world().get(16, 16).2 > Temp::from_kelvin(1100));
    let drift = (sim.total_energy() - energy_before).abs();
    assert!(drift <= 2 * 2000 * sim.total_capacity());
}

#[test]
fn water_falls_covers_the_floor_and_loses_no_mass() {
    let mut sim = scenario::pool(64, 64);
    let max = sim.table().max_mass(builtin::WATER.0) as u64
        + colony_sim::fluid::LIQUID_FLOOR.raw() as u64;
    let water = |sim: &Sim| -> Vec<(usize, usize, u32)> {
        let w = sim.world();
        (0..w.height)
            .flat_map(|y| (0..w.width).map(move |x| (x, y)))
            .filter(|&(x, y)| w.get(x, y).0 == builtin::WATER)
            .map(|(x, y)| (x, y, w.get(x, y).1.raw()))
            .collect()
    };
    let start = sim.total_mass();
    let water_start: u64 = water(&sim).iter().map(|c| c.2 as u64).sum();
    for _ in 0..1500 {
        sim.step();
        assert_eq!(sim.total_mass(), start);
    }
    let before = water(&sim);
    for _ in 0..100 {
        sim.step();
    }
    let after = water(&sim);
    assert_eq!(
        after.iter().map(|c| c.2 as u64).sum::<u64>(),
        water_start,
        "water neither froze nor vanished"
    );
    // The pool keeps levelling for thousands of ticks, so "at rest" is judged by what
    // cannot change any more: the bottom row is covered and nothing is left up high.
    assert!(
        after.len() >= before.len(),
        "water cells only spread, never vanish"
    );
    let width = sim.world().width;
    let bottom_row = after.iter().filter(|c| c.1 == 1).count();
    assert!(
        bottom_row >= (width - 2) * 9 / 10,
        "bottom row covered by {bottom_row} of {} cells",
        width - 2
    );
    let highest_water_row = after.iter().map(|c| c.1).max().unwrap();
    assert!(
        highest_water_row < sim.world().height / 2,
        "water should have fallen to the floor, highest row {highest_water_row}"
    );
    for (x, y, mass) in &after {
        assert!(
            *mass as u64 <= max,
            "cell ({x}, {y}) holds {} g",
            colony_sim::Mass(*mass).to_grams()
        );
    }
}

#[test]
fn save_then_continue_matches_the_uninterrupted_run() {
    let mut whole = scenario::pool(48, 40);
    let mut first_half = scenario::pool(48, 40);
    for _ in 0..40 {
        whole.step();
        first_half.step();
    }
    let bytes = save::encode(&first_half);
    let mut resumed =
        save::decode(ElementTable::builtin(), BuildingTable::builtin(), &bytes).unwrap();
    assert_eq!(resumed.state_hash(), whole.state_hash());
    for _ in 0..60 {
        whole.step();
        resumed.step();
        assert_eq!(resumed.state_hash(), whole.state_hash());
    }
}

#[test]
fn a_logged_run_replays_to_the_same_hashes() {
    let mut live = scenario::cave(96, 64);
    let mut hashes = Vec::new();
    for tick in 0..120u64 {
        if tick % 25 == 3 {
            live.queue(Command::FillRect {
                x0: 10,
                y0: 40,
                x1: 14,
                y1: 44,
                element: builtin::WATER,
                mass: Mass::from_kg(800),
                temp: Temp::from_kelvin(285),
            });
            live.queue(Command::SetCell {
                x: 30,
                y: 30,
                element: builtin::CARBON_DIOXIDE,
                mass: Mass::from_kg(3),
                temp: Temp::from_kelvin(320),
            });
        }
        live.step();
        hashes.push(live.state_hash());
    }
    let log = live.log().to_vec();
    assert_eq!(log.len(), 10);
    let bytes = colony_sim::command::encode_log(&log);
    let decoded = colony_sim::command::decode_log(&bytes).unwrap();
    let mut replayed = scenario::cave(96, 64);
    replayed.replay(&decoded, 120);
    assert_eq!(replayed.state_hash(), *hashes.last().unwrap());
    assert_eq!(replayed.log(), log.as_slice());
}

#[test]
fn processed_chunks_name_exactly_the_chunks_that_can_change() {
    let mut sim = scenario::settled(128, 128);
    assert!(
        sim.processed_chunks().iter().all(|p| *p),
        "everything is dirty before the first tick"
    );
    sim.step();
    sim.step();
    assert!(
        sim.processed_chunks().iter().all(|p| !p),
        "a settled world changes nothing"
    );
    sim.set_cell(
        40,
        40,
        builtin::OXYGEN,
        Mass::from_kg(3),
        Temp::from_kelvin(300),
    );
    sim.step();
    let chunks = sim.chunks();
    let processed = sim.processed_chunks();
    assert!(processed[chunks.index(1, 1)]);
    assert!(
        processed[chunks.index(0, 0)] && processed[chunks.index(2, 2)],
        "the 3×3 ring is visited"
    );
    assert!(!processed[chunks.index(3, 3)]);
    assert_eq!(processed.iter().filter(|p| **p).count(), 9);
}
