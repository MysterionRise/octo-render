//! Kernel invariants the design document promises: exact mass conservation, bounded
//! energy drift, bit-identical runs, parallel equals sequential, and chunk skipping.

use colony_sim::{builtin, scenario, Mass, Sim, Temp};

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
        let start = sim.total_energy();
        let ticks = 300;
        for _ in 0..ticks {
            sim.step();
        }
        // Each cell can lose at most one raw temperature unit per exchange to
        // truncation, in the heat pass and again in the flux mixing: two LSBs × its
        // capacity per tick, summed over the world.
        let bound = 2 * ticks as i128 * sim.total_capacity();
        let drift = (sim.total_energy() - start).abs();
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
