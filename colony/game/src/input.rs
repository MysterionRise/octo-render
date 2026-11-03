//! Key mapping and the set of held keys, kept free of winit event plumbing so it can be
//! unit-tested.

use std::collections::HashSet;

use winit::keyboard::KeyCode;

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
        assert_eq!(map_key(KeyCode::KeyQ), None);
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
