//! Debris items: the mass of a dug cell on the ground (or in a colonist's hands), so
//! digging and building move mass around instead of creating or destroying it.

use crate::element::ElementId;
use crate::entity::EntityId;
use crate::fixed::{Mass, Temp};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item {
    pub x: u32,
    pub y: u32,
    pub element: ElementId,
    pub mass: Mass,
    pub temp: Temp,
    /// The colonist carrying the item, or `NONE` on the ground.
    pub holder: EntityId,
    /// The colonist that has claimed the item for a build job, or `NONE`.
    pub reserved_by: EntityId,
}

impl Item {
    pub fn on_ground(&self) -> bool {
        self.holder.is_none()
    }
}
