//! Colonists at work: the dig-then-build loop, conservation across cells and items,
//! budgets, replay, and saves with entities.

use colony_sim::agent::{AgentState, DEFAULT_PRIORITY, PATH_BUDGET};
use colony_sim::{
    builtin, save, scenario, Command, ElementTable, JobKind, JobState, Mass, Sim, Temp,
};

fn run(sim: &mut Sim, ticks: usize) {
    for _ in 0..ticks {
        sim.step();
    }
}

fn solid_cells_next_to_a_colonist(sim: &Sim, n: usize) -> Vec<(u32, u32)> {
    // Wall and ceiling cells near the party: solid, reachable from a standing cell, and
    // not the floor under anyone, so digging them strands nobody. Nearest first.
    let nav = sim.nav();
    let mut candidates: Vec<(u32, u32, u32)> = Vec::new();
    for (_, c) in sim.colonists().iter() {
        let (x, y) = c.cell;
        for dy in -3i64..=6 {
            for dx in -10i64..=10 {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx <= 0
                    || ny <= 0
                    || nx as u32 >= nav.width() - 1
                    || ny as u32 + 1 >= nav.height() - 1
                {
                    continue;
                }
                let (nx, ny) = (nx as u32, ny as u32);
                let reachable = !nav
                    .goal_cells(colony_sim::Goal::Adjacent { x: nx, y: ny })
                    .is_empty();
                if nav.is_solid(nx, ny) && !nav.is_standing(nx, ny + 1) && reachable {
                    candidates.push((dx.unsigned_abs() as u32 + dy.unsigned_abs() as u32, nx, ny));
                }
            }
        }
    }
    candidates.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (_, x, y) in candidates {
        if !out.contains(&(x, y)) {
            out.push((x, y));
        }
        if out.len() == n {
            break;
        }
    }
    out
}

/// Head-height cells above standing cells three to ten cells from the party (so they
/// are in the same cavern): non-solid, reachable, and not where anyone stands.
fn build_sites(sim: &Sim, n: usize) -> Vec<(u32, u32)> {
    let nav = sim.nav();
    let colonists: Vec<(u32, u32)> = sim.colonists().iter().map(|(_, c)| c.cell).collect();
    let mut candidates: Vec<(u32, u32, u32)> = Vec::new();
    for &(cx, cy) in &colonists {
        for dy in -6i64..=6 {
            for dx in -10i64..=10 {
                let (x, y) = (cx as i64 + dx, cy as i64 + dy);
                if x <= 0
                    || y <= 0
                    || x as u32 >= nav.width() - 1
                    || y as u32 + 2 >= nav.height() - 1
                {
                    continue;
                }
                let (x, y) = (x as u32, y as u32);
                let d = dx.unsigned_abs() as u32 + dy.unsigned_abs() as u32;
                let far = colonists
                    .iter()
                    .all(|&(ox, oy)| ox.abs_diff(x) + oy.abs_diff(y) >= 3);
                // Floor-supported only: a pillar top stops standing once its base is dug.
                if d >= 3 && far && y == 2 && nav.is_standing(x, y) && !nav.is_solid(x, y + 1) {
                    candidates.push((d, x, y + 1));
                }
            }
        }
    }
    candidates.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (_, x, y) in candidates {
        if !out.contains(&(x, y)) {
            out.push((x, y));
        }
        if out.len() == n {
            break;
        }
    }
    out
}

#[test]
fn dig_then_build_loop_conserves_mass_and_finishes_every_job() {
    let mut sim = scenario::hall(64, 20, 1, 4, 0);
    assert_eq!(sim.colonists().len(), 4);
    let total = sim.total_mass();
    let targets = solid_cells_next_to_a_colonist(&sim, 6);
    assert_eq!(targets.len(), 6, "need six diggable cells near the party");
    let dug_elements: Vec<_> = targets
        .iter()
        .map(|&(x, y)| sim.world().get(x as usize, y as usize).0)
        .collect();
    for &(x, y) in &targets {
        assert_ne!(sim.order(JobKind::Dig { x, y }, DEFAULT_PRIORITY), u32::MAX);
    }
    let mut ticks = 0;
    while sim.items().len() < 6 && ticks < 3000 {
        sim.step();
        assert_eq!(sim.total_mass(), total, "mass changed at tick {ticks}");
        ticks += 1;
    }
    assert_eq!(
        sim.items().len(),
        6,
        "six debris items after {ticks} ticks; jobs {:?}",
        sim.jobs().counts()
    );
    for &(x, y) in &targets {
        // Walkability refreshes at the start of the next agents pass, so check the cells.
        assert_ne!(
            sim.world().get(x as usize, y as usize).0.phase(),
            colony_sim::Phase::Solid,
            "({x}, {y}) was dug; jobs {:?}",
            sim.jobs().counts()
        );
    }
    // Build six tiles from the debris at head height above standing cells away from the party.
    let sites = build_sites(&sim, 6);
    assert_eq!(sites.len(), 6, "need six build sites");
    for &(x, y) in &sites {
        assert_ne!(
            sim.order(JobKind::Build { x, y }, DEFAULT_PRIORITY),
            u32::MAX
        );
    }
    let mut ticks = 0;
    while !sim.items().is_empty() && ticks < 6000 {
        sim.step();
        assert_eq!(
            sim.total_mass(),
            total,
            "mass changed while building at tick {ticks}"
        );
        ticks += 1;
    }
    assert_eq!(
        sim.items().len(),
        0,
        "every item was used after {ticks} ticks; jobs {:?}",
        sim.jobs().counts()
    );
    let counts = sim.jobs().counts();
    assert_eq!(counts.done, 12, "{counts:?}");
    for &(x, y) in &sites {
        let built = sim.world().get(x as usize, y as usize).0;
        assert_eq!(built.phase(), colony_sim::Phase::Solid, "({x}, {y}) built");
        assert!(
            dug_elements.contains(&built),
            "built from dug material: {built:?}"
        );
    }
    assert_eq!(sim.total_mass(), total);
}

#[test]
fn digging_a_colonists_own_floor_drops_it_one_cell() {
    let table = ElementTable::builtin();
    let mut sim = Sim::new(table, 8, 8);
    sim.fill_rect(
        0,
        0,
        8,
        3,
        builtin::SANDSTONE,
        Mass::from_kg(100),
        Temp::from_kelvin(300),
    );
    sim.fill_rect(
        0,
        3,
        8,
        8,
        builtin::OXYGEN,
        Mass::from_kg(1),
        Temp::from_kelvin(300),
    );
    sim.refresh_nav();
    let id = sim.spawn_colonist(4, 3).unwrap();
    // The colonist digs the floor under its neighbour (5, 2) first, then we dig (4, 2) by hand.
    sim.set_cell(
        4,
        2,
        builtin::OXYGEN,
        Mass::from_kg(1),
        Temp::from_kelvin(300),
    );
    sim.step();
    let c = sim.colonists().get(id).unwrap();
    assert_eq!(c.cell, (4, 2), "fell onto the next floor: {c:?}");
    assert!(matches!(c.state, AgentState::Idle));
}

#[test]
fn build_into_gas_displaces_the_gas_instead_of_destroying_it() {
    let table = ElementTable::builtin();
    let mut sim = Sim::new(table, 8, 6);
    sim.fill_rect(
        0,
        0,
        8,
        2,
        builtin::SANDSTONE,
        Mass::from_kg(100),
        Temp::from_kelvin(300),
    );
    sim.fill_rect(
        0,
        2,
        8,
        6,
        builtin::OXYGEN,
        Mass::from_kg(1),
        Temp::from_kelvin(300),
    );
    // An extra solid cell to dig, next to the colonist.
    sim.set_cell(
        6,
        2,
        builtin::SANDSTONE,
        Mass::from_kg(100),
        Temp::from_kelvin(300),
    );
    sim.refresh_nav();
    sim.spawn_colonist(4, 2).unwrap();
    let total = sim.total_mass();
    sim.order(JobKind::Dig { x: 6, y: 2 }, DEFAULT_PRIORITY);
    run(&mut sim, 200);
    assert_eq!(sim.items().len(), 1);
    sim.order(JobKind::Build { x: 1, y: 2 }, DEFAULT_PRIORITY);
    let mut ticks = 0;
    while !sim.items().is_empty() && ticks < 2000 {
        sim.step();
        ticks += 1;
    }
    assert_eq!(
        sim.items().len(),
        0,
        "built after {ticks} ticks: {:?}",
        sim.jobs().counts()
    );
    assert_eq!(sim.world().get(1, 2).0, builtin::SANDSTONE);
    assert_eq!(
        sim.total_mass(),
        total,
        "the displaced oxygen went somewhere"
    );
}

#[test]
fn an_unreachable_order_is_skipped_and_stays_open() {
    let table = ElementTable::builtin();
    let mut sim = Sim::new(table, 10, 6);
    sim.fill_rect(
        0,
        0,
        10,
        2,
        builtin::SANDSTONE,
        Mass::from_kg(100),
        Temp::from_kelvin(300),
    );
    sim.fill_rect(
        0,
        2,
        10,
        6,
        builtin::OXYGEN,
        Mass::from_kg(1),
        Temp::from_kelvin(300),
    );
    // A wall three cells high separates the colonist from the right side.
    sim.fill_rect(
        5,
        2,
        6,
        5,
        builtin::SANDSTONE,
        Mass::from_kg(100),
        Temp::from_kelvin(300),
    );
    sim.refresh_nav();
    let id = sim.spawn_colonist(2, 2).unwrap();
    let job = sim.order(JobKind::Dig { x: 8, y: 1 }, DEFAULT_PRIORITY);
    run(&mut sim, 50);
    assert_eq!(sim.jobs().get(job).unwrap().state, JobState::Open);
    assert!(matches!(
        sim.colonists().get(id).unwrap().state,
        AgentState::Idle
    ));
    // Digging the wall makes it reachable.
    sim.order(JobKind::Dig { x: 5, y: 2 }, 9);
    sim.order(JobKind::Dig { x: 5, y: 3 }, 9);
    run(&mut sim, 400);
    assert!(sim.jobs().counts().done >= 2, "{:?}", sim.jobs().counts());
}

#[test]
fn the_path_budget_is_per_tick_and_runs_are_identical() {
    let mut a = scenario::crew(128, 96, 1);
    let mut b = scenario::crew(128, 96, 1);
    for _ in 0..150 {
        let sa = a.step();
        let sb = b.step();
        assert_eq!(sa, sb);
        assert!(sa.path_solves <= PATH_BUDGET);
        assert_eq!(a.state_hash(), b.state_hash());
    }
    assert!(
        a.jobs().counts().done > 0,
        "the crew dug something: {:?}",
        a.jobs().counts()
    );
}

#[test]
fn a_logged_run_with_orders_replays_to_the_same_hash() {
    let mut live = scenario::cave_with_colonists(96, 64, 2, 3);
    let targets = solid_cells_next_to_a_colonist(&live, 3);
    assert!(
        !targets.is_empty(),
        "at least one diggable cell near the party"
    );
    let site = build_sites(&live, 1);
    let mut hashes = Vec::new();
    for tick in 0..240u64 {
        if tick == 2 {
            for &(x, y) in &targets {
                live.queue(Command::Dig {
                    x: x as usize,
                    y: y as usize,
                });
            }
            live.queue(Command::SpawnColonist { x: 1, y: 1 });
        }
        if tick == 100 {
            let (x, y) = targets[0];
            live.queue(Command::SetPriority {
                x: x as usize,
                y: y as usize,
                priority: 9,
            });
            if let Some(&(cx, cy)) = targets.last() {
                live.queue(Command::CancelOrders {
                    x: cx as usize,
                    y: cy as usize,
                });
            }
            if let Some(&(bx, by)) = site.first() {
                live.queue(Command::Build {
                    x: bx as usize,
                    y: by as usize,
                });
            }
        }
        live.step();
        hashes.push(live.state_hash());
    }
    let log = live.log().to_vec();
    assert!(log.len() >= 4, "orders were logged: {}", log.len());
    let bytes = colony_sim::command::encode_log(&log);
    let mut replayed = scenario::cave_with_colonists(96, 64, 2, 3);
    replayed.replay(&colony_sim::command::decode_log(&bytes).unwrap(), 240);
    assert_eq!(replayed.state_hash(), *hashes.last().unwrap());
}

#[test]
fn save_format_2_round_trips_entities_and_continues_identically() {
    let mut whole = scenario::crew(96, 64, 4);
    let mut half = scenario::crew(96, 64, 4);
    run(&mut whole, 80);
    run(&mut half, 80);
    let bytes = save::encode(&half);
    let mut resumed = save::decode(ElementTable::builtin(), &bytes).unwrap();
    assert_eq!(resumed.state_hash(), whole.state_hash());
    assert_eq!(resumed.colonists(), whole.colonists());
    assert_eq!(resumed.items(), whole.items());
    assert_eq!(resumed.jobs(), whole.jobs());
    for _ in 0..60 {
        whole.step();
        resumed.step();
        assert_eq!(resumed.state_hash(), whole.state_hash());
    }
}

#[test]
fn pool_floods_debris_without_mass_loss() {
    let mut sim = scenario::pool(64, 48);
    sim.refresh_nav();
    // A colonist on the floor digs the floor cell next to it; the item lands in the water later.
    let id = sim.spawn_colonist(10, 1).unwrap();
    assert!(sim.colonists().get(id).is_some());
    sim.order(JobKind::Dig { x: 11, y: 0 }, DEFAULT_PRIORITY);
    let total = sim.total_mass();
    for _ in 0..600 {
        sim.step();
        assert_eq!(sim.total_mass(), total);
    }
}
