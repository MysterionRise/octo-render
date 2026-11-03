//! `colony-render`: everything between the kernel and the screen.
//!
//! The kernel runs on its own thread ([`host::SimHost`]) and publishes a [`Snapshot`]
//! after each tick. The renderer keeps the world in integer textures and draws the whole
//! view with one fullscreen triangle whose fragment shader does every lookup with integer
//! coordinates, so pixels are exact at any integer zoom. [`shade::shade_pixel`] is the
//! CPU twin of that shader: tests compare the two, and the headless path compares both to
//! `colony-headless`. Colonists and items are drawn over the cells by the sprite pass
//! ([`sprites`]), whose CPU twin is [`sprites::stamp_sprites`].

pub mod atlas;
pub mod camera;
pub mod corners;
pub mod gpu;
pub mod headless;
pub mod host;
pub mod palette;
pub mod shade;
pub mod snapshot;
pub mod sprites;

pub use colony_sim::chunk::CHUNK;

/// Pixels per cell in the tile art at zoom 1.
pub const TILE_PX: u32 = 8;

pub use camera::PixelCamera;
pub use gpu::CellRenderer;
pub use host::{HostEvent, HostRequest, SimHost};
pub use snapshot::{CellProbe, ColonistPose, ItemPose, Snapshot};
pub use sprites::{sprite_instances, SpriteInstance};

/// Which overlay recolours the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Overlay {
    #[default]
    None = 0,
    Temperature = 1,
    Pressure = 2,
}

impl Overlay {
    pub const ALL: [Overlay; 3] = [Overlay::None, Overlay::Temperature, Overlay::Pressure];

    pub fn label(self) -> &'static str {
        match self {
            Overlay::None => "none",
            Overlay::Temperature => "temperature",
            Overlay::Pressure => "pressure",
        }
    }

    pub fn from_name(name: &str) -> Option<Overlay> {
        Overlay::ALL.into_iter().find(|o| o.label() == name)
    }
}
