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

- [x] M-cycle-accurate memory access timing (ADR 0002): Mooneye `*_timing` (call, jp, push, ret, rst, …) pass since OAM DMA (7bdaee2)
- [x] Interrupt dispatch timing, HALT bug, IE/IF behavior (`interrupts/ie_push`, HALT tasks 3ee75c7)
- [x] Decide first (ADR 0011: lazy, event-scheduled): lazy, event-scheduled timer (DIV from the clock; TIMA overflow, serial bit clock and DIV-APU as scheduled events) vs. the eager per-M-cycle tick, which costs ~25% of host time (M1 perf review). Record in an ADR before building M2 timing on it
- [x] ADR 0011 implementation, in order:
  - [x] Fixed event table (deadlines); move the PPU onto it (f74cceb)
  - [x] `bench --fingerprint` and `scripts/diff-fingerprint.sh` (7ebc7a5) (behavior fingerprints to compare models)
  - [x] Reload-window rules in the eager timer (0f3d68a), kept afterwards as a test-only reference model
  - [x] Lazy timer and serial clock (4b81a49), differential test against the eager model; rebaseline the benchmark and size budgets (keep timer I/O out of line so `write_high`/`read_high` stay in budget)
  - [x] HALT/STOP/lockup/power-off jump to the next deadline (84c7744)
  - [x] Open hardware questions (ADR 0011): does disabling the timer tick TIMA on CGB (Pan Docs vs SameBoy); serial clock phase after a DIV reset. Resolved in ADR 0011 (2026-11-21): keep SameBoy's tick; serial via the master flip-flop (b58c196)
- [x] HALT (3ee75c7): IME=1 with an interrupt arriving during HALT's own fetch (return address; SameBoy `halt()` re-executes HALT), `EI; HALT`, and CGB wake-up latency +1 M-cycle (M1 hardware review 2, 5)
- [x] DIV/TIMA with the falling-edge behavior, TMA reload windows (0f3d68a)
- [x] OAM DMA timing and bus conflicts (7bdaee2, 75b12ad) (DMA conflicts put state into the CPU read path, so `peek` needs its own side-effect-free path; consider driving DMA through `next_event`)
- [x] Inputs at M-cycle resolution (ADR 0012, acffc11..aadd26b): the contract applies inputs at a time "rounded down to the nearest M-cycle", but `run_until` stops only on instruction boundaries. Add a scheduled button change (e.g. `set_buttons_at(clock, buttons)`) applied in the M-cycle tick and switch romtest to it (review finding 5)
- [x] Quality-bar tooling in the gate (README "Quality and performance bar": the benchmark runs in the gate from M2 on):
  - [x] Deterministic instruction-count benchmark under valgrind (e.g. iai-callgrind) on fixed ROMs, committed baseline; the gate fails on a regression of more than a few percent
  - [x] Code-size budgets on thumbv7em (`-C target-cpu=cortex-m7`) for `Cpu::run::<Ctx>` and `execute_cb`, checked by the gate
  - [x] Short, deterministic fuzz pass in the gate over untrusted inputs: ROM header/`load`, save RAM, `set_rtc_state`
- [x] `hard_rules` test hardening (a2a781f) (M1 API review, low): lex `'\''` correctly (start at `q + 3`, add the case to the allow-test); require `#![forbid(unsafe_code)]` among lib.rs's leading crate attributes, not in a nested module; reject a `[lib] path`, a `build` key and a `build.rs` in gbc-core

**Acceptance:** `romtest --filter 'mooneye-test-suite/acceptance/{timer,instr,interrupts,oam_dma}/**' --gate must`
exits 0. Some top-level Mooneye acceptance tests depend on LY/VBlank timing or
OAM access windows, so the full `mooneye-acceptance` group is required in M3.

## M3: PPU (DMG-compatibility rendering) + screenshots

- [x] OAM DMA as the PPU sees it (Pan Docs "OAM DMA bus conflicts": objects off-screen in mode 2, the word being written in mode 3); sync DMA progress before PPU OAM reads (`docs/reference/oam-dma.md`) (c57dd2e: PPU sees each DMA step 2 dots early, fitted to gambatte `late_sp*` 30/30; scan, mode-3 fetch and STAT effects per SameBoy)
- [x] Confirm OAM DMA pausing in HALT (M2 review H4, SameBoy model) against gambatte `oamdma/oamdmasrc80_halt_*` and `mgb_oam_dma_halt_sprites` once STAT IRQs exist; a halted DMA blocks the mode-2 OAM scan on CGB C (SameBoy `add_object_from_index`) (c57dd2e: `oamdmasrc80_halt_*` pass; `oamdma_late_halt_stat_2` needs the last step not held through HALT, a deviation from SameBoy; `mgb_oam_dma_halt_sprites` is MGB-only)
- [x] Mode timing state machine (OAM scan, mode 3 length incl. SCX/window/sprite penalties, HBlank, VBlank), STAT/LYC interrupts (ADR 0014, f71dbce)
- [x] Scanline renderer with catch-up on mid-line register writes (ADR 0002) (265a078; mid-line writes land per SameBoy CGB table)
- [x] Event deadlines: the table exists since M2 (`schedule.rs`, ADR 0011; incremental minimum 2944113). Remaining: PPU mode/STAT deadlines on `EventKind::Ppu`; STAT and LYC writes must sync the PPU and reschedule (M1 reviews) (f71dbce)
- [x] BG, window, sprites (priority, 10-per-line limit), DMG compatibility palettes (53c0c37, 88c27b4; compat-mode object priority by X, OPRI=1)
- [x] Framebuffer output; screenshot comparison working in `romtest` (265a078; dmg-acid2 passes)
- [x] PPU and DIV phase at power-on (where the CGB boot ROM hands over), replacing the M0 stub that starts at LY=0 dot 0; checked by `mooneye-cgb-misc` boot_div/boot_hwio (M0 hardware review) (9de8fc2, f71dbce; boot_div, boot_hwio pass)
- [x] First line after LCD enable skips OAM scan (GBCTR, SameBoy); no manifest ROM covers it (the lcdon_* tests are excluded), so add a unit test (f71dbce, `first_line_after_lcd_enable_skips_the_oam_scan`)
- [x] Before writing the PPU: decide the output pixel format and stride (ADR 0013: keep RGB555, stride 160, colors resolved at palette-write time; the platform converts while scaling) (review finding 4)
- [x] First headless performance benchmark recorded in `docs/perf.md` (the callgrind Ir gate exists since M1 close, ADR 0010; add PPU-heavy numbers once the renderer runs) (265a078, docs/perf.md)

**Acceptance:** groups `mooneye-acceptance`, `dmg-acid2-compat`, `blargg-cpu-instrs`,
`blargg-instr-timing`, `blargg-mem-timing`, `blargg-mem-timing-2` and
`blargg-halt-bug` all passing.

## M4: Cartridges + saves

- [x] MBC1 (full), MBC2, MBC3 + RTC (on emulated time), MBC5 (incl. rumble bit ignored) (c096256, b863fe2, ae82317, 07f1de2, 6e3cb53, 73b3194)
- [x] (ADR 0016, 0b6705d) Cartridge header parsing, ROM/RAM size handling. Review gaps from M0: types without RAM (Pan Docs 0147) vs. a nonzero 0x149 code, MBC7 EEPROM size (SameBoy 0x100), HuC3 RTC; decide which are in scope
- [x] Save RAM and RTC state exposed through the core API (the caller owns persistence) (ADR 0015, 4cd1849..c2ab199)
- [x] (ADR 0015 `EjectedCart`) `load` over an MBC3 cartridge drops its `RtcState` silently (doc now says to save it first); consider returning it with the buffers (M1 API review, low)
- [x] (caeac2d) `RtcState::advance_seconds(u64)` in the core with hardware rollover (out-of-range wrap, day carry, halt bit), so front ends can sync to wall-clock time correctly; document load-then-set_rtc_state order (review finding 8)

- [x] (41a47b2, 2446bc2) CGB palette colors (pulled forward from M5's "CGB palettes" task, session 06): rtc3test logic passes every subtest after 07f1de2, but its screenshots draw "PASS" in CGB palette green, and BCPD/OCPD are still plain storage. Only the BCPS/BCPD/OCPS/OCPD -> color path that rtc3test needs; attributes/banks/priority stay in M5. Do it **after** the M5 "Before M5 CGB rendering" perf tasks (combined worst-case bench, ADR 0014 timing cuts, object-mix cut) and the `Hw` buffer move (`Hw` is 992/1,024 B after the RTC), which move into M4 with it

**Acceptance:** groups `mooneye-mbc`, `rtc3test-basic`, `rtc3test-range`,
`rtc3test-sub-second` and `mbc3-tester` all passing.

## M5: CGB features

- [x] Double speed (KEY1/STOP), VRAM/WRAM banking (banking existed; speed switch 8ac1f46, 3a9405a; ADR 0020)
  - [x] Speed-switch timing (M2 hardware review H3): the CPU halts ~0x20008 clocks (SameBoy `stop()`, `speed_switch_halt_countdown`) with DIV/TIMA running and OAM DMA paused, not a 2050 M-cycle frozen counter; no halt when IE&IF is pending; STOP with a button held does not reset DIV; replace `dma_rescale`. Arbiters: gambatte `speedchange*_tima*` cgb04c (19/35 fail at M2 close) (8ac1f46, 3a9405a: halt 0x20008 T, PPU 0x20003 dots to single speed per SameBoy/Gambatte; gambatte speedchange 111 -> 178/242)
  - [x] STOP consumes its second byte (SameBoy `stop()`; runs it as an opcode only with an interrupt pending), read before the pause (M2 review H5) (3f4e4d3; SST 0x10 compares all but PC/cycles, 9477b79, human decision)
- [ ] STOP accuracy: DIV/TIMA/serial frozen in STOP, wake-up only on a selected P1 line, DIV reset rules, 2-byte STOP (Pan Docs "Using the STOP instruction"; M1 hardware review 6)
- [ ] Screen blank while in STOP mode (SameBoy `GB_display_vblank` blanks while stopped); extend `screen_blank` (M0 hardware review)
- [x] CGB palettes (BCPS/BCPD/OCPS/OCPD), BG attributes, CGB sprite priority (e22721c, 17db555; cgb-acid2 passes)
- [ ] Palette RAM access blocked in mode 3 (BCPD/OCPD reads and writes; `TODO(M5)` in `bus.rs`)
- [ ] cgb-acid-hell: 2 pixels at (80,68)/(80,69) after 17db555 (session 07 investigation, unfixed). The ROM writes LCDC every 8 dots in mode 3 (cycling $80/$E1/$E3/$F3); pixel 80 is the last before the $E3->$F3 write. We draw it through map column 0 palette 0 (attr 0x00), the reference looks like the next tile's palette 1 (attr 0x01). Ruled out: span off-by-one, LCDC.4 timing, flips, VRAM bank, objects. Next: SameBoy `GB_IO_LCDC` write path + `display.c` fetcher for a CGB attribute-latch quirk at the write dot. Affects CGB ROMs writing LCDC in mode 3 only
- [x] ADR for the per-pixel object rank buffer (`Memory::obj_rank`): fetch order (X, drives mode-3 stalls) is separate from draw priority (OAM index in CGB mode); OPRI writes only latch during boot (SameBoy `memory.c`) (ADR 0019)
- [ ] Extend `synth:cgb-raster` with CGB BG and object attributes (bank 1 tiles, flips, mixed palettes), rebaseline in the same commit
- [x] GDMA/HDMA with correct timing, including during HALT and LCD off (dc1ef28, 86a13ca, b315d4a; ADR 0017). Deferred edge cases: byte-wise GDMA vs mode-3 VRAM reads; OAM DMA during VRAM DMA (SameBoy); VRAM-bank and CGB-E write corruptions; HALT wake with IME off runs the instruction before the block (SameBoy: block first); a second block when FF55 is written between mode-3 end and the trigger; VRAM/E000+ sources read FF instead of the data bus; a run call can overshoot by one transfer step (ADR 0017)
- [ ] OAM DMA CPU-side bus conflicts vs. hardware: ~70 gambatte `oamdma` ROMs (`busypush*`/`busypop*` etc.) fail because the SameBoy-derived CGB C conflict table differs from gambatte's cgb04c hardware results; decide which reference wins per case once romtest runs gambatte ROMs. Also add a unit test for the FF46 write -> PPU sync call in the bus (c57dd2e review notes)
- [x] CGB vs. DMG-compatibility mode selection from the header (`Bus.cgb_mode` from 0x143 bit 7; PGB-mode values are a separate low below)
- [x] `romtest`: implement the `gambatte` check (hex-pattern screen match, audio silence, PNG), so the stretch-tier Gambatte suite reports real results (78c6443, reviewed; 27d11fa; lockup rule 75902f9, human-approved; audio waits for M6)
- [x] (done in M4: 0fa7a86 bench, 0c23c48, e15d85f, 68c5d32, 386acba; 3.66 M Ir/frame) Before M5 CGB rendering (M3 perf review): a synthetic bench combining the event storm with objects, the window and STAT sources (event-storm 3.73 M + acid2 PPU 0.53 M ~ 3.97 M Ir/frame, 99% of the 4.02 M line, added from two runs); make the ADR 0014 timing cuts and the object-mix cut first
- [x] `Hw` buffer move: `Renderer::obj_buf` (176 B) moved into `Memory`; `Hw` is 832 B (was 1,008), size assert lowered from 1,024 to 864 B. The stall/timing arrays stayed (small). CGB palette tables go in `Memory` the same way
- [ ] Re-check the two provisional PPU-side OAM DMA deviations (2-dot lead, last step not held through HALT; `docs/reference/oam-dma.md`) against gambatte `late_sp*` and `oamdma_late_halt_stat_*` once the `gambatte` check exists; also which DMA word a mode-3 object fetch reads (SameBoy fetch timing + gambatte-fitted lead) (M3 reviews)
- [ ] Post-boot hand-over for CGB-mode cartridges: PPU phase (`ppu/mod.rs` post-boot), `POST_BOOT_COUNTER` (DIV), BCPS/OCPS are fitted to DMG-mode ROMs; the CGB boot ROM runs a different length (M3 reviews; `mooneye-cgb-boot-regs`)
- [x] The speed-switch snapshot test in `lib.rs` pins current (H3-wrong) behavior: rewrite it from references with the H3 fix (M3 test-integrity review) (8ac1f46, rewritten from SameBoy numbers; mutation-checked 3a9405a)
- [ ] Blargg `interrupt_time` (and cpu_instrs 03/06 singly) lose console text since VRAM blocking (22eccab, "iled"); the commit blames the double-speed half-frame timeout. Confirm against double-speed LY/mode-3 timing, not just the explanation (M3 test-integrity review) (still fails after the speed switch, session 07; not investigated)
- [ ] PPU LY/LYC/mode-1/mode-2 sub-M-cycle thresholds were tuned with the CPU at one clock phase only. The speed switch to single speed (2004f80, halt now 0x20003 dots per SameBoy/Gambatte) puts the CPU at the other phase (Gambatte "LCD offset" 1/3), and 25 stretch ROMs that passed only by phase luck fail: 11 gambatte `lcd_offset/offset{1,2,3}_*`, 12 `dma/hdma_late_m3speedchange_*` / `hdma_transition_speedchange_*`, `m2enable/late_enable_ly0_lcdoffset2_1`, `oamdma/oamdma_late_speedchange_stat_1`. Making the halt 2 clocks short fixed the lcd_offset ones but lost 42 others, so fix the PPU thresholds, not the halt (session 07)

- [x] M4 hardware review (session 06): in CGB mode LCDC.0 is BG/window master priority, not BG blank; `ppu/render.rs` still blanks the BG to palette 0 color 0 (now visible with real CGB colors). Fix with CGB priority (e22721c)
- [x] M4 hardware review: `sorted_object_stalls` relies on `priority_order()` being an X sort. When CGB priority follows OAM order (OPRI=0), keep a separate X sort for mode-3 stall timing (17db555: X order still drives fetch and stalls; OAM-index rank applied in the merge)
- [ ] M4 hardware review: MBC3 latch fires on 00->01 only; SameBoy `write_mbc` latches on every 6000-7FFF write. Settle against hardware docs (GBCTR) / tests before changing behavior
- [ ] M4 perf review lows: `fill_tiles` pair stores become unaligned word stores when SCX&7 is odd (80/line; matters if a platform puts the frame in FMC SDRAM, ES0491 2.7.3: document a frame-alignment/placement rule or split stores); `synth:worst-case` never hits VRAM/OAM out-of-line reads, cart RAM, MBC bank writes, or 10 objects on all 144 lines (OAM-DMA multiplexing, ~+60k Ir/frame est.); `Ppu::latch` has 6 bounds-check panic sites (code size)
- [ ] M4 perf review HIGH, tracked: the 4.02 M x86-Ir line assumes 1 Ir = 1 M7 cycle; thumbv7em static counts are 1-2x (u64 maths in `boundary`), so 3.66 M Ir/frame is ~59-91% of the M7, and `Cpu::run` (25 KB) exceeds the 16 KB I-cache. Get an M7 cycle measurement as early as possible (needs the human/hardware); cut u64 per-event maths (u32 epoch deadlines) before M6 adds the APU

- [ ] ADR 0015 review lows (session 06): RTC register writes set save-dirty even when the value is unchanged, unlike RAM writes (compare first; a seconds write must still dirty since it resets the sub-second count); `CartInfo::has_save` misses EEPROM saves (MBC7 0x22, TAMA5 0xFD, header RAM 00): document or add `eeprom_size` when those mappers arrive; `CartInfo::rtc` is documented as MBC3-only, HuC3 (FE) has a clock too: word it as "`rtc_state()` is Some"
- [ ] M4 perf fix follow-up: a BCPD write still costs ~147 x86 Ir (mostly `run_ppu_to`/`Ppu::sync` + `write_io`; ~370 Ir more per mid-mode-3 span draw); `synth:cgb-raster` 2.46 M Ir/frame. Cut the per-write sync cost when CGB rendering lands
- [ ] romtest recolor re-review low (92f4707): `a_merge_into_an_existing_color_is_exit_2` only reaches the up-front `verify_recolors` path; the check-time `apply_recolor` call (checks.rs:308) has no isolated test (same function, so no bypass today)

- [ ] M5 review (hardware) low: in plain STOP, HDMA triggers and OAM DMA keep running; SameBoy blocks HBlank blocks while `stopped`, runs no `GB_dma_run` while stopped, and applies the mode-0 wake rule in `leave_stop_mode`. Do with the STOP-accuracy task
- [ ] M5 review (hardware) low: an FF55 write in the last dot before mode 2 (`stat_mode0()` true for t < 4) starts a block at once; SameBoy `memory.c` requires `display_state != 7`
- [ ] M5 review (hardware) low: HDMA's STAT decisions (`write_hdma5`, `vdma_halt`, `stat_mode0`) ignore the OAM-DMA STAT quirk (SameBoy reads raw STAT, 0 in mode 2 during OAM DMA)
- [ ] M5 review (hardware) low: header 0x143 with bit 7 plus bit 2 or 3 (0x84/0x88/0x8C) is PGB mode on hardware (Pan Docs 0143; SameBoy `cgb_mode = !(value & 0xC)`); `bus.rs` picks CGB mode for them
- [ ] M5 review (hardware) info: FF55 read-back after a cancel returns the written length | 0x80 (SameBoy, Gambatte), not the remaining blocks Pan Docs implies; note it in ADR 0017
- [ ] M5 review (perf) low: BG attribute decode runs in DMG compatibility mode with `attr_mask` 0 (cpu_instrs +3.13% at e22721c); a const-generic CGB variant costs ~1.8 KB, revisit with the M6 code-size budget
- [ ] M5 review (perf) low: object rank merge does ~10 u64 ops (2 multiplies) per object fetch, also in compatibility mode (rank 0); <= ~30k M7 cycles/frame
- [ ] M5 review (perf) low: no per-function budgets for `fetch_objects` 776, `partial_tile` 182/162, `write_palette_data` 356, `vram_dma` 708, `vdma_arm` 210, `Emulator::run` 458
- [ ] M5 review (perf) low: benches miss OCPD writes and a combined palette raster + objects + HDMA frame; worst-case runs all-zero attributes (OK: decode is data-independent)
- [ ] Lockup re-review low (session 07): `gambatte-cgb` excludes `gambatte/undef_ops/**` but `gambatte-cgb-undef-ops` selects `gambatte/undef_ops/*cgb04c*`; a nested ROM in a future release would fall in neither. Harmless while `test-roms.sha256` pins the set; make the globs match (manifest change, human)
- [ ] HDMA perf leftovers (a3dff9b): `vdma_arm` ~30 Ir/call, 3.4 calls/line (1.76 M Ir per 2 s of synth:worst-case); every HBlank block ends a `Cpu::run` call (~27k extra calls per 2 s). Total `.text` headroom 32 B after a3dff9b (M6 budget decision pending with the human)

**Acceptance:** groups `cgb-acid2` and `mooneye-cgb-boot-regs` passing;
`same-suite-cgb` and `age-cgb` tracked (should tier).

## M6: APU

Design: ADR 0021 (state in `Memory::apu`, lazy catch-up with pure reads,
`DivApu` from the DIV falling edge, 32,768 Hz box-filtered output, empty
buffer = advance only). Original items, now covered by the tasks below:
four channels + frame sequencer + length/sweep/envelope + wave RAM quirks
(1, 3-6); batched sample generation into the caller's buffer (1);
resampling/mixing documented in architecture.md (10); FF76/FF77 live
(PCM12/PCM34 hard-wired to 00 in `bus.rs`, TODO(M6), M3 test-integrity
review) (1, 3-5).

Ordered; one focused agent each (~20-40 tool calls). Every new test is
mutation-checked. Tasks 1-2 run on branch `m6-apu` (over the `.text`
budget); nothing merges to `main` before step 2's human decision.

1. [x] (52b1756, f4e7eb0-344e112, a627b54 GDMA fix, 323c4f0 budgets) **APU framework + CH2** (branch `m6-apu`), two commits, `.text`
   measured after each (`check-code-size.sh` total and per symbol):
   - (a) Framework: `apu/` layout and `Apu` in `Memory` (size test <= 128 B;
     `Hw` unchanged), cold `Ctx::write_apu` for FF10-FF3F, NR52 power off/on
     with CGB semantics (zero FF10-FF25 and state, writes ignored while off,
     lengths included), NR52 read, `read_apu` for FF26/FF30-FF3F/FF76/FF77,
     `DivApu` deadline + `fs`/`skip` (power-on skip, DIV-write early step in
     `div_reset`, speed-switch reschedule, reset/load), output pipeline with
     no channels (window integrator, NR50/NR51 mixer, `AudioBuffer` event,
     end-of-call sync, advance-only path, `live` restart),
     `AUDIO_SAMPLE_RATE` + ADR 0006 amendment. Fingerprint diff vs parent:
     only the post-boot NR52/APU state may differ; explain every ROM.
   - (b) CH2: `Square` (duty, trigger countdown per ADR 0021 section 5,
     first-sample-zero), shared `Length` (incl. extra clocking and the
     trigger reload) and `Envelope` (incl. zombie mode), DAC, PCM12 high
     nibble, integration into the mixer. Report F (a), S (b) and the full
     estimate `F + 3.3 S` (ADR 0021 section 11).
   - The one API reviewer (AGENTS.md review rule 3) on the
     `AUDIO_SAMPLE_RATE`/ADR 0006 diff.
2. [x] (human: 96,000 B, 8c27f73; merged a1bb74a) **Code-size checkpoint (orchestrator + human, no agent):** bring F,
   S and the estimate (plus margin) to the human under "Needs human";
   the human sets the new total `.text` budget once. Then a budget commit
   on the branch and merge `m6-apu` into `main` (merge commit, no
   rebase/force). Per-function budgets for the new APU symbols are added
   here too. While waiting, tasks 3-5 may stack on the branch.
3. [ ] **CH1 = `Square` + `Sweep`**: classic sweep (trigger copy, period
   0 = 8, overflow check on trigger with shift != 0, second calculation,
   negate quirk on NR10 write), PCM12 low nibble. Arbiters: cgb_sound
   04, 05, 06 (`rom_singles`).
4. [ ] **CH3 wave**: NR30 DAC, 256 length, NR32 shift, sample buffer kept
   on trigger, trigger delay `(2051 - f) * 2` (verify the countdown
   convention in SameBoy), CGB wave RAM read/write while on (pure read via
   a channel copy), no corruption on retrigger, cached waveform sum,
   PCM34 low nibble. Arbiters: cgb_sound 09, 10, 12; gambatte
   `sound/ch3_*ff30*` hex ROMs as a cross-check.
5. [ ] **CH4 noise**: LFSR (XNOR, 7-bit mode), period `(r ? 16r : 8) << s`,
   shift 14/15 frozen, shared length/envelope, PCM34 high nibble.
6. [ ] **cgb_sound to pass**: run `--filter 'blargg/cgb_sound/**'`, fix what
   fails in 01/02/03/07/08/11 (registers, length, trigger, frame-sequencer
   sync, power). If more than two subtests fail here, one agent per
   subtest. Must-tier gate step green.
7. [ ] **Sync invariant checks** (ADR 0021 section 3): unit test advancing
   the APU with random split points vs one span (bit-identical state and,
   with a buffer that never fills, identical samples); `bench --fingerprint`
   gets an audio-buffer option and the fingerprint with and without it must
   be identical over the corpus. Mutation check: break one channel's
   closed-form advance, see both fail.
8. [ ] **Perf**: `tools/bench` passes per-call audio buffers (as the MCU
   will), `synth:worst-case` gains all four channels at their worst periods
   (square/wave f = 2047, noise r = 0 s = 0, 7-bit, both sides);
   rebaseline in the same commit. Budget: APU <= 200k x86 Ir/frame on
   worst-case; if over, the ADR 0021 options in order. Run `check-bench.sh`
   alone.
9. [ ] **romtest audio rule** (`_outaudio0/1`, 131 ROMs): empty buffer until
   the last frame, then 550 slots; silent = all returned samples equal
   (Gambatte testrunner). Cross-check once in a scratch worktree with
   `SAMPLE_PERIOD_LOG2 = 1`: all 131 verdicts must match the 32 kHz run
   (report any mismatch; fallback is a test-only rate option, not a
   manifest change). ADR 0018 amendment; this changes runner pass/fail
   logic, so it gets its review (or the milestone test-integrity review,
   before any result is relied on).
10. [ ] **Docs**: architecture.md (APU module, catch-up points, audio
    contract: rate, buffer sizing, front ends own resampling + HPF),
    `docs/reference/apu.md` (distilled rules with sources, the SameBoy
    countdown conversions), `docs/perf.md` APU numbers.
11. [ ] **Milestone close**: hardware, test-integrity and perf reviewers in
    parallel (the sample path is new per-frame work), triage, baseline,
    tag `stage1-m6`.

Deferred (stretch, gambatte `sound/` and SameSuite APU ROMs; ADR 0021
section 12), not part of the M6 gate:
- [ ] SameBoy envelope lock/clock model with the secondary (rising-edge)
  DIV event and the CGB-C `_nrx2_glitch`
- [ ] Square trigger delay / `lf_div` fit (gambatte `ch1_duty*_pos*`, `ch2_init_pos*`)
- [ ] SameBoy noise counter (14-bit counter, rising edge of the selected bit, alignment-dependent trigger)
- [ ] Sweep calculation delay; DAC fade to 0

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
- [ ] Mid-line PPU fidelity (Mealybug, stretch): tile-map/tile-data/SCY/object changes apply per pixel, hardware applies them per fetch; WX=0 is one pixel off when SCX&7 != 0 (`m3_window_timing_wx_0`); mid-line SCX carry (`m3_scx_low_3_bits` pixels 152-159). From the M3 renderer integration (265a078)
- [x] (M4: 0c23c48, e15d85f, 68c5d32; look-ahead reuse measured slower, dropped) PPU cost cuts (M3): the object-mixing path in `render` is the next cut (~375k Ir/frame with objects vs ~220k without); `start_line` is 2.2 KB on thumbv7em, large for what it does (now inlined into `Ppu::latch`, 4.4 KB, once per line); ADR 0014's two timing cuts: the cursor is done, reusing the look-ahead walk measured +0.3% on worst-case and was dropped (`docs/perf.md`, "PPU timing cuts")
- [ ] Code size: re-measure `execute`/`execute_cb` on thumbv7em (68 KB at M1, more than the H753's fixed 64 KB ITCM; the H735 ITCM can be configured up to 256 KB). Target <= 40 KB, e.g. by moving rare read regions out of line (M1 perf review 1). At M2 close `run` 28,560 B + `execute_cb` 13,158 B = 41.7 KB, ~45.5 KB with boundary/read_high/write_mapped, 2.6x the M7 16 KiB I-cache: plan ITCM placement (M2 perf review P5)
- [ ] Worst-case event density on the M7: `synth:event-storm` (double-speed timer TMA=FF + fast serial) is ~3.46 M x86 Ir/frame after 2944113, under the 4.02 M (50%) line only as an x86 proxy; the M2 perf review estimated M7 cycles at 1.3-2x Ir for this u64-heavy code. Measure on hardware (Stage 2) and, if over, e.g. u32 epoch-relative deadlines
- [ ] Unaligned ROM reads: the Stage 3 MCU (STM32H735) does not support unaligned reads from the FMC SDRAM that holds the ROM (ES0491 2.7.3). `-C target-feature=+strict-align` is unstable on Rust 1.100 (warns, slated to become a hard error, rust-lang/rust#162235), so ensure at code level that ROM bytes are never merged into wider loads (e.g. one ROM-read function using byte reads LLVM can't combine) and add a disassembly check; record in an ADR
- [ ] ROM placement hook for the MCU front ends (e.g. caller-supplied per-bank slices so bank 0 and the current bank can sit in internal SRAM) only if Stage 2 measurements show ROM fetch misses matter (M1 perf review 5)
- [ ] Benchmark: headless emulated frames/second on desktop for a fixed set of ROMs; record host cycles per emulated frame as the baseline for Stage 2
- [ ] PPU edge cases from the M3 hardware review (low, no ROM): WY==LY only checked at t=89 and on writes, not at mode-2 start (a WY/LCDC.5 write in [4,89) loses the window trigger); a palette write whose line end == `now` misses pixel 159 (`bus.rs` palette flush: run to `at`, flush, then sync); a coarse mid-line SCX change applies mid-tile, hardware at the next fetch; mode-3 window stall latched at t=89 (WY/LCDC.5/WX writes in mode 3 change pixels, not mode-3 length); LCDC.2 (object height) latched at t=89, not per scanned entry; running-DMA OAM scan uses the mode-2 bus (SameBoy) vs Pan Docs "off-screen"; `stat_during_dma` mode-0 window vs the PPU's start-delay "off"; WY write in HBlank triggers the window next line (SameBoy) vs Pan Docs mode-2-only
- [ ] Code size (M3 perf review): gbc-core `.text` 79.6 KB after 3b556ae (61.2 KB at M2), no longer fits the H753's 64 KB ITCM; per-frame hot set ~52 KB. Plan per-function ITCM placement (front-end link sections), and add a total-`.text` budget to `check-code-size.sh` (stricter)

**Acceptance:** `./scripts/gate-stage1.sh` exits 0.

---

## M9: Test-ROM completion

Human decision (2026-11-22): the core is not complete until every test ROM
in the manifest passes, whatever its tier (README.md "Done when").

- [ ] Triage every failing ROM by root cause (grouped plan tasks), starting with the `gambatte-cgb-png` failures (180 at M5; many `scy` ROMs fail by the same 2,112 pixels) and the mid-line PPU fidelity task (M8)
- [ ] Fix them, keeping the perf and code-size budgets (or bringing budget changes to the human with numbers)
- [ ] For each group that can't pass because the references contradict each other (e.g. the ~70 gambatte `oamdma` bus-conflict ROMs vs SameBoy) or because it doesn't apply to the targeted CGB revision: write up the evidence and get the human's explicit sign-off, recorded per group

**Acceptance:** every manifest ROM passes, or belongs to a group the human signed off with a recorded reason.

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
