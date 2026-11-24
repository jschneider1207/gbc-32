# Hudson mappers: HuC1 and HuC3

Implemented in `crates/gbc-core/src/cart/mod.rs` (registers, modes) and
`crates/gbc-core/src/cart/huc3.rs` (the HuC3 clock chip). Header types FF
(HuC1) and FE (HuC3), both battery-backed.

**No test ROM in the suite covers either mapper.** Everything below rests
on the two sources and on unit tests written from them. "Unverified" marks
what the sources leave open or disagree on; the code comment at each rule
says the same.

## HuC1 (type FF)
| Range | Write |
|---|---|
| 0000-1FFF | RAM/IR select: low nibble E = IR register at A000-BFFF, anything else = RAM |
| 2000-3FFF | ROM bank, 6 bits |
| 4000-5FFF | RAM bank, 3 bits (masked by the RAM size) |
| 6000-7FFF | nothing |

- There is **no RAM enable**: the RAM is always mapped unless IR mode is
  on (Pan Docs: games still write 0A and 00 to 0000-1FFF; both mean RAM).
- IR register: reads C0 (no light; C1 would be light seen), writes switch
  the transmitter LED and are ignored. No IR peer is emulated.
- Register widths are SameBoy's (`huc1`: `bank_low:6`, `bank_high:3`); Pan
  Docs says "at least 6 bits" and "at least 2 bits".
- Unverified: ROM bank 0 maps bank 0 at 4000-7FFF (SameBoy; Pan Docs is
  silent). The IR select compares the low nibble (SameBoy); Pan Docs says
  "write $0E". Power-on bank is 1 (SameBoy `GB_reset_mbc`).

## HuC3 (type FE)
ROM bank at 2000-3FFF (7 bits; bank 0 can be mapped, Pan Docs), RAM bank
at 4000-5FFF (4 bits in SameBoy, masked by the RAM size), nothing at
6000-7FFF. The low nibble written to 0000-1FFF is the **mode**: what
A000-BFFF shows.

| Mode | A000-BFFF | Read | Write |
|---|---|---|---|
| 0 | RAM, read-only (power-on) | RAM | ignored |
| A | RAM | RAM | RAM |
| B | clock command/argument | FF | command (bits 4-6), argument (bits 0-3) into the mailbox |
| C | clock command/response | 80, last command in bits 4-6, last result in bits 0-3 | ignored |
| D | clock semaphore | 81 (bit 0: ready) | bit 0 clear: run the mailbox command |
| E | IR | C0 (no light) | ignored |
| other | nothing | FF | ignored |

- D7 is not connected to the chip (Pan Docs), so bit 7 reads as open bus,
  "usually high": 1 here, and bit 7 of a command is dropped.
- Unverified: bits 1-6 of the semaphore and IR reads (SameBoy returns 01
  and 00); the value of mode B and unmapped-mode reads (Pan Docs: open
  bus, "often $FF"; SameBoy: 01); how long the chip stays busy after a
  command (never, here).

### Clock commands
The chip is a 4-bit microcontroller with 256 nibble locations; values are
little endian, one nibble per location. Games run a command by writing it
in mode B, then writing FE to the semaphore in mode D, then reading the
response in mode C.

| Cmd | Effect |
|---|---|
| 1 | response = nibble at the access address; address + 1 |
| 2 | write the argument at the address, no increment (SameBoy; "purpose unknown" in Pan Docs) |
| 3 | write the argument at the address; address + 1 |
| 4 / 5 | set the low / high nibble of the access address |
| 6 | extended command, by argument: 0 = copy the time to locations 00-06; 1 = set the time from 00-06; 2 = status, response 1 (games need it); E = tone, nothing happens |
| 0, 7 | nothing (never observed) |

| Location | Contents |
|---|---|
| 00-06 | time buffer: minute of the day (3 nibbles), day (4 nibbles) |
| 10-12, 13-16 | the running minute and day counters (read-only here) |
| 58-5A, 5B-5E, 5F | event (alarm) minutes, days, enable bit: stored, never fires |
| other | not kept, reads 0 |

- The minute of the day rolls over at 1440 into the day counter (16 bits,
  wraps to 0). A minute is 60 x `CLOCK_HZ` base clocks whatever the CGB
  speed. The clock is caught up lazily, like the MBC3 one: when a command
  runs, for the state API and at reset.
- **Model difference from SameBoy:** SameBoy runs the command on the mode
  B write and has no time buffer (its locations 00-06 are the live
  counters; extended commands 0 and 1 do nothing). Here the command runs
  on the semaphore write and 00-06 is a buffer, as Pan Docs describes. For
  the documented game sequences both give the same values.
- Unverified: the day counter width (4 nibbles as in SameBoy; Pan Docs
  lists 13-15, three); a minute value of 1440-4095 counts to the top of
  the 12 bits and wraps to 0 without a day carry (the MBC3 rule, assumed);
  setting the time restarts the minute (assumed); direct writes to 10-16
  are ignored (not documented); the mailbox is cleared by a reset
  (simplification); the event time is not moved when the time is set (Pan
  Docs says the chip keeps the remaining duration), since no event fires.
- Not emulated: the alarm, the tone generator and speaker, IR light.

## Save state
`Emulator::huc3_rtc_state` / `set_huc3_rtc_state`, `Huc3RtcState`
(`minutes`, `days`, `alarm_minutes`, `alarm_days`, `alarm_enabled`,
`subminute_clocks`), ADR 0032. Restoring masks the 12-bit fields and
clamps the part minute. The desktop `.sav` appends SameBoy's and BGB's
17-byte HuC3 footer (ADR 0023 amendment).

## Sources
- Pan Docs, HuC1: https://gbdev.io/pandocs/HuC1.html (from
  https://jrra.zone/blog/huc1.html)
- Pan Docs, HuC-3: https://gbdev.io/pandocs/HuC3.html
- SameBoy (MIT/Expat; read for behavior, no code copied): `Core/memory.c`
  (`write_mbc`, `read_mbc_ram`, `write_mbc_ram`, `huc3_write`),
  `Core/mbc.c` (`GB_cart_defs`, `GB_update_mbc_mappings`, `GB_reset_mbc`),
  `Core/gb.h` (`huc1`, `huc3`, `GB_huc3_rtc_time_t`), `Core/timing.c`
  (`GB_rtc_set_time`), `Core/gb.c` (battery save layout).
