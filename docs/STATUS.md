# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 is **COMPLETE** (human, 2026-11-25; tag `stage1-m10`).
  **Stage 2 (STM32H753 firmware): M0-M4 done** (M4: 10-minute paced runs
  on the board, milestone review fixed, tag `stage2-m4` at c0de0b5).
  **M5 (input and the player loop) in progress.** Plan:
  `docs/plan/stage-2.md` (D1-D11).
- **Dev-board clock (D11):** NUCLEO-H753ZI at **472 MHz from the HSI**
  (DIVM1 5, fractional N 73.75, AXI 236, APB 118, VOS0 via ODEN, flash
  4 WS; DIVQ1/DIVR1 zeroed, c2e8e2b); LSE-measured 473.5-473.6 MHz.
  Real-time frame **7,902,557 cycles**; 50% report line **3,951,279**
  (reported, not gated).
- **M1 results:** `headless` image byte-equal to the host on the board and
  in QEMU for cpu_instrs and cgb-acid2 (600 frames); cpu_instrs mean/max
  1,296,098 / 1,768,873 cycles per frame, cgb-acid2 684,112 / 756,317.
  `.itcm` 64,080 B of 64 KiB (1,200 B free). H735 size: docs/perf.md
  "Firmware-link size". M0 bench numbers: `tools/h7-bench/README.md`.
- **M2 results:** shield NOR = Macronix MX25L6433F (8 MiB, QE set),
  memory-mapped 0xEB at 59 MHz, ~770 cycles per random 32-byte miss.
  Loader at 2 Mbit/s. h7-bench from QSPI: worst-case 3,417,095 max (86.5%
  of the 50% line). Facts: `docs/reference/qspi-nor.md`, `loader.md`,
  `m2-evidence.md`, docs/perf.md "QSPI ROM placement (M2)".
- **M3 results:** the shield panel is an **ST7789V** (variant $AZ2), not
  the ILI9341V. FMC 8-bit, timing SPEC (per_ck 64 MHz, 5 cycles/byte),
  12.8 MB/s; MDMA from the TE ISR, 8.10 ms per 240x216 frame; TE tuned to
  16.735-16.749 ms (FRCTRL2 0Eh, PORCTRL 11h/11h). cgb-acid2 600 frames
  with the display: hash lines EQUAL, 587 readbacks 0 mismatches,
  scale+present max 196,480 cycles (2.5% of a frame), core +0.07%.
  Facts: `docs/reference/lcd-panel.md`, ADR 0039, docs/perf.md "Display
  in the loop (M3)".
- **M4 results:** DAC1 dual 12-bit on PA4/PA5, TIM6 236 MHz / 7202 =
  32,768.675 Hz, DMA1 stream 0 (2 x 256 frames, MPU region 9), watchdog
  restart. `paced-<rom>` images (audio-paced loop + display): cgb_sound and
  synth:worst-case 35,837 frames each, `paced` lines EQUAL to host, 0
  underruns, ppm +82/+71 vs DWT; worst-case core 3,328,804 / 3,350,486
  mean/max with display + audio. Display skips ~27% of frames (M4 P-7).
  Facts: `docs/reference/audio-dac.md`, `m4-evidence.md`, docs/perf.md
  "Paced loop (M4)". Check: `run-paced.sh` (tests `scripts/test-run-paced.sh`).
- **Board state:** `paced-cgb_sound` flashed (plays ~10 min after reset,
  then silent); bench pack in the NOR. `out/loader.bin` must be rebuilt
  from main before use.
- **Last updated:** 2026-11-26, session 25 (log
  `logs/2026-11-26-25-stage2-m4.md`; session 24:
  `logs/2026-11-25-24-stage2-m3.md`).
- **Model (human, 2026-11-25):** Opus is fine.

## Next up: Stage 2 M5 (input and the player loop)

Done: gbc-pc side (b303636, `docs/reference/link-protocol.md`), display pending-frame replace (cf86d75), `gbc_firmware::{buttons,save}` and `paced::run_with` (`InputSource`,
`Service` hooks), 6c2bcee. In flight: `tools/gbc-pc` live input + save
load/dump. Next: the `player` image (ROM from QSPI, display with the
pending-frame replace (M4 P-7), audio, joystick + PC input, saves over the
link, stats, input echo, DMG compat palette), then the scripted replay-set
run vs host and the save round trip (use a halted-RTC `.sav` fixture: a
running RTC changes the footer in emulated time). Open lows: M1 (T-4, T-8,
F-2, F-3, F-5..F-7), M2 (H-2..H-5, T-3, T-5), M3 (T-3, T-4, P-3, P-5, P-6),
M4 (T-3, P-3, P-4, P-5, P-7) at the end of their milestones in the plan.

Firmware map (ADR 0036): `crates/gbc-firmware` (no_std headless loop,
`CycleClock`, `rom_image` (ADR 0037), `video` (scaler), `headless::FrameSink`, `audio`, `paced` (`run_with`), `buttons`, `save`; root workspace), `crates/gbc-link`
(COBS + CRC-32 serial protocol, no deps), `tools/gbc-pc` (Windows .exe
loader client; `scripts/gbc-pc.sh` builds, runs it via interop, takes the
board lock; host tests incl. the board's own `loader.rs` and `baud.rs` via
`#[path]`), `crates/gbc-nucleo-h753` (own workspace: lib bring-up incl.
`qspi.rs`, `loader.rs`, `baud.rs`, `lcd.rs`, `lcd_dma.rs`, `gpio.rs`, `audio_dac.rs`; bins `boot`, `lcd-probe`, `lcd-dma-probe`, `dac-probe`, `paced` (images `paced-<rom>`), `qspi-probe`, `qspi-mmap`,
`qspi-write`, `loader`, `headless` (feature `headless`; `rom-qspi` = ROM
from the NOR; `display` = panel + readback self-test, images `headless-display-<rom>`); `build.sh [image...]`, `flash.sh`, `run-headless.sh`,
`run-headless-qspi.sh`, `run-loader-roundtrip.sh`, `gen-test-image.sh`),
`tools/headless` (host counterpart, `expected/`), `tools/headless-qemu`,
`tools/memory-x`, `tools/h7-bench` (cycle bench; QSPI pass from a ROM pack,
`run-qspi.sh`, `check-report.sh`). Gate: `scripts/gate-stage2.sh
--host-only` (H1-H8, ~7 min alone, exit 3 = all OK; board steps B1-B5 not
implemented, so without `--host-only` it exits 1). Board lock:
`scripts/lib/board-lock.sh`. Build env for the firmware scripts:
`GBC_BUILD_LOCK=<lock>`, `GBC_TEST_ROMS=<dir>`. Serial at another rate:
`H7_BAUD=<bps> tools/h7-bench/read-serial.sh`.

Key plan points: board performance vs the 50% bar is report-only (the README
target applies to the H735 at 550 MHz); gated are real time and the +3%
cycle ratchet. Input: shield joystick plus PC over serial; saves travel over
serial. Shared logic in `gbc-firmware`, protocol in `gbc-link`, PC side in
`tools/gbc-pc` (ADR 0005 amendment).

**Risk for M6 (open, decide there):** with the same clock and sources, a
rebuild moves cycles/frame by about +-2% (code layout), against the planned
+3% on-board ratchet. M6's baseline must say how it handles that.

Stage 1 leftovers (low value, any time): open M10 lows C-H1..C-H5;
unplayed checklist rows (Pokemon Crystal, Alone in the Dark). Stage 1 close
history: `docs/reviews/stage1-m10.md`, tag `stage1-m10`, logs for sessions
18-20.

**How work is run:** one agent per theme in a scratch worktree under the
session scratchpad (`git worktree add <scratch>/wt-x -b <branch> main`), every
cargo/valgrind command under `flock -x <scratch>/build.lock` (absolute
path). The orchestrator rebases the branch on `main`, re-runs clippy,
tests, and for `gbc-core` changes `check-code-size.sh`, `check-itcm-set.sh`,
`diff-fingerprint.sh main --audio-buffer 7 --hash-samples`,
`check-sync-invariance.sh`, romtest full manifest and `check-bench.sh`, then
`git merge --ff-only`, removes the worktree, and re-records
`perf/bench-baseline.txt` after perf merges. Correctness fixes need a test
seen failing first; refactors must be fingerprint-identical. Brief pattern:
goal, files, constraints, what to measure, exact report format, commit
trailer. Board: one agent at a time; standing permission to flash and read
serial (see Needs human).

**Gates now:** `./scripts/gate-stage1.sh` keeps running unchanged (step H2
of the Stage 2 gate; removing a step needs the human). Total gbc-core
`.text` <= 122,880 B (128 KiB ITCM on the H735 minus 8 KiB reserve;
provisional until M1 settles it on a firmware link); +3% Ir vs
`perf/bench-baseline.txt`; fuzz under the `checked` profile; invariance
checks. Per-function size lines and the H753 ITCM hot set are report-only.
Release profile: `codegen-units = 1` plus
`-C llvm-args=-cgp-max-address-users-to-scan=100000` (human approved).
Per commit: `cargo fmt`, `clippy`, `test`, plus `gate-stage2.sh --host-only`
for firmware commits once it exists. `stage2` gate and manifest are proposed
in the plan, not yet created.

**Human (session 14): the desktop client is a throwaway used to validate
the core; spend minimal time on it.** Fix only what blocks core validation.

**Safety (read AGENTS.md "Machine resources"):** never open the Linux
window under WSLg and never use the GPU from WSL. Window/audio/input smoke
tests use `scripts/run-windows.sh <rom> --frames N [--out shot.png]` on a
scratch ROM copy; short, and only for front-end changes. Windows Update can
shut WSL down mid-session: commit often.

Desktop front end (`crates/gbc-desktop`): `gbc-desktop <rom> [--scale K]
[--frames N] [--screenshot out.png] [--dmg-palette auto|gray]
[--headless]`. Save: `<stem>.sav` (ADR 0023). Keys: arrows, Z = B, X = A,
Enter = Start, Right Shift/Backspace = Select, Esc quits.

Working rules that matter here:
- Every new test is mutation-checked: break the code, see it fail (AGENTS.md).
- `scripts/diff-fingerprint.sh <rev>` proves "no behavior change" (in a
  worktree pass `--root /home/jess/gbc-32/test-roms`).
- Resources: at most 2 concurrent builds; heavy steps (check-bench,
  diff-fingerprint, gate) alone; launch at most 2 building agents.
- Core work in scratch worktrees, merged by rebase + `--ff-only`.
- ADR numbers: check `docs/decisions/` before committing one (next free 0040).
- `end pass=N` in h7-bench is a repeat counter, not a verdict; the first
  report after a flash is often truncated by the ST-LINK. Don't time with
  the WSL clock (it ran ~2% slow).

## In flight
- Session 25: merged gbc-pc `load --sav`/`dump`/`play` (b303636; `play` reads keys with GetAsyncKeyState, so keys pressed in other windows count, incl. Esc) and the paced-loop smoothing (cf86d75: skips 27% -> 7%). Running: `m5-link` (board link session: UART RX, input, save load/dump, `link-probe`, save round trip).

## Needs human

Open (session 25, 2026-11-26):
- **Gate hooks for three new script tests** (stricter only; not done
  because gate edits prompt you): `scripts/test-run-headless.sh`,
  `scripts/test-check-report.sh`, `scripts/test-run-paced.sh` all pass and
  never touch the board. Hook each into `scripts/gate-stage2.sh` next to
  the `scripts/test-board-lock.sh` block (~lines 504-506: `sub ...`,
  `run_status <script>`, `[ "${rc}" -eq 0 ] || fail ...`). Say "hook them"
  and an agent will do it (the edit will prompt).
- FYI, no answer needed unless you object: the M5 save round-trip will use
  a `.sav` fixture with the MBC3 RTC **halted** (DH bit 6). With a running
  RTC the footer advances in emulated time, so "dump after a scripted run
  equals the original" can't hold; a no-RTC cart is also covered.
- FYI: `paced-*` images set DBGMCU_CR.DBGSLEEP_D1 so DWT counts in WFI
  (needed for the speed figure). It survives a system reset until a power
  cycle and keeps the CPU clock running in sleep (more power); dev-board
  only, Stage 3 should time with a TIM (M4 P-5).

Open (session 24, 2026-11-25):
- **Audio-paced emulation runs ~0.33% fast in real time** (no answer
  needed unless you object). The DAC trigger is the HSI-derived 236 MHz ÷
  7202; against the LSE the sample rate measured 32,871.7-32,881.5 Hz
  (+0.32-0.35%, the HSI's own error, within D11's accepted tolerance).
  M4's ±0.1% speed check is against DWT (same HSI), so it can't see
  this; ~6 cents sharp. Agent plan: keep ÷7202 and meet M4 as written.
  Option if you want true real time: pick ARR at boot from an LSE
  measurement (±70 ppm), and measure the M4 check against the LSE instead
  of DWT (that changes the check's reference, so it's your call).
- **Audio wiring** for sign-off item 2: left PA4 = CN11 pin 32, right PA5
  = CN12 pin 11, AGND CN12 pin 32; line level (≥ 5 kΩ load: powered
  speakers or an amp), ~1.65 V DC bias, so AC-couple (e.g. 10 µF series,
  100 kΩ to ground). `docs/reference/audio-dac.md`.
- **The shield's panel is an ST7789V, not the ILI9341V** (RDDID 85 85 52,
  TE 60 Hz; UM2905 §6.2: variant XNGFX02Z1$AZ2 has the ST7789V, $AZ1 the
  ILI9341V). Please read the shield sticker to confirm `$AZ2`. Plan and
  architecture wording was corrected (chip name and commands only).
- **Look at the panel** (sign-off item 1 early check; also check for
  inverted colours or poor contrast: the init relies on ST7789V reset
  values, M3 review P-5). The cgb-acid2 frame is left on it now; the
  lcd-dma-probe test image (reflash `out/lcd-dma-probe.bin`) should show
  8 vertical colour bars; with MADCTL BGR as planned the
  first bar should be blue and the third red, and a white L-shaped
  marker top-left. To show dmg-acid2: `cd crates/gbc-nucleo-h753 &&
  ./build.sh headless-display-dmg-acid2 && ./flash.sh
  out/headless-display-dmg-acid2.bin` (about 70 s until the last frame).
  If red/blue are swapped or the marker isn't top-left as you hold the
  board, say so (MADCTL changes; `docs/reference/lcd-panel.md`).
- The ST7789V datasheet (Sitronix V1.3) is not in
  `../gbc-32-hardware/datasheets/`; URL and sha256 are in
  `docs/reference/lcd-panel.md` if you want to add it (agents can't).

Open (session 22, 2026-11-25):
- **H735 ITCM reserve (C-P5, no answer needed now):** measured on the firmware
  link (docs/perf.md "Firmware-link size"), a core grown to the 122,880 B gate
  plus the compiler_builtins it calls (4,278 B) leaves 3,854 B of the 8 KiB
  reserve for ISRs and the blit. Gate unchanged; lowering it to 118,784 B is
  your call once the H735 front end exists.
- FYI, no answer needed unless you object: ADR 0036 drops h7-bench's `h755`
  feature and its 392 MHz and HSI-64 images when h7-bench moves onto the
  `gbc-nucleo-h753` bring-up lib (the H755 is retired; D8, D11). Sources and
  numbers stay in git history and `docs/perf.md`; `m4-stub/` is no longer built.

Open (session 21, 2026-11-25):
- **NUCLEO-H753ZI** (X-NUCLEO-GFX02Z1 shield; E: / COM5): standing
  permission (human, 2026-11-25) for agents to flash it and read its serial
  port. It dropped off USB once right after a flash (cause unknown;
  re-plugged, then fine). Left running `bench-itcm-dtcm-pll480` (the 472 MHz
  clock; the image keeps the `pll480` name, banner `clock=pll472`). No answer
  needed. The NUCLEO-H755ZI-Q is retired; `tools/h7-bench` keeps an `h755`
  feature.

Open (M10, session 18):
- **Costs of the filter beyond the worst case** (told to the human, no
  answer needed unless they object): apu-writes +33.6%, halt-irq +14.6%,
  cgb-acid2 +10.6%. The filter saturates to i16 (unity gain kept; only a
  four-channel swing above 81% of full scale reaches it).
- **Mitigations for the outside-the-bound loops** (C-P2, measured:
  docs/perf.md "Outside the bound"): dense APU writes cost about 1.05 M
  Ir per 1,000 writes (real players do 70-414 per frame); demos with
  tens of mid-mode-3 PPU writes per line are partly reachable (about
  +2.7 M Ir/frame at 16 per line, estimate). Cheap options are listed in
  the plan; none is implemented. Measured at 472 MHz (Stage 2 M0): storm-ppu, storm-apu,
  storm-div and storm-spsw exceed a frame; event-storm is 25.7% max.
- **Hardware captures that would settle things:** see
  `docs/reference/unpinned.md` (48+ rows). Most likely wrong: STAT during
  OAM DMA, the speed-switch IME-on timer offset (now AGE, 4 T behind
  SameBoy), WX=0 window grid (ADR 0034), HDMA re-arm in HBlank, the APU
  double-speed trigger delay phase.

Open (M10, session 17):
- **Hardware capture that settles ADR 0034** (WX=0 window grid): CGB,
  WX=0, WY=0, each SCX&7 in 1..=7, window tiles with distinguishable
  columns: the window pixel at screen x=0 is column 7, 7+f, or 8+f.
- **Cortex-M7 cycle proxy:** the board now measures cycles. u32 clocks (B4),
  dot-stepped mode 3 in the core (E1) and the CPU fetch rework (E5) still
  wait for Stage 2.
- **Gate edit made on the retire decision:** the byte-wide ROM load step
  was removed from `gate-stage1.sh` and `check-rom-loads.sh` deleted
  (6d8434f); say if a replacement check is wanted.
- FYI: Shantae (heavy HDMA) is worth a replay after cab6e24 (HBlank HDMA
  trigger fix); no evidence it was affected.

Open (M10, session 16):
- **HDMA cancel with a non-zero value while a block is already triggered**
  (A4-2, 6094acb): we now copy that one block (gambatte); SameBoy master
  copies value+1 blocks (what we did before). The CGB captures only cancel
  with 00. Single-source; a hardware capture would settle it.
- **Filler-header homebrew** (ADR 0031): a ROM-only image whose header byte
  0147 is FF/FE (padding) is now refused as HuC1/HuC3 (after those mappers
  land it will load as them). SameBoy and real no-MBC boards ignore 0147.
- **Audio aliasing** (A3-1): fix candidates roughly double the channel and
  mixing cost; will come back with a measurement before changing anything.

Open (earlier sessions):
- **Stage 3 ROM memory (hardware repo, human-owned):** independent review
  `docs/reference/stage3-rom-memory-options.md` (session 12) recommends
  3.0 V HyperRAM on OCTOSPI (as on the STM32H735G-DK) over the ADR 0001 FMC
  SDRAM, medium confidence, pending a DK measurement; revisiting ADR 0001 is
  the human's call. Also found: the ADR 0002 pin draft puts SDNE0 on PC2_C,
  which adds 4.5 ns (DS13312 Table 76 note 2, checked) and breaks 95 MHz
  SDRAM timing if SDRAM stays.
- **Real-game checklist** (plan, bottom; M9 task), on native Windows.
  Session 14: Wario Land 3 and Shantae played (look and sound right);
  Ctrl+C and Esc saves, `.sav` across restart and the audio device switch
  all passed. Open: smoothness vs SameBoy (desktop client), a
  gamepad, Pokémon Crystal (RTC across a restart), Alone in the Dark, Link's
  Awakening DX, and Pokémon Red (DMG compat palette; not in the plan table).
- Optional, strongest guard against a repeat of the BSOD: set
  `guiApplications=false` in `%UserProfile%\.wslconfig` (disables WSLg).
- FYI: `wsl-pro.service` restarts every 2 s because a cmd.exe AutoRun entry
  points at a missing `E:\Tools\ansicon\x64\ansicon`; harmless noise.
- Optional: disable or schedule WSL auto-updates (Windows Update shut WSL down
  mid-session on 2026-11-22).

Open: `../gbc-32-hardware/docs/firmware-interface.md` says the audio rate is "decided in firmware M6"; ADR 0021 fixed it at 32,768 Hz (agents may not edit that repo).

Decided in session 23 (2026-11-25), human: the NUCLEO taking ROMs only up
to 8 MiB - 256 B (M2 review H-6; Stage 3 loads into >= 8 MiB HyperRAM, so
unaffected): "that doesnt matter then for the nucleo prototype". No
answer came to the M3-before-M2 question; M2 was done first as planned.

Decided in session 18 (2026-11-24), human ("Go with your recommendation
for each", to the three questions put to them):
- Speed switch on one model (A1-06): the ADR 0020 change is approved.
  Land `m10-spsw-div-code` (AGE model for two-byte STOPs: DIV resets
  after STOP's last byte; `Timer::stop_edge` and the IME-off 4 T hold
  go), re-applied onto the `bus/` layout, and amend ADR 0020.
- Audio aliasing (A3-1): implement box 64 + 8-tap FIR (about -41 dBc in
  1-8 kHz for about +10% worst-case Ir, +250 B, +24 B RAM, estimated);
  the bench baseline is re-recorded with it.
- Partly fixed mediums (A2-02 latch as one register copy, E4 `LAND`
  table, D4 and D6 rests): carried over as plan tasks; they do not block
  `stage1-m10`. (The agent's recommendation, which the question left
  open; say if these should be finished first.)
- `--long` frame check ("Do the first one"): count expected frames from
  the first completed frame and drop the +-1 tolerance (d017dd5; the
  reported clock overshoots a boundary by up to 23 clocks, so each frame
  is checked within 23 clocks of its due clock, with no drift).

Decided in session 16 (2026-11-24, M10 review), human:
- `stage1-m9` tagged at 49c0756 on the human's instruction ("Tag m9"); the
  M10 whole-tree review stands in for the M9 milestone review. Open M9
  plan tasks (M8 review lows, real-game checklist) carry over.
- Budget reform ("Reform"): keep the ITCM hot-set fit, the hot-function
  caps and the relative +3% Ir guard as gates; drop the `Hw` size cap;
  total `.text` and cold-function caps become reported numbers, not gates.
  State parked in the wrong struct because of the caps moves back.
- Release profile pinned to `codegen-units = 1` ("Pin to 1"): one
  re-baseline of bench, size and ITCM lists, numbers in the commit.
- `synth:worst-case` gains a serviced timer interrupt (IE=timer + RETI
  handler) once the unobservable timer/serial events are gone ("Add
  serviced IRQ"); baseline re-recorded. The absolute 4.02 M line stays
  until a real M7 measurement.
- STOP mode as implemented (ADR 0020 amendment, 6f228f9), including the
  APU channels holding their tone while stopped (SameBoy CGB path):
  "Thats fine for stop."
- Size caps must be the ones valid for the final MCU (STM32H735IGK6: ITCM
  configurable to 128/192/256 KiB, 32 KiB caches), not the dev board; a
  slower dev board (H753, Stage 2) is acceptable, it is a temporary step.
  Per-function caps and the 57,344 B ITCM hot set become a dev-board
  report; the product gate is total gbc-core `.text` against the H735 ITCM
  (branch `m10-gates-h735`).
- Stage 3 ROM memory is HyperRAM on OCTOSPI, not FMC SDRAM (the human
  referred to it as decided; hardware ADR 0001 not yet updated). ADR 0027
  (byte-wide ROM reads, for the FMC erratum) stays until retired.
- Permission given to edit `acceptance/README.md` item 5 (now: only total
  `.text` gates) and the hardware repo docs for HyperRAM/H735 facts. Done:
  hardware ADR 0003 (HyperRAM on OCTOSPI, supersedes 0001), ADR 0001 status
  line, `firmware-interface.md`, `requirements.md`: left UNCOMMITTED in
  `../gbc-32-hardware` for the human to review and commit.
- Use the hidden LLVM option `-C llvm-args=-cgp-max-address-users-to-scan=100000`
  in every build (bench, size check, firmware): "Sure go ahead and use
  that flag". It lets LLVM fold the hot-struct address in `Cpu::run`
  (measured on the cpu-loop branch: cpu_instrs -18.2% vs -12.3% without,
  thumb `.text` smaller). Tied to the pinned toolchain: re-check on any
  toolchain bump.
- Retire ADR 0027 (byte-wide ROM reads): "Yes retire that ADR". Work
  (done).
- Mappers ("Add HuC1 + HuC3"): Stage 1 adds HuC1 and HuC3 (ADR 0016 scope
  change) on top of a safe failure for every other unsupported mapper
  (caller-visible, no RAM). MBC7 and the rest stay out.

Decided in session 15 (2026-11-23), human:
- gbc-core total `.text` budget 97,500 -> 99,000 B ("Raise .text to
  99,000, try") for the last 3 Mealybug ROMs; leftovers come back for
  sign-off (c6c4ea7).
- SameSuite `channel_{1,2}_volume`, `channel_4_align`,
  `channel_1_sweep_restart`: "Sign off all 4" as CGB E results (each
  failing cell is SameBoy's CGB C value). Moved into
  `same-suite-apu-cgbE` (0be5600). FYI: 4 earlier-signed-off cgbE ROMs
  (`channel_4_lfsr_7_15`, `_lfsr_restart{,_fast}`,
  `_frequency_alignment`) now pass on our CGB C model; they stay in that
  group (moving them back would be a manifest change: stricter, the
  human's call).

Decided in session 14 (2026-11-23), human:

- SameSuite `channel_1_freq_change_timing-cgb0BC`: sign off ("Sign off
  target"): it needs the CGB C PCM12 read glitch; a model fitted to it broke
  14 passing SameSuite ROMs that expect CGB E reads. Own stretch group
  `same-suite-apu-pcm-glitch` (22fe162). The frequency-timing half landed
  (91ff0de).
- Mealybug `m3_lcdc_win_en_change_multiple_wx`, `m3_wx_{4,5,6}_change`,
  `win_without_bg`: sign off, no CGB C reference upstream ("yes do #1").
  Own group `mealybug-no-cgb-reference` (d801a6c), still run.
- Desktop: no colour correction; raw colours stay ("dont do that color
  correction"). Copied SameBoy (R) glyph bytes with notice: "fine".
- Display sync (vsync, would supersede ADR 0024's audio-driven pacing):
  "Decide after stall fix".

Decided in session 13 (2026-11-23), human:
- cgb-acid2 bench at +3.37% after the STAT 144/153/0 correctness fixes
  (perf pass recovered what it could, fingerprint identical; worst-case
  4.007 M Ir/frame stays under the 4.02 M line): **rebaseline**
  `perf/bench-baseline.txt` ("Rebaseline (Recommended)"), over raising the
  `point_irq` budget, raising the `Hw` size limit, or backing fixes out.

Decided in session 12 (2026-11-23), human:
- SameSuite group 15: sign off the 9 E-only ROMs (ch1/2 `nrx2_glitch`,
  `nrx2_speed_change`; ch4 `lfsr_7_15`, `lfsr_restart{,_fast}`,
  `freq_change`, `frequency_alignment`) as "CGB E result". "Sign off the
  9". Moved into stretch group `same-suite-apu-cgbE` (cde49e4, baseline
  303bc6d). The other 8 stay fix targets.

Decided in session 11 (2026-11-23), human:
- APU double speed: follow gambatte's CGB C linear trigger rule; sign off
  SameSuite `channel_{1,2}_align{,_cpu}` (4) as "CGB E result" (SameSuite
  apu/README: CPU-CGB-C fails most ch1/2 tests). "Yes". Manifest change
  approved: exclude them from `same-suite-apu`, own stretch group
  `same-suite-apu-cgbE` (keeps running, reported), baseline refresh with
  `--allow-removed`.

- Power-on OBJ palette RAM: sign off gambatte `scx_during_m3_spx2`,
  `scx_during_m3_spx2_ds`, `scx_attrib_during_m3_spx2_ds` as "depends on
  uninitialised palette RAM" (they draw OBJ palette 0 colour 2, never
  written; the cgb04c capture shows that console's power-on 0x21926C; we
  start at 0, deterministic). "Sign off". Manifest change approved: own
  stretch group (keeps running, reported), baseline with `--allow-removed`.

Decided in session 10 (2026-11-23), human:
- gbc-core total `.text` budget 96,000 -> 97,500 B (for mid-line PPU
  steps E/F/G and the M9 fixes; per-function budgets unchanged). "That's
  fine."
- OAM DMA CPU writes during a conflicting transfer (M9 triage groups 2 and
  13, ~65 ROMs): follow gambatte's CGB C hardware captures over SameBoy's
  rule. "gambatte is fine."
- M9 sign-off: the 7 SameSuite APU ROMs that target other CGB revisions
  (`channel_{1,2,4}_extra_length_clocking-cgb0B`,
  `channel_3_extra_length_clocking-cgb0`, `-cgbB`,
  `channel_1_freq_change_timing-A`, `-cgbDE`) are signed off as "targets
  another CGB revision" (ADR 0003: we emulate CPU CGB C). They stay in the
  manifest and keep running; they don't block Stage 1. "Sign off on the 7."
- The 50% bar covers realistic worst cases; dense mid-mode-3 register
  writes (many per line) are outside it, documented in docs/perf.md
  "Outside the bound" with a Stage 2 measurement task. "Yes that is fine."
- `synth:worst-case` gains one mid-line SCX write per line so the
  held-fetch slow path counts against the 4.02 M line (stricter; moves the
  bench baseline). "Do the other thing." Queued after the OAM DMA and
  window-timing work lands.

Decided in session 08 (2026-11-22), human:
- APU perf budget. The exact optimizations brought the APU to 224.1k; the
  human accepted that over the output-changing fallbacks ("Accept, budget
  230k"). After the perf review showed the per-write cost, the budget was
  split ("Split it"): 230k Ir/frame for channels and mixing, plus <= 950
  Ir per APU register write. It stays recorded as over the original 200k
  estimate and is an optimization target (human request; plan M8).
- Total gbc-core `.text` budget = 96,000 B (was 85,100), from the measured full-APU estimate 94,336 B + margin. Per-function budgets stay. Over it later: come back with numbers.

Decided in session 07 (2026-11-22), human: Stage 1 is done only when every
manifest test ROM passes, except groups the human signs off with a recorded
reason (README "Done when", plan M9). Both manifest drafts approved in
advance ("You have my ok") and committed after validation matched
(8543723: PNG-reference group, 198 ROMs; undef_ops lockup opt-in group).

Decided in session 07 (2026-11-22), human, after the M5 test-integrity
review (medium: the lockup rule covered all 3,154 gambatte ROMs, broader
than "those specific tests"): "Narrow to undef_ops". Plan: an explicit
per-group manifest opt-in (e.g. `lockup = "run_on"`), the 10
`gambatte/undef_ops` ROMs in their own group with it, every other group
treats a lockup as `crash` again; contract text + ADR 0018 amendment.
Manifest change drafted for the human's OK; re-review (runner pass/fail
logic changes again).

Decided in session 07 (2026-11-22), human: "Both those sound fine" to
(a) a new stretch-tier group selecting exactly the Gambatte ROMs with a
sibling `_cgb04c.png` (198 ROMs whose names lack `cgb04c`, so
`gambatte-cgb` never selects them; a directory glob would also take ROMs
with no CGB rule). The agent drafts it, the human looks it over before
commit. (b) The total `.text` budget (85,100 B; 84,942 used after M5)
will be raised once for M6, to a figure measured after the first APU
channel lands: come back to the human with the number. Per-function
budgets stay.

Also decided in session 07 (2026-11-22): STOP is 2 bytes unless IE&IF is
pending (SameBoy `stop()`, which encodes the Pan Docs flowchart). The
human first chose "Keep pending cases only", but no SST 0x10 case sets
IF, so that kept 0 of 1000. The human then chose: run all 1000 0x10
cases and compare everything except PC and the bus-cycle list
(registers, flags, SP, IME, RAM), where SST's 1-byte expectation
contradicts hardware. The 5 SameBoy STOP branches have dedicated,
mutation-checked unit tests (ADR 0020). The human also approved loosening the
lockup rule for `gambatte/undef_ops` ("Those specific tests should
probably have looser restrictions then right? If the whole point is to
crash."). Scope chosen: for the `gambatte` check only, an invalid-opcode
CPU lockup is not `crash`; the machine runs on to `run_frames` and the
screen check decides (as upstream testrunner). Panics, stalled time and
lockups elsewhere stay `crash`. The romtest `gambatte`
check may re-encode the hex-digit glyphs clean-room (read GPL
testrunner.cpp for the rules only; own data; ADR 0018). Decided in
session 06 (2026-11-22): the mbc3-tester reference
uses a counted, contract-constrained in-memory recolor (ADR 0007
amendment; a recolor may only relabel a color absent from the reference);
mbc3-tester `run_frames` 41; ADR 0015 extends ADR 0006 and doesn't
change it. Any new `recolor` or manifest change still needs the human.

FYI (session 05): the human saw "a lot of compile errors" (session 05).
build, clippy `-D warnings`, tests, the thumbv7em core build and the
nightly fuzz crate are all clean with the pinned toolchain, so this is
editor-side (likely rust-analyzer using a different toolchain, e.g. a
Windows editor on WSL files, or loading `tools/fuzz/fuzz`, which needs
nightly). Waiting on an example error if it persists.

Context on earlier human decisions:
- Acceptance screenshot fixes: a counted `recolor` on the pinned upstream
  PNG, never a committed replacement image (session 06, ADR 0007).
- `acceptance/test-roms.sha256` is human-owned.
- Strictly-tightening gate/contract changes and factual README fixes are
  agent decisions.
- README quality bar: the core uses at most 50% of an H753 at 480 MHz in
  the worst case.
- Unsafe-free core (ADR 0009).
- The project is MIT-licensed. AGENTS.md has a rule on dependency licenses
  and on not copying code from copyleft emulators.
- Token cost (session 05): agents must be small, one behavior each.
- Remote `origin` = `git@github.com:jschneider1207/gbc-32.git`. The human
  pushes; agents never do.


## Environment

- **Rust:** via rustup (user-level, `~/.rustup` and `~/.cargo`).
  - `cargo` is **not on PATH in non-interactive shells**: prefix commands
    with `source ~/.cargo/env &&`.
  - The toolchain is pinned to 1.100.0. Nightly is installed, but only for
    cargo-fuzz (`tools/fuzz/fuzz/`, outside the workspace; see
    `docs/fuzzing.md`).
- **valgrind** 3.22.0. Benchmark Ir counts are tied to it and the pinned
  toolchain; a change to either is a rebaseline commit.
- **SM83 single-step data:** `scripts/fetch-sm83-tests.sh` →
  `test-data/sm83/`. Set `GBC_REQUIRE_TEST_DATA=1` locally.
- **Desktop system libraries:** installed. Windows target `x86_64-pc-windows-gnu` added to rustup (user-level, session 09) for `cargo check`. WSL2 Ubuntu 24.04, no `jq`.
  Scripts are bash or Elixir, not Python.
- **Real-game ROMs (human):** `X:\gbc-roms` = `/mnt/x/gbc-roms` (all 6 checklist games). Read-only for agents: copy to a scratch dir before any run, never write `.sav` files there, never commit ROMs.
- **Datasheets:** `../gbc-32-hardware/datasheets/` (read-only for agents).
- **Commits:** stage by explicit path only (AGENTS.md Git). Other agents
  and the human commit concurrently.
- **Worktrees:** never point a worktree at another tree's `target/` (`CARGO_TARGET_DIR`): cargo then reuses stale binaries (session 07). Agents in `git worktree`s must not symlink `test-roms`
  (a deny rule blocks removing it); use `romtest --roms-dir`,
  `bench --root`, `TEST_ROMS=` for check-bench. Agent worktrees are
  "locked": removal needs `--force --force` (ask the human). Cherry-picked
  branches need `git branch -D` after `git cherry` shows all `-`.
- **romtest filters** match ROM paths, not group names (e.g.
  `--filter 'mooneye-test-suite/acceptance/**'`).
- **Gate:** `scripts/gate-*.sh` edits prompt the human (`.claude/settings.json`).

## Blockers

None.

## Acceptance snapshot

Baseline 6def429 (`acceptance/results/stage1.json`, session 15; session 14
ended at 3,410; M8 tag `stage1-m8` was stretch 3,126). `scripts/gate-stage1.sh`
passed at 4f32363 (session 15).

| Tier | Pass | Total | Notes |
|---|---|---|---|
| must | 86 | 86 | |
| should | 40 | 40 | |
| stretch | 3,437 | 3,461 | 24 fail, 5 no-reference. Signed off (still run, counted as fail where failing): 7 other-revision SameSuite, 13 failing of 17 in `same-suite-apu-cgbE`, 1 in `same-suite-apu-pcm-glitch`, 3 in `gambatte-uninit-palram`, 5 no-reference in `mealybug-no-cgb-reference`. 0 open fix targets |
