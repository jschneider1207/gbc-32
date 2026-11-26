//! The Win32 keyboard for `play`: `GetAsyncKeyState` polled once per loop
//! (about 100 Hz). Chosen over console input events (`ReadConsoleInputW`)
//! because it needs no console: through WSL interop the `.exe`'s stdin
//! is a pipe, not a console input buffer, and polling the physical key
//! state works either way. Bit 15 of the result is "held now" (Microsoft
//! Learn, `GetAsyncKeyState`); `VK_RSHIFT` is distinguished from the left
//! Shift by this call.
//!
//! The state is global: keys pressed while another window has the focus
//! count too (including Esc, which ends `play`).

use std::io;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_BACK, VK_DOWN, VK_ESCAPE, VK_F5, VK_LEFT, VK_RETURN,
    VK_RIGHT, VK_RSHIFT, VK_UP,
};

use crate::input::{Key, KeySet};
use crate::play::Keyboard;

/// The virtual-key code of each [`Key`] (letters are their ASCII
/// capitals).
fn vk(k: Key) -> VIRTUAL_KEY {
    match k {
        Key::Up => VK_UP,
        Key::Down => VK_DOWN,
        Key::Left => VK_LEFT,
        Key::Right => VK_RIGHT,
        Key::Z => b'Z' as VIRTUAL_KEY,
        Key::X => b'X' as VIRTUAL_KEY,
        Key::Enter => VK_RETURN,
        Key::RightShift => VK_RSHIFT,
        Key::Backspace => VK_BACK,
        Key::Esc => VK_ESCAPE,
        Key::F5 => VK_F5,
    }
}

pub struct AsyncKeyboard;

impl Keyboard for AsyncKeyboard {
    fn held(&mut self) -> io::Result<KeySet> {
        let mut s = KeySet::default();
        for k in Key::ALL {
            // SAFETY: GetAsyncKeyState takes any virtual-key code by value
            // and touches no memory of ours.
            let state = unsafe { GetAsyncKeyState(i32::from(vk(k))) };
            if state as u16 & 0x8000 != 0 {
                s.insert(k);
            }
        }
        Ok(s)
    }
}
