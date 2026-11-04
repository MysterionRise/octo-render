//! Key mapping, the set of held keys, and the tool a click applies, kept free of winit
//! event plumbing so it can be unit-tested.

use std::collections::HashSet;

use colony_sim::{builtin, Command, Mass, Temp};
use winit::keyboard::KeyCode;

/// What a left click does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Tool {
    /// Order the cell dug (it must be solid).
    #[default]
    Dig,
    /// Order the cell built from debris (it must not be solid).
    Build,
    /// Place the selected building with its bottom-left cell here.
    Place,
    /// Remove a planned building, or order a built one taken apart.
    Deconstruct,
    /// Flip a building's enabled switch.
    Toggle,
    /// Cancel the orders on the cell.
    Cancel,
    /// Add a colonist on the cell.
    Spawn,
    /// Replace the cell with a tonne of water.
    Pour,
    /// Replace the cell with a kilogram of oxygen (an instant dig, for debugging).
    Oxygen,
}

impl Tool {
    pub const ALL: [Tool; 9] = [
        Tool::Dig,
        Tool::Build,
        Tool::Place,
        Tool::Deconstruct,
        Tool::Toggle,
        Tool::Cancel,
        Tool::Spawn,
        Tool::Pour,
        Tool::Oxygen,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Dig => "dig (Q)",
            Tool::Build => "build (E)",
            Tool::Place => "place (B)",
            Tool::Deconstruct => "deconstruct (X)",
            Tool::Toggle => "toggle (T)",
            Tool::Cancel => "cancel (C)",
            Tool::Spawn => "spawn (N)",
            Tool::Pour => "pour (P)",
            Tool::Oxygen => "oxygen (O)",
        }
    }
}

/// The command a tool sends for a cell; `def` is the building the place tool uses.
pub fn command_for(tool: Tool, x: usize, y: usize, def: u16) -> Command {
    match tool {
        Tool::Dig => Command::Dig { x, y },
        Tool::Build => Command::Build { x, y },
        Tool::Place => Command::Place { def, x, y },
        Tool::Deconstruct => Command::Deconstruct { x, y },
        Tool::Toggle => Command::Toggle { x, y },
        Tool::Cancel => Command::CancelOrders { x, y },
        Tool::Spawn => Command::SpawnColonist { x, y },
        Tool::Pour => Command::SetCell {
            x,
            y,
            element: builtin::WATER,
            mass: Mass::from_kg(1000),
            temp: Temp::from_kelvin(290),
        },
        Tool::Oxygen => Command::SetCell {
            x,
            y,
            element: builtin::OXYGEN,
            mass: Mass::from_kg(1),
            temp: Temp::from_kelvin(300),
        },
    }
}

/// Whether dragging with the button held keeps applying the tool to new cells
/// (orders, placements, and paints do; spawning and toggling act once per click).
pub fn drags(tool: Tool) -> bool {
    !matches!(tool, Tool::Spawn | Tool::Toggle)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    PanLeft,
    PanRight,
    PanUp,
    PanDown,
    ZoomIn,
    ZoomOut,
    TogglePause,
    Speed(u32),
    Tool(Tool),
    Quit,
}

pub fn map_key(code: KeyCode) -> Option<Action> {
    use KeyCode::*;
    Some(match code {
        KeyA | ArrowLeft => Action::PanLeft,
        KeyD | ArrowRight => Action::PanRight,
        KeyW | ArrowUp => Action::PanUp,
        KeyS | ArrowDown => Action::PanDown,
        Equal | NumpadAdd => Action::ZoomIn,
        Minus | NumpadSubtract => Action::ZoomOut,
        Space => Action::TogglePause,
        Digit1 | Numpad1 => Action::Speed(1),
        Digit2 | Numpad2 => Action::Speed(2),
        Digit3 | Numpad3 => Action::Speed(3),
        KeyQ => Action::Tool(Tool::Dig),
        KeyE => Action::Tool(Tool::Build),
        KeyB => Action::Tool(Tool::Place),
        KeyX => Action::Tool(Tool::Deconstruct),
        KeyT => Action::Tool(Tool::Toggle),
        KeyC => Action::Tool(Tool::Cancel),
        KeyN => Action::Tool(Tool::Spawn),
        KeyP => Action::Tool(Tool::Pour),
        KeyO => Action::Tool(Tool::Oxygen),
        Escape => Action::Quit,
        _ => return None,
    })
}

/// Keys currently held, for continuous panning.
#[derive(Default, Debug)]
pub struct Held {
    keys: HashSet<KeyCode>,
}

impl Held {
    pub fn press(&mut self, code: KeyCode) {
        self.keys.insert(code);
    }

    pub fn release(&mut self, code: KeyCode) {
        self.keys.remove(&code);
    }

    /// Net pan direction from the held keys: `x` right-positive, `y` up-positive.
    pub fn pan_direction(&self) -> (i32, i32) {
        let mut d: (i32, i32) = (0, 0);
        for code in &self.keys {
            match map_key(*code) {
                Some(Action::PanLeft) => d.0 -= 1,
                Some(Action::PanRight) => d.0 += 1,
                Some(Action::PanUp) => d.1 += 1,
                Some(Action::PanDown) => d.1 -= 1,
                _ => {}
            }
        }
        (d.0.signum(), d.1.signum())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_actions() {
        assert_eq!(map_key(KeyCode::KeyA), Some(Action::PanLeft));
        assert_eq!(map_key(KeyCode::ArrowUp), Some(Action::PanUp));
        assert_eq!(map_key(KeyCode::Equal), Some(Action::ZoomIn));
        assert_eq!(map_key(KeyCode::Digit3), Some(Action::Speed(3)));
        assert_eq!(map_key(KeyCode::Escape), Some(Action::Quit));
        assert_eq!(map_key(KeyCode::KeyQ), Some(Action::Tool(Tool::Dig)));
        assert_eq!(map_key(KeyCode::KeyE), Some(Action::Tool(Tool::Build)));
        assert_eq!(map_key(KeyCode::KeyB), Some(Action::Tool(Tool::Place)));
        assert_eq!(
            map_key(KeyCode::KeyX),
            Some(Action::Tool(Tool::Deconstruct))
        );
        assert_eq!(map_key(KeyCode::KeyT), Some(Action::Tool(Tool::Toggle)));
        assert_eq!(map_key(KeyCode::KeyC), Some(Action::Tool(Tool::Cancel)));
        assert_eq!(map_key(KeyCode::KeyN), Some(Action::Tool(Tool::Spawn)));
        assert_eq!(map_key(KeyCode::KeyP), Some(Action::Tool(Tool::Pour)));
        assert_eq!(map_key(KeyCode::KeyO), Some(Action::Tool(Tool::Oxygen)));
        assert_eq!(map_key(KeyCode::KeyZ), None);
    }

    #[test]
    fn every_tool_maps_a_click_to_a_command() {
        assert_eq!(command_for(Tool::Dig, 3, 4, 0), Command::Dig { x: 3, y: 4 });
        assert_eq!(
            command_for(Tool::Build, 3, 4, 0),
            Command::Build { x: 3, y: 4 }
        );
        assert_eq!(
            command_for(Tool::Place, 3, 4, 7),
            Command::Place { def: 7, x: 3, y: 4 }
        );
        assert_eq!(
            command_for(Tool::Deconstruct, 3, 4, 7),
            Command::Deconstruct { x: 3, y: 4 }
        );
        assert_eq!(
            command_for(Tool::Toggle, 3, 4, 7),
            Command::Toggle { x: 3, y: 4 }
        );
        assert_eq!(
            command_for(Tool::Cancel, 3, 4, 0),
            Command::CancelOrders { x: 3, y: 4 }
        );
        assert_eq!(
            command_for(Tool::Spawn, 3, 4, 0),
            Command::SpawnColonist { x: 3, y: 4 }
        );
        assert!(matches!(
            command_for(Tool::Pour, 3, 4, 0),
            Command::SetCell {
                x: 3,
                y: 4,
                element: builtin::WATER,
                ..
            }
        ));
        assert!(matches!(
            command_for(Tool::Oxygen, 1, 1, 0),
            Command::SetCell {
                element: builtin::OXYGEN,
                ..
            }
        ));
        assert!(drags(Tool::Dig) && drags(Tool::Place));
        assert!(!drags(Tool::Spawn) && !drags(Tool::Toggle));
        assert_eq!(Tool::default(), Tool::Dig);
        assert_eq!(Tool::ALL.len(), 9);
    }

    #[test]
    fn held_keys_combine_into_a_direction_and_release_clears_it() {
        let mut held = Held::default();
        held.press(KeyCode::KeyA);
        held.press(KeyCode::ArrowLeft);
        held.press(KeyCode::KeyW);
        assert_eq!(held.pan_direction(), (-1, 1));
        held.press(KeyCode::KeyD);
        assert_eq!(
            held.pan_direction(),
            (-1, 1),
            "two lefts and one right still lean left"
        );
        held.release(KeyCode::KeyW);
        held.release(KeyCode::KeyA);
        held.release(KeyCode::ArrowLeft);
        assert_eq!(held.pan_direction(), (1, 0));
    }
}
