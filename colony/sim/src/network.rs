//! Networks (section 9 of the design document): power components over wire, pipe runs
//! that carry packets toward vents, and automation signals. They are derived from the
//! buildings, rebuilt only when buildings were placed, removed, or finished, and
//! stepped every [`NETWORK_STRIDE`] ticks inside the buildings pass. Every step reads
//! kernel state in slot order, so it is deterministic and a loaded save rebuilds the
//! same networks.

use crate::building::{BuildingKind, Packet};
use crate::construct::NO_BUILDING;
use crate::element::{ElementId, Phase};
use crate::fixed::{Mass, Temp};
use crate::fluid::mix_temp;
use crate::heat::{pair_flow, Body};
use crate::sim::Sim;

/// Networks step on ticks that are multiples of this (5 Hz at a 10 Hz tick).
pub const NETWORK_STRIDE: u64 = 2;
/// How much gas a vent may bring a cell up to (and a pump's cell may hold).
pub const GAS_CAP: Mass = Mass::from_kg(8);
/// Pipe cells without a path to a vent.
pub const NO_DISTANCE: u32 = u32::MAX;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PowerComponent {
    /// Every member slot (wires and attached buildings), ascending.
    pub members: Vec<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PipeNet {
    /// `(distance to the nearest vent, slot)`, ascending.
    pub cells: Vec<(u32, u32)>,
    /// Per entry of `cells`: the slot a packet moves to next, or `NO_BUILDING`.
    pub next: Vec<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignalNet {
    /// Sensors and gates, ascending; the last one is the network's output.
    pub logic: Vec<u32>,
    /// Buildings the network switches on and off, ascending.
    pub controlled: Vec<u32>,
}

/// What the last network step did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetworkStats {
    pub power_components: u32,
    pub pipe_networks: u32,
    pub signal_networks: u32,
    /// Q16.16 joules generated and consumed in the last power step.
    pub generated: i64,
    pub consumed: i64,
    /// Consumers left unpowered in the last power step.
    pub brownouts: u32,
    /// Packets in pipes after the last pipe step.
    pub packets: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Networks {
    pub power: Vec<PowerComponent>,
    pub pipes: Vec<PipeNet>,
    pub signals: Vec<SignalNet>,
    /// Per building slot, its power component index, or `NO_BUILDING`.
    pub component_of: Vec<u32>,
    pub stats: NetworkStats,
}

/// Union-find over building slots.
struct Union {
    parent: Vec<u32>,
}

impl Union {
    fn new(n: usize) -> Union {
        Union {
            parent: (0..n as u32).collect(),
        }
    }

    fn find(&mut self, i: u32) -> u32 {
        let mut r = i;
        while self.parent[r as usize] != r {
            r = self.parent[r as usize];
        }
        let mut c = i;
        while self.parent[c as usize] != r {
            let next = self.parent[c as usize];
            self.parent[c as usize] = r;
            c = next;
        }
        r
    }

    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            // The lower root wins, so group ids are a function of membership.
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            self.parent[hi as usize] = lo;
        }
    }
}

impl Sim {
    /// The slot of a built building of a kind selected by `pred` at a cell.
    fn built_slot_at(&self, x: i64, y: i64, pred: impl Fn(&BuildingKind) -> bool) -> Option<u32> {
        if x < 0 || y < 0 || x as usize >= self.world.width || y as usize >= self.world.height {
            return None;
        }
        let slot = self.cell_building[self.world.index(x as usize, y as usize)];
        if slot == NO_BUILDING {
            return None;
        }
        let (_, b) = self.buildings.slot(slot as usize);
        let b = b?;
        (b.is_built() && pred(&self.building_table.get(b.def).kind)).then_some(slot)
    }

    /// Groups built buildings whose cells of `pred` kind touch, plus (when `attach`) any
    /// other built building whose footprint neighbours such a cell.
    fn group(&self, pred: impl Fn(&BuildingKind) -> bool + Copy, attach: bool) -> Vec<Vec<u32>> {
        let n = self.buildings.slot_count();
        let mut uf = Union::new(n);
        let mut in_graph = vec![false; n];
        for (id, b) in self.buildings.iter() {
            if !b.is_built() {
                continue;
            }
            let def = self.building_table.get(b.def);
            let is_cell = pred(&def.kind);
            for (x, y) in b.footprint(def) {
                for (nx, ny) in [
                    (x as i64 - 1, y as i64),
                    (x as i64 + 1, y as i64),
                    (x as i64, y as i64 - 1),
                    (x as i64, y as i64 + 1),
                ] {
                    if let Some(other) = self.built_slot_at(nx, ny, pred) {
                        if is_cell || attach {
                            uf.union(id.index, other);
                            in_graph[id.index as usize] = true;
                            in_graph[other as usize] = true;
                        }
                    }
                }
            }
            if is_cell {
                in_graph[id.index as usize] = true;
            }
        }
        let mut groups: Vec<(u32, Vec<u32>)> = Vec::new();
        for slot in 0..n as u32 {
            if !in_graph[slot as usize] {
                continue;
            }
            let root = uf.find(slot);
            match groups.iter_mut().find(|(r, _)| *r == root) {
                Some((_, members)) => members.push(slot),
                None => groups.push((root, vec![slot])),
            }
        }
        groups.sort_by_key(|(r, _)| *r);
        groups.into_iter().map(|(_, m)| m).collect()
    }

    /// Rebuilds every network from the buildings.
    pub(crate) fn rebuild_networks(&mut self) {
        let n = self.buildings.slot_count();
        let mut component_of = vec![NO_BUILDING; n];
        let power: Vec<PowerComponent> = self
            .group(|k| k.is_wire(), true)
            .into_iter()
            .map(|members| PowerComponent { members })
            .collect();
        for (i, c) in power.iter().enumerate() {
            for &m in &c.members {
                component_of[m as usize] = i as u32;
            }
        }
        let pipes = self
            .group(|k| k.is_pipe_cell(), false)
            .into_iter()
            .map(|members| self.pipe_net(members))
            .collect();
        let signals = self
            .group(|k| k.is_signal_cell(), true)
            .into_iter()
            .map(|members| {
                let (logic, controlled): (Vec<u32>, Vec<u32>) =
                    members.into_iter().partition(|&s| {
                        let kind = &self
                            .building_table
                            .get(self.buildings.slot(s as usize).1.unwrap().def)
                            .kind;
                        kind.is_sensor() || kind.is_gate()
                    });
                // Plain wires and pipes carry no switch; everything else next to a
                // signal cell is controlled.
                let controlled = controlled
                    .into_iter()
                    .filter(|&s| {
                        let kind = &self
                            .building_table
                            .get(self.buildings.slot(s as usize).1.unwrap().def)
                            .kind;
                        !kind.is_signal_cell() && !kind.is_wire() && *kind != BuildingKind::Pipe
                    })
                    .collect();
                SignalNet { logic, controlled }
            })
            .collect();
        let stats = self.networks.stats;
        self.networks = Networks {
            power,
            pipes,
            signals,
            component_of,
            stats,
        };
    }

    /// Orders a pipe group by distance to its vents and records where packets go next.
    fn pipe_net(&self, members: Vec<u32>) -> PipeNet {
        let cell_of = |slot: u32| {
            let b = self.buildings.slot(slot as usize).1.unwrap();
            (b.x as i64, b.y as i64)
        };
        let mut dist = vec![NO_DISTANCE; members.len()];
        let mut queue: Vec<usize> = Vec::new();
        for (i, &s) in members.iter().enumerate() {
            let b = self.buildings.slot(s as usize).1.unwrap();
            if self.building_table.get(b.def).kind == BuildingKind::Vent {
                dist[i] = 0;
                queue.push(i);
            }
        }
        let index_of = |slot: u32| members.binary_search(&slot).ok();
        let mut head = 0;
        while head < queue.len() {
            let i = queue[head];
            head += 1;
            let (x, y) = cell_of(members[i]);
            for (nx, ny) in [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)] {
                if let Some(j) = self
                    .built_slot_at(nx, ny, |k| k.is_pipe_cell())
                    .and_then(index_of)
                {
                    if dist[j] == NO_DISTANCE {
                        dist[j] = dist[i] + 1;
                        queue.push(j);
                    }
                }
            }
        }
        let mut cells: Vec<(u32, u32)> = members
            .iter()
            .enumerate()
            .map(|(i, &s)| (dist[i], s))
            .collect();
        cells.sort_unstable();
        let next = cells
            .iter()
            .map(|&(d, s)| {
                if d == 0 || d == NO_DISTANCE {
                    return NO_BUILDING;
                }
                let (x, y) = cell_of(s);
                [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)]
                    .into_iter()
                    .filter_map(|(nx, ny)| self.built_slot_at(nx, ny, |k| k.is_pipe_cell()))
                    .filter(|&n| index_of(n).is_some_and(|j| dist[j] + 1 == d))
                    .min()
                    .unwrap_or(NO_BUILDING)
            })
            .collect();
        PipeNet { cells, next }
    }

    /// The power step: per component, generation against demand, consumers powered in
    /// slot order until the supply runs out, batteries charged by the surplus or
    /// drained by the deficit.
    pub(crate) fn step_power(&mut self) {
        let stride = NETWORK_STRIDE as i64;
        let mut generated_total = 0i64;
        let mut consumed_total = 0i64;
        let mut brownouts = 0u32;
        for ci in 0..self.networks.power.len() {
            let members = self.networks.power[ci].members.clone();
            let mut generation = 0i64;
            let mut stored = 0i64;
            for &s in &members {
                let b = self.buildings.get_mut_slot(s).unwrap();
                if !b.enabled {
                    continue;
                }
                let def = self.building_table.get(b.def);
                match def.kind {
                    BuildingKind::HandCrank { joules_per_tick } => {
                        let ticks = (b.work as i64).min(stride);
                        b.work -= ticks as u16;
                        generation += joules_per_tick * ticks;
                    }
                    BuildingKind::Battery { .. } => stored += b.battery,
                    _ => {
                        if def.power_per_tick > 0 {
                            generation += def.power_per_tick * stride;
                        }
                    }
                }
            }
            let mut available = generation + stored;
            let mut consumed = 0i64;
            for &s in &members {
                let b = self.buildings.get_mut_slot(s).unwrap();
                let def = self.building_table.get(b.def);
                if !def.consumes_power() {
                    b.powered = true;
                    continue;
                }
                if !b.enabled {
                    b.powered = false;
                    continue;
                }
                let need = -def.power_per_tick * stride;
                if available >= need {
                    available -= need;
                    consumed += need;
                    b.powered = true;
                } else {
                    b.powered = false;
                    brownouts += 1;
                }
            }
            let mut delta = generation - consumed;
            for &s in &members {
                let b = self.buildings.get_mut_slot(s).unwrap();
                if let BuildingKind::Battery { capacity } = self.building_table.get(b.def).kind {
                    if delta >= 0 {
                        let room = (capacity - b.battery).max(0);
                        let charge = delta.min(room);
                        b.battery += charge;
                        delta -= charge;
                    } else {
                        let drain = (-delta).min(b.battery);
                        b.battery -= drain;
                        delta += drain;
                    }
                }
            }
            generated_total += generation;
            consumed_total += consumed;
        }
        for id in self.buildings.ids() {
            if self
                .networks
                .component_of
                .get(id.index as usize)
                .copied()
                .unwrap_or(NO_BUILDING)
                == NO_BUILDING
            {
                let b = self.buildings.get_mut(id).unwrap();
                b.powered = !self.building_table.get(b.def).consumes_power();
            }
        }
        self.networks.stats.generated = generated_total;
        self.networks.stats.consumed = consumed_total;
        self.networks.stats.brownouts = brownouts;
    }

    /// Whether a vent can put a packet into its cell: vacuum, or the same gas with room.
    fn vent_room(&self, x: usize, y: usize, packet: Packet) -> bool {
        let (el, m, _) = self.world.get(x, y);
        if el == ElementId::VACUUM {
            return true;
        }
        if el != packet.element || el.phase() != Phase::Gas {
            return false;
        }
        let cap = self.table.max_mass(el.0);
        let cap = if cap == u32::MAX { GAS_CAP.raw() } else { cap };
        m.raw() as u64 + packet.mass.raw() as u64 <= cap as u64
    }

    /// The pipe step: vents emit, packets advance one cell toward the nearest vent,
    /// pumps draw a packet from their cell, and every packet exchanges heat with the
    /// cell it sits in.
    pub(crate) fn step_pipes(&mut self) {
        let stride = NETWORK_STRIDE as u32;
        let mut packets = 0u32;
        for ni in 0..self.networks.pipes.len() {
            let net = self.networks.pipes[ni].clone();
            for (i, &(d, s)) in net.cells.iter().enumerate() {
                let b = self.buildings.get_mut_slot(s).unwrap();
                let (x, y) = (b.x as usize, b.y as usize);
                let kind = self.building_table.get(b.def).kind.clone();
                let Some(packet) = b.packet else {
                    continue;
                };
                if d == 0 && kind == BuildingKind::Vent {
                    if !b.enabled {
                        b.blocked = true;
                        continue;
                    }
                    let room = self.vent_room(x, y, packet);
                    if room {
                        let (el, m, t) = self.world.get(x, y);
                        let (mass, temp) = if el == ElementId::VACUUM {
                            (packet.mass, packet.temp)
                        } else {
                            (
                                Mass(m.raw() + packet.mass.raw()),
                                Temp(mix_temp(
                                    m.raw() as u64,
                                    t.raw(),
                                    packet.mass.raw() as u64,
                                    packet.mass.raw() as i128 * packet.temp.raw() as i128,
                                )),
                            )
                        };
                        self.set_cell(x, y, packet.element, mass, temp);
                        let b = self.buildings.get_mut_slot(s).unwrap();
                        b.packet = None;
                        b.blocked = false;
                    } else {
                        self.buildings.get_mut_slot(s).unwrap().blocked = true;
                    }
                    continue;
                }
                let next = net.next[i];
                if next == NO_BUILDING {
                    self.buildings.get_mut_slot(s).unwrap().blocked = true;
                    continue;
                }
                if self.buildings.get_mut_slot(next).unwrap().packet.is_none() {
                    self.buildings.get_mut_slot(next).unwrap().packet = Some(packet);
                    let b = self.buildings.get_mut_slot(s).unwrap();
                    b.packet = None;
                    b.blocked = false;
                } else {
                    self.buildings.get_mut_slot(s).unwrap().blocked = true;
                }
            }
            // Pumps draw after the train moved, so a pump's own cell may be free again.
            for &(_, s) in &net.cells {
                let b = self.buildings.slot(s as usize).1.unwrap();
                let BuildingKind::GasPump { per_tick } = self.building_table.get(b.def).kind else {
                    continue;
                };
                if !b.enabled || !b.powered || b.packet.is_some() {
                    continue;
                }
                let (x, y) = (b.x as usize, b.y as usize);
                let (el, m, t) = self.world.get(x, y);
                if el.phase() != Phase::Gas || m.raw() == 0 {
                    self.buildings.get_mut_slot(s).unwrap().blocked = true;
                    continue;
                }
                let take = (per_tick.raw().saturating_mul(stride)).min(m.raw());
                let pump = self.buildings.get_mut_slot(s).unwrap();
                pump.packet = Some(Packet {
                    element: el,
                    mass: Mass(take),
                    temp: t,
                });
                pump.blocked = false;
                self.set_cell(x, y, el, Mass(m.raw() - take), t);
            }
            // Heat exchange between each packet and the cell around it.
            for &(_, s) in &net.cells {
                let b = self.buildings.slot(s as usize).1.unwrap();
                let Some(packet) = b.packet else {
                    continue;
                };
                packets += 1;
                let (x, y) = (b.x as usize, b.y as usize);
                let (el, m, t) = self.world.get(x, y);
                let body_p = Body {
                    cap: (packet.mass.raw() as i64 * self.table.shc(packet.element.0)) >> 16,
                    conduct: self.table.conduct(packet.element.0),
                    phase: packet.element.phase(),
                    temp: packet.temp.raw(),
                };
                let body_c = Body {
                    cap: (m.raw() as i64 * self.table.shc(el.0)) >> 16,
                    conduct: self.table.conduct(el.0),
                    phase: el.phase(),
                    temp: t.raw(),
                };
                let q = pair_flow(&self.table, body_p, body_c) as i128;
                if q == 0 {
                    continue;
                }
                let sign: i128 = if body_c.temp > body_p.temp { 1 } else { -1 };
                let dp = ((sign * q) << 16) / body_p.cap as i128;
                let dc = ((-sign * q) << 16) / body_c.cap as i128;
                let new_p = (packet.temp.raw() as i128 + dp)
                    .clamp(i32::MIN as i128, i32::MAX as i128) as i32;
                let new_c = (t.raw() as i128 + dc).clamp(i32::MIN as i128, i32::MAX as i128) as i32;
                self.buildings.get_mut_slot(s).unwrap().packet = Some(Packet {
                    temp: Temp(new_p),
                    ..packet
                });
                if new_c != t.raw() {
                    self.set_cell(x, y, el, m, Temp(new_c));
                }
            }
        }
        self.networks.stats.packets = packets;
    }

    /// The automation step: sensors sample their cells, gates compute from the previous
    /// step's signals of lower-slot members (one-step latency), the last sensor or gate
    /// drives every controlled building's enabled switch.
    pub(crate) fn step_signals(&mut self) {
        for ni in 0..self.networks.signals.len() {
            let net = self.networks.signals[ni].clone();
            let previous: Vec<(u32, bool)> = net
                .logic
                .iter()
                .map(|&s| (s, self.buildings.slot(s as usize).1.unwrap().signal))
                .collect();
            let mut new_signals = Vec::with_capacity(net.logic.len());
            for (i, &s) in net.logic.iter().enumerate() {
                let b = self.buildings.slot(s as usize).1.unwrap();
                let kind = self.building_table.get(b.def).kind.clone();
                let inputs = previous[..i].iter().map(|(_, v)| *v);
                let value = match kind {
                    BuildingKind::TemperatureSensor { above } => {
                        self.world.get(b.x as usize, b.y as usize).2 > above
                    }
                    BuildingKind::PressureSensor { above } => {
                        let (el, m, _) = self.world.get(b.x as usize, b.y as usize);
                        el.phase() == Phase::Gas && m > above
                    }
                    BuildingKind::And => i > 0 && inputs.clone().all(|v| v),
                    BuildingKind::Or => inputs.clone().any(|v| v),
                    BuildingKind::Not => previous[..i].last().is_some_and(|(_, v)| !*v),
                    _ => false,
                };
                new_signals.push(value && b.enabled);
            }
            for (&s, &v) in net.logic.iter().zip(&new_signals) {
                self.buildings.get_mut_slot(s).unwrap().signal = v;
            }
            if let Some(&output) = new_signals.last() {
                for &s in &net.controlled {
                    self.buildings.get_mut_slot(s).unwrap().enabled = output;
                }
            }
        }
    }

    /// Rebuilds the networks when buildings changed and steps them on stride ticks.
    pub(crate) fn step_networks(&mut self) {
        if self.networks_dirty {
            self.rebuild_networks();
            self.networks_dirty = false;
        }
        self.networks.stats.power_components = self.networks.power.len() as u32;
        self.networks.stats.pipe_networks = self.networks.pipes.len() as u32;
        self.networks.stats.signal_networks = self.networks.signals.len() as u32;
        if !self.tick.is_multiple_of(NETWORK_STRIDE) {
            return;
        }
        self.step_signals();
        self.step_power();
        self.step_pipes();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_find_groups_by_lowest_root() {
        let mut uf = Union::new(6);
        uf.union(4, 2);
        uf.union(2, 5);
        uf.union(0, 3);
        assert_eq!(uf.find(5), 2);
        assert_eq!(uf.find(4), 2);
        assert_eq!(uf.find(3), 0);
        assert_eq!(uf.find(1), 1);
        uf.union(5, 1);
        assert_eq!(uf.find(2), 1);
    }
}
