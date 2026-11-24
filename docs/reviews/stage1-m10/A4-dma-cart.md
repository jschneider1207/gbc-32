# A4: emulation correctness: OAM DMA, HDMA/GDMA, speed switch, cartridges/MBC/RTC, memory, public API robustness

Reviewer A4, tree at HEAD of `main` (49c0756). Read-only; nothing was built or run, so every
finding is from reading the code path end to end. "Confidence" says how sure I am of the code
reading and of the reference. Paths are under `/home/jess/gbc-32/crates/gbc-core/src/` unless
stated.

Summary: 0 high, 3 medium, 8 low. The mapper, RTC and banking code is in good shape and I found
no panic reachable from caller data in a release build. The real problems are (1) what happens to
cartridges the core does not emulate, (2) one HDMA cancel path that matches neither reference,
(3) two arithmetic underflows that the gate cannot see because the fuzz pass runs without
overflow checks.

---

### A4-1 Unsupported mappers run silently as "no MBC" with always-writable, battery-flagged RAM   [severity: medium] [confidence: high]
- Where: `cart/mod.rs:396-408` (`Mapper::from_header`, `_ => Self::None`), `cart/mod.rs:171-199`
  (`cart_ram_size` trusts 0149 for every type but 00), `cart/mod.rs:271-276` (`has_battery`
  includes 0D, 22, FC-FF), `cart/mod.rs:592` (`ram_enabled = mapper == None`), `cart_info`
  (`cart/mod.rs:232`): no "supported" field.
- What: types 0B-0D (MMM01), 20 (MBC6), 22 (MBC7), FC (camera), FD (TAMA5), FE (HuC3), FF (HuC1)
  and every unknown code load successfully and run with no banking. Their RAM (sized from 0149)
  is permanently enabled and `CartInfo::has_save()` is true. The caller cannot tell the
  cartridge is unsupported.
- Scenario: the user puts a HuC1 game (type FF, 0149=03) and its existing `.sav` on the device.
  `load` succeeds. The game's first bank switch does nothing, it jumps into bank 1 expecting
  another bank and executes garbage; stray writes to A000-BFFF land in the (always enabled) save
  RAM, `save_dirty` goes true, and the front end (desktop today, MCU flash later) overwrites the
  user's good save with garbage. On the handheld the visible result is a hang or garbage screen
  with no message.
- Which mappers matter (licensed library; the handheld plays ROM images from flash, so sensors
  are a board question):
  - Covered today (MBC1/1M, MBC2, MBC3/30+RTC, MBC5+rumble, ROM, ROM+RAM): well over 95% of the
    GB/GBC library, and all five named Stage 1 games.
  - **HuC1** (FF): Pokemon Card GB (JP), Nectaris GB, Super B-Daman, a dozen Hudson titles.
    MBC1-like without mode register, 0E at 0000-1FFF selects IR instead of RAM. Effort S
    (about 25 lines + IR reads C0). Worth doing.
  - **HuC3** (FE): Robopon Sun/Star/Moon (incl. the US release), Pocket Family GB 1/2. Banking is
    trivial; the RTC/command protocol at A000 is about 150 lines in SameBoy. Effort M.
  - **MBC7** (22): Kirby Tilt 'n' Tumble, Command Master. Needs an accelerometer input in the
    public API (ADR 0006 extension) and the 93LC56 EEPROM state machine. Effort M-L; only
    worth it if the board gets an accelerometer.
  - **Camera** (FC), **TAMA5** (FD), **MMM01** (0B-0D), **MBC6** (20): one or two titles each,
    DMG-era or needing hardware the handheld lacks. Skip, but reject cleanly.
  - Not in the header at all: unlicensed mappers (Wisdom Tree, Sachen, BBD/Hitek, "type 00 but
    larger than 32 KiB" pirates). Skip.
- Reference: Pan Docs "The Cartridge Header" 0147, "MBC7", "HuC1", "HuC3"; SameBoy `Core/mbc.c`
  `GB_cart_defs`. ADR 0016 "Out of scope for Stage 1" accepted running them as no-MBC, so the
  mapper additions are a human decision; the silent failure and the save hazard are not covered
  by that ADR.
- Proposed change (two parts):
  1. Small, now: add `CartInfo::supported: bool` (or a public `MapperKind` enum incl.
     `Unsupported(u8)`); the struct is `#[non_exhaustive]` so it is not breaking. For an
     unsupported type give the cart no RAM (`ram_len = 0`) or at least `ram_enabled = false`,
     so a garbage run cannot dirty a save. Desktop/firmware show "unsupported cartridge".
     Effort S, risk none for the ROM suite (no manifest ROM uses these types; confirm with
     `diff-fingerprint.sh`). Test: unit test that type FF/FE/22 report `supported == false` and
     a write to A000 leaves the buffer and `save_dirty` untouched (mutation: revert the guard).
  2. Human decision: HuC1 now (S), HuC3 and MBC7 as Stage 2+ tasks.

### A4-2 Cancelling an HDMA while a block is pending copies `value + 1` blocks at once   [severity: medium] [confidence: medium]
- Where: `hdma.rs:144-155` (`VramDma::write_hdma5`: `blocks` is overwritten first, the cancel
  branch clears `hblank` but leaves `pending`), `bus.rs:1897-1915` (`vram_dma` loop: "one
  block only" is decided by `v.hblank`, which the cancel just cleared), `bus.rs:1787-1800`
  (`vdma_hblank` sets `pending`).
- What: the HBlank trigger is an event at an M-cycle boundary. If it fires during the first two
  M-cycles of the instruction whose last M-cycle writes FF55 with bit 7 clear, `pending` is
  already true when the cancel is processed. `write_hdma5` sets `blocks = (value & 0x7F) + 1`,
  clears `hblank`, returns. The pending step then runs as a GDMA of `blocks` blocks, because the
  loop only stops after one block when `v.hblank` is set.
- Scenario: HDMA active, LCD on. `LD A,$7F / LDH ($55),A` (a game clearing bit 7 with `res 7`
  on a shadow value, or writing a leftover length) whose write lands 1-2 M-cycles after
  `mode0_start + 2` (8 of 456 dots per visible line). Result: 128 blocks = 2,048 bytes from the
  running source counter are copied into VRAM and the CPU stalls 1,025 M-cycles; FF55 then
  reads FF. With the usual `xor a` cancel (value 00) it is one block, which is harmless.
- Reference: the two references disagree with each other and both disagree with this.
  gambatte `Memory::dma`: `length = hdmaReqFlagged ? 0x10 : dmaLength` (a flagged HBlank request
  always moves exactly one block); gambatte's FF55 write stores `data & 0x7F` first, which is
  where the core's "read-back shows the written length" comes from. SameBoy `GB_hdma_run`:
  `hdma_steps_left` is *not* changed by a cancel write, and with `hdma_on` set and
  `hdma_on_hblank` cleared its loop runs the *remaining* blocks. The core combines gambatte's
  length rule with SameBoy's loop, giving "written value + 1 blocks", which neither does. Given
  that the project already follows gambatte's cgb04c captures for FF55 (`hdma_late_disable_*`),
  one block is the consistent choice.
- Proposed change: record at request time that the step is a single HBlank block instead of
  inferring it from `hblank` at run time: e.g. `pending` becomes a 3-state (`None`, `Block`,
  `All`) set by `vdma_request`/LCD-off/wake (`Block`) and by `write_hdma5` for GDMA (`All`);
  `vram_dma` breaks after one block for `Block`. Fits the existing `u8` padding. Effort S, risk
  low. Test (unit, `hdma.rs` harness): start `0xFF` (128 blocks), step until
  `vdma.pending` is set inside an `LDH ($55),A` with A = 7F placed so the trigger lands in its
  first M-cycle; assert exactly 16 bytes changed and the stall is 9 M-cycles. Must be seen
  failing first (128 blocks today). Then the full gambatte `dma/` group for regressions.
  Unverified part: I did not run it; what would confirm is that test failing on HEAD.

### A4-3 The gate's fuzz pass cannot see arithmetic overflow or `debug_assert` violations   [severity: medium] [confidence: high]
- Where: `/home/jess/gbc-32/scripts/check-fuzz.sh:13` (`cargo build --release -p fuzz`),
  `/home/jess/gbc-32/Cargo.toml` (`[profile.release]` has no `overflow-checks`),
  `/home/jess/gbc-32/tools/fuzz/src/lib.rs` (`advance_seconds` is never called on fuzzed state).
- What: the README's robustness bar ("every untrusted input is fuzzed") is checked only in a
  build where `u64` subtraction wraps silently and `debug_assert!` is compiled out. A4-4 and
  A4-5 below are both reachable from untrusted input and both are invisible to it. The core
  has about 15 `debug_assert!`s guarding invariants on the DMA and DMA-block paths that the
  fuzzer therefore never checks either.
- Scenario: see A4-4 (a 30-byte ROM) and A4-5 (a corrupt RTC footer): both panic in a
  `cargo test`/debug build and pass the gate.
- Proposed change (stricter gate, agent-decidable): build the fuzz pass with a dedicated
  profile, e.g. `[profile.fuzz] inherits = "release"`, `overflow-checks = true`,
  `debug-assertions = true`, and add to `scenario` the documented load flow
  `emu.set_rtc_state(rtc.advance_seconds(n))` with raw `rtc` and a fuzzed `n`. Effort S. Cost:
  one extra build tree in the gate (machine-resource rules apply). Expect it to fail on HEAD
  until A4-4/A4-5 are fixed; that failure is the mutation check.

### A4-4 `dma_retime` underflows for a switch to single speed in the first 640 clocks   [severity: low] [confidence: high]
- Where: `bus.rs:1510-1530` (`at`: `p - scale(p - t)` with `scale = x << 1` when `m == 4`),
  called from `Ctx::speed_switch` (`bus.rs:1557-1572`).
- What: retiming a running transfer to single speed moves `start` back to
  `p - 2 * (p - start)`. When more than `p / 2` clocks of the transfer have run, that is
  negative. Debug/overflow-checked builds panic ("attempt to subtract with overflow"); release
  wraps `start` to about 2^64, after which `due()` and `dma_conflict` see `now < start`: the
  remaining bytes are never copied and no conflicts apply, while OAM stays blocked until `end`.
- Scenario (CGB ROM, IF = 01 after boot): `LD A,1 / LDH ($FF),A / LDH ($4D),A / STOP` (pending
  interrupt: instant switch to double speed at clock ~56), `LD A,$C0 / LDH ($46),A` (start ~72),
  a 140-M-cycle delay loop, `LD A,1 / LDH ($4D),A / STOP` at p ~ 370:
  `370 - 2 * (370 - 72)` underflows. Only reachable within 640 clocks of reset, so no real game,
  but it is a caller-data panic in any overflow-checked build, and it is the only use of
  `dma_retime` toward single speed: that direction has no test at all
  (`lib.rs:1966 speed_switch_halt_pauses_oam_dma` covers to-double only).
- Reference: none needed (arithmetic); the intended behavior is `docs/reference/oam-dma.md`
  "Speed-switch pause" (itself listed as unverified on hardware).
- Proposed change: stop encoding progress as a `start` in the past. Minimal: do the retime in
  `i64` and store a signed `start` (the clock fits 63 bits), or keep `u64` and carry the bytes
  already done separately (`first: u8`, with `start = p` rounded to the grid and
  `due = first + ((now - start) >> shift) + 1`). See the structural note at the end. Effort S,
  risk low. Test: the scenario above as a unit test through `Ctx` (to single speed, early),
  asserting all 160 bytes arrive and `end` is `p + remaining * 4`; seen failing first.

### A4-5 `RtcState::advance_seconds` on an unmasked state underflows in `count`   [severity: low] [confidence: high]
- Where: `cart/rtc.rs:20-34` (`count`: `width as u64 - v` when `v >= modulus`),
  `cart/rtc.rs:69-74` (`advance_seconds` does not mask), `cart/rtc.rs:50-54` (`days > 0x1FF`
  sets `day_carry`). Documented flow: `lib.rs:54`, `lib.rs:742`
  (`emu.set_rtc_state(saved.advance_seconds(elapsed))`); real caller:
  `/home/jess/gbc-32/crates/gbc-desktop/src/save.rs:140`.
- What: `RtcState` has public fields and the documented restore path calls `advance_seconds`
  on the value read from the save file *before* `set_rtc_state` masks it. With
  `seconds`/`minutes` >= 64 or `hours` >= 32, `width - v` underflows: panic with overflow checks;
  in release `to_wrap` is ~2^64, so the register becomes `(v + n) as u8` with no carry (garbage,
  later masked). Independently, `days` > 511 in the file sets the sticky `day_carry` even for
  `advance_seconds(0)`, which a game shows as "clock overflowed / reset your clock".
- Scenario: a `.sav` whose RTC footer has `seconds = 0xC8` (bit rot, a footer from another
  emulator's format read at the wrong offset, a partially written flash page on the MCU).
  Desktop debug build: panic at load. Release: Pokemon Crystal's clock silently loses the
  elapsed time, or reports the day counter overflow.
- Reference: the type's own contract ("Each register holds only its hardware width",
  `cart/mod.rs:279-288`).
- Proposed change: `advance_seconds` starts with `self = self.masked()` (and say so in its doc).
  Effort S (one line), no cost on any hot path. Test: `RtcTime { seconds: 200, hours: 77, days:
  600, .. }.advance_seconds(1)` equals the masked state advanced by 1, no `day_carry`; panics on
  HEAD under `cargo test` (seen failing for free). Add the call to the fuzz scenario (A4-3).

### A4-6 Re-arming an active HDMA inside the HBlank starts a second block on the same line   [severity: low] [confidence: medium, unverified]
- Where: `hdma.rs:144-155` (`self.pending = !hblank || mode0` regardless of `self.hblank`
  already being set), `bus.rs:1737-1750`.
- What: writing FF55 with bit 7 set while an HBlank transfer is already active restarts it with
  the new length and, if the write is inside HDMA's period, requests a block immediately even
  though this line's block already ran: two blocks in one HBlank (and a 9/17 M-cycle stall the
  game did not budget for).
- Scenario: a raster routine that rewrites FF55 = 0x8n from its HBlank STAT handler each line
  to "keep the transfer alive" gets 32 bytes per line instead of 16; the source/destination
  counters run ahead by one block per line.
- Reference: conflicting. SameBoy `write_high_memory` `GB_IO_HDMA5` behaves like the core (sets
  `hdma_on` when STAT mode is 0). gambatte `Memory::nontrivial_ff_write` case 0x55: when
  `lcd_.hdmaIsEnabled()` it only stores the length and returns (no new request). From memory of
  both sources; no test ROM in the manifest that I know of exercises it.
- Proposed change: none until checked; add a plan task to check gambatte's `dma/hdma_*enable*`
  captures for an already-enabled case, or write the two-outcome ROM for the human to run on
  CGB C. If gambatte is right: `pending = !hblank || (mode0 && !was_hblank)`. Effort S.

### A4-7 RP (FF56) is not implemented: reads FF, writes dropped   [severity: low] [confidence: high]
- Where: `bus.rs:1233-1273` (`write_io`: no 0x56 arm), `bus.rs:2395-2441` (`read_io`: falls to
  `read_io_other` -> FF).
- What: on a CGB in CGB mode RP reads `3E | (written & C1)` with no IR light received (bit 1 =
  1 "no signal", bits 2-5 = 1; bits 0, 6, 7 read back). The core reads FF whatever was written.
- Scenario: a game that writes RP = 00 and reads it back to probe for the port (or
  read-modify-writes it) sees LED-on and read-enable stuck at 1. Bit 1 = 1 is the right "no
  signal" answer, so Mystery Gift style code just times out as on a lone console; I know of no
  game that hangs. Cheap to make exact.
- Reference: Pan Docs "FF56 - RP"; SameBoy `read_high_memory` `GB_IO_RP`
  (`(io[RP] & 0xC1) | 0x2E`, bit 1 cleared only with input and read enabled).
- Proposed change: `0x56 if cgb => mem.io[reg] = value & 0xC1` and read
  `0x56 if cgb => 0x3E | mem.io[0x56]` (in `read_io_other`, off the hot arm). Effort S.
  Test: unit test write 00/C1 and read 3E/FF; Mooneye `misc/bits/unused_hwio-C` must stay green.

### A4-8 MBC1M detection treats two equal blank logo areas as a multicart   [severity: low] [confidence: high]
- Where: `cart/mod.rs:251-258` (`is_mbc1m`).
- What: any type 01-03 ROM of at least 0x44000 bytes whose 0104-0133 equals 40104-40133 gets
  4-bit BANK1 wiring. That includes ROMs where both are filler (00 or FF): emulator-only
  homebrew without a logo, or a logo-less header with an empty bank 0x10. ADR 0016 notes this
  for test fixtures only.
- Scenario: a 512 KiB MBC1 homebrew ROM with a zeroed logo and zero padding at the start of bank
  0x10: bank 0x11 maps to bank 0x01 and bank2 shifts by 4; the game crashes on its first far
  call.
- Reference: SameBoy `GB_configure_cart` has the same blind spot; Pan Docs "MBC1M" suggests
  checking for the *logo*.
- Proposed change: additionally require the 48 bytes to be the Nintendo logo, or simply to be
  non-uniform (not all one value). That is still cartridge-content inspection, not test-ROM
  detection. It can only turn MBC1M into MBC1 for ROMs with blank logos, so it changes no
  manifest ROM (`diff-fingerprint.sh`). Effort S. ADR 0016 amendment (the decision itself is
  unchanged).

### A4-9 MBC3 latch edge rule is still Pan Docs' against SameBoy's, unverified   [severity: low] [confidence: medium]
- Where: `cart/mod.rs:862-872`; `docs/reference/mbc3-rtc.md` "Latch".
- What: the core latches on bit 0 going 0 -> 1; SameBoy latches on every 6000-7FFF write. A
  game that writes 01 every frame without a 00 in between (after a first latch) reads a frozen
  clock here and a live one in SameBoy.
- Scenario: `ld a,1 / ld [$6000],a` once per frame with no `xor a` write: the in-game clock
  never advances. Commercial games follow the documented 00-then-01 sequence, so this is about
  homebrew.
- Reference: Pan Docs MBC3 "6000-7FFF - Latch Clock Data"; SameBoy `Core/memory.c` `write_mbc`.
- Proposed change: keep; it is already a plan task. If it stays unverifiable, the forgiving
  choice for a player device is SameBoy's (a superset: every sequence that latches today still
  latches). Needs rtc3test to stay green (it only uses 00 -> 01).

### A4-10 MBC5 rumble bit is discarded with no way out of the core   [severity: low] [confidence: high]
- Where: `cart/mod.rs:881-885` (`4 | 5 => mbc5_ram = value & 0x07` on rumble carts).
- What: correct for banking (Pan Docs MBC5: bit 3 is the motor), but the motor state is not
  stored or reported. Not a correctness bug; a missing output for a handheld that may want a
  vibration motor (Pokemon Pinball, Perfect Dark, Star Wars Ep. I Racer and about 40 more).
- Proposed change: if the board gets a motor, a one-bit `Emulator::rumble() -> bool` (or an
  `Events::RUMBLE` edge) fed from that write; `Cart` has padding for a `bool` only if a field
  is merged, so check the `Hw` size test. Human/scope decision; record as a Stage 3 note.

### A4-11 Test gaps in this subsystem (no ROM and no unit test)   [severity: low] [confidence: high]
- Speed switch to **single** speed during an OAM DMA (`dma_retime` with `m == 4`), and a switch
  with an interrupt pending during a DMA (pause + retime + immediate resume). See A4-4.
- HDMA cancel with a block pending (A4-2); FF55 bit-7 rewrite while active (A4-6).
- HDMA/GDMA destination overflow: the `v.dest == 0` end (`bus.rs:1908`) with FF53 = FF has no
  unit test (`hdma.rs` tests never reach FFF0 -> 0000), nor does the read-back afterwards
  (`0x80 | (blocks - 1)` with blocks left).
- GDMA whose source crosses a region edge mid-transfer (7FF0 -> 8000 reads FF; BFF0 -> C000;
  DFF0 -> E000) and a source in disabled cartridge RAM / a selected RTC register
  (`Cart::dma_block` RTC arm).
- ROM whose length is not a multiple of 16 inside a VRAM DMA block (`rom_tail`), and ROM
  lengths that are not a power of two with a bank number between the file end and the mask
  (`rom_window` returning `EMPTY`). `lib.rs:1267` covers the plain CPU read only.
- No-MBC with RAM (types 08/09): only used as a fixture, never asserted (always enabled,
  mirrors for 2 KiB).
- MBC3: types 11-13 with bit 3 set at 4000-5FFF (bit ignored); RTC cart with RAM bank 4-7
  (reads FF) has a test, MBC30 has two. MBC1 mode 1 with RAM banking beyond Mooneye's ROMs is
  fine (Mooneye covers it).
- `RtcState::advance_seconds` with out-of-width fields (A4-5).
- `EjectedCart::save_ram` after `load` over a cartridge (covered for `eject` only, by reading).

---

## Reachable panics from caller-provided data (enumeration)

Method: every index, slice, `unwrap`, division and `+`/`-` on caller-influenced values in
`cart/mod.rs`, `cart/rtc.rs`, `rom.rs`, `dma.rs`, `hdma.rs`, `memory.rs`, the non-test part of
`lib.rs`, and the DMA/VRAM-DMA/speed-switch parts of `bus.rs`.

Release build (no overflow checks), which is what the MCU would run: **none found.**
- ROM bytes: only through `Rom::get` (bounds-checked `Option`); header reads use `header()`
  (0 past the end) after the `MIN_ROM_SIZE` check; zero-length and sub-0x150 ROMs return
  `RomTooSmall`; `rom_window` uses `slice::get` with `start > end` yielding `EMPTY`.
- Cartridge RAM: `Cart::new` checks `ram.len() >= needed` before `ram_buf[..needed]`;
  `ram()` slices `..ram_len` (same invariant); `read_ram`/`write_ram`/`dma_block` use
  `get`/`as_chunks().get` and guard `ram_len == 0` before `ram_len - 1`.
- RTC state: `set_rtc_state` masks and clamps; `count` is total for masked values;
  `advance`/`add_seconds` split the arithmetic so `u64::MAX` seconds cannot overflow.
- OAM indices: `mem.oam[i]` in `dma_sync` (`i < due <= 160`) and `write_during_dma`
  (`i = (now - start) / m`, below 160 because the `Dma` deadline clears `Hot::dma` at `end`
  before any access at or after it; I checked the halt, speed-switch and restart-tail paths
  keep `end - start == 160 * m` and that a pause cannot begin inside the restart tail, since
  the tail's deadline fires at the boundary before `halt_begin`/`speed_switch` can run).
  `ppu_read` reduces mod 0xA0; `ReadClears::record`/`clear_bits` use `get`.
- VRAM/WRAM/extra-OAM/palette indices are all masked (`& 0x3FFF`, `& 0x7FFF`, `& 0x3FF`,
  `& 0x7FF`, `& 0x3F`; `extra_oam_index` is 0..=0x47 for FEA0-FEFF).

Overflow-checked or debug builds (tests, a future firmware profile with `overflow-checks`):
1. `bus.rs:1522` `p - scale(p - t)` (A4-4): ROM-controlled.
2. `cart/rtc.rs:24` `width as u64 - v` (A4-5): save-file-controlled, via the documented call.
3. Theoretical only: `cart/mod.rs:509,517` `next_multiple_of`/`next_power_of_two` for a ROM
   slice longer than `usize::MAX / 2` (impossible on the MCU, 2 GiB on a 32-bit host);
   `lib.rs:452` `clock() + mcycle` and `lib.rs:494` are saturating or need a clock near 2^64
   (139,000 years of emulated time).

Hangs: a pending VRAM DMA step with the CPU halted/stopped/locked makes `Emulator::run` loop
one CPU step per deadline (`lib.rs:605-616`); each iteration advances to the next deadline, so
it terminates at the target. Fine.

Determinism: no wall clock, no host randomness, no `std` in these files; the RTC counts
`CLOCK_HZ` emulated clocks per second (`cart/rtc.rs:82-90`). `Memory::clear` makes power-on RAM
all zero. (Side note for the front end, not a core defect: a display paced to 60.00 Hz instead
of 59.73 Hz runs the RTC 0.45% fast while playing, about 6.5 minutes per 24 h of play.)

---

## Bigger-picture recommendations

Ranked by value / cost.

1. **Make the fuzz gate see what the MCU must never do (A4-3).** A fuzz profile with overflow
   checks and debug assertions, plus the documented RTC restore flow in the scenario. Smallest
   change in this report, and it turns the 15 `debug_assert!` invariants on the DMA paths into
   checked properties. Strictly stricter, so it is an agent decision.

2. **Make "what cartridge is this" an explicit, caller-visible fact (A4-1, A4-8, A4-10).**
   Today the mapper is a private enum whose unknown arm quietly means "no MBC", detection
   heuristics (MBC1M, MBC30) are scattered over `is_mbc1m`, `is_mbc30` (evaluated on every bank
   write from `rom_mask`/`ram_len`) and `from_header`, and register storage is shared under
   another mapper's names (`mbc5_rom` holds MBC3's and `bank1` holds MBC2's ROM bank;
   `mbc5_ram` MBC3's raw select). Proposal: resolve everything once in `Cart::new` into
   `Mapper { None, Mbc1, Mbc1M, Mbc2, Mbc3, Mbc3Rtc, Mbc30, Mbc30Rtc, Mbc5, Mbc5Rumble,
   Unsupported }` (still one byte), rename the registers `rom_bank: u16`, `ram_bank: u8`,
   `bank2`, `mode`, and export a public mirror of the enum in `CartInfo`. That removes the
   per-write `is_mbc30()` test, gives the front end its "unsupported" answer, and gives HuC1 /
   HuC3 an obvious place to land. Behavior-preserving except for the unsupported-RAM rule;
   `diff-fingerprint.sh` proves it. Effort M.

3. **OAM DMA: store progress, not a start clock in the past (A4-4).** `OamDma` has 14 fields and
   three mechanisms that exist only because progress is derived from `start`: the restart
   "stand-in start" with `tail`/`copied` fix-ups (`oam_dma`, `dma_tail_end`, special cases in
   `ppu_at` and `oam_dma_during_vdma`), the halt shift of `start`/`end`/`block_from`
   (`dma_resume`), and `dma_retime`'s rescaling around the pause, which is where the underflow
   is. An equivalent model is `{ pos: u8 (bytes done at `at`), at: u64 (clock `pos` was
   valid), mcycle }` with `due = pos + ((now - at) >> shift)`: a pause is "bring `pos`/`at` to
   now and stop", a resume or speed change is "set `at = now` and the new `mcycle`", and the
   restart tail is one extra byte at the old `pos`. Same cost on the conflict path (one
   subtract, one shift, one add), no clock ever precedes the transfer, and `end` becomes
   derived. This is a refactor of fitted code, so: gambatte `oamdma/` + `dma/` groups and
   `diff-fingerprint.sh` identical, bench within noise on `synth:dma-storm`. Effort M, risk
   medium; do it only together with the to-single-speed tests from A4-11.

4. **VRAM DMA: name the step kind and give the speed-switch countdown its own home (A4-2).**
   `pending: bool` + `hblank: bool` + `refetch: u8` + `halted` + `wake_ok` encode about six
   real states, and `switch_end` (the speed-switch halt's end) lives in `VramDma` only because
   of padding, doubling as "end of a RAN block's stall". A4-2 is a direct result of inferring
   the step kind late. A small `enum Step { None, Block, All }` for `pending` fixes the bug;
   moving `switch_end` next to the CPU's `Mode::SpeedSwitch` state (or into `Timer`, which
   already owns the switch's other clock state) is a readability fix for dimension D.

5. **Decide the reference policy for FF55 once.** The file header says "register semantics
   follow SameBoy", the constants and the cancel read-back follow gambatte captures, and the
   two disagree in at least three places (trigger delay 2 vs 3, cancel length, re-arm while
   active). A4-2 and A4-6 both sit in the seam. A short table in `docs/reference/` (per rule:
   which source, which capture) would have caught the mix.

---

## Looked at and found fine

- **ROM handling** (`rom.rs`, `cart/mod.rs:504-538, 594-602, 938-943`): byte-wide reads only
  (ADR 0027); short, truncated, non-power-of-two and oversize ROMs (file length decides, 0148
  is ignored, as SameBoy); bank numbers mask with the padded power-of-two size; reads past the
  file end give FF; `MIN_ROM_SIZE` and zero length rejected with the buffers returned.
- **MBC1 / MBC1M**: 5-bit BANK1 with the 0 -> 1 fix-up before masking (so 0x10 on a small ROM
  maps bank 0, 0x20/0x40/0x60 quirk in mode 0), BANK2 at bit 5 (bit 4 multicart), mode 1
  applying BANK2 to 0000-3FFF and RAM, RAM enable on the low nibble, RAM mirroring for 2/8 KiB
  (Pan Docs MBC1; GBCTR MBC1; Mooneye `emulator-only/mbc1`).
- **MBC2**: address bit 8 select below 4000 only, 4-bit bank with 0 -> 1, 512 nibbles mirrored
  through A000-BFFF, upper nibble reads 1, save normalized on load (Pan Docs MBC2).
- **MBC3 / MBC30 / RTC**: 7-bit (8-bit MBC30) ROM bank with 0 -> 1; SameBoy's 4000-5FFF decode;
  RTC shares the RAM enable; reads return the latched copy; register widths and zero unused
  bits; writes hit the live counters, seconds write clears the sub-second counter; halt keeps
  the sub-second count; out-of-range values wrap without carry; day counter 511 -> 0 sets the
  sticky carry; O(1) catch-up, no overflow for any gap; the RTC ignores double speed
  (`docs/reference/mbc3-rtc.md`, rtc3test). `reset` counts the elapsed time first; `load` over
  a cart returns the old RTC caught up and starts the new one clean.
- **MBC5**: 9-bit ROM bank with bank 0 selectable, full-byte 0A enable (SameBoy), 4-bit RAM
  bank (3 with rumble), 6000-7FFF ignored.
- **No MBC / open bus**: disabled or absent RAM, unmapped RTC selects and an empty slot read
  FF; ROM ONLY ignores 0149 (ADR 0016).
- **Save surface (ADR 0015)**: `cart_ram()` is exactly the header size whatever the buffer
  length; a short buffer is `CartRamTooSmall` with nothing modified; the dirty flag compares
  the stored form (MBC2 `| F0`), is not set by latches, ticks or `set_rtc_state`, survives
  `reset`, and leaves in `EjectedCart`.
- **WRAM / VRAM banking**: SVBK 0 -> 1, 3 bits, reads `F8 | n`, FF in DMG mode; VBK reads
  `FE | n` in both modes, writable in CGB mode only; echo E000-FDFF picks the bank from
  address bit 12; FEA0-FEFF as CGB C (`& !0x18`); FF72-FF75 masks as SameBoy; KEY1 read
  `7E | armed | speed << 7`, FF in DMG mode.
- **OAM DMA**: 2 M-cycle start delay, byte `i` in M-cycle `2 + i`, 160 M-cycles at either
  speed, restart blocking rule, source >= E000 writes FF (SameBoy CGB), the bus-conflict table
  and the write/read side effects per ADR 0028, HALT pause/resume arithmetic, lazy copy synced
  before every write, PPU sync, restart, VRAM DMA and at the end of a run call
  (`Ctx::drop`). The conflict path cannot index OAM out of range (see the enumeration).
- **HDMA/GDMA**: running 16-bit counters, FF52/FF54 low nibbles ignored, E000+ source forced to
  F000 and reading FF, VRAM source reading FF (correct for CGB C: SameBoy's `hdma_open_bus`
  is FF below CGB D), destination masked to the current VBK bank per block, end on the 16-bit
  destination wrap (SameBoy and gambatte agree), stall `1 + 8n` / `1 + 16n` M-cycles, LCD-off
  start (one block) and LCD turned off outside mode 0 (one block), HALT rules, FF55 read-back
  incl. DMG mode (FF). Deferred items already in ADR 0017 (instant copy instead of 2 bytes per
  M-cycle, the other-bank write while VRAM is blocked) are cosmetic: a GDMA started mid-frame
  shows its tearing up to 9 lines early.
- **Speed switch** (`bus.rs:1554-1632`, ADR 0020): KEY1 cleared, no switch with a selected
  button held (SameBoy `stop()` / Pan Docs STOP chart), DIV reset, serial/timer/input
  deadlines re-derived for the new grid, PPU and APU deadlines reset, OAM DMA paused and
  retimed, HDMA trigger handling. I did not re-derive the fitted clock offsets (0x8000 * m,
  the 3 odd clocks, freeze 4/8); they are pinned by gambatte/AGE captures and belong to the
  CPU/timer reviewer. Plain STOP still does not pause an OAM DMA (documented stub in
  `docs/reference/oam-dma.md`).
- **`Memory`**: all-zero `const fn new`; `dma_read_clears` is empty between run calls by
  construction. One thing I could not close (unverified, PPU reviewer): `Memory::clear`
  (`memory.rs:119-137`) does not reset `obj_rank.hold` or `obj_rank.win_zero`, so after
  `reset`/`load`/`eject` in the middle of a line `Memory` is not byte-equal to `Memory::new()`
  despite the doc on `Emulator::new` ("the result is always the same state"). `hold[2].end` is
  cleared at `ppu/mod.rs:2120` and `win_zero` is rewritten by `render` (`ppu/render.rs:611`),
  so it is probably harmless, but if either is read before being rewritten on the first line
  after a reset, the same ROM and inputs could draw that line differently depending on what
  ran before: a determinism hole. Clearing both in `clear` costs nothing and removes the
  question.
