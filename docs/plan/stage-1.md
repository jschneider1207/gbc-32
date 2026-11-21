# Stage 1: portable core + desktop front end

**Stage goal:** `gbc-core` passes every `must` ROM in `acceptance/stage1.toml`
and plays real games correctly on the desktop.

**Stage gate:** `./scripts/gate-stage1.sh` exits 0, **and** the human signs
off on the real-game checklist at the bottom of this file.

Each milestone closes when its acceptance command passes **and** the
milestone review (AGENTS.md, "Independent review") has no confirmed
findings left open. Then update the baseline, commit, and tag `stage1-m<N>`. Tasks are a starting point: split,
reorder, or add tasks as needed (record notable changes in the session log).
Milestone acceptance criteria are fixed unless the human changes them.

---

## M0: Toolchain, workspace, runner

Set up the infrastructure that every later milestone is measured with.

- [x] `rust-toolchain.toml` pinning the installed stable version (see STATUS "Environment"), with rustfmt, clippy and the `thumbv7em-none-eabihf` target
- [x] Cargo workspace per `docs/architecture.md`: `gbc-core`, `gbc-desktop` (stub), `tools/romtest`
- [x] `gbc-core` skeleton: public API shape from architecture.md, compiling for host and `thumbv7em-none-eabihf`
- [x] `romtest` implementing everything in `acceptance/README.md` except the `gambatte` check, which reports `unsupported` until M5: manifest schema validation, exit codes 0/1/2, all CLI flags, JSON/markdown output, failure PNGs, baseline and regression logic, timing report
- [x] Decide how to test `romtest` before a real CPU exists (e.g. a fake emulator behind a trait, or tiny hand-assembled ROMs) and record it in an ADR
- [x] ADR for the core public API (buffer ownership and lifetimes, building the `Emulator` without putting it on the stack, event set, debug access)
- [x] `romtest` unit tests for manifest parsing, scoring and regression detection
- [x] Initial baseline committed (everything failing is expected)

**Acceptance:** `scripts/gate-stage1.sh` gets through the toolchain, fmt,
clippy, test and embedded-build steps, and fails at the ROM step with
**exit code 1** (ROM failures), not 2 (runner error).
`target/acceptance/stage1.md` lists every ROM in the manifest.

## M1: CPU + memory bus

- [x] SM83 instruction set (all opcodes including CB), flags, HALT, STOP stub, EI delay
- [x] Memory map, echo RAM, unusable region, I/O register stubs
- [x] MBC1 (enough for Blargg), serial port output capture
- [x] Post-boot register state for CGB (no boot ROM), including the different DMG-compatibility-mode values (Pan Docs "Power Up Sequence": e.g. D=$00, E=$08, L=$7C, B from the title checksum)
- [x] Unusable area FEA0-FEFF reads as CGB revision C does (Pan Docs "FEA0-FEFF range": revisions 0-D differ from E; ADR 0003)
- [x] Instruction-level unit tests (e.g. against the SM83 JSON single-step tests, if adopted)
- [x] Before fixing the bus signature: a per-run-call context struct (memory, hardware state, frame, audio + write position) so lazy PPU/APU catch-up on any bus write costs one pointer, not threaded arguments (ADR 0006 review, finding 6)
- [x] ROM size: record size/mask at `load` so opcode fetch avoids a per-access bounds check; define out-of-range reads for non-power-of-two ROMs (review finding 11)

**Acceptance:** `romtest --group blargg-cpu-instrs --group blargg-instr-timing --only-check serial --gate must`
exits 0. (Their screenshot checks need the PPU and are covered in M3.)

## M2: Timing, interrupts, timers, OAM DMA

- [ ] M-cycle-accurate memory access timing (ADR 0002)
- [ ] Interrupt dispatch timing, HALT bug, IE/IF behavior
- [ ] Decide first: lazy, event-scheduled timer (DIV from the clock; TIMA overflow, serial bit clock and DIV-APU as scheduled events) vs. the eager per-M-cycle tick, which costs ~25% of host time (M1 perf review). Record in an ADR before building M2 timing on it
- [ ] HALT: IME=1 with an interrupt arriving during HALT's own fetch (return address; SameBoy `halt()` re-executes HALT), `EI; HALT`, and CGB wake-up latency +1 M-cycle (M1 hardware review 2, 5)
- [ ] DIV/TIMA with the falling-edge behavior, TMA reload windows
- [ ] OAM DMA timing and bus conflicts (DMA conflicts put state into the CPU read path, so `peek` needs its own side-effect-free path; consider driving DMA through `next_event`)
- [ ] Inputs at M-cycle resolution: the contract applies inputs at a time "rounded down to the nearest M-cycle", but `run_until` stops only on instruction boundaries. Add a scheduled button change (e.g. `set_buttons_at(clock, buttons)`) applied in the M-cycle tick and switch romtest to it (review finding 5)

- [ ] Quality-bar tooling in the gate (README "Quality and performance bar": the benchmark runs in the gate from M2 on):
  - [x] Deterministic instruction-count benchmark under valgrind (e.g. iai-callgrind) on fixed ROMs, committed baseline; the gate fails on a regression of more than a few percent
  - [x] Code-size budgets on thumbv7em (`-C target-cpu=cortex-m7`) for `Cpu::run::<Ctx>` and `execute_cb`, checked by the gate
  - [ ] Short, deterministic fuzz pass in the gate over untrusted inputs: ROM header/`load`, save RAM, `set_rtc_state`
- [ ] `hard_rules` test hardening (M1 API review, low): lex `'\''` correctly (start at `q + 3`, add the case to the allow-test); require `#![forbid(unsafe_code)]` among lib.rs's leading crate attributes, not in a nested module; reject a `[lib] path`, a `build` key and a `build.rs` in gbc-core

**Acceptance:** `romtest --filter 'mooneye-test-suite/acceptance/{timer,instr,interrupts,oam_dma}/**' --gate must`
exits 0. Some top-level Mooneye acceptance tests depend on LY/VBlank timing or
OAM access windows, so the full `mooneye-acceptance` group is required in M3.

## M3: PPU (DMG-compatibility rendering) + screenshots

- [ ] Mode timing state machine (OAM scan, mode 3 length incl. SCX/window/sprite penalties, HBlank, VBlank), STAT/LYC interrupts
- [ ] Scanline renderer with catch-up on mid-line register writes (ADR 0002)
- [ ] Event deadlines: a small fixed array (timer, PPU mode/STAT, APU buffer) with `next_event = min`, recomputed only when a deadline changes; STAT and LYC writes must sync the PPU and reschedule (M1 reviews)
- [ ] BG, window, sprites (priority, 10-per-line limit), DMG compatibility palettes
- [ ] Framebuffer output; screenshot comparison working in `romtest`
- [ ] PPU and DIV phase at power-on (where the CGB boot ROM hands over), replacing the M0 stub that starts at LY=0 dot 0; checked by `mooneye-cgb-misc` boot_div/boot_hwio (M0 hardware review)
- [ ] First line after LCD enable skips OAM scan (GBCTR, SameBoy); no manifest ROM covers it (the lcdon_* tests are excluded), so add a unit test
- [ ] Before writing the PPU: decide the output pixel format and stride (STM32 LTDC wants RGB565 or ARGB1555; converting per palette entry at palette-write time is free on the hot path). Record it in an ADR (review finding 4)
- [ ] First headless performance benchmark recorded in `docs/perf.md`

**Acceptance:** groups `mooneye-acceptance`, `dmg-acid2-compat`, `blargg-cpu-instrs`,
`blargg-instr-timing`, `blargg-mem-timing`, `blargg-mem-timing-2` and
`blargg-halt-bug` all passing.

## M4: Cartridges + saves

- [ ] MBC1 (full), MBC2, MBC3 + RTC (on emulated time), MBC5 (incl. rumble bit ignored)
- [ ] Cartridge header parsing, ROM/RAM size handling. Review gaps from M0: types without RAM (Pan Docs 0147) vs. a nonzero 0x149 code, MBC7 EEPROM size (SameBoy 0x100), HuC3 RTC; decide which are in scope
- [ ] Save RAM and RTC state exposed through the core API (the caller owns persistence)
- [ ] `load` over an MBC3 cartridge drops its `RtcState` silently (doc now says to save it first); consider returning it with the buffers (M1 API review, low)
- [ ] `RtcState::advance_seconds(u64)` in the core with hardware rollover (out-of-range wrap, day carry, halt bit), so front ends can sync to wall-clock time correctly; document load-then-set_rtc_state order (review finding 8)

**Acceptance:** groups `mooneye-mbc`, `rtc3test-basic`, `rtc3test-range`,
`rtc3test-sub-second` and `mbc3-tester` all passing.

## M5: CGB features

- [ ] Double speed (KEY1/STOP), VRAM/WRAM banking
- [ ] STOP accuracy: DIV/TIMA/serial frozen in STOP, wake-up only on a selected P1 line, DIV reset rules, 2-byte STOP (Pan Docs "Using the STOP instruction"; M1 hardware review 6)
- [ ] Screen blank while in STOP mode (SameBoy `GB_display_vblank` blanks while stopped); extend `screen_blank` (M0 hardware review)
- [ ] CGB palettes (BCPS/BCPD/OCPS/OCPD, access blocking), BG attributes, CGB sprite priority
- [ ] GDMA/HDMA with correct timing, including during HALT and LCD off
- [ ] CGB vs. DMG-compatibility mode selection from the header
- [ ] `romtest`: implement the `gambatte` check (hex-pattern screen match, audio silence, PNG), so the stretch-tier Gambatte suite reports real results

**Acceptance:** groups `cgb-acid2` and `mooneye-cgb-boot-regs` passing;
`same-suite-cgb` and `age-cgb` tracked (should tier).

## M6: APU

- [ ] Four channels, frame sequencer, length/sweep/envelope, wave RAM quirks
- [ ] Batched sample generation (ADR 0002) into a caller-provided buffer
- [ ] Resampling/mixing strategy documented in architecture.md

**Acceptance:** group `blargg-cgb-sound` passing.

## M7: Desktop front end

- [ ] Window with integer-scaled 160×144 output, keyboard and gamepad input
- [ ] Audio output with frame pacing driven by audio
- [ ] Load ROM from a path; battery saves and RTC persisted next to the ROM (`.sav`)
- [ ] Builds and runs on Windows and on WSL2 (WSLg)
- [ ] Before starting: likely needs system libraries (ALSA, udev, Wayland, xkbcommon dev packages). Check STATUS "Needs human" and don't install them yourself

**Acceptance:** `cargo build -p gbc-desktop` succeeds; the front end runs a
test ROM headlessly for N frames via `--headless --frames N` and exits 0.
The human checks interactive play.

## M8: Game compatibility + performance baseline

- [ ] Fix issues found in the real-game checklist
- [ ] Bring `should`-tier scores up where cheap
- [ ] Code size: re-measure `execute`/`execute_cb` on thumbv7em (68 KB at M1, more than the H753's fixed 64 KB ITCM; the H735 ITCM can be configured up to 256 KB). Target <= 40 KB, e.g. by moving rare read regions out of line (M1 perf review 1)
- [ ] Unaligned ROM reads: the Stage 3 MCU (STM32H735) does not support unaligned reads from the FMC SDRAM that holds the ROM (ES0491 2.7.3). `-C target-feature=+strict-align` is unstable on Rust 1.100 (warns, slated to become a hard error, rust-lang/rust#162235), so ensure at code level that ROM bytes are never merged into wider loads (e.g. one ROM-read function using byte reads LLVM can't combine) and add a disassembly check; record in an ADR
- [ ] ROM placement hook for the MCU front ends (e.g. caller-supplied per-bank slices so bank 0 and the current bank can sit in internal SRAM) only if Stage 2 measurements show ROM fetch misses matter (M1 perf review 5)
- [ ] Benchmark: headless emulated frames/second on desktop for a fixed set of ROMs; record host cycles per emulated frame as the baseline for Stage 2

**Acceptance:** `./scripts/gate-stage1.sh` exits 0.

---

## Real-game checklist (human sign-off)

The human supplies these ROMs in `roms/` and plays each one for about
10 minutes.

| Game | Exercises | Result |
|---|---|---|
| Pokémon Crystal | MBC3 + RTC, saves | |
| Link's Awakening DX | General compatibility | |
| Shantae | Heavy HDMA, CGB graphics | |
| Alone in the Dark: The New Nightmare | HDMA, CGB graphics | |
| Wario Land 3 | General CGB | |
