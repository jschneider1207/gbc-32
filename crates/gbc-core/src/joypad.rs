//! Joypad input.

use crate::bitset::bitset;

bitset! {
    /// Buttons currently held, passed to [`Emulator::set_buttons`](crate::Emulator::set_buttons).
    ///
    /// The bit layout matches the P1/JOYP register halves (Pan Docs:
    /// Joypad Input): the low nibble is the action buttons (A, B, Select,
    /// Start) and the high nibble the d-pad (Right, Left, Up, Down). A set
    /// bit means *pressed*; the hardware register is active-low.
    pub struct Buttons(u8) {
        const A = 1 << 0;
        const B = 1 << 1;
        const SELECT = 1 << 2;
        const START = 1 << 3;
        const RIGHT = 1 << 4;
        const LEFT = 1 << 5;
        const UP = 1 << 6;
        const DOWN = 1 << 7;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::Buttons;
    use std::format;

    #[test]
    fn set_operations() {
        let mut b = Buttons::EMPTY;
        assert!(b.is_empty());
        b.insert(Buttons::A | Buttons::START);
        assert!(b.contains(Buttons::A));
        assert!(b.contains(Buttons::A | Buttons::START));
        assert!(!b.contains(Buttons::A | Buttons::B));
        assert!(b.intersects(Buttons::A | Buttons::B));
        b.remove(Buttons::A);
        assert_eq!(b, Buttons::START);
        b.set(Buttons::DOWN, true);
        b.set(Buttons::START, false);
        assert_eq!(b, Buttons::DOWN);
        assert_eq!(b.bits(), 0x80);
        assert_eq!(!Buttons::EMPTY, Buttons::ALL);
        assert_eq!(Buttons::ALL - Buttons::A, Buttons::from_bits_truncate(0xFE));
        assert_eq!(Buttons::ALL.bits(), 0xFF);
    }

    #[test]
    fn nibbles_match_joyp_layout() {
        let action = Buttons::A | Buttons::B | Buttons::SELECT | Buttons::START;
        let dpad = Buttons::RIGHT | Buttons::LEFT | Buttons::UP | Buttons::DOWN;
        assert_eq!(action.bits(), 0x0F);
        assert_eq!(dpad.bits(), 0xF0);
    }

    #[test]
    fn debug_lists_names() {
        assert_eq!(format!("{:?}", Buttons::EMPTY), "Buttons()");
        assert_eq!(format!("{:?}", Buttons::A | Buttons::UP), "Buttons(A | UP)");
    }
}
