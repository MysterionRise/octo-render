//! Buildings in the simulation: the occupancy map, placement, construction and
//! deconstruction by colonists, deliveries, and the buildings pass that runs before the
//! first gather of a tick. Every cell edit goes through `Sim::set_cell` or
//! `Sim::add_heat`, so the gather passes see it next tick and nothing is written during a
//! pass. Mass moves between cells, items, and buildings in closed transfers.

use crate::agent::{Colonist, DEFAULT_PRIORITY, OPERATE_TICKS};
use crate::building::{Building, BuildingKind, BuildingState, BuildingTable};
use crate::element::{ElementId, Phase};
use crate::entity::EntityId;
use crate::fixed::{Mass, Temp};
use crate::fluid::mix_temp;
use crate::item::Item;
use crate::job::JobKind;
use crate::nav::{SPECIAL_DOOR, SPECIAL_LADDER};
use crate::sim::Sim;

/// The occupancy map's value for a free cell.
pub const NO_BUILDING: u32 = u32::MAX;

/// Deliveries to a bin or a generator are low priority, so digging and building win.
pub const DELIVER_PRIORITY: u8 = 2;

/// How many ticks of feed an oxygen generator stores.
pub const FEED_STORE_TICKS: u64 = 600;

/// Operating a hand crank ranks below digging and building.
pub const OPERATE_PRIORITY: u8 = 4;

impl Sim {
    pub fn buildings(&self) -> &crate::entity::Arena<Building> {
        &self.buildings
    }

    pub fn building_table(&self) -> &BuildingTable {
        &self.building_table
    }

    /// Per cell, the arena slot of the building there, or [`NO_BUILDING`].
    pub fn cell_building(&self) -> &[u32] {
        &self.cell_building
    }

    /// Per cell, the `nav::SPECIAL_*` bits (ladders, doors).
    pub fn special(&self) -> &[u8] {
        &self.special
    }

    /// The building occupying a cell.
    pub fn building_at(&self, x: usize, y: usize) -> Option<EntityId> {
        if x >= self.world.width || y >= self.world.height {
            return None;
        }
        let slot = self.cell_building[self.world.index(x, y)];
        if slot == NO_BUILDING {
            return None;
        }
        let (generation, live) = self.buildings.slot(slot as usize);
        live.map(|_| EntityId {
            index: slot,
            generation,
        })
    }

    /// Replaces the building table; only an append-only extension is accepted (the
    /// hot-reload rule), so every placed building keeps a valid definition.
    pub fn set_building_table(&mut self, table: BuildingTable) -> Result<(), String> {
        if !table.is_append_only_extension(&self.building_table) {
            return Err(
                "a reload may only append buildings; existing entries must keep their order and names"
                    .to_string(),
            );
        }
        self.building_table = table;
        self.networks_dirty = true;
        Ok(())
    }

    /// Whether `def` can be placed with its bottom-left cell at `(x, y)`: in bounds,
    /// every footprint cell free of buildings and not solid.
    pub fn placement_valid(&self, def: u16, x: usize, y: usize) -> bool {
        if !self.building_table.contains(def) {
            return false;
        }
        let size = self.building_table.get(def).size;
        let (w, h) = (size.0 as usize, size.1 as usize);
        if x + w > self.world.width || y + h > self.world.height {
            return false;
        }
        for cy in y..y + h {
            for cx in x..x + w {
                let i = self.world.index(cx, cy);
                if self.cell_building[i] != NO_BUILDING
                    || ElementId(self.world.cells.element[i]).phase() == Phase::Solid
                {
                    return false;
                }
            }
        }
        true
    }

    /// Places a building already built, with no material taken from anywhere (door
    /// cells are created from the cost). For scenarios and tests; it does not conserve
    /// mass against an earlier state.
    pub fn place_built(&mut self, def: u16, x: usize, y: usize) -> Option<EntityId> {
        if !self.placement_valid(def, x, y) {
            return None;
        }
        let d = self.building_table.get(def).clone();
        let mut b = Building::new(def, x as u32, y as u32);
        b.state = BuildingState::Built;
        if d.kind == BuildingKind::Door {
            if let Some((element, cost)) = d.cost {
                let n = d.cells();
                let each = cost.raw() / n;
                let remainder = cost.raw() - each * n;
                for (i, (cx, cy)) in b.footprint(&d).enumerate() {
                    let m = each + if i == 0 { remainder } else { 0 };
                    self.set_cell(
                        cx as usize,
                        cy as usize,
                        element,
                        Mass(m),
                        Temp::from_kelvin(295),
                    );
                }
            }
        } else {
            b.material = d.cost.map(|(e, m)| (e, m, Temp::from_kelvin(295)));
        }
        let id = self.buildings.insert(b);
        self.occupy(id);
        Some(id)
    }

    /// Places a building as `Planned` and opens its construction job.
    pub fn place(&mut self, def: u16, x: usize, y: usize) -> Option<EntityId> {
        if !self.placement_valid(def, x, y) {
            return None;
        }
        let id = self
            .buildings
            .insert(Building::new(def, x as u32, y as u32));
        self.occupy(id);
        self.jobs
            .add(JobKind::Construct { building: id }, DEFAULT_PRIORITY);
        Some(id)
    }

    /// Removes a planned building at the cell at once, or orders a built one taken
    /// apart; `false` if no building is there.
    pub fn deconstruct(&mut self, x: usize, y: usize) -> bool {
        let Some(id) = self.building_at(x, y) else {
            return false;
        };
        let built = self.buildings.get(id).is_some_and(|b| b.is_built());
        if built {
            self.jobs
                .add(JobKind::Deconstruct { building: id }, DEFAULT_PRIORITY);
        } else {
            self.remove_building(id, (x as u32, y as u32));
        }
        true
    }

    /// Banks operating ticks on a hand crank, as a colonist's stint would; `false` for
    /// anything else. For tests and tools.
    pub fn operate(&mut self, id: EntityId, ticks: u16) -> bool {
        let Some(b) = self.buildings.get_mut(id) else {
            return false;
        };
        if !matches!(
            self.building_table.get(b.def).kind,
            BuildingKind::HandCrank { .. }
        ) {
            return false;
        }
        b.work = b.work.saturating_add(ticks);
        true
    }

    /// Flips the enabled switch of the building at the cell.
    pub fn toggle_building(&mut self, x: usize, y: usize) -> bool {
        let Some(id) = self.building_at(x, y) else {
            return false;
        };
        let b = self.buildings.get_mut(id).unwrap();
        b.enabled = !b.enabled;
        true
    }

    /// Writes the occupancy map and special bits of one building.
    fn occupy(&mut self, id: EntityId) {
        let b = self.buildings.get(id).unwrap().clone();
        let def = self.building_table.get(b.def);
        let bits = match def.kind {
            BuildingKind::Ladder => SPECIAL_LADDER,
            BuildingKind::Door if b.is_built() => SPECIAL_DOOR,
            _ => 0,
        };
        for (x, y) in b.footprint(def) {
            let i = self.world.index(x as usize, y as usize);
            self.cell_building[i] = id.index;
            self.special[i] |= bits;
            self.touch_chunk(x as usize, y as usize);
        }
        self.networks_dirty = true;
    }

    /// Clears the occupancy map and special bits of one building.
    fn vacate(&mut self, id: EntityId) {
        let b = self.buildings.get(id).unwrap().clone();
        let def = self.building_table.get(b.def);
        for (x, y) in b.footprint(def) {
            let i = self.world.index(x as usize, y as usize);
            self.cell_building[i] = NO_BUILDING;
            self.special[i] = 0;
            self.touch_chunk(x as usize, y as usize);
        }
        self.networks_dirty = true;
    }

    /// Rebuilds the occupancy map and special bits from the arena (after a load).
    pub(crate) fn rebuild_occupancy(&mut self) {
        self.cell_building.fill(NO_BUILDING);
        self.special.fill(0);
        for id in self.buildings.ids() {
            self.occupy(id);
        }
    }

    /// Drops an amount of an element as an item at a cell (gravity settles it).
    fn drop_item(&mut self, at: (u32, u32), element: ElementId, mass: Mass, temp: Temp) {
        if mass.raw() == 0 {
            return;
        }
        self.items.insert(Item {
            x: at.0,
            y: at.1,
            element,
            mass,
            temp,
            holder: EntityId::NONE,
            reserved_by: EntityId::NONE,
        });
    }

    /// Takes a building out of the world: door cells return to vacuum into the material,
    /// contents and material drop as items at `drop_at`, jobs are cancelled.
    pub(crate) fn remove_building(&mut self, id: EntityId, drop_at: (u32, u32)) {
        let Some(mut b) = self.buildings.get(id).cloned() else {
            return;
        };
        let def = self.building_table.get(b.def).clone();
        if def.kind == BuildingKind::Door && b.is_built() {
            // A door's material is its cells: gather them back into one lot.
            if let Some((element, _)) = def.cost {
                let mut lot: Option<(Mass, Temp)> = None;
                for (x, y) in b.footprint(&def) {
                    let (el, m, t) = self.world.get(x as usize, y as usize);
                    if el != element || m.raw() == 0 {
                        continue;
                    }
                    lot = Some(match lot {
                        None => (m, t),
                        Some((lm, lt)) => (
                            Mass(lm.raw() + m.raw()),
                            Temp(mix_temp(
                                lm.raw() as u64,
                                lt.raw(),
                                m.raw() as u64,
                                m.raw() as i128 * t.raw() as i128,
                            )),
                        ),
                    });
                    self.set_cell(
                        x as usize,
                        y as usize,
                        ElementId::VACUUM,
                        Mass::ZERO,
                        Temp::ZERO,
                    );
                }
                b.material = lot.map(|(m, t)| (element, m, t));
            }
        }
        self.jobs.cancel_for_building(id, self.tick);
        self.vacate(id);
        self.buildings.remove(id);
        if let Some((e, m, t)) = b.material {
            self.drop_item(drop_at, e, m, t);
        }
        if let Some((e, m, t)) = b.stored {
            self.drop_item(drop_at, e, m, t);
        }
        if let Some(p) = b.packet {
            self.drop_item(drop_at, p.element, p.mass, p.temp);
        }
    }

    /// The rectangle a building covers.
    pub(crate) fn building_rect(&self, id: EntityId) -> Option<(u32, u32, u8, u8)> {
        let b = self.buildings.get(id)?;
        let def = self.building_table.get(b.def);
        Some((b.x, b.y, def.size.0, def.size.1))
    }

    /// What a building wants delivered: `(element or any, room)`, or `None` when full
    /// or not a receiver.
    pub(crate) fn delivery_wanted(&self, id: EntityId) -> Option<(Option<ElementId>, Mass)> {
        let b = self.buildings.get(id)?;
        if !b.is_built() {
            return None;
        }
        let def = self.building_table.get(b.def);
        let (element, capacity, threshold) = match def.kind {
            BuildingKind::Storage { capacity } => {
                (None, capacity.raw() as u64, capacity.raw() as u64)
            }
            BuildingKind::OxygenGenerator {
                feedstock,
                feed_per_tick,
                ..
            } => {
                let capacity = (feed_per_tick.raw() as u64 * FEED_STORE_TICKS).min(u32::MAX as u64);
                (Some(feedstock), capacity, capacity / 2)
            }
            _ => return None,
        };
        let (held_element, held) = match b.stored {
            Some((e, m, _)) => (Some(e), m.raw() as u64),
            None => (None, 0),
        };
        if held >= threshold || held >= capacity {
            return None;
        }
        let wanted = match (element, held_element) {
            (Some(e), Some(h)) if e != h => return None,
            (Some(e), _) => Some(e),
            (None, h) => h,
        };
        Some((wanted, Mass((capacity - held) as u32)))
    }

    /// Finishes a construction job: consumes the cost from the carried item, makes door
    /// cells solid, marks the building built. `false` releases the job.
    pub(crate) fn complete_construct(&mut self, c: &mut Colonist, id: EntityId) -> bool {
        let Some(b) = self.buildings.get(id).cloned() else {
            return false;
        };
        if b.is_built() {
            return false;
        }
        let def = self.building_table.get(b.def).clone();
        let mut material = None;
        if let Some((element, cost)) = def.cost {
            let Some(item) = self.items.get(c.carrying).copied() else {
                return false;
            };
            if item.element != element || item.mass < cost {
                return false;
            }
            material = Some((element, cost, item.temp));
        }
        if def.kind == BuildingKind::Door {
            let cells: Vec<(u32, u32)> = b.footprint(&def).collect();
            let occupied = cells.iter().any(|&(x, y)| {
                self.nav.is_solid(x, y)
                    || self.colonists.iter().any(|(_, o)| {
                        o.cell == (x, y)
                            || (o.from == (x, y) && o.progress < crate::agent::MOVE_TICKS)
                    })
                    || self
                        .items
                        .iter()
                        .any(|(_, it)| it.on_ground() && (it.x, it.y) == (x, y))
            });
            if occupied {
                return false;
            }
            for &(x, y) in &cells {
                if !self.displace_fluid((x, y), &cells, c.cell) {
                    return false;
                }
            }
            let (element, cost, temp) = material.expect("doors have a cost");
            let n = cells.len() as u32;
            let each = cost.raw() / n;
            let remainder = cost.raw() - each * n;
            for (i, &(x, y)) in cells.iter().enumerate() {
                let m = each + if i == 0 { remainder } else { 0 };
                self.set_cell(x as usize, y as usize, element, Mass(m), temp);
            }
        }
        if let Some((_, cost, _)) = material {
            let item = self.items.get_mut(c.carrying).unwrap();
            item.mass = Mass(item.mass.raw() - cost.raw());
            if item.mass.raw() == 0 {
                self.items.remove(c.carrying);
            } else {
                item.holder = EntityId::NONE;
                item.reserved_by = EntityId::NONE;
                item.x = c.cell.0;
                item.y = c.cell.1;
            }
            c.carrying = EntityId::NONE;
        }
        let built = self.buildings.get_mut(id).unwrap();
        built.state = BuildingState::Built;
        // A door's material lives in its cells; everything else keeps it for deconstruction.
        built.material = if def.kind == BuildingKind::Door {
            None
        } else {
            material
        };
        self.occupy(id);
        true
    }

    /// Finishes a deconstruction job at the colonist's cell.
    pub(crate) fn complete_deconstruct(&mut self, c: &mut Colonist, id: EntityId) -> bool {
        if !self.buildings.get(id).is_some_and(|b| b.is_built()) {
            return false;
        }
        self.remove_building(id, c.cell);
        true
    }

    /// Finishes a delivery: moves as much of the carried item as the building has room
    /// for into its contents and drops the rest.
    pub(crate) fn complete_deliver(&mut self, c: &mut Colonist, id: EntityId) -> bool {
        let Some((wanted, room)) = self.delivery_wanted(id) else {
            return false;
        };
        let Some(item) = self.items.get(c.carrying).copied() else {
            return false;
        };
        if wanted.is_some_and(|e| e != item.element) {
            return false;
        }
        let deposit = item.mass.raw().min(room.raw());
        if deposit == 0 {
            return false;
        }
        let b = self.buildings.get_mut(id).unwrap();
        b.stored = Some(match b.stored {
            Some((e, m, t)) => (
                e,
                Mass(m.raw() + deposit),
                Temp(mix_temp(
                    m.raw() as u64,
                    t.raw(),
                    deposit as u64,
                    deposit as i128 * item.temp.raw() as i128,
                )),
            ),
            None => (item.element, Mass(deposit), item.temp),
        });
        let it = self.items.get_mut(c.carrying).unwrap();
        it.mass = Mass(item.mass.raw() - deposit);
        if it.mass.raw() == 0 {
            self.items.remove(c.carrying);
        } else {
            it.holder = EntityId::NONE;
            it.reserved_by = EntityId::NONE;
            it.x = c.cell.0;
            it.y = c.cell.1;
        }
        c.carrying = EntityId::NONE;
        true
    }

    /// Whether a hand crank wants a colonist: built, enabled, its bank nearly empty, and
    /// its component has a consumer or a battery that is not full.
    fn crank_wanted(&self, id: EntityId) -> bool {
        let Some(b) = self.buildings.get(id) else {
            return false;
        };
        if !b.is_built() || !b.enabled || b.work >= OPERATE_TICKS {
            return false;
        }
        if !matches!(
            self.building_table.get(b.def).kind,
            BuildingKind::HandCrank { .. }
        ) {
            return false;
        }
        let Some(&ci) = self.networks.component_of.get(id.index as usize) else {
            return false;
        };
        if ci == crate::construct::NO_BUILDING {
            return false;
        }
        self.networks.power[ci as usize].members.iter().any(|&s| {
            let Some(m) = self.buildings.slot(s as usize).1 else {
                return false;
            };
            let def = self.building_table.get(m.def);
            match def.kind {
                BuildingKind::Battery { capacity } => m.is_built() && m.battery < capacity,
                _ => m.is_built() && m.enabled && def.consumes_power(),
            }
        })
    }

    /// Opens the delivery and operating jobs buildings want (deduplicated by the board).
    fn regenerate_building_jobs(&mut self) {
        for id in self.buildings.ids() {
            if self.delivery_wanted(id).is_some() {
                self.jobs
                    .add(JobKind::Deliver { building: id }, DELIVER_PRIORITY);
            }
            if self.crank_wanted(id) {
                self.jobs
                    .add(JobKind::Operate { building: id }, OPERATE_PRIORITY);
            }
        }
    }

    /// Every tick: heaters warm their cell, oxygen generators turn stored feedstock into
    /// gas and heat. Each effect is a closed transfer plus an accounted injection.
    fn building_effects(&mut self) {
        for id in self.buildings.ids() {
            let b = self.buildings.get(id).unwrap().clone();
            if !b.is_built() || !b.enabled || !b.powered {
                continue;
            }
            let (x, y) = (b.x as usize, b.y as usize);
            match self.building_table.get(b.def).kind {
                BuildingKind::Heater { joules_per_tick } => {
                    let added = self.add_heat(x, y, joules_per_tick);
                    self.energy_injected += added;
                    self.buildings.get_mut(id).unwrap().blocked = added == 0;
                }
                BuildingKind::OxygenGenerator {
                    feed_per_tick,
                    output,
                    joules_per_tick,
                    ..
                } => {
                    let Some((fe, fm, ft)) = b.stored else {
                        self.buildings.get_mut(id).unwrap().blocked = true;
                        continue;
                    };
                    let (el, m, t) = self.world.get(x, y);
                    let room = el == ElementId::VACUUM
                        || (el == output
                            && m.raw() as u64 + feed_per_tick.raw() as u64
                                <= crate::network::GAS_CAP.raw() as u64);
                    if fm < feed_per_tick || !room {
                        self.buildings.get_mut(id).unwrap().blocked = true;
                        continue;
                    }
                    let feed = feed_per_tick.raw();
                    let before = self.cell_energy(x, y)
                        + (((feed as i64 * self.table.shc(fe.0)) >> 16) as i128 * ft.raw() as i128);
                    let (mass, temp) = if el == ElementId::VACUUM {
                        (Mass(feed), ft)
                    } else {
                        (
                            Mass(m.raw() + feed),
                            Temp(mix_temp(
                                m.raw() as u64,
                                t.raw(),
                                feed as u64,
                                feed as i128 * ft.raw() as i128,
                            )),
                        )
                    };
                    self.set_cell(x, y, output, mass, temp);
                    self.energy_injected += self.cell_energy(x, y) - before;
                    self.energy_injected += self.add_heat(x, y, joules_per_tick);
                    let g = self.buildings.get_mut(id).unwrap();
                    g.stored = (fm.raw() > feed).then_some((fe, Mass(fm.raw() - feed), ft));
                    g.blocked = false;
                }
                _ => {}
            }
        }
    }

    /// The buildings pass: network rebuilds and steps, every building's effect, then
    /// the jobs buildings want.
    pub(crate) fn buildings_step(&mut self) {
        self.step_networks();
        self.building_effects();
        self.regenerate_building_jobs();
    }
}
