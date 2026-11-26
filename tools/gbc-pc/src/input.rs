//! Keyboard state to `Input` frames (Stage 2 M5): which PC keys are the
//! Game Boy buttons, and when to send. The keys are the desktop front
//! end's (`docs/STATUS.md`): arrows = D-pad, Z = B, X = A, Enter = Start,
//! Right Shift or Backspace = Select. Esc ends `play`; F5 asks the board
//! for a save dump.
//!
//! **When to send:** on every change of the button bitmap, and again
//! [`REFRESH`] after the last frame if nothing changed. The board does not
//! answer `Input` (it would stall the loop on a round trip per frame), so
//! a frame lost on the link is healed by the next refresh, at most
//! [`REFRESH`] later.

use std::time::{Duration, Instant};

use gbc_link::buttons;

/// How long the board may go without an `Input` frame while nothing
/// changes.
pub const REFRESH: Duration = Duration::from_millis(500);

/// A PC key `play` reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Z,
    X,
    Enter,
    RightShift,
    Backspace,
    Esc,
    F5,
}

impl Key {
    pub const ALL: [Key; 11] = [
        Key::Up,
        Key::Down,
        Key::Left,
        Key::Right,
        Key::Z,
        Key::X,
        Key::Enter,
        Key::RightShift,
        Key::Backspace,
        Key::Esc,
        Key::F5,
    ];

    /// The Game Boy button bit ([`gbc_link::buttons`]) this key holds, if
    /// any.
    pub const fn button(self) -> u8 {
        match self {
            Key::Up => buttons::UP,
            Key::Down => buttons::DOWN,
            Key::Left => buttons::LEFT,
            Key::Right => buttons::RIGHT,
            Key::Z => buttons::B,
            Key::X => buttons::A,
            Key::Enter => buttons::START,
            Key::RightShift | Key::Backspace => buttons::SELECT,
            Key::Esc | Key::F5 => 0,
        }
    }

    const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// The set of keys held at one poll.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeySet(u16);

impl KeySet {
    pub fn of(keys: &[Key]) -> Self {
        let mut s = Self::default();
        for &k in keys {
            s.insert(k);
        }
        s
    }

    pub fn insert(&mut self, k: Key) {
        self.0 |= k.bit();
    }

    pub fn contains(self, k: Key) -> bool {
        self.0 & k.bit() != 0
    }

    /// The `Input` bitmap for these keys (1 = pressed).
    pub fn buttons(self) -> u8 {
        Key::ALL
            .iter()
            .filter(|&&k| self.contains(k))
            .fold(0, |b, k| b | k.button())
    }
}

/// Decides when an `Input` frame goes out: on a change, or [`REFRESH`]
/// (or the given period) after the last one.
#[derive(Clone, Copy, Debug)]
pub struct InputSchedule {
    refresh: Duration,
    last: Option<(u8, Instant)>,
}

impl InputSchedule {
    pub fn new(refresh: Duration) -> Self {
        Self {
            refresh,
            last: None,
        }
    }

    /// True when `buttons` should be sent at `now` (the first call, a
    /// change, or the refresh period since the last send); records it as
    /// sent.
    pub fn due(&mut self, buttons: u8, now: Instant) -> bool {
        let send = match self.last {
            None => true,
            Some((b, at)) => b != buttons || now.saturating_duration_since(at) >= self.refresh,
        };
        if send {
            self.last = Some((buttons, now));
        }
        send
    }
}

impl Default for InputSchedule {
    fn default() -> Self {
        Self::new(REFRESH)
    }
}
