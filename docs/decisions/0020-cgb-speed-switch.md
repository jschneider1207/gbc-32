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
- Plain STOP (no switch) stays one byte and stops until a button press;
  making it two bytes is a separate task (SingleStepTests harness).

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
