//! The Win32 keyboard for `play`: `GetAsyncKeyState` polled once per loop
//! (about 100 Hz). Chosen over console input events (`ReadConsoleInputW`)
//! because it needs no console: through WSL interop the `.exe`'s stdin
//! is a pipe, not a console input buffer, and polling the physical key
//! state works either way. Bit 15 of the result is "held now" (Microsoft
//! Learn, `GetAsyncKeyState`); `VK_RSHIFT` is distinguished from the left
//! Shift by this call.
//!
//! The key state is global, so [`held`](Keyboard::held) reads keys only
//! while the console's window is in the foreground (K6); otherwise no key
//! is held (typing in another window must not reach the game, F5 dumps or
//! Esc quits). "The console's window" is `GetConsoleWindow()`, or its
//! root owner: under conhost that handle is the visible window itself;
//! under Windows Terminal (ConPTY) it is a hidden pseudo-console window
//! that the terminal owns, and the foreground window is the terminal's
//! top-level one, which `GetAncestor(.., GA_ROOTOWNER)` of the pseudo
//! window reaches. With no console at all (`GetConsoleWindow` is null,
//! e.g. through WSL interop with piped stdio) there is nothing to compare
//! with, so keys are read unconditionally as before.

use std::io;

use windows_sys::Win32::System::Console::GetConsoleWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_BACK, VK_DOWN, VK_ESCAPE, VK_F5, VK_LEFT, VK_RETURN,
    VK_RIGHT, VK_RSHIFT, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GA_ROOTOWNER, GetAncestor, GetForegroundWindow};

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

/// Whether this process's console window (or the top-level window that
/// owns it) is the foreground window; true when there is no console.
fn console_has_focus() -> bool {
    // SAFETY: these calls take no arguments (GetAncestor: a window handle
    // by value, which may be stale or null and is then just rejected) and
    // touch no memory of ours.
    unsafe {
        let console = GetConsoleWindow();
        if console.is_null() {
            return true;
        }
        let fg = GetForegroundWindow();
        fg == console || fg == GetAncestor(console, GA_ROOTOWNER)
    }
}

impl Keyboard for AsyncKeyboard {
    fn held(&mut self) -> io::Result<KeySet> {
        let mut s = KeySet::default();
        if !console_has_focus() {
            return Ok(s);
        }
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
