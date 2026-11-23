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
- Otherwise DIV resets at the end of STOP's fetch. With nothing pending
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
  T-cycles late (the held DIV reset). With an interrupt pending there is
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

Rule (`Timer::stop_edge`, called by `Ctx::stop_div_reset` before the
reset when STOP is two bytes, i.e. IE & IF = 0): if the timer runs at
262, 65 or 16 kHz and c mod 2b = b - 4, TIMA increments. The pending
(one-byte) STOP has no second byte and is unchanged. The eager reference
model of the ADR 0011 differential test mirrors it. CGB E (glitch on all
but 262 kHz) is not modelled. Gains `spsw-tima-cgbBC` and 14 Gambatte
`speedchange*_tima0[123]*` ROMs, no regressions (full stage1 manifest).
