//! Colonists: a small explicit state machine per colonist, a per-tick path budget, and
//! the agents pass that runs after the heat pass. Every edit a colonist makes to a cell
//! goes through `Sim::set_cell`, exactly like a command, so the gather passes see it next
//! tick and the conservation and determinism arguments are unchanged. Every gram a dig
//! removes becomes a debris item; every build consumes one.

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
pub const DIG_TICKS: u8 = 20;
pub const BUILD_TICKS: u8 = 20;
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
    Working { ticks_left: u8 },
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

fn adjacent(a: (u32, u32), b: (u32, u32)) -> bool {
    (a.0 == b.0 && a.1.abs_diff(b.1) == 1) || (a.1 == b.1 && a.0.abs_diff(b.0) == 1)
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
    /// otherwise the order is ignored and `NO_JOB` returned.
    pub fn order(&mut self, kind: JobKind, priority: u8) -> u32 {
        let (x, y) = kind.target();
        if x as usize >= self.world.width || y as usize >= self.world.height {
            return NO_JOB;
        }
        let solid =
            self.world.get(x as usize, y as usize).0.phase() == crate::element::Phase::Solid;
        let valid = match kind {
            JobKind::Dig { .. } => solid,
            JobKind::Build { .. } => !solid,
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

    fn job_valid(&self, job_id: u32) -> bool {
        let Some(job) = self.jobs.get(job_id) else {
            return false;
        };
        if !matches!(job.state, JobState::Reserved(_)) {
            return false;
        }
        let (x, y) = job.kind.target();
        let solid = self.nav.is_solid(x, y);
        match job.kind {
            JobKind::Dig { .. } => solid,
            JobKind::Build { .. } => !solid,
        }
    }

    /// Gives the colonist's job back and drops what it carries.
    fn release(&mut self, c: &mut Colonist) {
        if c.job != NO_JOB {
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
            while y > 0 && !self.nav.is_solid(x, y - 1) {
                y -= 1;
            }
            if y != it.y {
                self.items.get_mut(id).unwrap().y = y;
            }
        }
    }

    fn gravity(&mut self, c: &mut Colonist) {
        let (x, y) = c.cell;
        if self.nav.is_solid(x, y) {
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
                        if let Some(job) = self.jobs.get(c.job) {
                            let (tx, ty) = job.kind.target();
                            c.state = AgentState::Walking {
                                goal: Goal::Adjacent { x: tx, y: ty },
                                path: None,
                            };
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
            let (tx, ty) = job.kind.target();
            let goal = Goal::Adjacent { x: tx, y: ty };
            let target_dist = self
                .nav
                .goal_regions(goal)
                .iter()
                .filter_map(|r| self.nav.region_dist(*r))
                .min();
            let Some(target_dist) = target_dist else {
                continue;
            };
            let (dist, item) = match job.kind {
                JobKind::Dig { .. } => (target_dist, None),
                JobKind::Build { .. } => {
                    let mut nearest: Option<(u32, EntityId)> = None;
                    for (iid, it) in self.items.iter() {
                        if !it.on_ground() || it.reserved_by.is_some() {
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
                    let Some((d, iid)) = nearest else {
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
        let (tx, ty) = self.jobs.get(job_id).unwrap().kind.target();
        match item {
            Some(iid) => {
                self.items.get_mut(iid).unwrap().reserved_by = id;
                c.state = AgentState::Fetching {
                    item: iid,
                    path: None,
                };
            }
            None => {
                c.state = AgentState::Walking {
                    goal: Goal::Adjacent { x: tx, y: ty },
                    path: None,
                };
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
                match c.state.clone() {
                    AgentState::Fetching { item, .. } => {
                        let it = self.items.get_mut(item).unwrap();
                        it.holder = id;
                        c.carrying = item;
                        let (tx, ty) = self.jobs.get(c.job).unwrap().kind.target();
                        c.state = AgentState::Walking {
                            goal: Goal::Adjacent { x: tx, y: ty },
                            path: None,
                        };
                    }
                    AgentState::Walking { .. } => {
                        let ticks = match self.jobs.get(c.job).unwrap().kind {
                            JobKind::Dig { .. } => DIG_TICKS,
                            JobKind::Build { .. } => BUILD_TICKS,
                        };
                        c.state = AgentState::Working { ticks_left: ticks };
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

    fn work(&mut self, id: EntityId, c: &mut Colonist) {
        if !self.job_valid(c.job) {
            self.release(c);
            return;
        }
        let job = *self.jobs.get(c.job).unwrap();
        let (tx, ty) = job.kind.target();
        if !adjacent(c.cell, (tx, ty)) {
            c.state = AgentState::Walking {
                goal: Goal::Adjacent { x: tx, y: ty },
                path: None,
            };
            return;
        }
        let AgentState::Working { ticks_left } = &mut c.state else {
            return;
        };
        *ticks_left -= 1;
        if *ticks_left > 0 {
            return;
        }
        match job.kind {
            JobKind::Dig { .. } => {
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
            }
            JobKind::Build { .. } => {
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
                if occupied {
                    self.release(c);
                    return;
                }
                let (fluid, fmass, ftemp) = self.world.get(tx as usize, ty as usize);
                if fmass.raw() > 0 {
                    let candidates = [
                        c.cell,
                        (tx, ty.wrapping_sub(1)),
                        (tx.wrapping_sub(1), ty),
                        (tx + 1, ty),
                        (tx, ty + 1),
                    ];
                    let (w, h) = (self.world.width as u32, self.world.height as u32);
                    let receiver = candidates.into_iter().find(|&(x, y)| {
                        x < w && y < h && (x, y) != (tx, ty) && self.nav.is_passable(x, y) && {
                            let (el, _, _) = self.world.get(x as usize, y as usize);
                            el == fluid || el == ElementId::VACUUM
                        }
                    });
                    let Some((rx, ry)) = receiver else {
                        self.release(c);
                        return;
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
                }
                self.set_cell(tx as usize, ty as usize, item.element, item.mass, item.temp);
                self.items.remove(c.carrying);
                c.carrying = EntityId::NONE;
            }
        }
        self.jobs.complete(c.job, self.tick);
        c.job = NO_JOB;
        c.state = AgentState::Idle;
        let _ = id;
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
                    if let Some(job) = self.jobs.get(c.job) {
                        let (tx, ty) = job.kind.target();
                        c.state = AgentState::Walking {
                            goal: Goal::Adjacent { x: tx, y: ty },
                            path: None,
                        };
                    } else {
                        c.job = NO_JOB;
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
            AgentState::Working { .. } => self.work(id, &mut c),
        }
        if let Some(slot) = self.colonists.get_mut(id) {
            *slot = c;
        }
    }

    /// The agents pass: refresh walkability from this tick's processed chunks, settle
    /// items, then run every colonist in a rotating index order under a shared path
    /// budget. Returns the number of path solves.
    pub(crate) fn agents_step(&mut self) -> u32 {
        self.nav
            .refresh(&self.world.cells, &self.table, &self.process_heat);
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
