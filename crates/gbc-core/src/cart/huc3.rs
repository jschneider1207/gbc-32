//! The HuC3 clock chip: a minute-of-day counter and a day counter behind a
//! nibble-wide command mailbox (see `docs/reference/hudson-mappers.md`).
//!
//! Sources: Pan Docs, "HuC-3" (the mode register, the mailbox registers,
//! the command list and the location map), and SameBoy `Core/memory.c`
//! (`huc3_write`, `read_mbc_ram`), `Core/timing.c` (`GB_rtc_set_time`) and
//! `Core/gb.h` (`huc3`, `GB_huc3_rtc_time_t`) for what Pan Docs leaves
//! open. Nothing here is copied from SameBoy; where the two disagree the
//! comment at that rule says which one is followed. No test ROM covers
//! the chip, so every rule is unverified on hardware beyond those sources.
//!
//! UNPINNED: huc3-clock-protocol (a): the whole command protocol and
//! location map (Pan Docs, SameBoy for the gaps); no test ROM. The rules
//! where the two sources differ or are silent carry their own tag below.
//! A HuC3 cartridge on hardware, or a test ROM run on one, would settle
//! them.
//!
//! Like the MBC3 clock (`rtc.rs`) it counts emulated time only and is
//! caught up lazily: [`Cart`](super::Cart) advances it by the elapsed base
//! clocks when a command that reads or sets the time runs, and for
//! `huc3_rtc_state`/`set_huc3_rtc_state` and a reset.

use crate::CLOCK_HZ;

/// Minutes in a day: where the minute-of-day counter rolls over (Pan Docs:
/// HuC-3, "Minute of day counter (rolls over at 1440)").
const DAY_MINUTES: u64 = 1440;
/// Base clocks per minute. The clock chip has its own crystal, so, like
/// the MBC3 clock, it does not follow CGB double speed.
const MINUTE_CLOCKS: u32 = 60 * CLOCK_HZ;

/// Complete HuC3 clock state, for the caller to save and restore
/// ([`Emulator::huc3_rtc_state`], [`Emulator::set_huc3_rtc_state`]). The
/// clock advances only with emulated time; the caller may add wall-clock
/// time on load ([`Huc3RtcState::advance_seconds`]).
///
/// The fields are raw counter bits, not a validated time: `minutes` may
/// hold 1440-4095, which a running clock never reaches but a game can
/// write. The four 16-bit values are the ones SameBoy and BGB store in
/// their HuC3 battery saves (`GB_huc3_rtc_time_t`). The chip's mailbox
/// (command, access address, response, the time buffer at locations
/// 00-06) is not part of it: games set those up for every command.
///
/// `#[non_exhaustive]`: start from `Huc3RtcState::default()` and set the
/// fields.
///
/// [`Emulator::huc3_rtc_state`]: crate::Emulator::huc3_rtc_state
/// [`Emulator::set_huc3_rtc_state`]: crate::Emulator::set_huc3_rtc_state
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Huc3RtcState {
    /// Minute of the day, 12 bits (three nibbles). A running clock keeps
    /// it in 0-1439.
    pub minutes: u16,
    /// Day counter, 16 bits (four nibbles in the time buffer, as in
    /// SameBoy; Pan Docs lists three for the counter itself). Wraps to 0
    /// after 65535 with no overflow flag.
    ///
    /// UNPINNED: huc3-day-width (c): four nibbles (SameBoy, and the
    /// 17-byte save footer) against Pan Docs' three (locations 13-15).
    pub days: u16,
    /// Event (alarm) time, minute of the day: locations 58-5A, 12 bits.
    /// Stored for the game to read back; the alarm itself (a tone from
    /// the cartridge's speaker) is not emulated.
    pub alarm_minutes: u16,
    /// Event (alarm) time, day: locations 5B-5E, 16 bits.
    pub alarm_days: u16,
    /// Location 5F bit 0 (SameBoy: alarm enabled).
    pub alarm_enabled: bool,
    /// Base clocks elapsed in the current minute, `0..60 * CLOCK_HZ`
    /// ([`CLOCK_HZ`]).
    pub subminute_clocks: u32,
}

impl Huc3RtcState {
    /// The counters cut to their widths (`minutes` and `alarm_minutes` to
    /// 12 bits) and `subminute_clocks` clamped below one minute. What
    /// [`Emulator::set_huc3_rtc_state`](crate::Emulator::set_huc3_rtc_state)
    /// stores.
    #[must_use]
    pub const fn masked(self) -> Self {
        let max = MINUTE_CLOCKS - 1;
        Self {
            minutes: self.minutes & 0xFFF,
            days: self.days,
            alarm_minutes: self.alarm_minutes & 0xFFF,
            alarm_days: self.alarm_days,
            alarm_enabled: self.alarm_enabled,
            subminute_clocks: if self.subminute_clocks > max {
                max
            } else {
                self.subminute_clocks
            },
        }
    }

    /// Adds `seconds` of wall-clock time: whole minutes go to the
    /// counters (see [`Huc3RtcState`] for the rollovers), the rest stays
    /// in `subminute_clocks`. Correct for any `seconds` (not a loop). The
    /// fields are public, so `self` may come straight from a save file:
    /// it is [`masked`](Self::masked) first. The alarm fields are
    /// untouched.
    ///
    /// For syncing on load, exactly as for the MBC3 clock: `emu.load(..)`,
    /// then `emu.set_huc3_rtc_state(saved.advance_seconds(elapsed))`.
    #[must_use]
    pub fn advance_seconds(self, seconds: u64) -> Self {
        let mut s = self.masked();
        // Seconds already in the current minute, and the part of a second.
        let whole = u64::from(s.subminute_clocks / CLOCK_HZ);
        let part = s.subminute_clocks % CLOCK_HZ;
        // `whole < 60`, so neither sum can overflow for any `seconds`.
        let rem = whole + seconds % 60;
        s.add_minutes(seconds / 60 + rem / 60);
        s.subminute_clocks = (rem % 60) as u32 * CLOCK_HZ + part;
        s
    }

    /// `n` minute ticks. The minute of the day rolls over at 1440 into
    /// the day counter (Pan Docs: HuC-3 location map; SameBoy
    /// `GB_rtc_set_time`), which wraps at 16 bits.
    ///
    /// A value of 1440 or more (only reachable by a write: a game, or a
    /// save) counts up to the top of the 12-bit register and wraps to 0
    /// without a day carry. That is the rule the MBC3 registers follow
    /// (`rtc.rs` `count`), assumed here: neither source says what the
    /// chip does (SameBoy compares for equality with 1440 on a wider
    /// variable, so such a value would count on to 65535). `minutes`
    /// must be 12 bits (callers mask).
    ///
    /// UNPINNED: huc3-minute-overflow (a): no source; the MBC3 rule by
    /// analogy. Setting the minute to 1440 or more on a cartridge and
    /// reading it back over time would settle it.
    fn add_minutes(&mut self, mut n: u64) {
        let mut m = u64::from(self.minutes & 0xFFF);
        if m >= DAY_MINUTES {
            let to_wrap = 0x1000 - m;
            if n < to_wrap {
                self.minutes = (m + n) as u16;
                return;
            }
            n -= to_wrap;
            m = 0;
        }
        // Split so `m + n` cannot overflow for any `n`.
        let rem = m + n % DAY_MINUTES;
        self.minutes = (rem % DAY_MINUTES) as u16;
        let days = n / DAY_MINUTES + rem / DAY_MINUTES;
        self.days = self.days.wrapping_add(days as u16);
    }

    /// Advances the clock by `clocks` base clocks of emulated time. Out
    /// of line: the 64-bit divisions are bulky and every caller is cold.
    #[inline(never)]
    pub(super) fn advance(&mut self, clocks: u64) {
        let minute = u64::from(MINUTE_CLOCKS);
        // `subminute_clocks < minute` (masked), so no overflow.
        let rem = u64::from(self.subminute_clocks) + clocks % minute;
        self.add_minutes(clocks / minute + rem / minute);
        self.subminute_clocks = (rem % minute) as u32;
    }
}

/// Nibble `i` of `x` (nibble 0 is the least significant: multi-nibble
/// values are little endian in the chip's memory, Pan Docs: HuC-3, "RTC
/// Communication Protocol").
const fn nibble(x: u32, i: u8) -> u8 {
    (x >> (4 * i as u32)) as u8 & 0x0F
}

/// `x` with nibble `i` replaced by the low 4 bits of `v`.
const fn with_nibble(x: u32, i: u8, v: u8) -> u32 {
    let shift = 4 * i as u32;
    x & !(0x0F << shift) | ((v & 0x0F) as u32) << shift
}

/// The clock chip as the cartridge holds it: the clock (as of the clock
/// `Cart::rtc_synced`) and the mailbox state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Huc3 {
    pub(super) clock: Huc3RtcState,
    /// Locations 00-06, the buffer the time is read and set through: the
    /// minute of the day in nibbles 0-2 and the day in nibbles 3-6.
    buffer: u32,
    /// The access address: which of the chip's 256 nibble locations the
    /// next read or write command uses.
    address: u8,
    /// The last value written to the command/argument register (mode B):
    /// the command in bits 4-6 and its argument in bits 0-3.
    mailbox: u8,
    /// The result of the last command that produced one, 4 bits.
    response: u8,
}

impl Huc3 {
    pub(super) const fn new() -> Self {
        Self {
            clock: Huc3RtcState {
                minutes: 0,
                days: 0,
                alarm_minutes: 0,
                alarm_days: 0,
                alarm_enabled: false,
                subminute_clocks: 0,
            },
            buffer: 0,
            address: 0,
            mailbox: 0,
            response: 0,
        }
    }

    /// The mailbox back to zero (the clock keeps counting: it has its own
    /// battery). On hardware the chip presumably keeps the mailbox over a
    /// console power cycle too; clearing it keeps a reset reproducible,
    /// and games set the address and buffer up before each use.
    pub(super) fn reset_mailbox(&mut self) {
        *self = Self {
            clock: self.clock,
            ..Self::new()
        };
    }

    /// A write to the command/argument register (mode B). It only fills
    /// the mailbox: the command runs when the game clears the semaphore
    /// (Pan Docs: HuC-3, "$B", "$D"). D7 is not connected.
    ///
    /// SameBoy runs the command at this write and ignores the semaphore
    /// write (`huc3_write`, "Not sure what writes here mean, they're
    /// always 0xFE"). Games always do both, in this order, so the two
    /// models differ only for a program that skips the semaphore write.
    ///
    /// UNPINNED: huc3-command-on-semaphore (c): Pan Docs (the semaphore
    /// write runs the command) against SameBoy (this write does).
    pub(super) fn write_command(&mut self, value: u8) {
        self.mailbox = value & 0x7F;
    }

    /// A read of the command/response register (mode C): the last command
    /// written in bits 4-6, the last result in bits 0-3 (Pan Docs: HuC-3,
    /// "$C"), and bit 7, which is not connected and reads as open bus,
    /// "usually high". SameBoy returns the result alone (bits 4-7 clear);
    /// games use the low nibble only.
    ///
    /// UNPINNED: huc3-response-high-bits (c): Pan Docs' command echo and
    /// open-bus bit 7 against SameBoy's zeros.
    pub(super) const fn read_response(&self) -> u8 {
        0x80 | (self.mailbox & 0x70) | self.response
    }

    /// A read of the semaphore register (mode D): bit 0 set means the
    /// chip is ready for a command (Pan Docs: HuC-3, "$D"). Commands
    /// finish at once here, so it always is; how long the real chip stays
    /// busy is not documented. Bit 7 as in `read_response`; bits 1-6 are
    /// not documented and read 0 (SameBoy returns 01).
    ///
    /// UNPINNED: huc3-semaphore-read (c): 81 against SameBoy's 01, and
    /// never busy (no source for the busy time).
    pub(super) const fn read_semaphore(&self) -> u8 {
        0x81
    }

    /// Runs the command in the mailbox: the game wrote the semaphore
    /// register with bit 0 clear. The caller has caught the clock up.
    /// Returns whether the save changed (the time or the alarm was set).
    ///
    /// Commands (Pan Docs: HuC-3, "Known commands"; the argument is the
    /// low nibble):
    /// - 1: read the nibble at the access address into the response,
    ///   then increment the address;
    /// - 3: write the argument there, then increment the address;
    /// - 2: write the argument there without incrementing. That is
    ///   SameBoy's reading (`huc3_write`, `case 2: case 3:`); Pan Docs
    ///   only says one game uses it with argument 0, purpose unknown;
    /// - 4, 5: set the low, high nibble of the access address;
    /// - 6: an extended command, chosen by the argument;
    /// - 0, 7: never observed; nothing happens.
    ///
    /// UNPINNED: huc3-command-2 (a): SameBoy's reading only (write, no
    /// increment); Pan Docs: "purpose unknown".
    pub(super) fn execute(&mut self) -> bool {
        let arg = self.mailbox & 0x0F;
        match self.mailbox >> 4 {
            1 => {
                self.response = self.read_location();
                self.address = self.address.wrapping_add(1);
                false
            }
            2 => self.write_location(arg),
            3 => {
                let dirty = self.write_location(arg);
                self.address = self.address.wrapping_add(1);
                dirty
            }
            4 => {
                self.address = self.address & 0xF0 | arg;
                false
            }
            5 => {
                self.address = self.address & 0x0F | arg << 4;
                false
            }
            6 => self.extended(arg),
            _ => false,
        }
    }

    /// Extended commands (Pan Docs: HuC-3, "extended commands"):
    /// - 0: copy the current time to locations 00-06;
    /// - 1: copy locations 00-06 to the current time. The minute starts
    ///   over (`subminute_clocks` = 0): an assumption, by analogy with a
    ///   write to the MBC3 seconds register; neither source says. Pan
    ///   Docs adds that the event time is moved along so that the time
    ///   left until the event stays the same; that is not done, since no
    ///   event ever fires here;
    /// - 2: a status request at startup; "the games will not start if the
    ///   result is not $1" (SameBoy answers 1 too);
    /// - E: run twice, it starts the tone generator. There is no speaker:
    ///   accepted, nothing happens. Any other value likewise.
    ///
    /// SameBoy has no buffer: its locations 00-06 are the live counters,
    /// and extended commands 0 and 1 do nothing. For a game that follows
    /// the documented sequence (0, then read; or write, then 1) the two
    /// models give the same values.
    ///
    /// UNPINNED: huc3-time-buffer (c): Pan Docs' buffer at 00-06 with
    /// extended commands 0 and 1, against SameBoy's live counters there.
    /// UNPINNED: huc3-set-time-restarts-minute (a): no source; assumed by
    /// analogy with the MBC3 seconds write.
    fn extended(&mut self, arg: u8) -> bool {
        match arg {
            0 => {
                self.buffer = u32::from(self.clock.minutes) | u32::from(self.clock.days) << 12;
                false
            }
            1 => {
                self.clock.minutes = (self.buffer & 0xFFF) as u16;
                self.clock.days = (self.buffer >> 12) as u16;
                self.clock.subminute_clocks = 0;
                true
            }
            2 => {
                self.response = 1;
                false
            }
            _ => false,
        }
    }

    /// The nibble at the access address. Location map (Pan Docs: HuC-3,
    /// "RTC Location Map", with SameBoy's `huc3_write` for 5B-5F):
    /// - 00-06: the time buffer (three nibbles of minutes, four of days);
    /// - 10-12: the running minute-of-day counter; 13-16: the running day
    ///   counter (Pan Docs lists 13-15; 16 is the fourth nibble of the
    ///   16-bit counter kept here);
    /// - 58-5A: event time minutes; 5B-5E: event time days (5B-5D in Pan
    ///   Docs); 5F: bit 0 is SameBoy's alarm enable;
    /// - anything else (the tone settings at 26-27 among them) is not
    ///   kept and reads 0. SameBoy leaves the previous response in place
    ///   for a location it does not know.
    fn read_location(&self) -> u8 {
        let c = &self.clock;
        match self.address {
            a @ 0x00..=0x06 => nibble(self.buffer, a),
            a @ 0x10..=0x12 => nibble(u32::from(c.minutes), a - 0x10),
            a @ 0x13..=0x16 => nibble(u32::from(c.days), a - 0x13),
            a @ 0x58..=0x5A => nibble(u32::from(c.alarm_minutes), a - 0x58),
            a @ 0x5B..=0x5E => nibble(u32::from(c.alarm_days), a - 0x5B),
            0x5F => u8::from(c.alarm_enabled),
            _ => 0,
        }
    }

    /// Writes nibble `v` at the access address; returns whether the save
    /// changed. The buffer and the event time are writable. The running
    /// counters at 10-16 are not: games set the time through the buffer
    /// and extended command 1, and whether the chip accepts a direct
    /// write there is not documented. Other locations are not kept.
    ///
    /// UNPINNED: huc3-counter-direct-write (a): no source says whether a
    /// write to 10-16 lands; ignored here (SameBoy has no such locations).
    fn write_location(&mut self, v: u8) -> bool {
        let c = &mut self.clock;
        match self.address {
            a @ 0x00..=0x06 => {
                self.buffer = with_nibble(self.buffer, a, v);
                return false;
            }
            a @ 0x58..=0x5A => {
                c.alarm_minutes = with_nibble(u32::from(c.alarm_minutes), a - 0x58, v) as u16;
            }
            a @ 0x5B..=0x5E => {
                c.alarm_days = with_nibble(u32::from(c.alarm_days), a - 0x5B, v) as u16;
            }
            0x5F => c.alarm_enabled = v & 1 != 0,
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::run_for;
    extern crate std;
    use crate::Config;
    use std::vec;

    const HZ: u64 = CLOCK_HZ as u64;
    const MINUTE: u64 = MINUTE_CLOCKS as u64;

    fn at(days: u16, minutes: u16) -> Huc3RtcState {
        Huc3RtcState {
            minutes,
            days,
            ..Huc3RtcState::default()
        }
    }

    #[test]
    fn minutes_roll_over_into_days_at_1440() {
        let mut s = at(7, 1438);
        s.add_minutes(1);
        assert_eq!(s, at(7, 1439));
        s.add_minutes(1);
        assert_eq!(s, at(8, 0));
        let mut s = at(7, 100);
        s.add_minutes(3 * 1440 + 1400);
        assert_eq!(s, at(11, 60));
    }

    #[test]
    fn day_counter_wraps_at_16_bits() {
        let mut s = at(0xFFFF, 1439);
        s.add_minutes(1);
        assert_eq!(s, at(0, 0));
        let mut s = at(0xFFF0, 0);
        s.add_minutes(1440 * 0x20);
        assert_eq!(s, at(0x0010, 0));
    }

    #[test]
    fn out_of_range_minutes_wrap_at_12_bits_without_a_day_carry() {
        let mut s = at(3, 1500);
        s.add_minutes(1);
        assert_eq!(s, at(3, 1501));
        let mut s = at(3, 0xFFF);
        s.add_minutes(1);
        assert_eq!(s, at(3, 0));
        // 1440 itself is out of range: it does not carry either.
        let mut s = at(3, 1440);
        s.add_minutes(0x1000 - 1440 + 5);
        assert_eq!(s, at(3, 5));
        // Past the wrap, counting is normal again.
        let mut s = at(3, 0xFFE);
        s.add_minutes(2 + 1440 + 7);
        assert_eq!(s, at(4, 7));
    }

    #[test]
    fn advance_counts_base_clocks_and_keeps_the_part_minute() {
        let mut s = at(0, 0);
        s.advance(MINUTE - 1);
        assert_eq!((s.minutes, u64::from(s.subminute_clocks)), (0, MINUTE - 1));
        s.advance(1);
        assert_eq!((s.minutes, s.subminute_clocks), (1, 0));
        s.advance(2 * MINUTE + 5);
        assert_eq!((s.minutes, s.subminute_clocks), (3, 5));
        s.advance(MINUTE - 5 + 1440 * MINUTE);
        assert_eq!((s.days, s.minutes, s.subminute_clocks), (1, 4, 0));
    }

    /// One minute tick, written from the rules rather than from
    /// `add_minutes`.
    fn tick(s: &mut Huc3RtcState) {
        s.minutes = (s.minutes + 1) & 0xFFF;
        if s.minutes == 1440 {
            s.minutes = 0;
            s.days = s.days.wrapping_add(1);
        }
    }

    #[test]
    fn large_gap_matches_stepwise_counting() {
        for start in [at(0xFFF0, 17), at(5, 1439), at(9, 0xF00), at(0, 1440)] {
            let gap = 1440 * 40 + 777;
            let mut big = start;
            big.subminute_clocks = 7;
            big.advance(gap * MINUTE + 3);
            let mut step = start;
            for _ in 0..gap {
                tick(&mut step);
            }
            assert_eq!((big.days, big.minutes), (step.days, step.minutes));
            assert_eq!(big.subminute_clocks, 10);
        }
    }

    #[test]
    fn advance_does_not_overflow() {
        for start in [at(0, 0), at(0xFFFF, 1439), at(1, 0xFFF)] {
            let mut s = start;
            s.subminute_clocks = MINUTE_CLOCKS - 1;
            s.advance(u64::MAX);
            assert!(s.minutes < 1440 && s.subminute_clocks < MINUTE_CLOCKS);
        }
        let mut s = at(0, 0);
        s.advance(u64::MAX);
        let n = u128::from(u64::MAX);
        let m = u128::from(MINUTE);
        assert_eq!(u128::from(s.subminute_clocks), n % m);
        assert_eq!(u128::from(s.minutes), n / m % 1440);
        assert_eq!(u128::from(s.days), n / m / 1440 % 65536);
    }

    #[test]
    fn advance_seconds_carries_whole_minutes_and_keeps_the_rest() {
        let mut s = at(2, 1439);
        s.subminute_clocks = 30 * CLOCK_HZ + 123;
        s.alarm_minutes = 0x123;
        s.alarm_days = 0x4567;
        s.alarm_enabled = true;
        // 29 s: still the same minute.
        let a = s.advance_seconds(29);
        assert_eq!((a.days, a.minutes), (2, 1439));
        assert_eq!(a.subminute_clocks, 59 * CLOCK_HZ + 123);
        // 30 s: the minute and the day roll over; the part second stays.
        let a = s.advance_seconds(30);
        assert_eq!((a.days, a.minutes, a.subminute_clocks), (3, 0, 123));
        // Days and minutes at once.
        let a = s.advance_seconds(2 * 86_400 + 61 * 60 + 45);
        assert_eq!((a.days, a.minutes), (5, 61));
        assert_eq!(a.subminute_clocks, 15 * CLOCK_HZ + 123);
        // The alarm is not a counter.
        assert_eq!(
            (a.alarm_minutes, a.alarm_days, a.alarm_enabled),
            (0x123, 0x4567, true)
        );
        assert_eq!(s.advance_seconds(0), s);
    }

    #[test]
    fn advance_seconds_matches_the_clock_path() {
        let mut s = at(0xFFFF, 1000);
        s.subminute_clocks = 17 * CLOCK_HZ + 9;
        for secs in [1u64, 59, 60, 43, 86_400 * 3 + 5, 1440 * 60 * 70_000 + 59] {
            let mut by_clocks = s;
            by_clocks.advance(secs * HZ);
            assert_eq!(s.advance_seconds(secs), by_clocks, "{secs} s");
        }
    }

    #[test]
    fn advance_seconds_takes_any_input() {
        let n = u128::from(u64::MAX);
        let a = at(0, 0).advance_seconds(u64::MAX);
        assert_eq!(u128::from(a.subminute_clocks / CLOCK_HZ), n % 60);
        assert_eq!(u128::from(a.minutes), n / 60 % 1440);
        assert_eq!(u128::from(a.days), n / 86_400 % 65536);
        // A state straight from a hostile save file: every field past
        // its width. It is masked first, then counts.
        let raw = Huc3RtcState {
            minutes: 0xFFFF,
            days: 0xFFFF,
            alarm_minutes: 0xFFFF,
            alarm_days: 0xFFFF,
            alarm_enabled: true,
            subminute_clocks: u32::MAX,
        };
        for secs in [0, 1, 59, 60, 86_400 * 70_000, u64::MAX] {
            let a = raw.advance_seconds(secs);
            assert_eq!(a, raw.masked().advance_seconds(secs));
            assert!(a.minutes <= 0xFFF && a.subminute_clocks < MINUTE_CLOCKS);
        }
        // Masked: FFF minutes, one clock short of a minute: one second
        // later the out-of-range counter wraps to 0 with no day carry.
        let a = raw.advance_seconds(1);
        assert_eq!((a.days, a.minutes), (0xFFFF, 0));
        assert_eq!(a.subminute_clocks, CLOCK_HZ - 1);
    }

    #[test]
    fn masked_cuts_to_widths_and_keeps_what_fits() {
        let m = Huc3RtcState {
            minutes: 0xF5A5,
            days: 0xFFFF,
            alarm_minutes: 0x1234,
            alarm_days: 0x8001,
            alarm_enabled: true,
            subminute_clocks: MINUTE_CLOCKS,
        }
        .masked();
        assert_eq!((m.minutes, m.days), (0x5A5, 0xFFFF));
        assert_eq!((m.alarm_minutes, m.alarm_days), (0x234, 0x8001));
        assert!(m.alarm_enabled);
        assert_eq!(m.subminute_clocks, MINUTE_CLOCKS - 1);
        // Out of range for a clock, but it fits the register: kept.
        let odd = at(1, 1500);
        assert_eq!(odd.masked(), odd);
    }

    fn huc3_rom() -> &'static mut [u8] {
        let rom = vec![0u8; 0x8000].leak();
        rom[0x147] = 0xFE;
        rom[0x149] = 0x02;
        rom
    }

    #[test]
    fn state_api_counts_emulated_time_and_round_trips() {
        let emu = crate::testkit::fresh();
        assert_eq!(emu.huc3_rtc_state(), None);
        emu.load(huc3_rom(), vec![0u8; 0x2000].leak(), Config::default())
            .unwrap();
        let info = emu.cart_info().unwrap();
        assert!(info.huc3_rtc && !info.rtc && info.battery && info.has_save());
        // The HuC3 clock is not the MBC3 one.
        assert_eq!(emu.rtc_state(), None);
        assert_eq!(emu.huc3_rtc_state(), Some(Huc3RtcState::default()));

        // One second before 23:59 ends on day 9.
        let mut start = at(9, 1439);
        start.subminute_clocks = MINUTE_CLOCKS - CLOCK_HZ;
        start.alarm_minutes = 0x321;
        emu.set_huc3_rtc_state(start);
        assert!(!emu.save_dirty());
        assert_eq!(emu.huc3_rtc_state(), Some(start));
        let set_at = emu.clock();
        run_for(emu, HZ);
        let s = emu.huc3_rtc_state().unwrap();
        assert_eq!((s.days, s.minutes), (10, 0));
        assert_eq!(u64::from(s.subminute_clocks), emu.clock() - set_at - HZ);
        assert_eq!(s.alarm_minutes, 0x321);

        // A reset restarts the emulator clock but keeps the time so far.
        emu.reset();
        assert_eq!(emu.clock(), 0);
        assert_eq!(emu.huc3_rtc_state(), Some(s));

        // Ejecting hands the clock back, caught up to that moment.
        run_for(emu, HZ / 2);
        let now = emu.huc3_rtc_state().unwrap();
        let ej = emu.eject().unwrap();
        assert_eq!(ej.huc3_rtc, Some(now));
        assert_eq!(ej.rtc, None);
        assert!(!ej.save_dirty);

        // The next session: load, then restore with the wall-clock time
        // that passed (2 days, 1 hour, 59 seconds), as for MBC3.
        let saved = ej.huc3_rtc.unwrap();
        let (rom, ram) = ej.into_buffers();
        emu.load(rom, ram, Config::default()).unwrap();
        assert_eq!(emu.huc3_rtc_state(), Some(Huc3RtcState::default()));
        emu.set_huc3_rtc_state(saved.advance_seconds(2 * 86_400 + 3600 + 59));
        let s = emu.huc3_rtc_state().unwrap();
        assert_eq!((s.days, s.minutes), (12, 60));
        assert_eq!(s.subminute_clocks, saved.subminute_clocks + 59 * CLOCK_HZ);
        assert_eq!(s.alarm_minutes, 0x321);
    }

    #[test]
    fn set_state_masks_a_hostile_save() {
        let emu = crate::testkit::fresh();
        emu.load(huc3_rom(), vec![0u8; 0x2000].leak(), Config::default())
            .unwrap();
        let raw = Huc3RtcState {
            minutes: 0xFFFF,
            days: 0xFFFF,
            alarm_minutes: 0xFFFF,
            alarm_days: 0xFFFF,
            alarm_enabled: true,
            subminute_clocks: u32::MAX,
        };
        emu.set_huc3_rtc_state(raw);
        assert_eq!(emu.huc3_rtc_state(), Some(raw.masked()));
        // It keeps running from there: FFF wraps to 0 within a second.
        run_for(emu, HZ);
        let s = emu.huc3_rtc_state().unwrap();
        assert_eq!((s.days, s.minutes), (0xFFFF, 0));
    }

    #[test]
    fn other_mappers_have_no_huc3_clock() {
        let emu = crate::testkit::fresh();
        let rom = vec![0u8; 0x8000].leak();
        rom[0x147] = 0x10; // MBC3+TIMER+RAM+BATTERY
        emu.load(rom, &mut [], Config::default()).unwrap();
        assert!(!emu.cart_info().unwrap().huc3_rtc);
        assert_eq!(emu.huc3_rtc_state(), None);
        // Setting a HuC3 state is ignored, and leaves the MBC3 clock (and
        // the point it was last counted up to) alone.
        run_for(emu, HZ + HZ / 3);
        let rtc = emu.rtc_state();
        emu.set_huc3_rtc_state(at(5, 5));
        assert_eq!(emu.huc3_rtc_state(), None);
        assert_eq!(emu.rtc_state(), rtc);
        assert_eq!(rtc.unwrap().live.seconds, 1);
        assert_eq!(emu.eject().unwrap().huc3_rtc, None);
    }
}
