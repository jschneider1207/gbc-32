# 0032: The HuC3 clock has its own state type and accessors

- Status: Accepted
- Date: 2026-11-24
- Extends: ADR 0015 (save persistence surface), ADR 0031 (HuC1 and HuC3
  now load)

## Context
The human approved HuC1 and HuC3 for Stage 1 (STATUS, "Decided in session
16"). HuC1 adds nothing to persist beyond its battery RAM. HuC3 has a
clock chip that keeps running on its own battery, so its state has to
reach the front end like the MBC3 RTC does (ADR 0015): readable, settable
after `load`, handed back on `eject`, and with an `advance_seconds` for
the wall-clock sync.

The HuC3 clock is not an MBC3 clock with other names. It counts a minute
of the day (12 bits) and a day (16 bits), has no seconds, hours, halt bit,
day carry or latched copy, and has an event (alarm) time. `RtcState` is a
plain public struct that front ends build with struct literals
(`gbc-desktop` `save.rs`, `input_log.rs`, `tools/fuzz`).

Options considered:
1. **Add the HuC3 fields to `RtcState`.** Every struct literal outside the
   core stops compiling, each field means something for one mapper only,
   and `masked`/`advance_seconds` would have to guess which half is live.
2. **Make the state an enum** (`RtcState::Mbc3 | Huc3`) behind the
   existing `rtc_state`/`set_rtc_state`. Clean to read, but it breaks
   every caller's field access, and the desktop's footer code and input
   log format are written against the MBC3 fields.
3. **A second, mapper-specific state with its own accessors**, next to
   the unchanged MBC3 one.

## Decision
Option 3. Additive only; nothing that exists changes meaning.

```text
#[non_exhaustive]
Huc3RtcState { minutes: u16, days: u16, alarm_minutes: u16, alarm_days: u16,
               alarm_enabled: bool, subminute_clocks: u32 }   // Default, Copy, Eq
  const fn masked(self) -> Self            // minutes fields to 12 bits, part minute below 60 s
  fn advance_seconds(self, u64) -> Self    // masks first; any u64; alarm untouched
Emulator::huc3_rtc_state(&self) -> Option<Huc3RtcState>   // None unless HuC3
Emulator::set_huc3_rtc_state(&mut self, Huc3RtcState)     // masks; ignored unless HuC3
EjectedCart { .., huc3_rtc: Option<Huc3RtcState> }        // #[non_exhaustive] struct
CartInfo { .., huc3_rtc: bool }                           // #[non_exhaustive] struct
CartInfo::has_save = battery && (ram_size != 0 || rtc || huc3_rtc)
```

- `rtc_state()` stays MBC3-only and is `None` on a HuC3 cartridge;
  `CartInfo::rtc` keeps meaning "`rtc_state()` is `Some`". A front end
  that knows nothing about HuC3 still saves the RAM and simply does not
  persist the clock.
- The restore sequence is the MBC3 one: `load`, then
  `set_huc3_rtc_state(saved.advance_seconds(elapsed))`.
- The state is the four 16-bit values and the flag that SameBoy and BGB
  store in their HuC3 battery saves (`GB_huc3_rtc_time_t`), plus the part
  minute. It is `#[non_exhaustive]` (unlike `RtcState`), so the tone
  settings or the rest of the chip's memory can be added later without
  breaking callers; they build it from `Default` and set fields.
- The mailbox (pending command, access address, response, the time buffer
  at locations 00-06) is not in the state and a reset clears it: games
  set it up before every use. A save state feature, if one is ever added,
  would need it.
- A hostile state is safe: `set_huc3_rtc_state` and `advance_seconds` mask
  first, and the counting is written so that no input overflows (`fuzz`
  runs both with overflow checks on).
- Dirty flag (ADR 0015): set when a clock command sets the time (extended
  command 1) or writes the event time; not by counting, by reading, or by
  `set_huc3_rtc_state`.

Cost: `Cart` grows by the chip's state (24 bytes); the clock shares the
MBC3 clock's sync point (`rtc_synced`). ROM reads and plain RAM access are
unchanged: the HuC modes sit behind the `rtc_sel != 0` dispatch the MBC3
RTC uses (`read_rtc` now takes the address, for HuC3's read-only RAM
mode; the HuC part is a `#[cold]` function, so nothing new enters the
ITCM set).

## Consequences
- Old MBC3 saves and front-end code are untouched: same types, same
  calls, same bytes on disk.
- A front end has two clock paths to persist. The desktop wraps them in
  one enum (`save::Clock`) and writes the 17-byte SameBoy/BGB HuC3 footer
  (ADR 0023 amendment).
- The desktop input recording (ADR 0030) stores the MBC3 RTC only: a
  replay of a HuC3 game starts its clock from zero. Follow-up if HuC3
  recordings matter.
- Correctness of the two mappers rests on unit tests and citations
  (`docs/reference/hudson-mappers.md` lists what is unverified on
  hardware); no test ROM covers them.
