//! Pins the kernel's output bit for bit. `UPDATE_GOLDENS=1 cargo test -p colony-sim golden`
//! rewrites the file; only do that for an intentional change to the simulation rules.

use std::fs;
use std::path::PathBuf;

use colony_sim::scenario;

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn check(name: &str, hash: u64) {
    let path = golden_path(name);
    let value = format!("{hash:016x}\n");
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        fs::write(&path, value).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing {}: run with UPDATE_GOLDENS=1 to create it",
            path.display()
        )
    });
    assert_eq!(expected, value, "{name} diverged from the golden hash");
}

#[test]
fn golden_gas_mixing_64_after_120_ticks() {
    let mut sim = scenario::gas_mixing(64, 64);
    for _ in 0..120 {
        sim.step();
    }
    check("gas-mixing-64-120.hash", sim.cell_hash());
}

#[test]
fn golden_pressure_burst_48_after_200_ticks() {
    let mut sim = scenario::pressure_burst(48, 48);
    for _ in 0..200 {
        sim.step();
    }
    check("pressure-burst-48-200.hash", sim.cell_hash());
}

#[test]
fn golden_pool_64_after_300_ticks() {
    let mut sim = scenario::pool(64, 64);
    for _ in 0..300 {
        sim.step();
    }
    check("pool-64-300.hash", sim.cell_hash());
}

#[test]
fn golden_cave_96x64_seed_1_after_200_ticks() {
    let mut sim = scenario::cave(96, 64);
    for _ in 0..200 {
        sim.step();
    }
    check("cave-96x64-1-200.hash", sim.cell_hash());
}

#[test]
fn golden_crew_128x96_seed_1_after_200_ticks_pins_entities_too() {
    let mut sim = scenario::crew(128, 96, 1);
    for _ in 0..200 {
        sim.step();
    }
    check("crew-128x96-1-200.hash", sim.state_hash());
}

#[test]
fn golden_powered_128x64_seed_1_after_300_ticks_pins_buildings_and_networks() {
    let mut sim = scenario::powered(128, 64, 1, 4);
    for _ in 0..300 {
        sim.step();
    }
    assert!(sim.energy_injected() > 0, "the heater ran");
    assert!(sim.networks().stats.packets > 0, "the pump ran");
    check("powered-128x64-1-300.hash", sim.state_hash());
}
