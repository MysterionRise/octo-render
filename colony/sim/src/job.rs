//! The job board: dig and build orders, reservations, retries, and counts.

use crate::entity::EntityId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    Dig { x: u32, y: u32 },
    Build { x: u32, y: u32 },
}

impl JobKind {
    pub fn target(&self) -> (u32, u32) {
        match *self {
            JobKind::Dig { x, y } | JobKind::Build { x, y } => (x, y),
        }
    }

    pub fn tag(&self) -> u8 {
        match self {
            JobKind::Dig { .. } => 0,
            JobKind::Build { .. } => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Open,
    Reserved(EntityId),
    Done,
    Cancelled,
}

/// A job is released back to `Open` this many times before it is cancelled.
pub const MAX_FAILURES: u8 = 3;

/// Finished jobs are kept this many ticks for the inspector, then pruned.
pub const PRUNE_AFTER_TICKS: u64 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Job {
    pub id: u32,
    pub kind: JobKind,
    pub priority: u8,
    pub state: JobState,
    pub failures: u8,
    /// Tick the job finished at, `u64::MAX` while open or reserved.
    pub finished_tick: u64,
}

impl Job {
    pub fn is_live(&self) -> bool {
        matches!(self.state, JobState::Open | JobState::Reserved(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct JobCounts {
    pub open: u32,
    pub reserved: u32,
    pub done: u32,
    pub cancelled: u32,
}

/// Jobs in id order. `version` changes whenever anything a colonist might care about
/// changes, so idle colonists re-score only when needed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobBoard {
    pub jobs: Vec<Job>,
    pub version: u32,
    pub next_id: u32,
    pub done_count: u32,
    pub cancelled_count: u32,
}

impl Default for JobBoard {
    fn default() -> Self {
        JobBoard {
            jobs: Vec::new(),
            version: 1,
            next_id: 0,
            done_count: 0,
            cancelled_count: 0,
        }
    }
}

impl JobBoard {
    pub fn new() -> JobBoard {
        JobBoard::default()
    }

    fn bump(&mut self) {
        self.version = self.version.wrapping_add(1).max(1);
    }

    /// Adds an order; a live order of the same kind at the same cell is updated instead.
    pub fn add(&mut self, kind: JobKind, priority: u8) -> u32 {
        if let Some(i) = self.jobs.iter().position(|j| j.is_live() && j.kind == kind) {
            if self.jobs[i].priority != priority {
                self.jobs[i].priority = priority;
                self.bump();
            }
            return self.jobs[i].id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.jobs.push(Job {
            id,
            kind,
            priority,
            state: JobState::Open,
            failures: 0,
            finished_tick: u64::MAX,
        });
        self.bump();
        id
    }

    pub fn get(&self, id: u32) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    /// The live job at a cell, if any.
    pub fn live_at(&self, x: u32, y: u32) -> Option<&Job> {
        self.jobs
            .iter()
            .find(|j| j.is_live() && j.kind.target() == (x, y))
    }

    /// Cancels every live job at a cell; returns how many.
    pub fn cancel_at(&mut self, x: u32, y: u32, tick: u64) -> usize {
        let mut n = 0;
        for job in self
            .jobs
            .iter_mut()
            .filter(|j| j.is_live() && j.kind.target() == (x, y))
        {
            job.state = JobState::Cancelled;
            job.finished_tick = tick;
            n += 1;
        }
        if n > 0 {
            self.cancelled_count += n as u32;
            self.bump();
        }
        n
    }

    pub fn set_priority_at(&mut self, x: u32, y: u32, priority: u8) -> bool {
        let mut changed = false;
        for job in self
            .jobs
            .iter_mut()
            .filter(|j| j.is_live() && j.kind.target() == (x, y))
        {
            if job.priority != priority {
                job.priority = priority;
                changed = true;
            }
        }
        if changed {
            self.bump();
        }
        changed
    }

    pub fn reserve(&mut self, id: u32, colonist: EntityId) -> bool {
        match self.get_mut(id) {
            Some(job) if job.state == JobState::Open => {
                job.state = JobState::Reserved(colonist);
                true
            }
            _ => false,
        }
    }

    /// Gives a reserved job back; after [`MAX_FAILURES`] releases it is cancelled.
    pub fn release(&mut self, id: u32, tick: u64) {
        let mut cancelled = false;
        if let Some(job) = self.get_mut(id) {
            if let JobState::Reserved(_) = job.state {
                job.failures += 1;
                if job.failures >= MAX_FAILURES {
                    job.state = JobState::Cancelled;
                    job.finished_tick = tick;
                    cancelled = true;
                } else {
                    job.state = JobState::Open;
                }
            }
        }
        if cancelled {
            self.cancelled_count += 1;
        }
        self.bump();
    }

    pub fn complete(&mut self, id: u32, tick: u64) {
        if let Some(job) = self.get_mut(id) {
            if job.is_live() {
                job.state = JobState::Done;
                job.finished_tick = tick;
                self.done_count += 1;
                self.bump();
            }
        }
    }

    /// Open jobs in id order.
    pub fn open_jobs(&self) -> impl Iterator<Item = &Job> {
        self.jobs.iter().filter(|j| j.state == JobState::Open)
    }

    pub fn counts(&self) -> JobCounts {
        let mut c = JobCounts {
            done: self.done_count,
            cancelled: self.cancelled_count,
            ..JobCounts::default()
        };
        for job in &self.jobs {
            match job.state {
                JobState::Open => c.open += 1,
                JobState::Reserved(_) => c.reserved += 1,
                _ => {}
            }
        }
        c
    }

    /// Drops finished jobs older than [`PRUNE_AFTER_TICKS`].
    pub fn prune(&mut self, tick: u64) {
        let before = self.jobs.len();
        self.jobs
            .retain(|j| j.is_live() || tick.saturating_sub(j.finished_tick) < PRUNE_AFTER_TICKS);
        if self.jobs.len() != before {
            self.bump();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: EntityId = EntityId {
        index: 3,
        generation: 0,
    };

    #[test]
    fn orders_dedupe_reserve_release_and_cancel_after_three_failures() {
        let mut board = JobBoard::new();
        let id = board.add(JobKind::Dig { x: 4, y: 5 }, 5);
        assert_eq!(
            board.add(JobKind::Dig { x: 4, y: 5 }, 9),
            id,
            "same order updates priority"
        );
        assert_eq!(board.get(id).unwrap().priority, 9);
        assert_eq!(board.open_jobs().count(), 1);
        assert!(board.reserve(id, C));
        assert!(!board.reserve(id, C), "already reserved");
        assert_eq!(board.counts().reserved, 1);
        board.release(id, 10);
        assert_eq!(board.get(id).unwrap().state, JobState::Open);
        board.reserve(id, C);
        board.release(id, 11);
        board.reserve(id, C);
        board.release(id, 12);
        assert_eq!(board.get(id).unwrap().state, JobState::Cancelled);
        assert_eq!(board.counts().cancelled, 1);
        assert_eq!(board.open_jobs().count(), 0);
        let other = board.add(JobKind::Build { x: 4, y: 5 }, 1);
        assert_ne!(other, id);
        assert_eq!(board.cancel_at(4, 5, 20), 1);
        assert_eq!(board.counts().cancelled, 2);
    }

    #[test]
    fn completion_counts_and_pruning() {
        let mut board = JobBoard::new();
        let id = board.add(JobKind::Dig { x: 1, y: 1 }, 1);
        board.reserve(id, C);
        let v = board.version;
        board.complete(id, 5);
        assert!(board.version != v);
        assert_eq!(board.counts().done, 1);
        board.prune(5 + PRUNE_AFTER_TICKS - 1);
        assert_eq!(board.jobs.len(), 1);
        board.prune(5 + PRUNE_AFTER_TICKS);
        assert!(board.jobs.is_empty());
        assert_eq!(board.counts().done, 1, "counts survive pruning");
        assert!(!board.set_priority_at(1, 1, 3), "no live job there");
    }
}
