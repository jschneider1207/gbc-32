//! Keyboard and gamepad mapping (physical keys, so the layout doesn't matter).

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

/// Gamepad face buttons follow the GB's physical layout, where A is the
/// right button: South = B, East = A. D-pad = D-pad, Start/Select as
/// labelled. The left stick is handled by `stick_to_buttons`.
pub fn pad_button_to_button(b: gilrs::Button) -> Option<Buttons> {
    use gilrs::Button as G;
    Some(match b {
        G::DPadUp => Buttons::UP,
        G::DPadDown => Buttons::DOWN,
        G::DPadLeft => Buttons::LEFT,
        G::DPadRight => Buttons::RIGHT,
        G::South => Buttons::B,
        G::East => Buttons::A,
        G::Start => Buttons::START,
        G::Select => Buttons::SELECT,
        _ => return None,
    })
}

/// Deflection beyond this fraction of full travel counts as a press.
const STICK_THRESHOLD: f32 = 0.5;

/// Left stick as D-pad directions. gilrs reports Y positive = up (unlike
/// SDL/winit, where down is positive).
pub fn stick_to_buttons(x: f32, y: f32) -> Buttons {
    let mut b = Buttons::default();
    if x > STICK_THRESHOLD {
        b |= Buttons::RIGHT;
    } else if x < -STICK_THRESHOLD {
        b |= Buttons::LEFT;
    }
    if y > STICK_THRESHOLD {
        b |= Buttons::UP;
    } else if y < -STICK_THRESHOLD {
        b |= Buttons::DOWN;
    }
    b
}

/// One gamepad's contribution: its buttons plus each stick axis, kept
/// apart because gilrs reports the axes in separate events.
#[derive(Clone, Copy)]
struct PadState<Id> {
    id: Id,
    buttons: Buttons,
    stick_x: Buttons,
    stick_y: Buttons,
}

/// Everything currently held: the keyboard plus every connected pad.
pub struct HeldInput<Id> {
    keys: Buttons,
    pads: Vec<PadState<Id>>,
}

impl<Id: Copy + PartialEq> HeldInput<Id> {
    pub fn new() -> Self {
        Self {
            keys: Buttons::default(),
            pads: Vec::new(),
        }
    }

    pub fn set_key(&mut self, b: Buttons, down: bool) {
        self.keys.set(b, down);
    }

    fn pad(&mut self, id: Id) -> &mut PadState<Id> {
        if let Some(i) = self.pads.iter().position(|p| p.id == id) {
            return &mut self.pads[i];
        }
        self.pads.push(PadState {
            id,
            buttons: Buttons::default(),
            stick_x: Buttons::default(),
            stick_y: Buttons::default(),
        });
        self.pads.last_mut().expect("just pushed")
    }

    pub fn set_pad_button(&mut self, id: Id, b: Buttons, down: bool) {
        self.pad(id).buttons.set(b, down);
    }

    pub fn set_pad_stick_x(&mut self, id: Id, x: f32) {
        self.pad(id).stick_x = stick_to_buttons(x, 0.0);
    }

    pub fn set_pad_stick_y(&mut self, id: Id, y: f32) {
        self.pad(id).stick_y = stick_to_buttons(0.0, y);
    }

    /// A disconnected pad must not leave buttons stuck down.
    pub fn release_pad(&mut self, id: Id) {
        self.pads.retain(|p| p.id != id);
    }

    /// Focus loss: release the keyboard and every pad.
    pub fn clear(&mut self) {
        self.keys = Buttons::default();
        for p in &mut self.pads {
            p.buttons = Buttons::default();
            p.stick_x = Buttons::default();
            p.stick_y = Buttons::default();
        }
    }

    pub fn held(&self) -> Buttons {
        self.pads
            .iter()
            .fold(self.keys, |acc, p| acc | p.buttons | p.stick_x | p.stick_y)
    }
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

    #[test]
    fn maps_every_pad_button() {
        use gilrs::Button as G;
        let table = [
            (G::DPadUp, Buttons::UP),
            (G::DPadDown, Buttons::DOWN),
            (G::DPadLeft, Buttons::LEFT),
            (G::DPadRight, Buttons::RIGHT),
            (G::South, Buttons::B),
            (G::East, Buttons::A),
            (G::Start, Buttons::START),
            (G::Select, Buttons::SELECT),
        ];
        for (g, b) in table {
            assert_eq!(pad_button_to_button(g), Some(b), "{g:?}");
        }
        for g in [G::North, G::West, G::LeftTrigger, G::Mode] {
            assert_eq!(pad_button_to_button(g), None, "{g:?}");
        }
    }

    #[test]
    fn stick_needs_more_than_half_deflection() {
        assert_eq!(stick_to_buttons(0.0, 0.0), Buttons::default());
        assert_eq!(stick_to_buttons(0.5, -0.5), Buttons::default());
        assert_eq!(stick_to_buttons(0.51, 0.0), Buttons::RIGHT);
        assert_eq!(stick_to_buttons(-0.51, 0.0), Buttons::LEFT);
        // gilrs: Y positive is up.
        assert_eq!(stick_to_buttons(0.0, 0.9), Buttons::UP);
        assert_eq!(stick_to_buttons(0.0, -0.9), Buttons::DOWN);
        assert_eq!(stick_to_buttons(-1.0, 1.0), Buttons::LEFT | Buttons::UP);
    }

    #[test]
    fn held_is_the_union_of_keys_and_all_pads() {
        let mut h = HeldInput::<u32>::new();
        h.set_key(Buttons::A, true);
        h.set_pad_button(1, Buttons::B, true);
        h.set_pad_button(2, Buttons::START, true);
        h.set_pad_stick_x(2, 1.0);
        h.set_pad_stick_y(1, -1.0);
        assert_eq!(
            h.held(),
            Buttons::A | Buttons::B | Buttons::START | Buttons::RIGHT | Buttons::DOWN
        );
        h.set_pad_button(1, Buttons::B, false);
        h.set_pad_stick_x(2, 0.0);
        h.set_key(Buttons::A, false);
        assert_eq!(h.held(), Buttons::START | Buttons::DOWN);
    }

    #[test]
    fn releasing_a_pad_leaves_the_others() {
        let mut h = HeldInput::<u32>::new();
        h.set_key(Buttons::A, true);
        h.set_pad_button(1, Buttons::B, true);
        h.set_pad_stick_x(1, -1.0);
        h.set_pad_button(2, Buttons::SELECT, true);
        h.release_pad(1);
        assert_eq!(h.held(), Buttons::A | Buttons::SELECT);
        h.release_pad(9); // unknown id is a no-op
        assert_eq!(h.held(), Buttons::A | Buttons::SELECT);
    }

    #[test]
    fn clear_releases_keys_and_pads() {
        let mut h = HeldInput::<u32>::new();
        h.set_key(Buttons::A, true);
        h.set_pad_button(1, Buttons::B, true);
        h.set_pad_stick_y(1, 1.0);
        h.clear();
        assert_eq!(h.held(), Buttons::default());
        // Still usable afterwards.
        h.set_pad_button(1, Buttons::B, true);
        assert_eq!(h.held(), Buttons::B);
    }
}
