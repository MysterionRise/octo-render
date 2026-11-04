//! Colonists: a small explicit state machine per colonist, a per-tick path budget, and
//! the agents pass that runs after the heat pass. Every edit a colonist makes to a cell
//! goes through `Sim::set_cell`, exactly like a command, so the gather passes see it next
//! tick and the conservation and determinism arguments are unchanged. Every gram a dig
//! removes becomes a debris item; every build consumes one; buildings take their cost
//! from a carried item and give it back on deconstruction.

use crate::element::ElementId;
use crate::entity::EntityId;
use crate::fixed::{Mass, Temp};
use crate::fluid::mix_temp;
use crate::item::Item;
use crate::job::{JobKind, JobState};
use crate::nav::{Goal, Path, NO_REGION};
use crate::sim::Sim;

/// Ticks one move between cells takes (5 cells per second at 10 Hz).
pub const MOVE_TICKS: u8 = 2;
pub const DIG_TICKS: u16 = 20;
pub const BUILD_TICKS: u16 = 20;
pub const DELIVER_TICKS: u16 = 10;
/// One stint at a hand crank; the building banks the ticks for its power step.
pub const OPERATE_TICKS: u16 = 10;
/// Path solves (assignment scans and route planning) per tick, shared by all colonists.
pub const PATH_BUDGET: u32 = 16;
/// At most this many of the budget go to job-assignment scans, so walkers always get
/// the rest for route planning.
pub const ASSIGN_BUDGET: u32 = 6;
/// An idle colonist re-scans the board because walkability changed only every this
/// many ticks (staggered by its index); a board change triggers a re-scan at once.
pub const RESCAN_PERIOD: u64 = 10;
/// Region distance cap for job assignment.
pub const ASSIGN_CAP: u32 = 1024;
pub const NO_JOB: u32 = u32::MAX;
pub const DEFAULT_PRIORITY: u8 = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Walking { goal: Goal, path: Option<Path> },
    Fetching { item: EntityId, path: Option<Path> },
    Working { ticks_left: u16 },
    Stuck,
}

impl AgentState {
    pub fn tag(&self) -> u8 {
        match self {
            AgentState::Idle => 0,
            AgentState::Walking { .. } => 1,
            AgentState::Fetching { .. } => 2,
            AgentState::Working { .. } => 3,
            AgentState::Stuck => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Colonist {
    pub cell: (u32, u32),
    pub from: (u32, u32),
    /// Ticks into the current move; `MOVE_TICKS` once arrived.
    pub progress: u8,
    pub state: AgentState,
    pub job: u32,
    pub carrying: EntityId,
    /// Job board version at the last assignment attempt.
    pub seen_board: u32,
}

impl Colonist {
    pub fn new(cell: (u32, u32)) -> Colonist {
        Colonist {
            cell,
            from: cell,
            progress: MOVE_TICKS,
            state: AgentState::Idle,
            job: NO_JOB,
            carrying: EntityId::NONE,
            seen_board: 0,
        }
    }
}

impl Sim {
    /// Adds a colonist on a non-solid, non-deep-liquid cell; gravity settles it.
    pub fn spawn_colonist(&mut self, x: usize, y: usize) -> Option<EntityId> {
        if x >= self.world.width || y >= self.world.height {
            return None;
        }
        let (element, mass, _) = self.world.get(x, y);
        let phase = element.phase();
        if phase == crate::element::Phase::Solid
            || (phase == crate::element::Phase::Liquid && mass.raw() >= crate::nav::DEEP_LIQUID)
        {
            return None;
        }
        Some(self.colonists.insert(Colonist::new((x as u32, y as u32))))
    }

    /// Adds a dig or build order; a dig needs a solid target and a build a non-solid one,
    /// neither may touch a building's cell, otherwise the order is ignored and `NO_JOB`
    /// returned. Building orders come from placement, not from here.
    pub fn order(&mut self, kind: JobKind, priority: u8) -> u32 {
        let Some((x, y)) = kind.cell() else {
            return NO_JOB;
        };
        if x as usize >= self.world.width || y as usize >= self.world.height {
            return NO_JOB;
        }
        let i = self.world.index(x as usize, y as usize);
        if self.cell_building[i] != crate::construct::NO_BUILDING {
            return NO_JOB;
        }
        let solid = ElementId(self.world.cells.element[i]).phase() == crate::element::Phase::Solid;
        let valid = match kind {
            JobKind::Dig { .. } => solid,
            JobKind::Build { .. } => !solid,
            _ => false,
        };
        if !valid {
            return NO_JOB;
        }
        self.jobs.add(kind, priority)
    }

    pub fn cancel_orders(&mut self, x: u32, y: u32) -> usize {
        self.jobs.cancel_at(x, y, self.tick)
    }

    pub fn set_priority(&mut self, x: u32, y: u32, priority: u8) -> bool {
        self.jobs.set_priority_at(x, y, priority)
    }

    /// Where a colonist stands to work on a job.
    pub(crate) fn job_goal(&self, kind: JobKind) -> Option<Goal> {
        match kind {
            JobKind::Dig { x, y } | JobKind::Build { x, y } => Some(Goal::Adjacent { x, y }),
            _ => {
                let (x, y, w, h) = self.building_rect(kind.building()?)?;
                Some(Goal::Near { x, y, w, h })
            }
        }
    }

    /// Ticks of work a job takes once the colonist is in place.
    fn work_ticks(&self, kind: JobKind) -> u16 {
        match kind {
            JobKind::Dig { .. } => DIG_TICKS,
            JobKind::Build { .. } => BUILD_TICKS,
            JobKind::Construct { building } => self
                .buildings
                .get(building)
                .map_or(1, |b| self.building_table.get(b.def).build_ticks.max(1)),
            JobKind::Deconstruct { building } => self.buildings.get(building).map_or(1, |b| {
                (self.building_table.get(b.def).build_ticks / 2).max(1)
            }),
            JobKind::Deliver { .. } => DELIVER_TICKS,
            JobKind::Operate { .. } => OPERATE_TICKS,
        }
    }

    fn job_valid(&self, job_id: u32) -> bool {
        let Some(job) = self.jobs.get(job_id) else {
            return false;
        };
        if !matches!(job.state, JobState::Reserved(_)) {
            return false;
        }
        match job.kind {
            JobKind::Dig { x, y } => self.nav.is_solid(x, y),
            JobKind::Build { x, y } => !self.nav.is_solid(x, y),
            JobKind::Construct { building } => {
                self.buildings.get(building).is_some_and(|b| !b.is_built())
            }
            JobKind::Deconstruct { building } => {
                self.buildings.get(building).is_some_and(|b| b.is_built())
            }
            JobKind::Deliver { building } => self.delivery_wanted(building).is_some(),
            JobKind::Operate { building } => self
                .buildings
                .get(building)
                .is_some_and(|b| b.is_built() && b.enabled && b.work < 2 * OPERATE_TICKS),
        }
    }

    /// Gives the colonist's job back and drops what it carries.
    fn release(&mut self, c: &mut Colonist) {
        if c.job != NO_JOB {
            if let Some(JobKind::Construct { building }) = self.jobs.get(c.job).map(|j| j.kind) {
                if let Some(b) = self.buildings.get_mut(building) {
                    if b.state == crate::building::BuildingState::UnderConstruction {
                        b.state = crate::building::BuildingState::Planned;
                    }
                }
            }
            self.jobs.release(c.job, self.tick);
        }
        if let AgentState::Fetching { item, .. } = c.state {
            if let Some(it) = self.items.get_mut(item) {
                it.reserved_by = EntityId::NONE;
            }
        }
        if c.carrying.is_some() {
            if let Some(it) = self.items.get_mut(c.carrying) {
                it.holder = EntityId::NONE;
                it.reserved_by = EntityId::NONE;
                it.x = c.cell.0;
                it.y = c.cell.1;
            }
            c.carrying = EntityId::NONE;
        }
        c.job = NO_JOB;
        c.state = AgentState::Idle;
    }

    fn settle_items(&mut self) {
        for id in self.items.ids() {
            let Some(it) = self.items.get(id) else {
                continue;
            };
            if !it.on_ground() {
                continue;
            }
            let (x, mut y) = (it.x, it.y);
            while y > 0 && !self.nav.is_solid(x, y - 1) && !self.nav.is_ladder(x, y) {
                y -= 1;
            }
            if y != it.y {
                self.items.get_mut(id).unwrap().y = y;
            }
        }
    }

    fn gravity(&mut self, c: &mut Colonist) {
        let (x, y) = c.cell;
        if self.nav.is_solid(x, y) && !self.nav.is_passable(x, y) {
            self.release(c);
            c.state = AgentState::Stuck;
            return;
        }
        if self.nav.is_standing(x, y) {
            return;
        }
        let mut k = 1;
        while k <= crate::nav::MAX_FALL && y >= k {
            let ny = y - k;
            if self.nav.is_standing(x, ny) {
                c.from = c.cell;
                c.cell = (x, ny);
                c.progress = 0;
                match &mut c.state {
                    AgentState::Walking { path, .. } | AgentState::Fetching { path, .. } => {
                        *path = None
                    }
                    AgentState::Working { .. } => {
                        let goal = self.jobs.get(c.job).and_then(|j| self.job_goal(j.kind));
                        match goal {
                            Some(goal) => c.state = AgentState::Walking { goal, path: None },
                            None => self.release(c),
                        }
                    }
                    _ => {}
                }
                return;
            }
            if !self.nav.is_passable(x, ny) {
                break;
            }
            k += 1;
        }
        self.release(c);
        c.state = AgentState::Stuck;
    }

    /// The nearest unreserved ground item of `wanted` (any element when `None`) with at
    /// least `min_mass`, by region distance from the last Dijkstra.
    fn nearest_item(&self, wanted: Option<ElementId>, min_mass: Mass) -> Option<(u32, EntityId)> {
        let mut nearest: Option<(u32, EntityId)> = None;
        for (iid, it) in self.items.iter() {
            if !it.on_ground()
                || it.reserved_by.is_some()
                || it.mass < min_mass
                || wanted.is_some_and(|e| e != it.element)
            {
                continue;
            }
            let r = self.nav.region_of(it.x, it.y);
            if r == NO_REGION {
                continue;
            }
            if let Some(d) = self.nav.region_dist(r) {
                if nearest.is_none_or(|(bd, _)| d < bd) {
                    nearest = Some((d, iid));
                }
            }
        }
        nearest
    }

    /// Picks the best open job for an idle colonist; costs one budget unit.
    fn try_assign(
        &mut self,
        id: EntityId,
        c: &mut Colonist,
        budget: &mut u32,
        assign_left: &mut u32,
        solves: &mut u32,
    ) {
        let start = self.nav.region_of(c.cell.0, c.cell.1);
        if start == NO_REGION || *budget == 0 || *assign_left == 0 {
            return;
        }
        *budget -= 1;
        *assign_left -= 1;
        *solves += 1;
        c.seen_board = self.jobs.version;
        self.nav.dijkstra_regions(start, ASSIGN_CAP);
        let mut best: Option<(u32, u32, Option<EntityId>)> = None; // (score, job id, item)
        for job in self.jobs.open_jobs() {
            let Some(goal) = self.job_goal(job.kind) else {
                continue;
            };
            let target_dist = self
                .nav
                .goal_regions(goal)
                .iter()
                .filter_map(|r| self.nav.region_dist(*r))
                .min();
            let Some(target_dist) = target_dist else {
                continue;
            };
            let wanted = match job.kind {
                JobKind::Dig { .. } | JobKind::Deconstruct { .. } | JobKind::Operate { .. } => None,
                JobKind::Build { .. } => Some((None, Mass(1))),
                JobKind::Construct { building } => self
                    .buildings
                    .get(building)
                    .and_then(|b| self.building_table.get(b.def).cost)
                    .map(|(e, m)| (Some(e), m)),
                JobKind::Deliver { building } => match self.delivery_wanted(building) {
                    Some((e, _)) => Some((e, Mass(1))),
                    None => continue,
                },
            };
            let (dist, item) = match wanted {
                None => (target_dist, None),
                Some((element, min_mass)) => {
                    let Some((d, iid)) = self.nearest_item(element, min_mass) else {
                        continue;
                    };
                    (d + target_dist, Some(iid))
                }
            };
            let score = job.priority as u32 * 256 / (1 + dist);
            if best.is_none_or(|(bs, bid, _)| score > bs || (score == bs && job.id < bid)) {
                best = Some((score, job.id, item));
            }
        }
        let Some((_, job_id, item)) = best else {
            return;
        };
        if !self.jobs.reserve(job_id, id) {
            return;
        }
        c.job = job_id;
        let goal = self
            .job_goal(self.jobs.get(job_id).unwrap().kind)
            .expect("scored jobs have goals");
        match item {
            Some(iid) => {
                self.items.get_mut(iid).unwrap().reserved_by = id;
                c.state = AgentState::Fetching {
                    item: iid,
                    path: None,
                };
            }
            None => {
                c.state = AgentState::Walking { goal, path: None };
            }
        }
    }

    fn walk(&mut self, id: EntityId, c: &mut Colonist, budget: &mut u32, solves: &mut u32) {
        if c.progress < MOVE_TICKS {
            c.progress += 1;
            if c.progress < MOVE_TICKS {
                return;
            }
        }
        if !self.job_valid(c.job) {
            self.release(c);
            return;
        }
        let goal = match &c.state {
            AgentState::Walking { goal, .. } => *goal,
            AgentState::Fetching { item, .. } => match self.items.get(*item) {
                Some(it) if it.on_ground() && it.reserved_by == id => {
                    Goal::Cell { x: it.x, y: it.y }
                }
                _ => {
                    self.release(c);
                    return;
                }
            },
            _ => return,
        };
        let changed = self.nav.changed_chunks();
        let needs_plan = match &c.state {
            AgentState::Walking { path, .. } | AgentState::Fetching { path, .. } => {
                path.as_ref().is_none_or(|p| p.crosses_changed(changed))
            }
            _ => false,
        };
        if needs_plan {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            *solves += 1;
            let planned = self.nav.path_to(c.cell, goal);
            match planned {
                Some(p) => match &mut c.state {
                    AgentState::Walking { path, .. } | AgentState::Fetching { path, .. } => {
                        *path = Some(p)
                    }
                    _ => {}
                },
                None => {
                    self.release(c);
                    return;
                }
            }
        }
        let next = match &mut c.state {
            AgentState::Walking { path, .. } | AgentState::Fetching { path, .. } => {
                path.as_mut().and_then(|p| p.advance())
            }
            _ => None,
        };
        match next {
            None => {
                // Arrived.
                let kind = self.jobs.get(c.job).unwrap().kind;
                match c.state.clone() {
                    AgentState::Fetching { item, .. } => {
                        let it = self.items.get_mut(item).unwrap();
                        it.holder = id;
                        c.carrying = item;
                        let goal = self.job_goal(kind).expect("a reserved job has a goal");
                        c.state = AgentState::Walking { goal, path: None };
                    }
                    AgentState::Walking { .. } => {
                        if let JobKind::Construct { building } = kind {
                            if let Some(b) = self.buildings.get_mut(building) {
                                b.state = crate::building::BuildingState::UnderConstruction;
                            }
                        }
                        c.state = AgentState::Working {
                            ticks_left: self.work_ticks(kind),
                        };
                    }
                    _ => {}
                }
            }
            Some(step) => {
                if self.nav.is_legal_move(c.cell, step) {
                    c.from = c.cell;
                    c.cell = step;
                    c.progress = 0;
                } else if let AgentState::Walking { path, .. } | AgentState::Fetching { path, .. } =
                    &mut c.state
                {
                    *path = None;
                }
            }
        }
    }

    /// Moves the fluid in `cell` into a neighbouring passable cell holding the same
    /// element or vacuum (the colonist's own cell first, then down, left, right, up),
    /// never into `avoid`; `false` when nothing can take it.
    pub(crate) fn displace_fluid(
        &mut self,
        cell: (u32, u32),
        avoid: &[(u32, u32)],
        prefer: (u32, u32),
    ) -> bool {
        let (tx, ty) = cell;
        let (fluid, fmass, ftemp) = self.world.get(tx as usize, ty as usize);
        if fmass.raw() == 0 {
            return true;
        }
        let candidates = [
            prefer,
            (tx, ty.wrapping_sub(1)),
            (tx.wrapping_sub(1), ty),
            (tx + 1, ty),
            (tx, ty + 1),
        ];
        let (w, h) = (self.world.width as u32, self.world.height as u32);
        let receiver = candidates.into_iter().find(|&(x, y)| {
            x < w && y < h && !avoid.contains(&(x, y)) && self.nav.is_passable(x, y) && {
                let (el, _, _) = self.world.get(x as usize, y as usize);
                el == fluid || el == ElementId::VACUUM
            }
        });
        let Some((rx, ry)) = receiver else {
            return false;
        };
        let (_, rmass, rtemp) = self.world.get(rx as usize, ry as usize);
        let total = rmass.raw() as u64 + fmass.raw() as u64;
        assert!(total <= u32::MAX as u64, "fluid displacement overflow");
        let temp = mix_temp(
            rmass.raw() as u64,
            rtemp.raw(),
            fmass.raw() as u64,
            fmass.raw() as i128 * ftemp.raw() as i128,
        );
        self.set_cell(
            rx as usize,
            ry as usize,
            fluid,
            Mass(total as u32),
            Temp(temp),
        );
        self.set_cell(
            tx as usize,
            ty as usize,
            ElementId::VACUUM,
            Mass::ZERO,
            Temp::ZERO,
        );
        true
    }

    fn work(&mut self, c: &mut Colonist) {
        if !self.job_valid(c.job) {
            self.release(c);
            return;
        }
        let job = *self.jobs.get(c.job).unwrap();
        let Some(goal) = self.job_goal(job.kind) else {
            self.release(c);
            return;
        };
        if !goal.reached_from(c.cell.0, c.cell.1) {
            c.state = AgentState::Walking { goal, path: None };
            return;
        }
        let AgentState::Working { ticks_left } = &mut c.state else {
            return;
        };
        *ticks_left -= 1;
        if *ticks_left > 0 {
            return;
        }
        let done = match job.kind {
            JobKind::Dig { x: tx, y: ty } => {
                let (element, mass, temp) = self.world.get(tx as usize, ty as usize);
                self.items.insert(Item {
                    x: tx,
                    y: ty,
                    element,
                    mass,
                    temp,
                    holder: EntityId::NONE,
                    reserved_by: EntityId::NONE,
                });
                self.set_cell(
                    tx as usize,
                    ty as usize,
                    ElementId::VACUUM,
                    Mass::ZERO,
                    Temp::ZERO,
                );
                true
            }
            JobKind::Build { x: tx, y: ty } => {
                let Some(item) = self.items.get(c.carrying).copied() else {
                    self.release(c);
                    return;
                };
                let occupied = self.colonists.iter().any(|(_, o)| {
                    o.cell == (tx, ty) || (o.from == (tx, ty) && o.progress < MOVE_TICKS)
                }) || self
                    .items
                    .iter()
                    .any(|(_, it)| it.on_ground() && (it.x, it.y) == (tx, ty));
                if occupied || !self.displace_fluid((tx, ty), &[(tx, ty)], c.cell) {
                    self.release(c);
                    return;
                }
                self.set_cell(tx as usize, ty as usize, item.element, item.mass, item.temp);
                self.items.remove(c.carrying);
                c.carrying = EntityId::NONE;
                true
            }
            JobKind::Construct { building } => self.complete_construct(c, building),
            JobKind::Deconstruct { building } => self.complete_deconstruct(c, building),
            JobKind::Deliver { building } => self.complete_deliver(c, building),
            JobKind::Operate { building } => match self.buildings.get_mut(building) {
                Some(b) => {
                    b.work = b.work.saturating_add(OPERATE_TICKS);
                    true
                }
                None => false,
            },
        };
        if !done {
            self.release(c);
            return;
        }
        self.jobs.complete(c.job, self.tick);
        c.job = NO_JOB;
        c.state = AgentState::Idle;
    }

    fn agent_tick(
        &mut self,
        id: EntityId,
        nav_changed: bool,
        budget: &mut u32,
        assign_left: &mut u32,
        solves: &mut u32,
    ) {
        let Some(mut c) = self.colonists.get(id).cloned() else {
            return;
        };
        self.gravity(&mut c);
        match c.state {
            AgentState::Stuck => {
                if self.nav.is_standing(c.cell.0, c.cell.1) {
                    c.state = AgentState::Idle;
                }
            }
            AgentState::Idle => {
                if c.job != NO_JOB {
                    match self.jobs.get(c.job).and_then(|j| self.job_goal(j.kind)) {
                        Some(goal) => c.state = AgentState::Walking { goal, path: None },
                        None => self.release(&mut c),
                    }
                } else {
                    let due = (self.tick + id.index as u64).is_multiple_of(RESCAN_PERIOD);
                    if c.seen_board != self.jobs.version || (nav_changed && due) {
                        self.try_assign(id, &mut c, budget, assign_left, solves);
                    }
                    if matches!(
                        c.state,
                        AgentState::Walking { .. } | AgentState::Fetching { .. }
                    ) {
                        self.walk(id, &mut c, budget, solves);
                    }
                }
            }
            AgentState::Walking { .. } | AgentState::Fetching { .. } => {
                self.walk(id, &mut c, budget, solves)
            }
            AgentState::Working { .. } => self.work(&mut c),
        }
        if let Some(slot) = self.colonists.get_mut(id) {
            *slot = c;
        }
    }

    /// The agents pass: refresh walkability from this tick's processed chunks, settle
    /// items, then run every colonist in a rotating index order under a shared path
    /// budget. Returns the number of path solves.
    pub(crate) fn agents_step(&mut self) -> u32 {
        self.nav.refresh(
            &self.world.cells,
            &self.table,
            &self.special,
            &self.process_heat,
        );
        self.settle_items();
        let nav_changed = self.nav.changed_chunks().iter().any(|c| *c);
        let ids = self.colonists.ids();
        let n = ids.len();
        let mut budget = PATH_BUDGET;
        let mut assign_left = ASSIGN_BUDGET;
        let mut solves = 0;
        if n > 0 {
            let start = (self.tick % n as u64) as usize;
            for k in 0..n {
                let id = ids[(start + k) % n];
                self.agent_tick(id, nav_changed, &mut budget, &mut assign_left, &mut solves);
            }
        }
        self.nav.clear_changed();
        solves
    }
}
