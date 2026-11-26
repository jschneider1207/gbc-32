//! Scripted button input for the frame loops: a table of presses, each
//! holding some buttons for a number of frames; and [`InputSource`], the
//! per-frame input hook of [`crate::paced::run_with`] that a script and
//! live input ([`crate::buttons::LiveInput`]) both implement.
//!
//! # Text format
//!
//! The button-program format of `scripts/synth-input.exs` (used by
//! `scripts/inputs/*.txt`), one entry per line:
//!
//! ```text
//! # comment (from `#` to the end of the line; blank lines are ignored)
//! <frame> <frames held> <button>[+<button>...]    e.g. `600 4 start`, `30 2 a+b`
//! end <frames>                                    optional: the run length
//! ```
//!
//! Buttons: `a b select start right left up down` (lower case). Numbers
//! are decimal `u32`. Presses may overlap and come in any order; the held
//! set in a frame is the union of the presses covering it. A press with 0
//! frames held does nothing.
//!
//! # Timing
//!
//! Frame `n` is the `n`th run call of the headless loop, counted from 0
//! ([`crate::headless`]: one `run_frame` call per frame). The held set for
//! frame `n` is applied with `Emulator::set_buttons` right before call
//! `n`, so it is on the frame grid, not the clock grid of romtest's
//! `input` (`acceptance/README.md`), and needs no scheduling queue.

use gbc_core::Buttons;

/// One press: `buttons` held for frames `frame .. frame + hold`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Press {
    pub frame: u32,
    pub hold: u32,
    pub buttons: Buttons,
}

impl Press {
    /// An empty slot, for sizing parse buffers: `[Press::NONE; N]`.
    pub const NONE: Self = Self {
        frame: 0,
        hold: 0,
        buttons: Buttons::EMPTY,
    };

    /// Whether this press holds its buttons in `frame`.
    #[inline]
    pub const fn covers(&self, frame: u32) -> bool {
        // `frame - self.frame < hold`, written so `frame + hold` can't
        // overflow.
        frame >= self.frame && frame - self.frame < self.hold
    }
}

/// An input script: a borrowed table of presses (from [`parse`], or a
/// `const` table compiled into a firmware image).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Script<'a> {
    presses: &'a [Press],
}

impl<'a> Script<'a> {
    /// No input: nothing is ever pressed.
    pub const EMPTY: Script<'static> = Script { presses: &[] };

    pub const fn new(presses: &'a [Press]) -> Self {
        Self { presses }
    }

    pub const fn presses(&self) -> &'a [Press] {
        self.presses
    }

    /// The buttons held in `frame`: the union of the presses covering it.
    /// Linear in the table length, once per frame; a script is a few
    /// hundred entries at most, against ~70,000 emulated clocks a frame.
    pub fn held(&self, frame: u32) -> Buttons {
        let mut b = Buttons::EMPTY;
        for p in self.presses {
            if p.covers(frame) {
                b |= p.buttons;
            }
        }
        b
    }

    /// FNV-1a of the table (each press as `frame` and `hold`, little-endian
    /// `u32`, then the buttons byte), for the header line, so a board run
    /// can be seen to use the same script as the host run.
    pub fn fnv(&self) -> u64 {
        let mut h = crate::fnv::Fnv1a::new();
        for p in self.presses {
            h.u32(p.frame);
            h.u32(p.hold);
            h.byte(p.buttons.bits());
        }
        h.get()
    }
}

/// Where a frame loop's buttons come from ([`crate::paced::run_with`]):
/// a [`Script`] for deterministic replays, or live board and PC input
/// ([`crate::buttons::LiveInput`]).
///
/// The loop calls [`InputSource::buttons`] once per frame, for frame `n`
/// right before run call `n`, and applies the result at that call's start
/// clock (the emulator's clock between calls). That is
/// `Emulator::set_buttons`, which gbc-core defines as
/// `set_buttons_at(clock(), buttons)`: the plan's "applied with
/// `set_buttons_at` at the frame's start clock".
pub trait InputSource {
    /// The buttons held during frame `n`. Called exactly once per frame,
    /// with `n` counting up from 0.
    fn buttons(&mut self, frame: u32) -> Buttons;
    /// The `input_fnv=` value of the header line: [`Script::fnv`] for a
    /// script. A live source has no table to hash and returns
    /// [`LIVE_INPUT_FNV`]; its compared lines are not reproducible anyway.
    fn fnv(&self) -> u64;
}

/// The header's `input_fnv=` for live input (no table to hash).
pub const LIVE_INPUT_FNV: u64 = 0;

impl InputSource for Script<'_> {
    #[inline]
    fn buttons(&mut self, frame: u32) -> Buttons {
        self.held(frame)
    }

    fn fnv(&self) -> u64 {
        Script::fnv(self)
    }
}

/// What [`parse`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parsed {
    /// Presses written to the start of the output table.
    pub presses: usize,
    /// The `end <frames>` value, if there was one.
    pub end: Option<u32>,
}

/// Why [`parse`] failed, and on which line (1-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: u32,
    pub kind: ParseErrorKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseErrorKind {
    /// Not `<frame> <held> <buttons>` or `end <frames>`.
    Syntax,
    /// A number that is not a decimal `u32`.
    Number,
    /// A button name not in the list, or an empty one (`a++b`).
    Button,
    /// More presses than the output table holds.
    TooMany,
    /// A second `end` line.
    DuplicateEnd,
}

/// Parses the text format (module docs) into `out`, without allocating.
/// The presses keep their order in the text.
pub fn parse(text: &str, out: &mut [Press]) -> Result<Parsed, ParseError> {
    let mut n = 0;
    let mut end = None;
    for (i, raw) in text.lines().enumerate() {
        let line = u32::try_from(i + 1).unwrap_or(u32::MAX);
        let err = |kind| ParseError { line, kind };
        let body = match raw.find('#') {
            Some(k) => &raw[..k],
            None => raw,
        };
        let mut words = body.split_ascii_whitespace();
        let (Some(a), b, c, None) = (words.next(), words.next(), words.next(), words.next()) else {
            if body.trim().is_empty() {
                continue;
            }
            return Err(err(ParseErrorKind::Syntax));
        };
        match (a, b, c) {
            ("end", Some(f), None) => {
                if end.is_some() {
                    return Err(err(ParseErrorKind::DuplicateEnd));
                }
                end = Some(number(f).ok_or(err(ParseErrorKind::Number))?);
            }
            ("end", ..) => return Err(err(ParseErrorKind::Syntax)),
            (f, Some(h), Some(names)) => {
                let frame = number(f).ok_or(err(ParseErrorKind::Number))?;
                let hold = number(h).ok_or(err(ParseErrorKind::Number))?;
                let buttons = buttons(names).ok_or(err(ParseErrorKind::Button))?;
                let slot = out.get_mut(n).ok_or(err(ParseErrorKind::TooMany))?;
                *slot = Press {
                    frame,
                    hold,
                    buttons,
                };
                n += 1;
            }
            _ => return Err(err(ParseErrorKind::Syntax)),
        }
    }
    Ok(Parsed { presses: n, end })
}

/// A decimal `u32`: digits only (no sign, no `_`).
fn number(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `a+b+start` to buttons.
fn buttons(s: &str) -> Option<Buttons> {
    let mut b = Buttons::EMPTY;
    for name in s.split('+') {
        b |= match name {
            "a" => Buttons::A,
            "b" => Buttons::B,
            "select" => Buttons::SELECT,
            "start" => Buttons::START,
            "right" => Buttons::RIGHT,
            "left" => Buttons::LEFT,
            "up" => Buttons::UP,
            "down" => Buttons::DOWN,
            _ => return None,
        };
    }
    Some(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn press(frame: u32, hold: u32, buttons: Buttons) -> Press {
        Press {
            frame,
            hold,
            buttons,
        }
    }

    #[test]
    fn held_is_the_union_of_covering_presses() {
        let table = [
            press(10, 4, Buttons::A),
            press(12, 5, Buttons::START),
            press(20, 0, Buttons::B),
        ];
        let s = Script::new(&table);
        assert_eq!(s.held(9), Buttons::EMPTY);
        assert_eq!(s.held(10), Buttons::A);
        assert_eq!(s.held(11), Buttons::A);
        assert_eq!(s.held(12), Buttons::A | Buttons::START);
        assert_eq!(s.held(13), Buttons::A | Buttons::START);
        // A press covers `frame .. frame + hold`, exclusive.
        assert_eq!(s.held(14), Buttons::START);
        assert_eq!(s.held(16), Buttons::START);
        assert_eq!(s.held(17), Buttons::EMPTY);
        // A zero-length press holds nothing.
        assert_eq!(s.held(20), Buttons::EMPTY);
        assert_eq!(Script::EMPTY.held(0), Buttons::EMPTY);
    }

    #[test]
    fn held_does_not_overflow_near_u32_max() {
        let table = [press(u32::MAX - 1, u32::MAX, Buttons::UP)];
        let s = Script::new(&table);
        assert_eq!(s.held(u32::MAX - 2), Buttons::EMPTY);
        assert_eq!(s.held(u32::MAX - 1), Buttons::UP);
        assert_eq!(s.held(u32::MAX), Buttons::UP);
    }

    #[test]
    fn parses_the_program_format() {
        let text = "# header\n\n300 4 start\n  650 4 a+b   # trailing\nend 900\n30\t2\tup+down+left+right+select\n";
        let mut out = [Press::NONE; 4];
        let p = parse(text, &mut out).unwrap();
        assert_eq!(
            p,
            Parsed {
                presses: 3,
                end: Some(900)
            }
        );
        assert_eq!(out[0], press(300, 4, Buttons::START));
        assert_eq!(out[1], press(650, 4, Buttons::A | Buttons::B));
        assert_eq!(
            out[2],
            press(
                30,
                2,
                Buttons::UP | Buttons::DOWN | Buttons::LEFT | Buttons::RIGHT | Buttons::SELECT
            )
        );
        assert_eq!(out[3], Press::NONE);
        assert_eq!(
            parse("", &mut []).unwrap(),
            Parsed {
                presses: 0,
                end: None
            }
        );
    }

    #[test]
    fn rejects_bad_lines_with_their_line_number() {
        let mut out = [Press::NONE; 2];
        for (text, line, kind) in [
            ("1 2", 1, ParseErrorKind::Syntax),
            ("\n1 2 a b", 2, ParseErrorKind::Syntax),
            ("end", 1, ParseErrorKind::Syntax),
            ("end 1 2", 1, ParseErrorKind::Syntax),
            ("x 2 a", 1, ParseErrorKind::Number),
            ("1 -2 a", 1, ParseErrorKind::Number),
            ("1 +2 a", 1, ParseErrorKind::Number),
            ("1 4294967296 a", 1, ParseErrorKind::Number),
            ("end x", 1, ParseErrorKind::Number),
            ("1 2 A", 1, ParseErrorKind::Button),
            ("1 2 a++b", 1, ParseErrorKind::Button),
            ("1 2 a+", 1, ParseErrorKind::Button),
            ("1 1 a\n2 1 a\n3 1 a", 3, ParseErrorKind::TooMany),
            ("end 1\n# x\nend 2", 3, ParseErrorKind::DuplicateEnd),
        ] {
            assert_eq!(
                parse(text, &mut out),
                Err(ParseError { line, kind }),
                "{text:?}"
            );
        }
    }

    #[test]
    fn the_committed_ladx_program_parses() {
        // The one program in scripts/inputs/, to keep the formats in step.
        let text = include_str!("../../../scripts/inputs/ladx-new-file-slides.txt");
        let mut out = [Press::NONE; 512];
        let p = parse(text, &mut out).unwrap();
        assert!(p.presses > 10, "{p:?}");
        assert!(p.end.is_some(), "{p:?}");
    }

    #[test]
    fn script_hash_covers_every_field() {
        let base = [press(1, 2, Buttons::A)];
        let h = Script::new(&base).fnv();
        for other in [
            press(2, 2, Buttons::A),
            press(1, 3, Buttons::A),
            press(1, 2, Buttons::B),
        ] {
            assert_ne!(Script::new(&[other]).fnv(), h, "{other:?}");
        }
        assert_eq!(Script::EMPTY.fnv(), crate::fnv::OFFSET_BASIS);
    }
}
