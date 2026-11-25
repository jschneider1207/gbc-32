# 0020: CGB speed switch: a STOP-driven halt after SameBoy, one clock short to single speed

- Status: Accepted
- Date: 2026-11-22

## Context
A CGB speed switch is a STOP with KEY1 bit 0 set (Pan Docs: FF4D - KEY1).
The old model idled 2050 M-cycles at the new speed with the timer frozen.
Gambatte's cgb04c hardware tests (`speedchange`, `dma/*speedchange*`,
`lcd_offset`, `lycEnable/*lcdoffset*`) and the AGE `speed-switch` tests
need the length and the CPU/PPU phase after the switch to the clock.

References, read for behavior:
- SameBoy (MIT) `Core/sm83_cpu.c` `stop()`, `enter_stop_mode()`,
  `leave_stop_mode()` and `Core/timing.c` `GB_advance_cycles()`
  (`speed_switch_halt_countdown = 0x20008`, `speed_switch_freeze = 5`,
  `speed_switch_countdown = 6` to double speed, `div_cycles = -4` with
  IME off).
- Gambatte (GPL; behavior only) `Memory::stop()`, `LCD::speedChange()`,
  `PPU::speedChange()`: the CPU resumes 0x20004 cycles after the start of
  STOP's second byte; to single speed the PPU switches at that point and
  gains a clock (`p_.now -= isDoubleSpeed()`).

## Decision
`Cpu::stop` asks the bus what STOP does (`Bus::stop_begin` ->
`StopKind`), then for a switch calls `Bus::speed_switch(ime, pending)`:

- A button held on a selected P1 line: no DIV reset, no switch; with
  nothing pending the CPU halts (plain HALT).
- Otherwise DIV resets at the end of STOP's fetch. *(Superseded 2026-11-24: see the last amendment, "Speed switch and STOP follow AGE for two-byte STOPs".)* With nothing pending
  (IE & IF = 0) STOP's second byte is read (one M-cycle at the old speed),
  then the speed toggles and the CPU enters `Mode::SpeedSwitch`: a HALT
  that also ends when `Bus::speed_switch_over` (its countdown) is reached,
  then the usual wake-up M-cycle and the fetch. An interrupt ends it early.
  OAM DMA is paused through it and retimed to the new M-cycle length
  (`dma_retime`).
- Halt length, from the end of the second byte to the next fetch:
  - to double speed, 0x8001 double-speed M-cycles (0x10002 clocks). Same as
    SameBoy (0x20008 T-cycles, 5 frozen, 1 at single speed via the
    6-cycle countdown) and Gambatte (0x10006 dots from the second byte's
    start).
  - to single speed, 0x8001 single-speed M-cycles **less one clock**
    (0x20003 clocks). SameBoy: 0x20008 - 5 frozen T-cycles, doubled for the
    PPU = 0x20003 dots. Gambatte: 0x20004 dots after the second byte's
    start, the second byte now at single speed, plus the PPU's extra clock,
    against our 2-clock double-speed second byte. The halt idles its 3 odd
    clocks first, in `Ctx::speed_switch` (after `vdma_halt`, before the
    input, serial and timer deadlines are rescheduled), then 0x7FFF
    M-cycles. So the CPU's M-cycle grid moves by one clock against the
    PPU's: this is how hardware gets the "LCD offsets" 1 and 3 that the
    Gambatte `lcd_offset` tests name. The timer counts that clock
    (`Timer::speed_switch` with pause -1), so it stays on the CPU's grid.
- The timer runs through the halt (SameBoy). With IME off it starts 4
  T-cycles late (the held DIV reset). *(Superseded 2026-11-24: see the last amendment, "Speed switch and STOP follow AGE for two-byte STOPs".)* With an interrupt pending there is
  no halt, and to double speed the CPU idles one M-cycle the timer doesn't
  count.
- Plain STOP (no switch) follows the same length rule (see "STOP length")
  and then stops until a button press (a stub until the STOP-accuracy
  task).

## STOP length
Source: SameBoy `Core/sm83_cpu.c` `stop()`, which encodes the Pan Docs
"Using the STOP instruction" flowchart (same author). With
`pending = IE & IF & 1F != 0`, STOP is two bytes when nothing is pending
and one byte when something is; the second byte is read (one M-cycle)
before any pause. This holds on every path: button held (no DIV reset;
halt if nothing pending), speed switch, and plain STOP mode. With one
byte, the byte after STOP runs as the next opcode.

SingleStepTests (`tools/sm83-tests`) expects STOP to be one byte in every
case, and none of its 1000 `10.json` cases sets IE or IF (measured), so
all have nothing pending and the core's PC is +2 with a read of pc+1 where
SST has an idle cycle. The human's decision (2026-11-22, superseding "keep
pending cases only", which kept 0 and skipped 1000): the harness runs all
1000 cases and compares everything except PC and the bus-cycle list, the
two things the length rule changes by definition. Registers A-L, F, SP,
IME, EI and the final RAM are still compared, so STOP clobbering state
fails. The exemption is pinned in the harness (file `10`, 1000 cases
exempt; every other file 0 exempt), so it can't silently grow. Core unit
tests still cover the length rule itself (one per SameBoy branch).

## Consequences
- Measured against main (ce1ac76), full stage1 manifest: gambatte
  speedchange 111 -> 178/242, gambatte dma 157 -> 165/229, lycEnable
  126 -> 130/147, age-cgb 12 -> 14/21; 105 ROMs fail -> pass, 25 pass ->
  fail.
- The 25 that fail are the 13 `dma/*speedchange*` and
  `oamdma_late_speedchange_stat_1` ROMs that the first SameBoy port
  already lost, plus `lcd_offset`/`m2enable` ROMs that read LY, LYC/m1
  interrupts or mode 2 at odd offsets. Main passed those by accident (its
  2050-M-cycle pause left an even offset). The PPU's sub-M-cycle LY, LYC and
  m1/m2 thresholds were fitted with the CPU grid at one phase; odd phases
  are new to them. Halt lengths of 2 or 3 clocks short (even offsets) fix
  those ROMs but lose 26-32 others, so the PPU thresholds are the place
  to fix them, not this halt.
- SameBoy's "odd mode" (`double_speed_alignment & 7`, a 2-cycle freeze in
  the pending case) is not modelled; the pending case to single speed keeps
  whole M-cycles.

## Amendment (2026-11-23, session 10): frame-sequencer delay after switching to double speed

Each switch to double speed toggles a 2-clock (one double-speed M-cycle)
delay on the DIV-APU frame-sequencer edge: the 1st and 3rd switches add it,
the 2nd removes it. It survives DIV writes and the STOP DIV reset, is
cleared by NR52 power off/on, and is ignored in single speed (0a83ae5).
Sources: AGE `spsw-ch2-lc-delay` (`TEST_DS`, `TEST_DS_NS_DS`,
`TEST_DS_NS_DS_NS_DS`, `TEST_DS_OFF_ON`, verified on CGB B/C/E) and the AGE
emulator's `gb_sound::after_speed_change` (Apache-2.0, read only). SameBoy
does not model it. AGE also re-applies it after a single-speed DIV write;
no checked test covers that, so we don't. Gains `spsw-ch2-lc-delay-cgbBCE`
and Gambatte `speedchange{,2,5}_ch2_nr52_*` (6).

## Amendment (2026-11-23, session 10): LY read timing the switch alignments expose

The CPU/PPU alignments left by repeated switches put LY reads on dots the
default alignment never samples. AGE `lcd-align-ly` (verified on CGB B/C)
fixes two rules there (d2b6e56, `Ppu::ly`):
- LY changes 3 dots into every line 1-153 (lines 144-153 were at dot 2);
  153 reads for 4 dots (5 in double speed), then 0.
- For the 2 dots (1 in double speed) before LY changes it reads
  LY & (LY + 1), the counter mid-carry (the test's "glitch" rows).
Source for both: AGE `gb_lcd::read_ly` (Apache-2.0, read only), whose PPU
sits one dot later than ours in double speed (it moves the LCD at the
switch to double speed, we move the CPU grid at the switch back), so its
dot 454/453 double-speed thresholds map onto our dots 3/2. Gains
`lcd-align-ly-cgbBC` and Gambatte `lcd_offset/offset1_lyc98int_ly_count_{1,2}`,
`offset2_lyc98int_ly_count_2`; no regressions (full stage1 manifest).

Open: `spsw-tima-cgbBC` still fails. Rows: the 262 kHz TIMA read right
after a switch to double speed with no delay (03, want 04), and every
262 kHz and 65 kHz "immediate increment" row after a switch to single speed
(one count low; the 16 and 4 kHz rows pass). Making the timer count 4 or 8
more clocks to single speed fixes none of them and breaks the 4 kHz rows,
so it isn't a plain pause-length error. AGE's model to compare against:
DIV resets after STOP's second byte (ours: after its fetch, the timer
frozen through the byte), the halt is 0x8000 new-speed M-cycles with the
timer running, and on CGB B/C a DIV reset during STOP that would increment
TIMA immediately doesn't increment on that M-cycle, for the 4 kHz timer only
(`gb_timer::after_div_reset`, "speed change glitch").

## Amendment (2026-11-23, session 10): the TIMA edge of STOP's DIV reset

Resolves the `spsw-tima-cgbBC` question above. Our STOP resets DIV at the
end of its fetch; AGE resets it one M-cycle later, after the second byte
(`gb_bus::execute_stop`). Instrumenting every test row gives the counter
at our reset as c = 4 * (17 + DELAY) (4 per M-cycle at either speed), and
the post-switch phase already agrees with AGE (every non-edge cell passed).
So the only difference is which edge the reset itself makes, with timer
input bit b (8, 32, 128, 512):
- AGE: a fall during the second byte (c mod 2b = 2b - 4) plus an
  immediate increment if the bit is set at c + 4, except on CGB B/C for
  4 kHz when the bit rose on exactly that M-cycle ("speed change glitch",
  `gb_timer::after_div_reset`: (c + 4) mod 2b = b).
- Ours: an immediate increment if the bit is set at c.
These agree except at c mod 2b = b - 4 (the bit rises during the second
byte and falls at AGE's reset) for 262/65/16 kHz. That is every failing
cell: the 262 kHz read after a switch to double speed (c = 68, 68 mod 16
= 4), 262 kHz DELAY 4 (c = 84), 65 kHz DELAY 6 (c = 92), and 16 kHz
DELAY 14 (c = 124), in both directions; the 4 kHz rows pass because the
glitch cancels the same case. (The 16 kHz edge cells failed too; the
earlier note that they passed was a misread.)

*(This rule is superseded 2026-11-24: see the last amendment. The reset
itself is now where AGE has it, and only the 4 kHz carve-out remains.)*

Rule (`Timer::stop_edge`, called by `Ctx::stop_div_reset` before the
reset when STOP is two bytes, i.e. IE & IF = 0): if the timer runs at
262, 65 or 16 kHz and c mod 2b = b - 4, TIMA increments. The pending
(one-byte) STOP has no second byte and is unchanged. The eager reference
model of the ADR 0011 differential test mirrors it. CGB E (glitch on all
but 262 kHz) is not modelled. Gains `spsw-tima-cgbBC` and 14 Gambatte
`speedchange*_tima0[123]*` ROMs, no regressions (full stage1 manifest).

## Amendment (2026-11-23): the held DIV reset with an interrupt pending (CGB B/C)

With an interrupt pending at a speed-switch STOP (one byte, no halt) and
IME on, the timer now starts 4 T-cycles (one M-cycle at the new speed)
late, the same hold as with IME off: `Ctx::speed_switch` freezes for
`!ime || pending`, was `!ime`. In both directions the first counted
M-cycle after the DIV reset is now the dispatch's first M-cycle.

Source: AGE `spsw-interrupts` (`src/speed-switch/caution/`, verified on
CPU CGB B and C, `OFS_B = 1`; the `cgbE` build, verified on CGB E, uses
`OFS_B = 0`). Its "immediate interrupt" rows (`ld [rIF],a; ld [rIE],a;
...; ei; stop`) read DIV and the 16 kHz TIMA a fixed delay after the
dispatch; the edges sit one M-cycle later on CGB B/C than on CGB E, at
either speed. Before this change the core produced exactly the CGB E
values (SameBoy's model; SameBoy's default model is CGB E and its
`enter_stop_mode` holds the reset only with IME off, marked "I don't
trust this if"). The test's other rows (a timer interrupt ending the
post-switch halt early, IME on, nothing pending at the STOP) already
passed: the halt is interruptible as decided above. Pan Docs' STOP chart
calls the pending + IME-off case a non-deterministic CPU glitch, so the
existing IME-off hold there is unchanged and not stacked with this one.

The timer differential test's reference mirrors the hold (one more
M-cycle that doesn't tick, an idle as the dispatch's on hardware). Gains
`spsw-interrupts-cgbBC`; no other flips (full stage1 manifest).

## Amendment (2026-11-23): OAM DMA after the speed-switch halt

OAM DMA paused by the switch resumes at the end of the wake-up M-cycle,
one M-cycle later than after HALT (`Ctx::speed_switch_end`). gambatte CGB
C `oamdma/oamdmasrcC0_speedchange_readC000` and
`dma/hdma_transition_speedchange_oamdma` read WRAM mid-transfer right
after a switch to double speed and see it one byte earlier than the HALT
rule gives. The switch to single speed is untested.

## Amendment (2026-11-24): the order of operations in `Ctx::speed_switch`

Supersedes the order given under "Decision" ("the halt idles its 3 odd
clocks first ... before the input, serial and timer deadlines are
rescheduled"). The speed is an input to every lazily evaluated component
and to every deadline, so the switch now goes in four steps:

1. Everything lazy is brought up to the switch clock under the **old**
   speed's rules (the PPU, OAM DMA, the timer's reloads).
2. The speed changes (the M-cycle length, the counter rate, the PPU's
   walk, a paused OAM DMA's M-cycle length).
3. Every speed-dependent deadline is set again under the **new** speed
   (Ppu, DivApu, Input, Timer, Serial).
4. Only then does time pass and the event loop run: to single speed, the
   halt's 3 odd clocks. What is due in them runs at their end, the first
   boundary of the moved grid.

The halt lengths, the grid move and the timer's holds are unchanged. What
changes is what the event loop sees in step 4 (M10 review A1-02). Before,
it ran with the deadlines of the old speed:
- A fast-clock serial transfer in flight across a switch to single speed
  with the DIV reset held (IME off) shifted the same bit clock edge twice:
  the old deadline fired at the end of the 3 clocks, where the frozen
  counter hadn't reached the edge, and the handler re-armed for the same
  edge 4 clocks on. The byte completed one bit (16 clocks) early.
- A scheduled button change 5 or 6 clocks after the switch, inside the
  first M-cycle of the moved grid, landed one M-cycle late (its old
  deadline was past that M-cycle's start; ADR 0012 rounds down).
- The DivApu deadline was set without rescheduling the countdown.

Tests: `serial_bit_in_flight_across_a_switch_to_single_speed_shifts_once`,
`scheduled_input_in_the_first_mcycle_after_a_switch_lands_there`.

The PPU's part of step 1 (M10 review E8): `Ppu::sync` runs an update point
with the speed of the sync that reaches it, and only the points that raise
an interrupt or end a line or the frame are deadlines. The switch re-sought
the walk from the last sync, so points between that sync and the switch ran
later under the new speed's rules, and the result depended on where the
last sync happened to be. Reproduced: WX = 2, WY written to LY at dot 87
of a line (mode 3, before the pipeline start at 89) and STOP right after.
The pipeline start decides that write with `wy_lead`, 5 dots in single
speed (too late: the window starts on the next line), 1 in double. It was
decided 8 dots later with the double-speed lead and the window was drawn on
the WY line. Now the PPU is synced to the switch clock before the speed
changes. Test: `speed_switch_syncs_the_ppu_with_the_old_speed_first` (the
frame equals the one with two NOPs in place of the STOP).

## Amendment (2026-11-24): STOP mode

Supersedes "then stops until a button press (a stub until the
STOP-accuracy task)" under "Decision". Plain STOP (no button held on a
selected line, KEY1 not armed, or DMG compatibility mode) now enters a
real STOP mode. M10 review A1-01; the plan's "STOP accuracy", "Screen
blank while in STOP mode" and M5 HDMA/OAM DMA items. No ROM in the
manifest enters STOP mode and Pan Docs says no licensed game does, so
unit tests are the only arbiters.

References: Pan Docs "Using the STOP Instruction" (and its chart, already
encoded by "STOP length" above); SameBoy (MIT) `Core/sm83_cpu.c`
`enter_stop_mode`, `leave_stop_mode`, the `stopped` branch of
`GB_cpu_run`; `Core/timing.c` `timers_run`, `GB_advance_cycles`;
`Core/display.c` `GB_display_run`, `render_pixel_if_possible`;
`Core/memory.c` `GB_dma_run`. The model is a CGB in both CGB mode and DMG
compatibility mode (ADR 0003); where SameBoy separates DMG from CGB, the
CGB side is taken.

*(The entry point of the counter's stop, and which reload counts as "in
flight at the reset", are superseded 2026-11-24: see the last amendment.)*

**Entry** (`Bus::stop_begin` -> `Ctx::stop_freeze`, at the end of STOP's
fetch; then `Bus::stop_enter` after the second byte, if read). Which STOP
enters it, its length and the DIV reset are unchanged (the chart). It is
entered with an interrupt pending too (one byte).

**What stops:**
- The system counter, from the DIV reset on: DIV reads 00, TIMA holds, no
  timer interrupt (`Timer::freeze`; SameBoy `timers_run` returns while
  `stopped`). Everything clocked from the counter stops with it: the
  serial bit clock (a transfer in flight waits) and the APU frame
  sequencer (lengths, envelopes and the sweep don't step).
- A TIMA reload in flight at the reset (cycle A) is held: TIMA reads 00
  and the reload and its interrupt come one M-cycle after the wake-up. A
  choice, the same as across the speed-switch pause; no reference covers
  it.
- OAM DMA pauses and HBlanks start no VRAM DMA block, with HALT's
  machinery (`dma_pause`, `VramDma::halted`; SameBoy `GB_dma_run` returns
  while `stopped`, `GB_display_run` starts HDMA only with
  `!gb->stopped`). A block already requested waits for the wake-up.

**What keeps running** (a CGB; SameBoy `GB_display_run`: "The PPU does not
advance while in STOP mode on the DMG"; `timers_run` still advances
`apu_cycles` on a CGB while `stopped`):
- The PPU. LY, STAT and its interrupt requests go on (IF bits 0 and 1 get
  set; they end nothing), and `FRAME_READY` keeps its rate.
- The APU's channels: a channel that is playing keeps its tone.
- The emulated clock and the cartridge RTC.
So `run_until` needs no special case: frames and samples keep their
rates and a call ends at its target, also when nothing can end the STOP.

**The screen.** Pan Docs: "On CGB, leaving the LCD enabled when invoking
STOP will result in a black screen. Except if the LCD is in Mode 3, where
it will keep drawing the current screen." SameBoy: the PPU loses OAM, VRAM
and palette RAM unless the CPU's access to each was blocked at the STOP
(`cgb_palettes_ppu_blocked = !cgb_palettes_blocked`), and draws black
without palette RAM, in either CGB mode. Here: if palette RAM is open to
the CPU at the end of STOP's fetch (`Ppu::palette_blocked`, LCD on),
every frame completed in STOP mode is black (all pixels 0000), filled when
it completes (`Ctx::stop_frame`: one test after the stopped idle, nothing
in the renderer); otherwise frames are drawn as usual. `screen_blank`
(white, LCD off) is not involved.

**Wake-up** (`Bus::stop_halt` -> `Ctx::stop_wake`). Only a selected P1
line being low ends STOP mode (Pan Docs: "STOP is terminated by one of the
P10 to P13 lines going low"; SameBoy: `JOYP & 0xF != 0xF`, sampled every
M-cycle). The lines are all high at entry (otherwise STOP is the
button-held case) and P1 can't be written in STOP mode, so this is a
button pressed in a selected group since: the joypad interrupt's
condition, whatever IE says. With no group selected nothing ends it
(hardware: until reset). Interrupts don't end it. The stopped step skips
to the next deadline like HALT; a scheduled button change is one, so
`set_buttons_at` ends it on its M-cycle.

**Resume.** SameBoy: `leave_stop_mode`, then 8 T-cycles, then the next
`GB_cpu_run`. Here: at the boundary where the line is low the counter
starts again from the value it held (0000), OAM DMA resumes as after
HALT, and in HDMA's HBlank period after a STOP outside it a block is
requested (`allow_hdma_on_wake`); then two idle M-cycles (at either
speed); then the CPU runs: a dispatch if one is due (the press requested
the joypad interrupt), else the instruction after STOP.

**Not modelled:**
- The oscillator's start-up time. Neither Pan Docs nor GBCTR gives one
  for STOP mode; SameBoy has only the 8 T-cycles above.
- SameBoy's held DIV reset with IME off at `enter_stop_mode`
  (`div_cycles = -4`, "I don't trust this if"): the counter restarts
  exactly at the wake-up boundary in every case. (The speed switch keeps
  its verified hold.)
- Black per pixel: the frame STOP mode begins in is black from its top,
  the one it ends in is drawn from its top; and the PPU still reads real
  OAM and VRAM, so mode-3 lengths in STOP mode are those of the picture,
  not of an empty line.
- SameBoy's separate OAM and VRAM conditions (STOP in mode 2 keeps OAM):
  one condition, palette RAM, decides.
- An HDMA block requested in STOP's own M-cycles running the second byte
  as an opcode (the speed-switch STOP's `prefetched` case): it just runs
  at the wake-up.
- DMG and SGB behavior (white screen, PPU and APU stopped): not this
  model.

**Bus trait.** `stop_wakeup` is replaced by `stop_enter` and `stop_halt`
(defaults: nothing; one idle M-cycle and never woken). The trait is
public only for `tools/sm83-tests`, which uses neither; the `Emulator`
API is unchanged. `run_until`'s docs describe what a front end sees.

**Tests** (core unit tests; each failed on the old code or fails under
the named mutation of the new code):
`stop_mode_freezes_div_tima_and_the_serial_clock`,
`stop_mode_is_not_ended_by_interrupts` (mutation: IE & IF also wakes),
`stop_mode_ends_only_on_a_selected_p1_line`,
`run_until_in_stop_mode_returns_on_time_with_black_frames_and_audio`,
`stop_in_mode_3_keeps_the_picture`,
`stop_mode_resumes_two_mcycles_after_the_line_goes_low`,
`scheduled_input_wakes_stop_on_its_mcycle` (now d + 12),
`stop_mode_holds_the_frame_sequencer_but_not_the_channels`,
`the_wake_up_restarts_the_serial_clock_and_the_frame_sequencer`,
`stop_mode_holds_a_tima_reload_in_flight`,
`stop_mode_pauses_the_transfer` (OAM DMA),
`stop_mode_skips_hblanks_and_waking_in_mode_0_starts_a_block`. The timer
differential test ends its plain STOP on the same clock, which checks
that a freeze and thaw with no time between change nothing.

**Measured** (d316018 against its parent 2cc605c): full stage1 manifest,
no result changes. `diff-fingerprint.sh`, also with `--audio-buffer 7
--hash-samples`: one ROM of the corpus differs, in both of its runs,
`blargg/dmg_sound/dmg_sound.gb` (DMG-only, not in the manifest), which
executes STOP 00 at 1.07 s with P1 = FF and is in STOP mode from there.

**Open for the human:** whether the APU should go silent instead. The
review finding and the task brief assumed it stops; SameBoy's CGB path
keeps the channels running, which is what is implemented. No hardware
test decides it.

## Amendment (2026-11-24): Speed switch and STOP follow AGE for two-byte STOPs (human approved, 2026-11-24)

M10 review A1-06. Approved by the human ("Go with your recommendation",
2026-11-24, relayed by the orchestrating session).

**Supersedes** (the old text stays above, marked):
1. Decision: "Otherwise DIV resets at the end of STOP's fetch."
2. Decision: "With IME off it starts 4 T-cycles late (the held DIV reset)."
3. Amendment "the TIMA edge of STOP's DIV reset": its rule
   (`Timer::stop_edge`: "if the timer runs at 262, 65 or 16 kHz and
   c mod 2b = b - 4, TIMA increments"). Its analysis stands and is the
   reason for this change.
4. Amendment "STOP mode": "Entry (`Bus::stop_begin` -> `Ctx::stop_freeze`,
   at the end of STOP's fetch ...)" for the counter, and which TIMA reload
   is "in flight at the reset".

**Decision.**
- STOP resets DIV **after its last byte**: after the second byte when
  nothing is pending (IE & IF = 0), at the end of the fetch when STOP is
  one byte. For the speed switch (`Ctx::speed_switch`) and for STOP mode
  (`Ctx::stop_enter` -> `stop_freeze`; a one-byte STOP's in
  `Bus::stop_begin`). Source: AGE `gb_cpu` opcode 10 and
  `gb_bus::execute_stop` (Apache-2.0, read only), whose `speed-switch`
  tests are verified on CGB B and C, the revision modelled (ADR 0003).
- The reset of a two-byte STOP has two carve-outs, both "a bit that rose
  on exactly the reset's M-cycle doesn't count as falling": the 4 kHz
  timer bit (`Timer::stop_div`; AGE `gb_timer::after_div_reset`, "speed
  change glitch", CGB B/C) and the DIV-APU bit (`apu_div_reset`; AGE
  `gb_sound::after_div_reset`, "sound STOP glitch"). `Timer::stop_edge` is
  gone: the later reset makes that edge itself. The serial clock gets the
  plain reset rule, as in AGE.
- After a two-byte switch the counter runs on from the reset's 0 with no
  hold, **whatever IME is**. With IME off this is the same counter as
  before (reset at the fetch, 4 through the byte, 4 T-cycles held). With
  IME on and nothing pending the timer is now 4 T-cycles behind the old
  model and SameBoy's (CGB E; "I don't trust this if" in its source).
- Unchanged: the one-byte (pending) rows (reset at the end of the fetch,
  4 T-cycles late on CGB B/C, the idle M-cycle to double speed), the halt
  lengths, the grid move to single speed, the order of operations, OAM
  DMA, and the STOP-mode screen, still decided at the end of STOP's fetch
  (`Ctx::stop_screen`).
- STOP mode: the counter, the serial clock and the frame sequencer run
  through the second byte before they stop. A TIMA overflow at the end of
  STOP's fetch reloads at the reset; one at the reset is the held one.
- `Bus::speed_switch` keeps its `ime` argument, now unused by the
  emulator's bus: the trait is exported (`gbc_core::sm83`), and CGB E's
  rule would need it.

**Why.** The old model was SameBoy's CGB E base with AGE's CGB B/C results
laid on top; `stop_edge` and the IME-off hold both existed only because
the reset was one M-cycle earlier than on the revision AGE measured
(`docs/reference/speed-switch.md` has the piece-by-piece table).

**Measured** (full stage 1 manifest, 3592 runs, against the parent):
- reset moved, `stop_edge` and the hold deleted, no APU carve-out: 8
  pass -> fail (`spsw-ch2-lc-delay-cgbBCE`, 7 gambatte
  `speedchange*_ch2_nr52_*`), 0 fail -> pass;
- with the APU carve-out: no status change in either direction.
- `diff-fingerprint.sh --audio-buffer 7 --hash-samples`: 4 ROMs differ,
  all IME-on speed switches: AGE `m3-bg-lcdc-ds`, `m3-bg-scx-ds`,
  `spsw-interrupts-cgbBC`, `spsw-interrupts-cgbE`.

**Not decided by any ROM** (`docs/reference/unpinned.md`:
`spsw-div-reset-point`, `spsw-ime-on-timer`, `spsw-reset-edges`): the
IME-on, nothing-pending timer phase (AGE's model against SameBoy's); the
serial edge of the reset; STOP mode.

**Tests** (each seen failing on the old core):
`speed_switch_timer_is_the_same_with_ime_on` (counter 000C, now 0008),
`serial_bit_in_flight_across_a_switch_to_single_speed_shifts_once` (a
clock bit rising during the second byte), `stop_mode_holds_a_tima_reload_in_flight`
(3 phases held, now 2);
`stop_div_reset_counts_a_timer_bit_rising_during_the_second_byte` fails
with the 4 kHz carve-out removed. The 8 ROMs above guard the APU carve-out.
