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
pub use snapshot::{BuildingPose, CellProbe, ColonistPose, ItemPose, PacketPose, Snapshot};
pub use sprites::{sprite_instances, SpriteInstance};

/// Which overlay recolours the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Overlay {
    #[default]
    None = 0,
    Temperature = 1,
    Pressure = 2,
    /// Recolours wires and the buildings on them by power status (sprites only).
    Power = 3,
    /// Recolours pipes by whether they hold a packet (sprites only).
    Pipes = 4,
}

impl Overlay {
    pub const ALL: [Overlay; 5] = [
        Overlay::None,
        Overlay::Temperature,
        Overlay::Pressure,
        Overlay::Power,
        Overlay::Pipes,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Overlay::None => "none",
            Overlay::Temperature => "temperature",
            Overlay::Pressure => "pressure",
            Overlay::Power => "power",
            Overlay::Pipes => "pipes",
        }
    }

    /// The overlay the cell shader applies: the network overlays leave cells alone.
    pub fn for_cells(self) -> Overlay {
        match self {
            Overlay::Temperature | Overlay::Pressure => self,
            _ => Overlay::None,
        }
    }

    pub fn from_name(name: &str) -> Option<Overlay> {
        Overlay::ALL.into_iter().find(|o| o.label() == name)
    }
}
