# MBC3 real-time clock

Implemented in `crates/gbc-core/src/cart/rtc.rs` (counting) and
`crates/gbc-core/src/cart/mod.rs` (`read_rtc`, `write_rtc`, latch). Types
0F and 10 have the clock.

## Registers (A000-BFFF after selecting 08-0C at 4000-5FFF)
| Sel | Reg | Bits kept | Rolls over (carries) at |
|---|---|---|---|
| 08 | S | 0-5 | 60 |
| 09 | M | 0-5 | 60 |
| 0A | H | 0-4 | 24 |
| 0B | DL | 0-7 | day bits 0-7 |
| 0C | DH | 0 (day bit 8), 6 (halt), 7 (day carry) | - |

- Unused bits are not stored and read 0 (rtc3test "Valid bits"; SameBoy
  masks to 3F/3F/1F/C1). Reading or writing needs the 0A enable at
  0000-1FFF, like RAM. Selects above 0C read FF.
- **Reads** return the latched copy. **Latch:** writing 00 then 01 to
  6000-7FFF copies the live counters (Pan Docs: MBC3). Implemented as
  SameBoy does (`Core/mbc.c`): latch when bit 0 goes 0 -> 1.
- **Writes** go to the live counters immediately, running or halted
  (rtc3test "Register writes" reads the value back after latching).

## Counting
- One tick per second of the 32,768 Hz crystal, unaffected by CGB double
  speed: `CLOCK_HZ` (4,194,304) base clocks per second.
- A tick increments S; S reaching 60 resets to 0 and increments M, and so
  on (60, 24), then the 9-bit day counter. Days 511 -> 0 sets DH bit 7,
  which stays set until written to 0 (Pan Docs: MBC3 "Day Counter";
  rtc3test "Overflow stickiness").
- **Out-of-range values** (only reachable by writes, e.g. S = 60-63,
  M = 60-63, H = 24-31) count up to the top of the register width and
  wrap to 0 **without** carrying (rtc3test "Invalid value tick", "Invalid
  rollovers", "High minutes", "High hours").
- **DH bit 6 (halt)** stops counting, including the sub-second counter:
  after restarting, the next tick comes after the rest of the interrupted
  second (rtc3test "RTC off/400").

## Sub-second counter on writes
rtc3test "Sub-second writes": writing **S** restarts the sub-second
counter (next tick a full second later: RTCS/500, RTCS/900 expect
1000 ms). Writing M, H, DL or DH leaves it alone (RTCM/50 expects 50 ms,
etc.). SameBoy `Core/memory.c` `write_mbc_ram` clears `rtc_cycles` only
for register 0 (seconds).

## Emulation model (lazy catch-up)
The cart stores the RTC as of an emulator clock (`rtc_synced`) and
advances it by the elapsed base clocks only when it can matter: before a
register write, at a latch, in `Emulator::rtc_state` (computed, not
stored), `set_rtc_state` (resyncs) and `reset` (the clock restarts at 0).
Reads need no catch-up because they see only the latched copy. The
multi-second advance is O(1) per register, not a per-second loop.
`load` does not count the previous cartridge's time.

## Sources
- Pan Docs, MBC3: https://gbdev.io/pandocs/MBC3.html
- rtc3test `tests.md` (`test-roms/rtc3test/tests.md`), which all 23
  subtests pass (results match the reference text; the screenshot check
  still differs in the CGB "PASS" green until CGB palettes land in M5).
- SameBoy `Core/mbc.c`, `Core/memory.c`, `Core/timing.c` (`GB_rtc_run`).
