//! Keyboard mapping (physical keys, so the layout doesn't matter).

use gbc_core::Buttons;
use winit::keyboard::KeyCode;

/// Arrows = D-pad, Z = B, X = A, Enter = Start, Right Shift or Backspace
/// = Select.
pub fn key_to_button(key: KeyCode) -> Option<Buttons> {
    Some(match key {
        KeyCode::ArrowUp => Buttons::UP,
        KeyCode::ArrowDown => Buttons::DOWN,
        KeyCode::ArrowLeft => Buttons::LEFT,
        KeyCode::ArrowRight => Buttons::RIGHT,
        KeyCode::KeyZ => Buttons::B,
        KeyCode::KeyX => Buttons::A,
        KeyCode::Enter => Buttons::START,
        KeyCode::ShiftRight | KeyCode::Backspace => Buttons::SELECT,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_documented_key() {
        let table = [
            (KeyCode::ArrowUp, Buttons::UP),
            (KeyCode::ArrowDown, Buttons::DOWN),
            (KeyCode::ArrowLeft, Buttons::LEFT),
            (KeyCode::ArrowRight, Buttons::RIGHT),
            (KeyCode::KeyZ, Buttons::B),
            (KeyCode::KeyX, Buttons::A),
            (KeyCode::Enter, Buttons::START),
            (KeyCode::ShiftRight, Buttons::SELECT),
            (KeyCode::Backspace, Buttons::SELECT),
        ];
        for (k, b) in table {
            assert_eq!(key_to_button(k), Some(b), "{k:?}");
        }
    }

    #[test]
    fn other_keys_map_to_nothing() {
        for k in [
            KeyCode::KeyA,
            KeyCode::Escape,
            KeyCode::ShiftLeft,
            KeyCode::Space,
        ] {
            assert_eq!(key_to_button(k), None, "{k:?}");
        }
    }
}
