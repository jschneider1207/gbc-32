//! Clock-scheduled scripted input (Stage 2 M5, "Scripted input mode"):
//! the button changes of an `acceptance/*.toml` `input` script, converted
//! to base clocks exactly as romtest does (`tools/replay-input`, which
//! calls romtest's own conversion), and handed to the core the way
//! romtest's run loop hands them (`tools/romtest/src/machine.rs` `run`):
//! every change the core's queue takes ([`gbc_core::SCHEDULED_INPUTS`]),
//! each with `Emulator::set_buttons_at` at its exact clock, so it lands on
//! its M-cycle wherever the run calls end (ADR 0012). A frame loop that
//! splits the run into `run_frame` calls therefore emulates exactly what
//! romtest's `run_until` calls emulate.
//!
//! This differs from [`crate::input::Script`], whose held set changes only
//! at frame starts (the frame grid of `scripts/inputs/*.txt`).
//!
//! # Text format (`clocks v1`)
//!
//! ```text
//! # comment (from `#` to the end of the line; blank lines are ignored)
//! clocks v1                  the first line that is not blank or a comment
//! <clock> <buttons>|none     from base clock <clock> on, exactly these are held
//! end <frames>               optional: the run length in frames
//! ```
//!
//! Clocks are decimal `u64`, strictly increasing and below `u64::MAX`
//! (the core reserves it). Buttons as in [`crate::input`] (`a+b`), or
//! `none` for a release of everything. The text is what romtest's
//! `input_timeline` returns: a sorted list of state changes.
//!
//! # Lateness
//!
//! [`ClockScript::apply`] runs between frames. A change whose clock has
//! already passed when its turn in the queue comes (more than
//! [`gbc_core::SCHEDULED_INPUTS`] changes inside about one frame) is
//! applied at once and counted in [`ClockScript::late`]; romtest stops
//! such a run as a crash, so a run with `late != 0` does not reproduce
//! romtest's emulation. `tools/replay-input` refuses scripts dense enough
//! for that to happen, and the `stats input` line ([`ClockScript::report`])
//! prints the count.

use core::fmt::Write;

use gbc_core::{Buttons, Emulator};

use crate::input::{InputSource, ParseError, ParseErrorKind, buttons, number_u64};

/// From `clock` on, exactly `buttons` are held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub clock: u64,
    pub buttons: Buttons,
}

impl Change {
    /// An empty slot, for sizing parse buffers: `[Change::NONE; N]`.
    pub const NONE: Self = Self {
        clock: 0,
        buttons: Buttons::EMPTY,
    };
}

/// The header line of the text format.
pub const HEADER: &str = "clocks v1";

/// What [`parse`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parsed {
    /// Changes written to the start of the output table.
    pub changes: usize,
    /// The `end <frames>` value, if there was one.
    pub end: Option<u32>,
}

/// Whether `text` is in this format (its first line that is not blank or
/// a comment is [`HEADER`]), rather than the frame format of
/// [`crate::input`].
pub fn is_clock_table(text: &str) -> bool {
    text.lines()
        .map(|l| l.find('#').map_or(l, |k| &l[..k]).trim())
        .find(|l| !l.is_empty())
        == Some(HEADER)
}

/// Parses the text format (module docs) into `out`, without allocating.
pub fn parse(text: &str, out: &mut [Change]) -> Result<Parsed, ParseError> {
    let mut n = 0;
    let mut end = None;
    let mut header = false;
    let mut last: Option<u64> = None;
    for (i, raw) in text.lines().enumerate() {
        let line = u32::try_from(i + 1).unwrap_or(u32::MAX);
        let err = |kind| ParseError { line, kind };
        let body = raw.find('#').map_or(raw, |k| &raw[..k]).trim();
        if body.is_empty() {
            continue;
        }
        if !header {
            if body != HEADER {
                return Err(err(ParseErrorKind::Syntax));
            }
            header = true;
            continue;
        }
        let mut words = body.split_ascii_whitespace();
        let (Some(a), Some(b), None) = (words.next(), words.next(), words.next()) else {
            return Err(err(ParseErrorKind::Syntax));
        };
        if a == "end" {
            if end.is_some() {
                return Err(err(ParseErrorKind::DuplicateEnd));
            }
            let f = number_u64(b)
                .and_then(|f| u32::try_from(f).ok())
                .ok_or(err(ParseErrorKind::Number))?;
            end = Some(f);
            continue;
        }
        let clock = number_u64(a)
            .filter(|&c| c != u64::MAX)
            .ok_or(err(ParseErrorKind::Number))?;
        if last.is_some_and(|l| clock <= l) {
            return Err(err(ParseErrorKind::Order));
        }
        let held = if b == "none" {
            Buttons::EMPTY
        } else {
            buttons(b).ok_or(err(ParseErrorKind::Button))?
        };
        let slot = out.get_mut(n).ok_or(err(ParseErrorKind::TooMany))?;
        *slot = Change {
            clock,
            buttons: held,
        };
        n += 1;
        last = Some(clock);
    }
    if !header {
        return Err(ParseError {
            line: 1,
            kind: ParseErrorKind::Syntax,
        });
    }
    Ok(Parsed { changes: n, end })
}

/// A clock-scheduled input script over a borrowed, clock-sorted table
/// (module docs). One per run: it remembers which changes it handed over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockScript<'a> {
    changes: &'a [Change],
    next: usize,
    late: u32,
    held: Buttons,
}

impl<'a> ClockScript<'a> {
    /// `changes` must be sorted by clock (as [`parse`] guarantees).
    pub const fn new(changes: &'a [Change]) -> Self {
        Self {
            changes,
            next: 0,
            late: 0,
            held: Buttons::EMPTY,
        }
    }

    /// Changes handed to the core so far.
    pub const fn handed(&self) -> usize {
        self.next
    }

    /// Changes applied late (module docs): 0 in a run that matches romtest.
    pub const fn late(&self) -> u32 {
        self.late
    }

    /// FNV-1a of the table: the tag byte `c` (so no table hashes like a
    /// [`crate::input::Script`] of the same length), then each change as
    /// its clock (little-endian `u64`) and buttons byte. The header's
    /// `input_fnv=`.
    pub fn table_fnv(&self) -> u64 {
        let mut h = crate::fnv::Fnv1a::new();
        h.byte(b'c');
        for c in self.changes {
            h.u64(c.clock);
            h.byte(c.buttons.bits());
        }
        h.get()
    }

    /// The report-only line
    /// `stats input clock_changes=<n> handed=<k> late=<l>`.
    pub fn report<W: Write + ?Sized>(&self, out: &mut W) -> core::fmt::Result {
        writeln!(
            out,
            "stats input clock_changes={} handed={} late={}",
            self.changes.len(),
            self.next,
            self.late
        )
    }
}

impl InputSource for ClockScript<'_> {
    /// The buttons of the last change handed over (they may not have
    /// landed yet). Not used by the loops: [`InputSource::apply`] is.
    fn buttons(&mut self, _frame: u32) -> Buttons {
        self.held
    }

    fn fnv(&self) -> u64 {
        self.table_fnv()
    }

    /// romtest's hand-over (`machine::run`): every change the queue takes,
    /// each at its own clock; one whose clock has passed is applied now and
    /// counted late. `held` is not used (changes are not frame-sampled).
    fn apply(&mut self, _frame: u32, emu: &mut Emulator<'_>, _held: &mut Buttons) {
        let now = emu.clock();
        while let Some(&c) = self.changes.get(self.next) {
            if c.clock < now {
                self.late = self.late.saturating_add(1);
                emu.set_buttons(c.buttons);
            } else if !emu.set_buttons_at(c.clock, c.buttons) {
                // Queue full: the rest waits for the next frame.
                break;
            }
            self.held = c.buttons;
            self.next += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ch(clock: u64, buttons: Buttons) -> Change {
        Change { clock, buttons }
    }

    #[test]
    fn parses_the_clock_format() {
        let text = "# from acceptance/stage1.toml rtc3test-basic\n\nclocks v1\n4194304 a\n4613732 none # release\nend 960\n";
        assert!(is_clock_table(text));
        let mut out = [Change::NONE; 3];
        let p = parse(text, &mut out).unwrap();
        assert_eq!(
            p,
            Parsed {
                changes: 2,
                end: Some(960)
            }
        );
        assert_eq!(out[0], ch(4_194_304, Buttons::A));
        assert_eq!(out[1], ch(4_613_732, Buttons::EMPTY));
        assert_eq!(
            parse("clocks v1\n", &mut []).unwrap(),
            Parsed {
                changes: 0,
                end: None
            }
        );
        // The frame format is not a clock table.
        assert!(!is_clock_table("300 4 start\n"));
        assert!(!is_clock_table(""));
    }

    #[test]
    fn rejects_bad_clock_lines_with_their_line_number() {
        let mut out = [Change::NONE; 2];
        for (text, line, kind) in [
            ("", 1, ParseErrorKind::Syntax),
            ("10 a", 1, ParseErrorKind::Syntax),
            ("clocks v2", 1, ParseErrorKind::Syntax),
            ("clocks v1\n10", 2, ParseErrorKind::Syntax),
            ("clocks v1\n10 a b", 2, ParseErrorKind::Syntax),
            ("clocks v1\nx a", 2, ParseErrorKind::Number),
            (
                "clocks v1\n18446744073709551615 a",
                2,
                ParseErrorKind::Number,
            ),
            (
                "clocks v1\n18446744073709551616 a",
                2,
                ParseErrorKind::Number,
            ),
            ("clocks v1\nend 4294967296", 2, ParseErrorKind::Number),
            ("clocks v1\n10 A", 2, ParseErrorKind::Button),
            ("clocks v1\n10 a\n10 b", 3, ParseErrorKind::Order),
            ("clocks v1\n10 a\n9 b", 3, ParseErrorKind::Order),
            ("clocks v1\n1 a\n2 b\n3 a", 4, ParseErrorKind::TooMany),
            ("clocks v1\nend 1\nend 2", 3, ParseErrorKind::DuplicateEnd),
        ] {
            assert_eq!(
                parse(text, &mut out),
                Err(ParseError { line, kind }),
                "{text:?}"
            );
        }
    }

    #[test]
    fn table_hash_covers_every_field_and_differs_from_a_frame_script() {
        let base = [ch(4, Buttons::A)];
        let h = ClockScript::new(&base).table_fnv();
        for other in [ch(8, Buttons::A), ch(4, Buttons::B)] {
            assert_ne!(ClockScript::new(&[other]).table_fnv(), h, "{other:?}");
        }
        assert_ne!(ClockScript::new(&[]).table_fnv(), crate::fnv::OFFSET_BASIS);
    }
}
