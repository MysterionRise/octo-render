//! `colony-sim`: the deterministic cell-simulation kernel for the colony engine.
//!
//! The kernel owns a finite tile world where every cell holds one element with a
//! mass and a temperature, and steps it with fixed-point integer arithmetic so that
//! two runs with the same inputs produce bit-identical state on any platform. The
//! design is in `docs/colony-engine-design.md`, sections 6 and 11.
//!
//! Rules of this crate:
//!
//! - no `f32`/`f64` in state or tick code; unit conversion happens in content loading
//! - no wall-clock reads and no hash-map iteration in mutating paths
//! - no dependency on `wgpu`, `winit`, or `glam`
//!
//! Every pass is a *gather*: a cell's new value depends only on old values of itself
//! and its neighbours plus fixed priorities, and every exchange is evaluated
//! identically from both ends. That makes passes parallel-safe, deterministic, and
//! exactly conserving by construction.

pub mod chunk;
pub mod command;
pub mod element;
pub mod entity;
pub mod fixed;
pub mod fluid;
pub mod hash;
pub mod heat;
pub mod item;
pub mod job;
pub mod nav;
pub mod rng;
pub mod save;
pub mod scenario;
pub mod sim;
pub mod world;
pub mod worldgen;

pub use command::{Command, LogEntry};
pub use element::{builtin, ElementDef, ElementId, ElementTable, Phase};
pub use entity::{Arena, EntityId};
pub use fixed::{Mass, Temp};
pub use item::Item;
pub use job::{Job, JobBoard, JobCounts, JobKind, JobState};
pub use nav::{Goal, NavGrid, Path};
pub use sim::{Sim, TickStats};
pub use world::{Cell, CellBuffers, CellOut, Dir, Grid, Stencil, World, DIRS, FLAG_PRESSED};
