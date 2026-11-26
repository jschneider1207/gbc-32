//! Live buttons (Stage 2 M5, decision D2): the board's six raw inputs,
//! debounced and mapped to Game Boy buttons, OR-ed with the PC's button
//! bitmap from the serial link.
//!
//! # Inputs and mapping
//!
//! The board has the shield's 5-way joystick (LEFT PE3, CENTER PA0, DOWN
//! PF8, UP PF9, RIGHT PF7; active low, common to GND: UM2905 Table 4) and
//! the user button B1 (PC13; UM2407 §7.6.2). The board crate reads the
//! pins and hands over a [`Raw`] with **1 = pressed** (it owns the pin
//! polarity). [`to_buttons`] maps them:
//!
//! | raw     | Game Boy |
//! |---------|----------|
//! | LEFT, RIGHT, UP, DOWN | the D-pad |
//! | B1      | Start ("if convenient", D2) |
//! | CENTER  | nothing (spare); with B1 it is the save-dump chord ([`SAVE_CHORD`]) |
//!
//! Holding the chord also holds Start (B1). All 8 buttons come from the
//! PC ([`from_pc`]), so PC input alone can play; [`merge`] ORs the two.
//!
//! # PC bitmap
//!
//! `gbc_link::Message::Input { buttons }` (and the constants in
//! `gbc_link::buttons`): bit 0 A, 1 B, 2 Select, 3 Start, 4 Right, 5 Left,
//! 6 Up, 7 Down; 1 = pressed. That is `gbc_core::Buttons`' layout, so
//! [`from_pc`] is the identity on all 8 bits. This crate does not depend
//! on gbc-link (ADR 0036); `tools/gbc-pc/tests/buttons.rs`, which sees
//! both, checks every bit.
//!
//! # Debounce
//!
//! [`Debouncer`] is an integer counter per input: a line changes state
//! once it has read the other value in `SAMPLES` consecutive samples, so
//! any run of fewer differing samples (a bounce or a glitch) is ignored.
//! Latency: a clean change is reported by the `SAMPLES`th sample that sees
//! it, i.e. `SAMPLES - 1` sample periods after the first.
//!
//! The player loop samples once per frame (in [`LiveInput`], right before
//! each run call) with [`FRAME_SAMPLES`] = 2: the 16.7 ms frame period is
//! longer than contact bounce (a few ms), so 2 samples reject a bounce or
//! glitch seen in one frame, at 1 frame (16.7 ms) of added latency. From
//! the press to the emulator: up to 1 frame until the next sample, plus
//! that 1 frame, so 16.7-33.5 ms. A board that samples faster (a timer
//! interrupt) can pick its own `SAMPLES` for the same rejection time.

use core::cell::Cell;

pub use gbc_core::Buttons;

use crate::input::{InputSource, LIVE_INPUT_FNV};

/// The board's raw inputs, 1 = pressed (the board crate converts the
/// joystick's active-low levels).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Raw(u8);

impl Raw {
    pub const NONE: Self = Self(0);
    pub const LEFT: Self = Self(1 << 0);
    pub const RIGHT: Self = Self(1 << 1);
    pub const UP: Self = Self(1 << 2);
    pub const DOWN: Self = Self(1 << 3);
    pub const CENTER: Self = Self(1 << 4);
    pub const B1: Self = Self(1 << 5);
    /// Every defined line.
    pub const ALL: Self = Self(0x3F);

    /// Unused bits are dropped.
    pub const fn from_bits_truncate(bits: u8) -> Self {
        Self(bits & Self::ALL.0)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl core::ops::BitOr for Raw {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

/// Number of raw lines ([`Raw::ALL`]).
pub const RAW_LINES: usize = 6;

/// `SAMPLES` for sampling once per frame (module docs).
pub const FRAME_SAMPLES: u8 = 2;

/// Joystick CENTER held with B1: the board's save-dump chord (plan M5).
pub const SAVE_CHORD: Raw = Raw::CENTER.union(Raw::B1);

/// A per-input integer-counter debouncer (module docs). Starts with
/// everything released.
#[derive(Clone, Debug)]
pub struct Debouncer<const SAMPLES: u8> {
    stable: u8,
    count: [u8; RAW_LINES],
}

impl<const SAMPLES: u8> Default for Debouncer<SAMPLES> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SAMPLES: u8> Debouncer<SAMPLES> {
    pub const fn new() -> Self {
        const { assert!(SAMPLES >= 1) };
        Self {
            stable: 0,
            count: [0; RAW_LINES],
        }
    }

    /// Feeds one sample and returns the debounced state.
    pub fn sample(&mut self, raw: Raw) -> Raw {
        let raw = Raw::from_bits_truncate(raw.0).0;
        for (i, count) in self.count.iter_mut().enumerate() {
            let bit = 1u8 << i;
            if (raw ^ self.stable) & bit == 0 {
                *count = 0;
            } else {
                *count += 1;
                if *count >= SAMPLES {
                    self.stable ^= bit;
                    *count = 0;
                }
            }
        }
        Raw(self.stable)
    }

    /// The debounced state after the last sample.
    pub const fn state(&self) -> Raw {
        Raw(self.stable)
    }
}

/// The board's raw inputs as Game Boy buttons (module docs, mapping).
pub const fn to_buttons(raw: Raw) -> Buttons {
    let mut b = Buttons::EMPTY;
    if raw.contains(Raw::LEFT) {
        b = b.union(Buttons::LEFT);
    }
    if raw.contains(Raw::RIGHT) {
        b = b.union(Buttons::RIGHT);
    }
    if raw.contains(Raw::UP) {
        b = b.union(Buttons::UP);
    }
    if raw.contains(Raw::DOWN) {
        b = b.union(Buttons::DOWN);
    }
    if raw.contains(Raw::B1) {
        b = b.union(Buttons::START);
    }
    b
}

/// The PC's `Input { buttons }` bitmap as buttons (module docs, PC
/// bitmap).
pub const fn from_pc(bits: u8) -> Buttons {
    Buttons::from_bits_truncate(bits)
}

/// Board and PC buttons together: a button is held if either holds it.
pub const fn merge(board: Buttons, pc: Buttons) -> Buttons {
    board.union(pc)
}

/// Reports the moment a chord becomes held (once per hold).
#[derive(Clone, Copy, Debug, Default)]
pub struct ChordEdge {
    held: bool,
}

impl ChordEdge {
    /// `true` on the first debounced state that holds all of `chord`
    /// after one that did not.
    pub fn update(&mut self, state: Raw, chord: Raw) -> bool {
        let now = state.contains(chord);
        let edge = now && !self.held;
        self.held = now;
        edge
    }
}

/// The board's pin read, once per sample.
pub trait RawInputs {
    fn read(&mut self) -> Raw;
}

/// State shared between [`LiveInput`] (inside the frame loop) and the
/// board's per-frame service hook ([`crate::paced::Service`]), which
/// polls the UART: `Cell`s, since both hold it by `&` and run on the same
/// thread (no `Sync`; put it on `main`'s stack, not in an interrupt).
#[derive(Debug)]
pub struct Shared {
    pc: Cell<Buttons>,
    dump: Cell<bool>,
}

impl Default for Shared {
    fn default() -> Self {
        Self::new()
    }
}

impl Shared {
    pub const fn new() -> Self {
        Self {
            pc: Cell::new(Buttons::EMPTY),
            dump: Cell::new(false),
        }
    }

    /// The latest `Input { buttons }` from the PC (sent on change, so it
    /// holds until the next one).
    pub fn set_pc(&self, bits: u8) {
        self.pc.set(from_pc(bits));
    }

    pub fn pc(&self) -> Buttons {
        self.pc.get()
    }

    /// Whether the save-dump chord was pressed since the last call.
    pub fn take_dump_request(&self) -> bool {
        self.dump.replace(false)
    }
}

/// Live input for the player loop: samples the pins once per frame,
/// debounces, maps, ORs the PC's buttons, and flags the save-dump chord
/// in [`Shared`].
pub struct LiveInput<'s, G, const SAMPLES: u8> {
    pub pins: G,
    debouncer: Debouncer<SAMPLES>,
    chord: ChordEdge,
    shared: &'s Shared,
}

impl<'s, G: RawInputs, const SAMPLES: u8> LiveInput<'s, G, SAMPLES> {
    pub const fn new(pins: G, shared: &'s Shared) -> Self {
        Self {
            pins,
            debouncer: Debouncer::new(),
            chord: ChordEdge { held: false },
            shared,
        }
    }
}

impl<G: RawInputs, const SAMPLES: u8> InputSource for LiveInput<'_, G, SAMPLES> {
    fn buttons(&mut self, _frame: u32) -> Buttons {
        let state = self.debouncer.sample(self.pins.read());
        if self.chord.update(state, SAVE_CHORD) {
            self.shared.dump.set(true);
        }
        merge(to_buttons(state), self.shared.pc())
    }

    fn fnv(&self) -> u64 {
        LIVE_INPUT_FNV
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `samples` and returns the debounced state after each.
    fn run<const N: u8>(samples: &[Raw]) -> [Raw; 16] {
        let mut d = Debouncer::<N>::new();
        let mut out = [Raw::NONE; 16];
        for (o, &s) in out.iter_mut().zip(samples) {
            *o = d.sample(s);
        }
        out
    }

    const L: Raw = Raw::LEFT;
    const O: Raw = Raw::NONE;

    #[test]
    fn a_clean_press_and_release_take_samples_minus_one_periods() {
        let r = run::<2>(&[O, L, L, L, L, O, O, O]);
        assert_eq!(&r[..8], &[O, O, L, L, L, L, O, O]);
        let r = run::<4>(&[L, L, L, L, L, O, O, O, O, O]);
        assert_eq!(&r[..10], &[O, O, O, L, L, L, L, L, O, O]);
        // SAMPLES = 1 is no debounce.
        assert_eq!(&run::<1>(&[L, O, L])[..3], &[L, O, L]);
    }

    #[test]
    fn runs_shorter_than_samples_are_rejected() {
        // Single-sample glitches never get through at 2.
        assert_eq!(&run::<2>(&[L, O, L, O, L, O])[..6], &[O; 6]);
        // A bounce on press (one stray release) restarts the count.
        assert_eq!(&run::<3>(&[L, L, O, L, L, L])[..6], &[O, O, O, O, O, L]);
        // ... and on release.
        let r = run::<2>(&[L, L, O, L, O, O]);
        assert_eq!(&r[..6], &[O, L, L, L, L, O]);
    }

    #[test]
    fn lines_debounce_independently_and_extra_bits_are_ignored() {
        let mut d = Debouncer::<2>::new();
        assert_eq!(d.sample(Raw::UP), O);
        assert_eq!(d.sample(Raw::UP | Raw::B1), Raw::UP);
        assert_eq!(d.sample(Raw(0xC0) | Raw::B1), Raw::UP | Raw::B1);
        assert_eq!(d.sample(Raw(0xFF)), Raw::UP | Raw::B1);
        assert_eq!(d.state(), Raw::UP | Raw::B1);
        assert_eq!(d.sample(Raw(0xFF)), Raw::ALL);
    }

    #[test]
    fn mapping() {
        for (raw, b) in [
            (Raw::LEFT, Buttons::LEFT),
            (Raw::RIGHT, Buttons::RIGHT),
            (Raw::UP, Buttons::UP),
            (Raw::DOWN, Buttons::DOWN),
            (Raw::B1, Buttons::START),
            (Raw::CENTER, Buttons::EMPTY),
            (Raw::NONE, Buttons::EMPTY),
        ] {
            assert_eq!(to_buttons(raw), b, "{raw:?}");
        }
        assert_eq!(
            to_buttons(Raw::ALL),
            Buttons::LEFT | Buttons::RIGHT | Buttons::UP | Buttons::DOWN | Buttons::START
        );
    }

    #[test]
    fn pc_bitmap_and_merge() {
        let names = [
            Buttons::A,
            Buttons::B,
            Buttons::SELECT,
            Buttons::START,
            Buttons::RIGHT,
            Buttons::LEFT,
            Buttons::UP,
            Buttons::DOWN,
        ];
        for (i, b) in names.into_iter().enumerate() {
            assert_eq!(from_pc(1 << i), b, "bit {i}");
        }
        assert_eq!(from_pc(0xFF).bits(), 0xFF);
        assert_eq!(
            merge(Buttons::LEFT | Buttons::START, Buttons::A | Buttons::LEFT),
            Buttons::LEFT | Buttons::START | Buttons::A
        );
        assert_eq!(merge(Buttons::EMPTY, Buttons::EMPTY), Buttons::EMPTY);
    }

    struct Pins<'a>(&'a [Raw], usize);
    impl RawInputs for Pins<'_> {
        fn read(&mut self) -> Raw {
            let r = self.0[self.1.min(self.0.len() - 1)];
            self.1 += 1;
            r
        }
    }

    #[test]
    fn live_input_debounces_merges_and_flags_the_chord_once() {
        let pins = [O, L, L, SAVE_CHORD, SAVE_CHORD, SAVE_CHORD, O, O];
        let shared = Shared::new();
        let mut live = LiveInput::<_, FRAME_SAMPLES>::new(Pins(&pins, 0), &shared);
        shared.set_pc(Buttons::A.bits());
        let got: [Buttons; 8] = core::array::from_fn(|n| live.buttons(n as u32));
        let a = Buttons::A;
        assert_eq!(
            got,
            [
                a,
                a,
                a | Buttons::LEFT,
                a | Buttons::LEFT,
                a | Buttons::START,
                a | Buttons::START,
                a | Buttons::START,
                a
            ]
        );
        assert!(shared.take_dump_request());
        assert!(!shared.take_dump_request());
        assert_eq!(live.fnv(), LIVE_INPUT_FNV);
    }

    #[test]
    fn chord_edge_fires_once_per_hold() {
        let mut c = ChordEdge::default();
        let seq = [
            O,
            Raw::CENTER,
            SAVE_CHORD,
            SAVE_CHORD | L,
            Raw::B1,
            SAVE_CHORD,
        ];
        let got: [bool; 6] = core::array::from_fn(|i| c.update(seq[i], SAVE_CHORD));
        assert_eq!(got, [false, false, true, false, false, true]);
    }
}
