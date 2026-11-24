# M10 review, dimension D: idiomatic, maintainable Rust

Reviewer D. Tree: `main` at 49c0756. Read-only; no builds, clippy or tests
were run (machine limits), so every codegen statement below is reasoned
from the source and marked where it needs a measurement.

Scope read: all non-test code of `crates/gbc-core/src/{lib,bus,memory,
schedule,hdma,rom,events,config}.rs` and `cpu/mod.rs` in full; `ppu/mod.rs`
lines 1-1015, 1563-1776, 2676-2740 plus its full outline; `ppu/edges.rs`
header and constant list; `ppu/render.rs` 440-650 plus outline; `apu/mod.rs`
225-420 plus the outline of every APU file; `timer`, `serial`, `dma`,
`joypad`, `cart` outlines and selected parts; the test-module headers and
helpers; `tools/romtest/src/machine.rs` run loop and a panic/unwrap sweep
of romtest. `tools/bench`, `tools/sm83-tests`, `tools/fuzz` and
`gbc-desktop` were only sized and grepped (see the last section).

## Numbers

| | lines |
|---|---|
| `gbc-core/src` total | 27,604 |
| of which tests (inline `#[cfg(test)]` modules, `timer/reference.rs`, `timer/differential.rs`) | ~12,700 (46%), 433 `#[test]`s |
| non-test | ~14,900 |
| of which comment lines (`//`, `///`, `//!`) | ~5,170 |
| of which code | ~8,860 |
| `bus.rs` non-test | 2,465 (778 comment lines); 49 `#[inline(never)]`, 20 `#[inline(always)]`, 15 `#[cold]` |
| `ppu/mod.rs` non-test | 3,105 (1,125 comment lines); the word `double` on 139 lines |

Reference counts in non-test core comments: SameBoy 261, Pan Docs 134,
gambatte 111, GBCTR 6. "code size"/"budget" appears on ~60 comment lines.

Severity scale used here: high = structural, makes every later change
riskier; medium = concrete maintainability or latent-bug hazard; low = nit
pattern. No finding in this report is an emulation-behaviour bug; D3 is the
only one where state differs from what a doc claims.

---

### D1 The `Hw` size cap drives state into the wrong owners; the cap itself is inconsistent and measured on the wrong target   [severity: high] [confidence: high]

- Where:
  - cap: `crates/gbc-core/src/lib.rs:263-270` (doc, "at most 864 B (832 now)", "It is not tight"), test `lib.rs:834-839` (`size_of::<Hw>() <= 864`, host, test cfg)
  - contradicting docs: `lib.rs:11` ("under 1 KiB"), `bus.rs:42` ("the 1 KiB limit"), `cart/mod.rs:389` ("`Hw` under 1 KiB"), ADR 0006:145 ("raised from 512 bytes to 1 KiB"), ADR 0021:26 ("capped at 864 B"), `docs/architecture.md:180` ("832 B now"), `docs/STATUS.md:41` ("`Hw` at its 864 B limit")
  - state placed by the cap, each with a comment saying so:
    1. `bus.rs:35-39`, `832-834`: a delayed LYC-write STAT interrupt and the parity of its write's M-cycle live in **IF bits 5 and 6** (`LYC_IRQ_PENDING`, `LYC_IRQ_PHASE`; "`Hw` has no room for a field")
    2. `hdma.rs:93-101`: `VramDma::switch_end` — "Not VRAM DMA state": the speed-switch halt's end, truncated to `u32`, and reused with a second meaning after a `RAN` block (`bus.rs:1924`); "Here because it fits this struct's padding"
    3. `timer/mod.rs:121-133`: `Timer::apu_ds_delay` — APU frame-sequencer state in the timer ("it fits this struct's padding (`Hw` budget)")
    4. `memory.rs:65-69`: `Memory::dma_read_clears` — OAM DMA bus state in the RAM arrays ("Here, not in `Sys`, for `Hw`'s size limit"), which in turn forces `AtomicU32` interior mutability (`dma.rs:70-77`) to keep `Memory: Sync`
    5. `memory.rs:137-170`: `ObjRank::{hold, next, sel_latch, sel_end, win_zero}` — fetcher/glitch state of the PPU in `Memory` (STATUS: "`Hw` was at its size limit")
    6. `ppu/mod.rs:232-244`: `IrqLine` packed into `bits: u8` + `lyc: bool` ("three `bool`s grow `Hw` past its limit")
    7. `cart/mod.rs:377-391`: `Mapper::Mbc1M` / `Mbc3Rtc` as unit variants "to keep `Mapper` one byte ... and `Hw` under 1 KiB"
- What: A number nobody can derive (864; the ADR says 1 KiB; the doc comment says the limit "is not tight") is treated as a hard wall, and six pieces of state have been put where they fit rather than where they belong. The test that enforces it runs on x86-64 in the test configuration: `usize`/slice fields are twice the MCU size (`Sys::vram_off`, `wram_off`, `Cart`'s `Rom` and `&mut [u8]`), and `Serial` has an extra `#[cfg(test)] ff: bool` field (`serial.rs:46-47`), so the asserted number is not the MCU's `size_of::<Hw>()` at all. The real constraint (ADR 0006: `Hw` may be built on a few-KiB MCU stack) is satisfied with several hundred bytes to spare either way.
- Scenario: the IF smuggling is the sharpest edge. `Sys::iflags` is documented "low 5 bits" (`bus.rs:57`) but carries bits 5-6 for up to 8 dots. It is safe today only because every consumer masks: `cpu/mod.rs:420,472,529,582,631` and `bus.rs:531,1216,2038,2105` use `& INT_MASK`, the FF0F read ORs `0xE0`, and `write_if` preserves `!INT_MASK` (`bus.rs:1201`). `Bus::set_interrupt_flags` for `Ctx` (`bus.rs:2054`) does `value & INT_MASK`, which would silently drop a waiting interrupt; it is dead for `Ctx` only because `push_acknowledge` is overridden. IE is a full 8-bit register, so a single future `ie & iflags != 0` without the mask (the natural way to write it) turns a game that sets IE bit 5 or 6 into a spurious HALT wake-up. Nothing in the type system or a test guards this.
- Reference: ADR 0006 (the stack argument), ADR 0009.
- Proposed change (each step behaviour-preserving; fingerprint must be identical):
  1. Decide the real cap from the real constraint and assert it where it is true: a `const _: () = assert!(size_of::<Hw>() <= N)` in non-test code, so the thumbv7em gate build (`gate-stage1.sh:47`) checks the MCU layout and the host test checks the host one. Raising the cap is a budget change: **needs the human** (plan M10 "Budget changes"). Suggested: 1 KiB as ADR 0006 already says.
  2. `Sys` is `#[repr(C)]` with the hot fields first (`bus.rs:43-49`). Appending cold fields **at the end** changes no existing field offset, so the inlined bus path's Thumb-2 immediates are untouched. Add there: `lyc_irq: LycIrq` (enum `None | Waiting { phase: bool }`, 1 B), `speed_switch_end: u32`, `apu_ds_delay: bool`. Expected hot-path cost: zero Ir (all three are read only on `#[cold]`/out-of-line paths: `lyc_irq_delay`, `lyc_irq_due`, `sync_ppu`'s one test, `speed_switch*`, `write_apu`); bytes: within noise, to be confirmed by `check-code-size.sh` (the `sync_ppu` test changes from `iflags & 0x20` to a byte load of a far field: +2-4 B).
  3. Give `VramDma` its own `stall_end: u32` for the `RAN` case instead of reusing `switch_end`.
  4. `ReadClears` and the `ObjRank` glitch latches: see D3.
  - Effort M, risk low (cold paths only), test: `diff-fingerprint.sh`, plus one new unit test that sets IE = 0xFF, does a single-speed LYC write that requests the delayed interrupt, and asserts no dispatch/HALT wake-up before the due M-cycle (mutation: remove one `& INT_MASK`; with the enum the test becomes unnecessary because the hazard is gone).

### D2 `bus.rs` is a god-module and `Ctx` a god-object: nine subsystems' glue in one `impl`   [severity: high] [confidence: high]

- Where: `crates/gbc-core/src/bus.rs:1-2465` (non-test). `impl Ctx` runs from line 423 to 1985 with ~95 methods, plus `impl Bus for Ctx` (1987-2168) and ~20 free functions.
- What: the file's own header says "the memory bus", but by line count the memory map is the minority. What is actually in it:

  | seam | lines (approx.) | content |
  |---|---|---|
  | reset / post-boot | 88-310 | `Sys::new`, `Sys::reset`, `boot_logo`, `POST_BOOT_IO`, `SOUND_READ_MASK` |
  | scheduler core | 312-440, 695-808 | `Hot`, `Ctx`, `tick`, `reschedule*`, `boundary`, `run_events`, `mcycles_to_deadline` |
  | timer + serial glue | 441-535, 783-793 | `write_timer_io`, `div_reset`, `stop_div_reset`, serial deadlines |
  | APU glue | 536-693 | `apu_div_reset`, `set_apu_deadline`, `apu_sync`, `finish`, `start_audio`, `div_apu_event`, `write_apu`, `write_nr52` |
  | PPU glue | 810-1059, 1154-1179 | `sync_ppu`, the delayed LYC IRQ, `write_ppu` (138 lines), `write_palette_data` |
  | interrupt-ack timing | 1181-1230, 2016-2051 | `write_if`, `push_acknowledge`, `push_acknowledge_if` |
  | OAM DMA | 1276-1529, 2198-2258 | `oam_dma`, `dma_tail_end`, `dma_sync`, `write_during_dma`, `dma_pause/resume/retime`, `dma_conflict`, `read_during_dma` |
  | speed switch | 1531-1677 | `speed_switch` (81 lines), `speed_switch_over/end`, `idle_speed_switch` |
  | VRAM DMA | 1679-1985 | `vdma_*`, `vram_dma`, `oam_dma_during_vdma` |
  | memory map proper | 1061-1152, 1232-1274, 2170-2464 | `write_mapped`, `write_to`, `write_high`, `write_ff`, `write_io`, `read*`, `peek` |

  The test module mirrors this: 2,440 lines, two modules (`tests`, `dma_tests`), covering VRAM DMA fast paths, mid-line rendering, audio splitting, STAT IRQ timing and OAM DMA.
  The layering `Emulator { mem, hw: Hw { cpu, sys, stop_on } }` -> per-call `Ctx { hot, mem, sys, frame, audio, .. }` -> `Bus` trait is coherent and well explained (ADR 0008, `lib.rs:263-275`, `bus.rs:356-382`); the problem is not the layers but that `Ctx` is the only place where two subsystems can meet, so every cross-subsystem rule lands in it.
- Scenario: a change to the HDMA/HALT interaction has to be read against `vdma_halt`, `vdma_stop`, `vdma_wake`, `vdma_hblank` (bus.rs), `VramDma` (hdma.rs), `Bus::{wake_stalls, hdma_refetch, fetch_peek}` (cpu/mod.rs) and `vdma_turn` (lib.rs): four files, with the logic in the one whose name says "bus".
- Proposed change: turn `bus.rs` into `bus/` with additional `impl Ctx<'_, '_>` blocks per seam; Rust allows inherent impls of a crate-local type in any module of the crate, so no API or visibility change is needed beyond `pub(super)` on a handful of helpers:
  - `bus/mod.rs`: `Sys`, `Hot`, `Ctx`, `tick`, `boundary`, `run_events`, `reschedule*`, `impl Bus for Ctx`, the read/write decode (about 700 lines)
  - `bus/reset.rs` (or `boot.rs` at crate level): `Sys::reset`, `boot_logo`, `POST_BOOT_IO`
  - `bus/ppu_io.rs`: `write_ppu` (split per register, D6), `sync_ppu*`, LYC IRQ delay, `write_palette_data`, `write_if`
  - `bus/apu_io.rs`, `bus/timer_io.rs`
  - `bus/oam_dma.rs` and `bus/vram_dma.rs` (or move these impl blocks into the existing `dma.rs` / `hdma.rs`, next to the state they drive)
  - `bus/speed.rs`
  - tests move with their seam (D9).
  - Behaviour-preserving: yes (pure moves). Hot-path cost: **not automatically zero**. The release profile is the default (`Cargo.toml` `[profile.release]` sets only `debug`): `codegen-units = 16` with thin-local LTO, and rustc partitions codegen units by module. Private helpers without an `#[inline]` attribute that are inlined today because caller and callee share a module (`reschedule`, `set_timer_deadline`, `schedule_ppu`, `serial_done`, `vdma_request`, `hdma_period`, ...) would then depend on ThinLTO import thresholds. Mitigation, in this order: (a) first set `codegen-units = 1` for the release profile (or `[profile.release.package.gbc-core]`), re-measure and re-baseline once (needs the human if a budget moves; `docs/perf.md:573` already says the firmware build will be LTO + `codegen-units = 1`, so today's budgets are measured on a configuration that will not ship); (b) then do the split, after which module boundaries cannot affect codegen. Without (a): do the split one seam per commit with `check-bench.sh` and `check-code-size.sh` after each, and add `#[inline]` where a number moves.
  - Effort L (mechanical), risk low-medium (codegen drift only), test: `diff-fingerprint.sh` identical, bench and size within noise.

### D3 `Memory` has become the overflow area for PPU and DMA state; `ObjRank` is misnamed; `Memory::clear()` does not restore `Memory::new()`   [severity: medium] [confidence: high]

- Where: `crates/gbc-core/src/memory.rs:26-74` (struct), `115-132` (`clear`), `137-190` (`ObjRank`, `Held`); test `lib.rs:900-920`; `memory.rs:200-258`.
- What:
  1. `Memory` is documented as "the console's large RAM blocks" (`memory.rs:7-9`). It now also holds: resolved palette pair tables (`bg_pairs`), the object line buffer and rank buffer, the APU state machine (`apu`), the OAM DMA read-clear record, the pre-DMA OAM copy, and, inside a struct called `ObjRank`, the mid-line fetch holds (`hold: [Held; 3]`), the object fetch cursor (`next`), the TILE_SEL glitch latches (`sel_latch`, `sel_end`) and the window zero pixel (`win_zero`). None of the last five is a "rank". The stated reasons are the `Hw` cap (D1) and a code-size budget ("a second [pointer] would grow `Ppu::draw` past its code-size budget", `memory.rs:136-141`).
  2. `clear()` claims "giving the same value as `Memory::new`" (`memory.rs:112-114`). It does not reset `obj_rank.hold`, `obj_rank.win_zero` or `dma_read_clears`. Verified benign today: `hold[2].end` is zeroed at every `latch` (`ppu/mod.rs:2120`) and the first hold of a line overwrites all three (`ppu/mod.rs:1767-1768`, `x >= 0`); `win_zero` is only read under `Renderer::slow & ZERO_DUE`, which `Ppu::reset` clears; `ReadClears` is empty between run calls (`Ctx::drop` syncs). But `Emulator::new`'s promise ("the result is always the same state", `lib.rs:300-302`) and the `load`/`eject` paths rely on it, and the next field added to `ObjRank` will not be benign by luck.
  3. The test that guards this (`lib.rs:904-913`) dirties only the byte arrays, so it cannot fail for the struct fields; `memory_new_is_all_zero_bytes` destructures exhaustively for `new()` but nothing does so for `clear()`.
  4. `Memory::io`'s doc is stale: "plain read/write storage until their component exists (sound until M6, most PPU and CGB palette registers until M3/M5)" (`memory.rs:47-50`). It is now the permanent home of SCY/SCX/WY/WX/BGP/OBPx, BCPS/OCPS, OPRI, the APU register bytes, wave RAM and the FF55 read-back.
- Scenario (2): game A is ejected mid-line with a hold active; `Emulator::new`/`load` reuse the `Memory`. `*mem == Memory::new()` is false. Unobservable now; a determinism trap for any later reader of `hold[0..2]` before the first write of a line.
- Proposed change:
  - Rename `ObjRank` to what it is (`LineScratch` or `PpuScratch`), and split it into `rank: [u8; N]` plus a `FetchState { hold, next, sel_latch, sel_end, win_zero }` sub-struct with `const NEW`. Field offsets inside the `repr(C)` struct can be kept identical, so this is a rename: zero Ir, zero bytes.
  - `clear()`: reset the sub-struct wholesale (`self.obj_rank.fetch = FetchState::NEW;`) and call `dma_read_clears` reset. Cold (once per load/reset/eject).
  - Test: fill every field of a `Memory` through an exhaustive destructuring `let Memory { .. } = &mut m` (so a new field fails to compile until listed), call `clear()`, assert `== Memory::new()`. Mutation check: today's `clear()` must fail it.
  - Update the `Memory` and `io` docs to say what the struct is now: "caller-provided state whose power-on value is zero" (the architecture doc already half says this at lines 185-187).
  - Once D1 lifts the cap: move `ReadClears` into `Sys` as a plain `[Cell<u32>; 5]` or, better, make the DMA-conflict read path take `&mut` (the CPU read path already has `&mut self` in `Ctx::read`; only `peek`, which bypasses conflicts anyway, needs `&`). That removes the atomics and restores the documented "reads have no side effects" (`architecture.md:282-284`, currently false). Cost: unknown without a build (the shared `read` is `inline(always)` and generic over `CPU`); measure on `synth:dma-storm` before committing to it.
  - Effort S for rename + clear + test; M for the `ReadClears` move. Risk low.

### D4 CPU speed is represented six ways and time units are all bare integers   [severity: medium] [confidence: high]

- Where:
  - speed as `Hot::mcycle: i32` (4/2) `bus.rs:320`; `OamDma::mcycle: u8` `dma.rs:63`; `Timer::shift: u32` (0/1) `timer/mod.rs:108`; a `double: bool` parameter (139 lines in `ppu/mod.rs`, 17 `self.hot.mcycle == 2` conversions in `bus.rs`); a bit-packed `ds: u8` = `mcycle | apu_ds_delay` (`bus.rs:654`, decoded as `ds & 2` and `ds & 1` in `apu/mod.rs:309-311,343`); array index `[u32; 2]` by `usize::from(double)` (`edges::M0_STAT_LATCH`, `WY_CHECK_AT`, `ppu/mod.rs:739`)
  - log2 of the M-cycle length computed three ways: `m >> 1` (`bus.rs:731`, `hdma.rs:28`), `mcycle.trailing_zeros()` (`bus.rs:1325,2211`), `6 - mcycle` to toggle (`bus.rs:1556`)
  - four `debug_assert!(mcycle == 2 || mcycle == 4)` (`bus.rs:729,2210`, `dma.rs:202,241`) guarding the shift tricks
  - per-speed constants in three idioms inside one file: `_SS`/`_DS` pairs, `[u32; 2]`, and `Observer { ss, ds }` (`ppu/edges.rs:42-70,83-84,154-155,169-172,209`)
  - units, all `u64`/`u32`/`i32`/`u16`/`u8` with no types: base clocks (`now`, `at`), frame positions (`p`), line positions (`t`, as `u32`, `i32`, `u16` and `u8` in different functions: `LineTiming` fields are `u8`/`u16`, `hold_fetch` works in `i32`, `At::t` is `u32`), half-dots (the one newtype, `edges::H`), system-counter ticks (`Timer::sc` returns `u64`, same type as the clock it was computed from), M-cycle counts, 2 MHz ticks (`al` in `apu/mod.rs:343`), T-cycles (`freeze` in `bus.rs:1582-1593`)
- What: the project demonstrably uses a zero-cost newtype where a mix-up would be a hardware fault (`rom::Rom`, ADR 0027) and for half-dots (`edges::H`), but the two unit confusions that are easiest to make are unprotected. (a) system-counter ticks vs base clocks: they are equal in single speed and differ by 2x in double speed, so a mix-up passes every single-speed test. Sites that cross the boundary by hand: `bus.rs:144-147` (`set_div_apu_deadline`), `459-467` (`serial_deadline`), `498-499`, `514-523` (`div_reset`: `old`/`new` are counter values, `now` a clock), `677-678` (`sc(now) + 4`, "4 counter ticks ... in either speed"), `serial.rs:96,109,124` (parameters named `c`). (b) "clock" vs "line position" vs "pixel x" in the PPU, where `hold_fetch` alone converts between `u64`, `u32`, `i32`, `u16` and `u8` 15 times (`ppu/mod.rs:1601-1670`).
- Scenario: a new call `serial.next_shift(now)` instead of `next_shift(timer.sc(now))` compiles, passes Blargg/Mooneye serial tests (single speed) and breaks link-cable timing only in double speed.
- Proposed change (all `#[repr(transparent)]` / fieldless enum; zero-cost by construction, but verify with `check-code-size.sh` because `Hot::mcycle` is on the tick):
  1. `#[derive(Clone, Copy, PartialEq, Eq)] #[repr(u8)] enum Speed { Single, Double }` with `const fn mcycle(self) -> u32`, `log2_mcycle()`, `is_double()`, `toggled()`, `pick<T>(self, [T; 2]) -> T`. Keep `Hot::mcycle: i32` exactly as it is (the tick subtracts it; do not touch that load) and add `Hot::speed()` deriving from it; replace the `double: bool` parameters and the three shift idioms with it. The four `debug_assert`s disappear because the type carries the invariant. Start with `bus.rs`, `dma.rs`, `hdma.rs` (about 60 sites); the PPU's 139 lines can follow or keep `double` behind `Speed::is_double()`.
  2. `struct Counter(u64)` for system-counter values: `Timer::sc() -> Counter`, `Timer::clock_at(Counter) -> u64`, `Serial::{write_sc, div_reset, next_shift}` take `Counter`. About 25 sites; this is the highest value-per-line change in this finding.
  3. In `edges.rs`, pick one idiom: `struct PerSpeed<T>([T; 2])` with `const fn at(self, Speed) -> T`, and express `_SS`/`_DS` pairs and `Observer` through it.
  4. Replace the packed `ds: u8` and `al: u8` arguments of `Apu::write`/`write_rest`/`Noise::write` with a small `Copy` struct (`WriteCtx { speed: Speed, ds_delay: bool, tick_phase: u8 }`); it is passed in registers either way.
  - Behaviour-preserving: yes. Ir/bytes: expected zero; (4) could change argument passing by a register, measure `APU per-write` (916.4 of 950 Ir budget).
  - Effort M (1, 2), S (3, 4). Risk low. Test: fingerprint identical; no new tests needed, but (2) deserves one double-speed serial unit test that fails if a clock is passed raw (it cannot be written wrong any more after the change, so the test is for the mutation check during the refactor only).

### D5 Overflow checks and `debug_assert!`s never run against the ROM corpus or the fuzzer; release arithmetic wraps silently   [severity: medium] [confidence: high]

- Where: `Cargo.toml:21-22` (release profile: defaults, so `overflow-checks = false`, `debug-assertions = false`); `scripts/gate-stage1.sh:75` (`romtest --release`), `scripts/check-fuzz.sh:13` (`--release`), `scripts/check-bench.sh:47`. The 13 `debug_assert!`s in the core (e.g. `ppu/mod.rs:1481` `debug_assert_eq!(self.cursor, self.seek_point(double))`, `ppu/mod.rs:2999`, `cpu/mod.rs:509`, `bus.rs:2267`) and every unchecked `+`/`-`/`as` narrowing therefore execute only under `cargo test`'s 433 unit tests.
- What: the core is full of deliberately tight arithmetic whose safety argument is a comment: `now - u64::from(double)` and `now - 1` (`bus.rs:931,1024,1026`), `(i32::from(lt.end) + start + n - first_pixel) as u16` (`ppu/mod.rs:1632`), `self.end + u16::from(self.edge)` (`ppu/mod.rs:475`), `(n * m) as i32` (`bus.rs:1672,1695`), `Ppu::at`'s "at most one frame past the last sync" precondition (`ppu/mod.rs:696-710`, not asserted at all: a violation yields `line >= 154` and garbage timing rather than a panic), `full as u16` (`bus.rs:1921`), `(now + 0x8000 * m - short as u64) as u32` (`bus.rs:1626`). In the 3,461-ROM manifest and the fuzzer these wrap or truncate silently; a wrong frame is then debugged from pixels instead of from a panic at the faulting line. The strongest invariant in the PPU (the cursor equals a fresh seek) is checked in no ROM run.
- Scenario: a regression makes `LineTiming::end` underflow on one Mealybug line. `cargo test` passes (no unit test hits it); romtest reports a screenshot mismatch on one ROM; with overflow checks it would be `attempt to subtract with overflow at ppu/mod.rs:497`, caught by romtest's per-ROM panic boundary and reported as such.
- Proposed change: add a profile and use it for one extra, non-gating-at-first run:
  ```toml
  [profile.checked]
  inherits = "release"
  overflow-checks = true
  debug-assertions = true
  ```
  Run `romtest --profile checked` over the full manifest and `fuzz-pass` once per milestone close (a third `target/` profile dir: mind the machine limits, run alone). Product builds are unaffected: zero Ir, zero bytes. Expect a first run to surface intentional wraps that need `wrapping_*` (that is the point: each one becomes explicit). Adding it to the gate is a stricter check (agent decision per AGENTS.md); do that after the first clean run.
  Also add `debug_assert!(p < 2 * FRAME)` in `Ppu::at` and state in AGENTS.md "Code conventions" that intentional wrap-around must be spelled `wrapping_*`.
  - Effort S. Risk none for the product. Unverified: how many sites trip on the first run (cannot be known without the build).

### D6 PPU mid-line code: one 184-line function, bit-packed state bytes, tuple returns and index-masking in place of types   [severity: medium] [confidence: high]

- Where:
  - `ppu/mod.rs:1593-1776` `hold_fetch` (184 lines, 5 params incl. `reg: usize` tested as `== 0x40`/`0x42`/`0x43`, 15 integer conversions, returns a `bool` whose meaning is not in the signature)
  - `ppu/mod.rs:1839` `tile_lead(&self, t: i32, x: i32, tile: i32) -> (i32, bool, bool, u8)` (four anonymous results, destructured as `(before, win_hit, obj_hit, last)` at 1682 and `.0` at 1662)
  - `memory.rs:176-189` `Held::planes: u8`: bits 0-1 "planes read after the write", bits 2-3 "planes read on an LCDC.4 change's dot", bits 4-7 "slot + 1 of the last object fetched ... else 15 ... else 0" (built at `ppu/mod.rs:1727-1739` as `(p & !g) | g << 2 | o << 4`)
  - `memory.rs:157-167` `sel_end: (u8, u8)` where the second element is a key with five meanings by range (8-175, 1, 0, 0xFF)
  - `ppu/mod.rs:141-148` STAT mode sources as `u8` constants `NO_MODE = 3`, `MODE_0_2 = 4`, `MODE_HOLD = 5`, `MODE_1_STICKY = 6` indexing `IrqLine::SOURCE[mode & 7]`; `ppu/mod.rs:627-628` `EARLY_ZERO`/`EARLY_PULSE` as `u8` return codes of `early_point`; `hdma.rs:49-54` `REQUESTED`/`RAN` as `u8` states of `VramDma::refetch`; `cpu/mod.rs:239,406-414` `LATCHED: u16 = 0x100` smuggled through `Option<u16>` "pc_step"
  - bounds-check elision by modulo on values that are not ranges: `lt.stalls[usize::from(lt.win_stall) % 11]` where `win_stall` may be `NO_STALL = 0xFF` (reads `stalls[2]`, discarded by the later `!= NO_STALL` test, `ppu/mod.rs:1659-1661`); `usize::from(self.n_stalls) % 12`, `(k - 1) % 11` (`ppu/mod.rs:490-494,510`); `% MAX_PER_LINE` (`render.rs:450`); `ly % SCREEN_HEIGHT` (`render.rs:243`)
  - `bus.rs:922-1059` `write_ppu`: 138 lines, a `match reg` whose `_` arm re-matches `reg` three more times (1023-1029, 1032, 1037, 1041-1054)
  - `render.rs:458,555,625,667,759,1018,1144,1168,1277`: nine `#[allow(clippy::too_many_arguments)]`, seven of them for the same `(row, x0, x1, regs, src, obj_buf, obj_rank)` list
- What: this is the part of the core that was fitted ROM by ROM, and the fitting is encoded in integers. The comments are thorough (they have to be: `planes` needs 12 lines of prose, `sel_end` needs 7), but the compiler checks none of it, and each `% 11` is a place where a wrong index is silently mapped to a valid one instead of panicking.
- Scenario: someone adds a fourth glitch flag to `planes` bit 4, colliding with the object-slot nibble; nothing fails to compile and only `m3_lcdc_tile_sel_change` screenshots notice.
- Proposed change (behaviour-preserving, layout-preserving, so zero Ir/bytes if done as described; verify size):
  - `#[repr(transparent)] struct Planes(u8)` with `const fn` accessors (`late()`, `glitched()`, `latch_slot() -> LatchSource`) and a constructor that takes the three parts; `enum SelKey` behind a `u8` newtype likewise. Same bytes in `Memory`, same codegen.
  - `enum ModeSource { M0, M1, M2, None, M0And2, Hold, M1Sticky }` `#[repr(u8)]` (values 0-6, as today) so `SOURCE[mode as usize]` needs no `& 7` and no "7 is unused, held as before" entry; `enum Early { None, Zero, Pulse }`; `enum Refetch { None, Requested, Ran }`; in the CPU, `enum Resume { Done, Fetch { pc_step: u16 }, Latched }` instead of `Option<u16>` + sentinel.
  - `struct TileLead { before: i32, win_hit: bool, obj_hit: bool, last: Option<u8> }` (a returned struct of this size is passed like the tuple).
  - `LineTiming`: make "no window stall" an `Option<NonZeroU8>`-style accessor (`fn win_stall(&self) -> Option<&(u8, u8)>`) so `hold_fetch` and `obj_size_fetch` (`ppu/mod.rs:2688-2694`, the `i > win`/`i > prev` index arithmetic) do not compare against `0xFF` by hand. Keep the stored byte.
  - Split `hold_fetch` along its own comment paragraphs: `rediscard_scx` (1617-1641), `fetch_phase` (1642-1692, returns a small struct), `held_end_and_planes` (1693-1754), `push_hold` (1755-1775). All `#[cold]`, all in one module: no hot-path effect. Split `write_ppu` into `write_lcdc`, `write_stat`, `write_lyc`, `write_scroll_or_palette` called from a 6-arm match; it is `#[inline(never)]` and each arm already begins with its own `run_ppu_to_call`, so code size should be flat (measure: `write_ppu` has a per-function budget).
  - Renderer: bundle the per-line mutable scratch into one `&mut` (see D3: `LineScratch { obj_buf, rank, fetch }`), so the seven-argument list becomes `(row, x0, x1, regs, src, scratch)` and the nine `allow`s go. On AAPCS only four arguments travel in registers, so this removes one stack-passed argument per call on the hottest non-CPU path; expected neutral-to-better, but this one is on the worst-case render path with "no room left" (STATUS): do it only with `check-bench.sh` numbers, and drop it if it costs anything.
  - The index-masking sites: leave the hot ones (`render.rs`), and for the cold ones (`ppu/mod.rs:490-510,1659`) prefer `get()`/`Option` so an out-of-range value is handled as "none" explicitly. Cost: a compare in cold code, a few bytes.
  - Effort M-L. Risk low for the type wrappers, medium for the render signature. Test: fingerprint identical.

### D7 `docs/architecture.md` describes the M3-M6 tree, not the current one   [severity: medium] [confidence: high]

- Where / what (each checked against the source):
  - line 7-11 status block stops at M6; lines 407-415 "CGB-mode rendering (attributes, VRAM bank 1, CGB palettes, OAM-index priority) is M5" (it is implemented: `render.rs`, `Memory::{bg_pal, obj_pal, bg_pairs}`)
  - line 184-185 "the CGB palette tables will follow" (they are in `Memory` now)
  - line 180 "`Hw` ... at most 864 B, 832 B now" vs STATUS "at its 864 B limit" vs ADR 0006 "1 KiB" (D1)
  - lines 14-24 workspace tree omits `tools/bench` and `tools/fuzz` (both workspace members, `Cargo.toml:3`)
  - module sketch (lines 211-236): no `rom.rs` (ADR 0027), `ppu/edges.rs`, `cart/rtc.rs`; `bus.rs` is described as "Sys, Hot, Ctx, memory map, I/O, peek", which is a third of it (D2); "All modules listed exist" is true but the list is not all modules
  - line 270-273 "On the event table so far: the TIMA reload, the serial bit clock, the PPU and scheduled button changes" omits `Dma`, `VramDma`, `DivApu`, `AudioBuffer`, all live (`bus.rs:757-776`)
  - line 282-284 "Reads have no side effects, so the CPU and `Emulator::peek` share one `read` function" is no longer true: `dma_read_clear` records through an atomic on the CPU read path (`bus.rs:2246-2258`; the code comment at 2170-2173 says so)
  - no section for VRAM DMA (ADR 0017), the speed switch (ADR 0020), the mid-line fetch model (`hold_fetch`, `Held`, TILE_SEL latch, window zero pixel; only `docs/design/mid-line-ppu.md` has it), or what lives in `Memory` beyond RAM
  - line 414 "`frame_done` no longer writes the frame" is change history, not structure
- Proposed change: rewrite the status block as "current as of stage1-m9"; regenerate the workspace and module lists from the tree; add three short sections (VRAM DMA + speed switch glue; mid-line fetch model with pointers to the design docs; "what is in `Memory` and why"); delete tense words ("so far", "will follow", "is M5", "no longer"). Docs only. Effort S-M. Doing it after D2/D3 saves writing it twice.

### D8 Stale, roadmap-tense and change-history comments; one broken doc link; no `cargo doc` in the gate   [severity: medium] [confidence: high]

- Where (examples per pattern; the grep patterns are given so the fix agent can sweep):
  - **Stale TODO / roadmap** (`grep -nE 'TODO|until M[0-9]|in M[0-9]|\(M[0-9]\)|so far|will follow'`): `bus.rs:2308` "TODO(M3): reads 0xFF while the PPU blocks OAM" (done: `read_oam`, `bus.rs:2355-2363`, covers FEA0-FEFF); `cart/mod.rs:2` "the other mappers arrive in M4" and `405` "TODO(M4): the rest"; `cpu/mod.rs:260` "STOP (stub until M5)" and `622-625` "Until the STOP-accuracy task it is a stub"; `schedule.rs:14,24-28,35` ("`Timer`, `Serial`, `Ppu` and `Input` are scheduled so far ... `DivApu` and `AudioBuffer` in M6", "mode/STAT events in M3"); `memory.rs:47-50`; `ppu/render.rs:9-12` ("Scope: DMG compatibility mode on a CGB (M3); in CGB mode (M5) ...")
  - **History instead of behaviour** (`grep -nE 'as before|before the split|the old |no longer|now take'`, ~27 hits in non-test code): `lib.rs:260` "exactly as before the split"; `ppu/mod.rs:160` "as before the walk's point moved"; `ppu/mod.rs:235-237` "as the two-`bool` struct before the mode part was ... the old layout keeps `Ppu` code 62 B smaller"; `ppu/mod.rs:1706-1714` "is what the old calibration ... fitted ... now take the latch an earlier line left"; `edges.rs:13-14` "the same immediate comparisons the code made before the table existed"; `cpu/mod.rs:197` "so the HALT path of `step_not_running` stays as it was". A reader at HEAD cannot know what "before" was; these belong in the commit message or ADR.
  - **Emulator-as-reason**: SameBoy and gambatte are cited far more than Pan Docs/GBCTR (372 vs 140). Most citations are properly formed (function name + the test ROM and the hardware revision that pins it, e.g. `bus.rs:1181-1188`), which is the right way to cite when the only evidence is a CGB04C capture. The weak form is a reference to an emulator's internal variable as the whole justification, e.g. `bus.rs:1533-1553` (`speed_switch_halt_countdown`, `speed_switch_freeze`, `div_cycles = -4`), `cpu/mod.rs:270` ("gambatte `prefetched_`"), `hdma.rs:66-90` (field docs that are SameBoy field names). Suggested convention: every such comment states the hardware-visible rule first ("the CPU does not run for 0x8002 M-cycles; DIV keeps counting"), then the evidence (ROM + revision), then the emulator pointer.
  - **Calibration by manifest count**: `ppu/mod.rs:373-389,391-449` (`LCDC_TILES_LATE`: "0 -> 30 flips, 1 -> 45, 2 -> 39"; `DISCARD_LEAD`: "0-6 regress ... 8 -> 21 / 0"; `WINDOW_*_LEAD`, `WY_LEAD`, `OBJ_LOW_READ`). These are honest and far better than a bare number, and correctness is dimension A's; the maintainability issue is that the sweep results are frozen in a doc comment and nothing re-checks them. Suggest moving each table to `docs/reference/` with the sweep command, leaving a one-line pointer.
  - **Broken intra-doc link**: `cpu/mod.rs:187` links ``[`Bus::speed_switch_over`]``; no such trait method exists (the method is `Ctx::speed_switch_over`, `bus.rs:1638`; the trait has `speed_switch_halt`). Many other links target private items (`[`Ctx::run_events`]`, `[`Emulator::run`]`), which only resolve with `--document-private-items`.
- Proposed change: one docs-only sweep commit per pattern. Add to the gate (stricter, agent decision): `RUSTDOCFLAGS="-D warnings" cargo doc -p gbc-core --no-deps --document-private-items`. Zero product cost. Effort S-M.

### D9 Test layout: 46% of the core's lines are inline test modules; helpers are re-implemented per file; size tests measure a different struct than the one that ships   [severity: medium] [confidence: high]

- Where:
  - inline test modules: `ppu/mod.rs:3185-5544` (2,361 lines), `bus.rs:2467-4905` (2,440, two modules), `lib.rs:780-2489` (1,711), `ppu/render.rs:1502-2378`, `cart/mod.rs:947-1717`, `apu/mod.rs:456-1222` (four modules: `tests`, `noise_tests`, `wave_tests`, `sync_tests`), `hdma.rs:166-775` (611 test lines under 164 lines of code)
  - re-implemented helpers: "write one register through a `Ctx`" exists as `bus.rs:2491` `write`, `hdma.rs:208` `write`, `lib.rs:813` `write_reg` (+ `808` `write_lcdc`), `apu/mod.rs:1006` `write`; "ROM with a header" as `bus.rs:2482` `rom`, `hdma.rs:179` `setup`, `bus.rs:3965-3970` `setup`/`setup_with`, `lib.rs:791` `emulator`; "a Ctx on an emulator" as `bus.rs:3992` `with_ctx` and ad-hoc `Ctx::new(...)` literals; `frame()` at `lib.rs:826`
  - 45 `.leak()`/`Box::leak` calls: every test leaks a 49 KiB `Memory`, its ROM and RAM because `Emulator<'a>` ties all three borrows to one lifetime; the crate-doc examples do the same (`lib.rs:~150`)
  - test-only state in production types: `serial.rs:45-47,62-63,142-166` (`#[cfg(test)] ff: bool` field plus `clock`/`write_sc_eager`: the eager reference model lives inside the production `Serial`); `cpu/mod.rs:359`, `schedule.rs:151`, `dma.rs:93,113`, `objects.rs:118,124`, `square.rs:177`, `wave.rs:258`
  - size tests on the host test build only: `lib.rs:834-839` (`Hw`), `apu/mod.rs:472` (`Apu <= 128`)
  - a test that cannot fail for what it claims: `lib.rs:900-913` (D3)
- What: reading `bus.rs` means scrolling past or folding 2,440 lines; a `git blame`/diff of a test-only change touches the hottest source files; and because the helpers are private to each `mod tests`, each new test module starts by re-writing them (four `write` helpers with slightly different M-cycle semantics: `bus.rs:2491` does not tick, others do). The `Serial::ff` field means `size_of::<Serial>()`, and so `size_of::<Hw>()`, differs between `cargo test` and the product.
- Proposed change (no product-code effect at all: `#[cfg(test)]` code is not compiled into the library):
  - Move each inline module to a sibling file, `#[cfg(test)] mod tests;` -> `src/bus/tests/{vdma,midline,audio,stat_irq,oam_dma}.rs`, `src/ppu/tests/...`, etc. They stay unit tests with private access (they need `Ctx`, `Sys`, `mem`), so `tests/` integration tests are not an option for most of them; the ones that only use the public API (much of `lib.rs`'s 61 tests: load/eject/reset/run-call splitting) can move to `crates/gbc-core/tests/api.rs`, which also proves the public API is sufficient to write them.
  - One `#[cfg(test)] pub(crate) mod testkit` with `rom(..)`, `emu(..)`, `write(emu, addr, v)`, `write_ticked`, `with_ctx`, `frame()`; delete the copies. A `TestMachine` owning `Box<Memory>`, ROM and RAM and lending an `Emulator` in a closure removes the leaks.
  - Move the eager serial reference out of `Serial` into `timer/reference.rs` (which already holds the eager timer) as `struct EagerSerial { inner: Serial, ff: bool }`; `Serial` then has one layout.
  - Size asserts as non-test `const _` items (D1) so the thumbv7em build checks them.
  - `hard_rules.rs` masks tokens inside `#[cfg(test)] mod name { .. }` bodies (`tests/hard_rules.rs:200-201`); with out-of-line test modules it must instead skip files reached through a `#[cfg(test)] mod x;` declaration. That is a change to a check: keep it at least as strict (a file is exempt only if its `mod` declaration carries `#[cfg(test)]`), and mutation-check it (an `unsafe` in a test file must stay allowed, an `unsafe` in a non-test file reached from one must be caught). This touches gate-adjacent logic: review per AGENTS.md "Changes to pass/fail logic" if `romtest` is involved (it is not; `hard_rules` is a core test).
  - Effort M (mechanical). Risk low. Test: `cargo test` count stays 433 + 13.

### D10 Register addresses and LCDC/STAT bits are raw literals outside `render.rs`   [severity: low] [confidence: high]

- Where: `render.rs:70-76` defines `LCDC_BG_ON .. LCDC_WIN_MAP` (private to the renderer); `ppu/mod.rs` uses raw masks on 12 code lines against 4 named uses (`self.lcdc & 0x80` at 687, `& 0x20` at 1655, `& 0x10` at 1678/2123, `& 0x78` at 1605), `bus.rs:937-946` (`value & 0x80`, `& 0x04`, `& 0x20`). I/O indices: `mem.io[0x42]`, `[0x43]`, `[0x47..0x49]`, `[0x4A]`, `[0x4B]` (`ppu/mod.rs:1521-1524,1761-1762,1806,1986`), `io[0x55]`, `[0x68]`, `[0x6A]`, `[0x6C]`, `[0x46]` (`bus.rs:190,212,220-221,1294,1712,1165`), and `reg == 0x40 / 0x42 / 0x43 / 0x4A / 0x4B` comparisons through `write_ppu` and `hold_fetch`. `POST_BOOT_IO` (`bus.rs:284-303`) uses full `0xFFxx` addresses, the rest the low 7 bits.
- What: two conventions for the same register set, and the SCX/SCY pair is easy to swap (`0x42` is SCY, `0x43` SCX; `Held` stores them in the order scx, scy, `Regs` in the order scy, scx).
- Proposed change: a crate-private `mod reg { pub const SCY: usize = 0x42; ... }` and move the `LCDC_*` constants (plus `STAT` selects, already named in `ppu/mod.rs:134-137`) to `ppu/mod.rs` as `pub(crate)`. Constants fold: zero cost. Effort S, mechanical; do it together with the `write_ppu` split (D6).

### D11 The `Bus` trait has accreted emulator-specific hooks; CPU mode state is split across an enum, a flag and a sentinel   [severity: low] [confidence: medium]

- Where: `cpu/mod.rs:109-225`: `Bus` has 3 M-cycle methods and 17 others, of which `halt_begin`, `halt_end`, `wake_stalls`, `speed_switch`, `speed_switch_halt`, `hdma_refetch`, `fetch_peek`, `stop_begin`, `stop_wakeup`, `push_acknowledge`, `idle_halted`, `run_done`, `event` exist for `Ctx` only (the sm83 test bus takes every default). `Mode` (9 variants, `cpu/mod.rs:243-278`) plus `Cpu::prefetched: bool` plus `Cpu::latched: u8` plus `LATCHED = 0x100`.
- What: the trait is public (`pub mod sm83`), so its surface is API. It reads as a list of the places SameBoy's/gambatte's CPU loops call into DMA code, named after the callee's needs (`hdma_refetch`) rather than the CPU's events. `set_interrupt_flags` is required but unused by `Ctx` in practice (D1). The doc for `speed_switch` points at a method that is not in the trait (D8).
- Proposed change: no behaviour or codegen change intended. (a) Split into `trait Bus` (read/write/idle/interrupt_enable/interrupt_flags) and `trait System: Bus` with the hooks and their defaults, so the test bus implements only `Bus` (blanket `impl<T: Bus> System for T` is not possible alongside `Ctx`'s impl without specialization; instead give `System` defaults and one empty `impl System for FlatBus {}` in sm83-tests). Monomorphization is unchanged. (b) Rename the hooks by CPU event (`on_halt`, `on_wake`, `after_wake_cycle`, `latched_opcode_survives`), keep docs. (c) `Mode::Halted { wake_prefetched: bool }` instead of the separate flag would change `Mode`'s layout and the `mode != Running` fast test: leave unless measured. Effort S-M, risk low; lowest priority of the structural items.

### D12 Unsupported mapper types load and run as ROM-only, with no signal to the caller   [severity: low] [confidence: high]

- Where: `cart/mod.rs:395-409` (`Mapper::from_header`: `_ => Self::None`, under a stale "TODO(M4)"); `cart/mod.rs:80-90` (`LoadErrorKind` has only `RomTooSmall`, `CartRamTooSmall`; it is `#[non_exhaustive]`); `CartInfo` (`cart/mod.rs:208`) has `ram_size`, `battery`, `rtc`.
- What: a cartridge with header type MBC6, MBC7, MMM01, HuC1/HuC3, TAMA5 or the Pocket Camera loads successfully and executes bank 0/1 forever. For a handheld that will be fed arbitrary ROM files from eMMC, "this game is not supported" is a state the front end must be able to show; today the only symptom is a hang or garbage.
- Proposed change: add `CartInfo::mapper_supported: bool` (or a public `MapperKind` enum) filled by `cart_info`. That is additive and changes no emulation behaviour. Returning a new `LoadErrorKind::UnsupportedMapper` from `load` would be cleaner but is a behaviour change (some test ROMs may carry odd type bytes) and so belongs to the human / dimension A. Delete the TODO either way. Cold path: zero hot-path cost. Effort S.

### D13 Hand-tuned inlining is undocumented as a system: 80+ attributes per file, "twin" functions, and budgets that depend on module layout   [severity: low] [confidence: medium]

- Where: `bus.rs`: 49 `#[inline(never)]`, 20 `#[inline(always)]`, 15 `#[cold]`; `ppu/mod.rs`: 32 / 1 / 25. Twins: `Schedule::set` / `set_inline` (`schedule.rs:108-131`), `IrqLine::eval` / `eval_inline` (`ppu/mod.rs:275-301`), `run_ppu_to` / `run_ppu_to_call` (`bus.rs:876-896`), `Cpu::step` / `step_inline`, `point_irq` / `point_irq_rare`, `write_mapped` / `write_to` (`bus.rs:1066-1092`), `write_high` / `write_ff` (`bus.rs:1096-1152`), `push_acknowledge` / `push_acknowledge_if` (`bus.rs:1213-1230,2032-2051`, 12 duplicated lines), `set_stalls` / `set_stalls_retriggered`; `speed_switch_over` (`bus.rs:1638`) vs its hand-inlined copy in `vdma_hblank` (`bus.rs:1791`).
- What: each attribute has a local comment with a measurement, which is good practice. What is missing is the rule: a reader cannot tell which attributes are load-bearing for a budget and which are leftovers from a layout three milestones ago, and (D2) all of them were tuned under `codegen-units = 16`, where moving a function between files can change the result. The duplicated bodies (`write_high`/`write_ff`, the two `push_acknowledge`s) are real duplication of emulation logic: a fix to the IF-clear timing must be made twice.
- Proposed change: (a) one paragraph in `docs/perf.md` naming the convention (`foo` = out-of-line entry, `foo_inline` = body for hot callers; `#[cold]` only with a named rare condition) and the measurement command that justifies each class; (b) factor the duplicated *bodies* into one `#[inline(always)]` helper called by both twins (e.g. `push_ack_tail(&mut self, pending: u8)` for lines 2039-2050 / 1217-1229; `write_ff` is `write_high` minus the OAM arm, so `write_high` can call `write_ff` in its two shared arms). Since the helper is `inline(always)` into the same two bodies, codegen should be identical; verify with `check-code-size.sh` per-function numbers. (c) pin `codegen-units = 1` (D2) so the attributes mean the same thing in the firmware build. Effort S. Risk low.

### D14 Public API nits   [severity: low] [confidence: medium]

- `config.rs` `Config` is a one-field struct with a public field and no `#[non_exhaustive]`/constructor; every future option (e.g. an RTC sync flag, a model revision) is a breaking change for `gbc-desktop`, `romtest`, `bench`, `fuzz` and the firmware. `Config::default()` exists (used in tests), so `#[non_exhaustive]` + `Config::new()` is free now.
- `Emulator::peek` is defined in `bus.rs:2445-2464`, away from the other debug accessors in `lib.rs:582-620`; move the wrapper to `lib.rs` and keep `bus::peek(mem, sys, addr)` private.
- `Emulator<'a>` uses one lifetime for `Memory`, ROM and cartridge RAM (`lib.rs:254-262`). Correct for `'static` MCU buffers; on the host it forces the three owners to outlive the emulator together and is why tests and doc examples leak (D9). Not worth changing the type; worth one sentence in the crate docs and a non-leaking example (owners declared before the emulator in the same scope).
- `run_until(u64::MAX, ..)`: `u64::MAX` is also `Ctx::target`'s "no target" sentinel (`bus.rs:376-378`) and `schedule::NEVER`; `set_buttons_at` documents and rejects it (`lib.rs:455-457`), `run_until` does not mention it. With the LCD off and an empty audio buffer such a call returns only on `CPU_LOCKED`. Document it ("`target` must be below `u64::MAX`") or clamp to `u64::MAX - 1`.
- `Emulator` has no `Debug` impl (`EjectedCart` and `LoadError` do), so `Result<_, _>::unwrap()` on types containing it and `dbg!` do not work; a hand-written `Debug` printing clock, registers and cart info is 10 cold lines.
- `Registers` exposes eight `u8` fields and no public `af()/bc()/de()/hl()` (the pair accessors in `cpu/exec.rs:75-90` are private to `Cpu`); callers that want a 16-bit pair rebuild it.
- `#[must_use]` coverage is good (`RunResult`, `load`, `eject`, `set_buttons_at`). Visibility: `pub(crate)` on ~all fields of `Sys`, `Ctx`, `OamDma`, `VramDma`, `Timer` is a symptom of D2 (the glue in `bus.rs` needs every field); it will shrink to `pub(super)`/private as the glue moves next to its state.
- All behaviour-preserving, zero hot-path cost. Effort S each.

### D15 Lints: the workspace enables two; a handful of pedantic/restriction lints would have caught findings above   [severity: low] [confidence: medium (not run)]

- Where: `Cargo.toml:13-17` (`unsafe_op_in_unsafe_fn`, `clippy::undocumented_unsafe_blocks` only); gate runs default clippy with `-D warnings`.
- What I expect to fire and matter (reasoned from the source, **unverified**: clippy was not run):
  - `clippy::cast_possible_truncation` / `cast_sign_loss` / `cast_possible_wrap`: ~200 `as` casts in non-test core code (45 in `bus.rs`, 46 in `ppu/mod.rs`, 27 in `render.rs`, 46 in the APU). Too noisy to deny globally; valuable as a one-off audit list for D5, then `#[expect(.., reason = "...")]` on the intentional ones in `hold_fetch`, `speed_switch`, `vram_dma`.
  - `clippy::cast_lossless`: `x as u64`/`as u32` where `u64::from` exists (the code mostly uses `from` already; the stragglers are in `bus.rs` `self.hot.mcycle as u64`, which is `i32 -> u64` and therefore *not* lossless: exactly what D4's `Speed::mcycle()` removes).
  - `clippy::indexing_slicing` (restriction): as an audit, lists every panic site on the hot path for dimension B/C; not for the gate.
  - `clippy::arithmetic_side_effects` (restriction): same role for D5.
  - `clippy::too_many_arguments` is already firing and suppressed nine times (D6); `clippy::too_many_lines` would flag `op` (212, inherent to a decoder), `hold_fetch` (184), `write_ppu` (138), `step_not_running` (89), `speed_switch` (81).
  - `clippy::struct_excessive_bools`: `VramDma` (4 bools + a tri-state `u8`), `Ppu` (6), `Renderer` (4): the HDMA one is a state machine written as flags (`hblank`, `pending`, `halted`, `wake_ok`, `refetch`), worth an enum once dimension A has confirmed which combinations are reachable.
  - `clippy::missing_const_for_fn`, `must_use_candidate`, `doc_markdown`, `semicolon_if_nothing_returned`, `items_after_statements` (e.g. `const PPU` inside `push_acknowledge`): style only.
  - `rustdoc::broken_intra_doc_links` with `--document-private-items` (D8).
  - `unreachable_pub` (rustc): `pub mod sm83` re-exports `Cpu` whose `pub fn`s are intended; elsewhere `pub(crate)` is used consistently.
- Proposed change: add to `[workspace.lints.clippy]`: `struct_excessive_bools = "warn"`, `too_many_lines = "warn"` (with `clippy.toml` `too-many-lines-threshold = 150`), `items_after_statements`, `semicolon_if_nothing_returned`, `doc_markdown`; run the three audit lints once by hand (`cargo clippy -p gbc-core -- -W clippy::cast_possible_truncation -W clippy::indexing_slicing -W clippy::arithmetic_side_effects`) and attach the output to the M10 record. Stricter gate = agent decision. Zero product cost.

### D16 Style drift between modules written in different sessions   [severity: low] [confidence: high]

- Patterns (2-3 examples each):
  - **"none" sentinels**: `NEVER = u64::MAX` (`schedule.rs`), `NO_STALL = 0xFF` (`ppu/mod.rs:355`), `0 = none` with value + 1 stored (`Renderer::win_wx`, `ObjRank::win_zero`, `LineTiming::prev_wx`, `wy_late`), `dma::LEN` as "no tail" (`dma.rs:60`), `Option<u8>` (`window_wx()`). `Option<NonZeroU8>` has the same size as the "+1" encoding and reads as what it is.
  - **register write entry points**: `write(reg: usize, ..)` by low-7-bit index (`write_io`, `write_apu`, `write_ppu`, `Timer::read`), by channel offset (`Square::write(off, ..)`), by `reg - 0x50` (`VramDma::write_addr`), by full address (`write_high`, `write_ff`, `POST_BOOT_IO`).
  - **constructor names**: `new()` (most), `empty()` (`Cart`), `powered_off()` (`Cpu`), `zero()` (`Registers`, `RtcState`), `NONE`/`EMPTY`/`LOW`/`STALE` associated consts (`LineTiming::NONE`, `LineObjects::EMPTY`, `IrqLine::LOW`, `ObjectLines::STALE`, `Rom::EMPTY`).
  - **interrupt bit constants** live in five modules (`ppu::INT_VBLANK/INT_STAT`, `timer::INT_TIMER`, `serial::INT_SERIAL`, `joypad::INT_JOYPAD`, `cpu::INT_MASK`) and `push_acknowledge` re-derives `PPU = INT_VBLANK | INT_STAT` twice (`bus.rs:1219,2033`); one `mod irq` would do.
  - **time parameter names**: `now`, `at`, `clock`, `c`, `t`, `t0`, `start`, `line_start`, `until`, `upto` for base clocks; `t` is also the line position in the PPU and `c` the system counter in `serial.rs`.
- Proposed change: pick one per pattern in AGENTS.md "Code conventions" and apply opportunistically inside the D2/D4/D6 refactors rather than as a separate sweep. Zero cost.

---

## Bigger-picture recommendations

Ranked by value / cost. All are behaviour-preserving; the fingerprint must be identical after each.

1. **Fix the measuring stick before refactoring (D2a, D5, D1 step 1).** Three settings make every later step cheaper to verify: `codegen-units = 1` for release (module layout stops affecting Ir/bytes, and the numbers match the planned firmware build), a `checked` profile for romtest/fuzz (overflow and `debug_assert` coverage over 3.4k ROMs), and the `Hw`/`Apu` size asserts as `const _` in non-test code (checked on thumbv7em). The first needs one re-baseline and therefore the human; the other two are stricter checks. Cost: one afternoon. Without the first, D2 is a measure-every-commit job.
2. **Raise or re-derive the `Hw` cap and move the six misplaced pieces of state home (D1, D3).** This removes the IF-bit smuggling, the dual-use `switch_end`, APU state in the timer, DMA state and atomics in `Memory`. Appending cold fields at the end of the `repr(C)` `Sys` leaves every hot offset alone. Needs the human for the number.
3. **Split `bus.rs` along the nine seams in D2, tests included (D2, D9).** Biggest readability gain per unit of risk once (1) is in place. Do tests first (pure `#[cfg(test)]` moves, no product effect), then one seam per commit.
4. **`Speed` and `Counter` types (D4).** Small, mechanical, and closes the one unit confusion (counter ticks vs clocks) that single-speed tests cannot see.
5. **Type the PPU's fitted state (D6).** `Planes`, `SelKey`, `ModeSource`, `Refetch`, `TileLead`; split `hold_fetch` and `write_ppu`. Cold code only, except the optional render-signature bundle, which must be measured and dropped if it costs.
6. **Docs sweep (D7, D8) and `cargo doc -D warnings` in the gate.** Do it after 2-5 so `architecture.md` is written once against the final shape.
7. **API and lint tidy-ups (D12, D14, D15, D16).** Opportunistic; `CartInfo::mapper_supported` and `Config` `#[non_exhaustive]` are worth doing before Stage 2 adds a second front end.

A note on process, since the brief asked what is accreted: the pattern behind D1, D3, D6 and D13 is the same. A budget (bytes of `Hw`, bytes of one function, Ir of one path) was at its limit when a new hardware quirk had to be fitted, and the quirk's state went wherever the budget allowed, with a comment recording why. Each instance is locally defensible and well documented; in aggregate the budgets, not the hardware, now shape the data model. The budgets that are real (worst-case Ir, total `.text`, ITCM set) should stay hard; the ones that are proxies (`Hw` 864 B, per-function byte counts under a non-shipping codegen configuration) should be re-derived from what they stand for before more state is placed by them.

## Looked at and found fine

- **Layering** `Emulator` / `Hw` / `Sys` / per-call `Ctx` / `Bus`: coherent, each layer's reason is written down (ADR 0008, `lib.rs:263-275`, `bus.rs:356-382`), and the `Hot` copy-in/copy-out with `Drop` is a clean way to keep the tick off `sys`. `Ctx::drop` doing a DMA sync is slightly surprising but documented.
- **`rom::Rom`** (`rom.rs`): a model newtype: private field, single read function, the reason in the module docs, a script that checks the build. `schedule.rs`: small, tested, `EventKind` order documented as behaviour.
- **`cpu/exec.rs`**: const-generic `op::<OP>` per opcode is unusual but well explained and keeps one source of truth for the decoder; flags helpers are small and named.
- **APU structure**: `Length`/`Envelope` shared in `units.rs`, one `Square` impl for CH1/CH2, channels share an informal interface (`digital`, `digital_at`, `advance`, `integrate`, `integrate1`, `write`, `clock_length`). A trait would add nothing. The pure-read-by-copy design (`read_apu`) is clean.
- **Public API docs** (`lib.rs`): thorough, with the frame-buffer and audio contracts stated precisely; `#[must_use]` where a dropped result loses data; `LoadError` hands buffers back; `LoadErrorKind` is `#[non_exhaustive]` and implements `core::error::Error`.
- **Error handling in the core**: one `unwrap` in non-test core code, `ppu/mod.rs:2177` (`split_first_chunk_mut::<10>()` on an `[_; 11]`, infallible and folded away); none in `bus.rs`, `cpu/*`, `cart/mod.rs`, `render.rs`. The three `u8::try_from(dt).unwrap()` in the APU (`square.rs:180`, `noise.rs:446`, `wave.rs:261`) are in `#[cfg(test)] integrate1` helpers; the six hits in `lib.rs` are doc examples. Panics on the hot path are therefore indexing and arithmetic only (dimension B/C; D5 for the arithmetic).
- **`hard_rules.rs`**: a tokenizer-based source scan with its own negative tests (`catches_unsafe_anywhere`, `catches_std_and_alloc_paths`, manifest escape hatches): each check has been made to fail.
- **`timer/reference.rs` + `differential.rs`**: an eager reference model checked against the lazy one is exactly the right test shape for the catch-up design; the same idea is used for `vdma_block` (`bus.rs:2500-2512`) and `check_long_advance` in the APU.
- **`tools/romtest`**: non-test code has no `unwrap`; the `expect`s are on infallible serialization and mutex locks; the `Machine` trait gives the runner a seam for fake machines (`app/tests.rs`, 1,281 lines); `core_call` keeps the panic boundary narrow (`machine.rs:89`). `machine.rs::run` (120 lines) is long but linear. Tests are in a separate file here (`app/tests.rs`), which is the layout D9 proposes for the core. Not reviewed: `checks.rs`, `manifest.rs`, `report.rs` logic (test-integrity is another reviewer's dimension).
- **`tools/bench`, `tools/fuzz`, `tools/sm83-tests`**: sized and grepped only (1,361 / 572 / 403 lines); nothing flagged, not read in depth.
- **`crates/gbc-desktop`** (7,437 lines, 21 files): treated as throwaway per the brief. Sampled `save.rs`/`save_writer.rs` signatures and tests (coalescing writer, exit save ordering, failure reporting all have tests); no bug found in the sample; not otherwise reviewed. Observation only: 7.4k lines is a lot of throwaway, and none of it should be carried into Stage 2 without its own review.
