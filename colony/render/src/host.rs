//! The sim thread: owns the kernel, steps it on a fixed schedule, applies requests from
//! the game thread, and publishes snapshots through a mailbox the renderer polls without
//! ever waiting.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use colony_sim::{save, Command, ElementTable, Sim, TickStats};

use crate::snapshot::Snapshot;

/// Simulated time per step at speed 1.
pub const STEP_PERIOD: Duration = Duration::from_millis(100);

pub enum HostRequest {
    Command(Command),
    SetSpeed(u32),
    SetPaused(bool),
    /// Replace the world (same element table). The next snapshot has a new generation.
    Restart(Box<Sim>),
    /// Load a snapshot written by `colony_sim::save::encode`.
    Load(Vec<u8>),
    Save(PathBuf),
    Stop,
}

#[derive(Debug)]
pub enum HostEvent {
    Saved(Result<PathBuf, String>),
    Loaded(Result<u64, String>),
    Restarted,
}

/// The latest snapshot, replaced by the sim thread and taken by the renderer.
#[derive(Default)]
pub struct Mailbox {
    latest: Mutex<Option<Arc<Snapshot>>>,
}

impl Mailbox {
    /// Stores `snapshot`; returns the previous one if the renderer never took it, so its
    /// dirty chunks can be carried forward.
    pub fn publish(&self, snapshot: Arc<Snapshot>) -> Option<Arc<Snapshot>> {
        self.latest.lock().unwrap().replace(snapshot)
    }

    pub fn take(&self) -> Option<Arc<Snapshot>> {
        self.latest.lock().unwrap().take()
    }
}

/// Folds an unconsumed snapshot's dirty chunks into the accumulator.
pub fn merge_unconsumed(dirty_accum: &mut [bool], previous: &Snapshot) {
    for (d, p) in dirty_accum.iter_mut().zip(&previous.dirty) {
        *d |= *p;
    }
}

pub struct SimHost {
    tx: Sender<HostRequest>,
    events: Receiver<HostEvent>,
    mailbox: Arc<Mailbox>,
    join: Option<JoinHandle<()>>,
    table: ElementTable,
    world: (u32, u32),
}

impl SimHost {
    /// Starts the thread; a first snapshot is published immediately so a window has
    /// something to show even while paused.
    pub fn spawn(sim: Sim, speed: u32, paused: bool) -> SimHost {
        let (tx, rx) = mpsc::channel();
        let (events_tx, events) = mpsc::channel();
        let mailbox = Arc::new(Mailbox::default());
        let table = sim.table().clone();
        let world = (sim.world().width as u32, sim.world().height as u32);
        let worker_mailbox = mailbox.clone();
        let join = std::thread::Builder::new()
            .name("colony-sim".to_string())
            .spawn(move || run(sim, rx, events_tx, worker_mailbox, speed.max(1), paused))
            .expect("spawn sim thread");
        SimHost {
            tx,
            events,
            mailbox,
            join: Some(join),
            table,
            world,
        }
    }

    pub fn send(&self, request: HostRequest) {
        let _ = self.tx.send(request);
    }

    /// The newest snapshot since the last call, without waiting.
    pub fn take_latest(&self) -> Option<Arc<Snapshot>> {
        self.mailbox.take()
    }

    pub fn events(&self) -> Vec<HostEvent> {
        self.events.try_iter().collect()
    }

    pub fn table(&self) -> &ElementTable {
        &self.table
    }

    pub fn world(&self) -> (u32, u32) {
        self.world
    }
}

impl Drop for SimHost {
    fn drop(&mut self) {
        let _ = self.tx.send(HostRequest::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct Worker {
    sim: Sim,
    speed: u32,
    paused: bool,
    generation: u64,
    dirty_accum: Vec<bool>,
    last_stats: TickStats,
    step_micros: u32,
    ticks_per_s_x100: u32,
    last_step_at: Option<Instant>,
}

impl Worker {
    fn replace_sim(&mut self, sim: Sim) {
        self.sim = sim;
        self.generation += 1;
        let n = self.sim.chunks().len();
        self.dirty_accum = vec![true; n];
        self.last_stats = TickStats::fresh(n, self.sim.tick_count());
    }

    fn step(&mut self) {
        let t0 = Instant::now();
        self.last_stats = self.sim.step();
        let took = t0.elapsed();
        self.step_micros = took.as_micros().min(u32::MAX as u128) as u32;
        for (d, p) in self.dirty_accum.iter_mut().zip(self.sim.processed_chunks()) {
            *d |= *p;
        }
        if let Some(prev) = self.last_step_at {
            let gap = t0.duration_since(prev).as_secs_f64().max(1e-6);
            let instant = (100.0 / gap) as u32;
            self.ticks_per_s_x100 = if self.ticks_per_s_x100 == 0 {
                instant
            } else {
                (self.ticks_per_s_x100 * 7 + instant) / 8
            };
        }
        self.last_step_at = Some(t0);
    }

    fn snapshot(&self) -> Arc<Snapshot> {
        Arc::new(Snapshot::from_sim(
            &self.sim,
            &self.dirty_accum,
            self.generation,
            self.last_stats,
            self.step_micros,
            self.ticks_per_s_x100,
        ))
    }

    fn publish(&mut self, mailbox: &Mailbox) {
        let snapshot = self.snapshot();
        self.dirty_accum.iter_mut().for_each(|d| *d = false);
        if let Some(previous) = mailbox.publish(snapshot) {
            merge_unconsumed(&mut self.dirty_accum, &previous);
        }
    }
}

fn run(
    sim: Sim,
    rx: Receiver<HostRequest>,
    events: Sender<HostEvent>,
    mailbox: Arc<Mailbox>,
    speed: u32,
    paused: bool,
) {
    let n = sim.chunks().len();
    let mut w = Worker {
        last_stats: TickStats::fresh(n, sim.tick_count()),
        sim,
        speed,
        paused,
        generation: 0,
        dirty_accum: vec![true; n],
        step_micros: 0,
        ticks_per_s_x100: 0,
        last_step_at: None,
    };
    w.publish(&mailbox);
    let mut next_due = Instant::now();
    loop {
        // 1. Wait for the next step or a request, then drain every pending request.
        let first = if w.paused {
            rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } else {
            let now = Instant::now();
            if next_due > now {
                rx.recv_timeout(next_due - now)
            } else {
                rx.try_recv().map_err(|e| match e {
                    mpsc::TryRecvError::Empty => RecvTimeoutError::Timeout,
                    mpsc::TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
                })
            }
        };
        let mut pending: Vec<HostRequest> = Vec::new();
        match first {
            Ok(req) => pending.push(req),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        pending.extend(rx.try_iter());
        let mut generation_changed = false;
        let mut stop = false;
        for req in pending {
            match req {
                HostRequest::Command(c) => w.sim.queue(c),
                HostRequest::SetSpeed(s) => {
                    w.speed = s.max(1);
                    next_due = Instant::now();
                }
                HostRequest::SetPaused(p) => {
                    if w.paused && !p {
                        next_due = Instant::now();
                    }
                    w.paused = p;
                }
                HostRequest::Restart(sim) => {
                    w.replace_sim(*sim);
                    generation_changed = true;
                    let _ = events.send(HostEvent::Restarted);
                }
                HostRequest::Load(bytes) => match save::decode(
                    w.sim.table().clone(),
                    w.sim.building_table().clone(),
                    &bytes,
                ) {
                    Ok(sim) => {
                        let tick = sim.tick_count();
                        w.replace_sim(sim);
                        generation_changed = true;
                        let _ = events.send(HostEvent::Loaded(Ok(tick)));
                    }
                    Err(e) => {
                        let _ = events.send(HostEvent::Loaded(Err(e.to_string())));
                    }
                },
                HostRequest::Save(path) => {
                    let result = std::fs::write(&path, save::encode(&w.sim))
                        .map(|_| path.clone())
                        .map_err(|e| format!("{}: {e}", path.display()));
                    let _ = events.send(HostEvent::Saved(result));
                }
                HostRequest::Stop => stop = true,
            }
        }
        if stop {
            break;
        }
        // 2. Step as many times as are due, but never more than two seconds of sim time,
        //    and drop any further backlog rather than spiralling.
        let period = STEP_PERIOD / w.speed;
        let mut steps = 0;
        if !w.paused {
            let now = Instant::now();
            while now >= next_due && steps < 2 * w.speed {
                w.step();
                next_due += period;
                steps += 1;
            }
            if now >= next_due {
                next_due = now + period;
            }
        }
        // 3. Publish if anything changed.
        if steps > 0 || generation_changed {
            w.publish(&mailbox);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use colony_sim::{builtin, scenario, Mass, Temp};
    use std::thread::sleep;

    fn wait_for<T>(mut f: impl FnMut() -> Option<T>, timeout: Duration) -> Option<T> {
        let start = Instant::now();
        loop {
            if let Some(v) = f() {
                return Some(v);
            }
            if start.elapsed() > timeout {
                return None;
            }
            sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn publishes_an_initial_snapshot_then_steps_and_stops_cleanly() {
        let host = SimHost::spawn(scenario::settled(64, 64), 10, true);
        let first =
            wait_for(|| host.take_latest(), Duration::from_secs(2)).expect("initial snapshot");
        assert_eq!(first.tick, 0);
        assert!(
            first.dirty.iter().all(|d| *d),
            "everything is dirty at first"
        );
        host.send(HostRequest::SetPaused(false));
        let later = wait_for(
            || host.take_latest().filter(|s| s.tick > 0),
            Duration::from_secs(2),
        )
        .expect("a stepped snapshot");
        assert!(later.tick >= 1);
        assert_eq!(later.generation, 0);
        drop(host);
    }

    #[test]
    fn paused_host_publishes_nothing_after_the_initial_snapshot() {
        let host = SimHost::spawn(scenario::settled(32, 32), 10, true);
        wait_for(|| host.take_latest(), Duration::from_secs(2)).expect("initial snapshot");
        sleep(Duration::from_millis(250));
        assert!(host.take_latest().is_none());
        host.send(HostRequest::SetPaused(false));
        assert!(wait_for(|| host.take_latest(), Duration::from_secs(2)).is_some());
    }

    #[test]
    fn mailbox_returns_the_unconsumed_snapshot_and_dirty_flags_merge() {
        let mut sim = scenario::settled(64, 64);
        let stats = sim.step();
        let a = Arc::new(Snapshot::from_sim(
            &sim,
            &[true, false, false, false],
            0,
            stats,
            0,
            0,
        ));
        let b = Arc::new(Snapshot::from_sim(
            &sim,
            &[false, false, true, false],
            0,
            stats,
            0,
            0,
        ));
        let mailbox = Mailbox::default();
        assert!(mailbox.publish(a.clone()).is_none());
        let unconsumed = mailbox.publish(b.clone()).expect("a was never taken");
        assert!(Arc::ptr_eq(&unconsumed, &a));
        let mut accum = vec![false, true, false, false];
        merge_unconsumed(&mut accum, &unconsumed);
        assert_eq!(accum, vec![true, true, false, false]);
        assert!(Arc::ptr_eq(&mailbox.take().unwrap(), &b));
        assert!(mailbox.take().is_none());
    }

    #[test]
    fn a_command_shows_up_in_the_next_snapshot_and_restart_bumps_the_generation() {
        let host = SimHost::spawn(scenario::settled(32, 32), 10, true);
        wait_for(|| host.take_latest(), Duration::from_secs(2)).expect("initial");
        host.send(HostRequest::Command(Command::SetCell {
            x: 5,
            y: 1,
            element: builtin::WATER,
            mass: Mass::from_kg(500),
            temp: Temp::from_kelvin(290),
        }));
        host.send(HostRequest::SetPaused(false));
        let snap = wait_for(
            || host.take_latest().filter(|s| s.tick >= 1),
            Duration::from_secs(2),
        )
        .expect("stepped");
        let probe = snap.probe(host.table(), 5, 1).unwrap();
        assert_eq!(probe.element, builtin::WATER);
        // Pause first so the restart's snapshot is the only one published after it.
        host.send(HostRequest::SetPaused(true));
        host.send(HostRequest::Restart(Box::new(scenario::settled(32, 32))));
        let fresh = wait_for(
            || host.take_latest().filter(|s| s.generation == 1),
            Duration::from_secs(2),
        )
        .expect("restarted");
        assert_eq!(fresh.tick, 0);
        assert!(fresh.dirty.iter().all(|d| *d));
        assert!(host
            .events()
            .iter()
            .any(|e| matches!(e, HostEvent::Restarted)));
    }
}
