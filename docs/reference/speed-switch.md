# CGB speed switch: where each piece of the timing comes from

Implemented in `crates/gbc-core/src/bus.rs` (`Ctx::speed_switch`,
`Bus::stop_begin`), `crates/gbc-core/src/timer/mod.rs`
(`Timer::speed_switch`, `Timer::stop_edge`) and `Cpu::stop`. The decisions
are in ADR 0020; this note records which reference each piece follows,
which ROMs pin it, and what the M10 review's A1-06 experiment measured.

## Sources and what each one models

- **SameBoy** (MIT) `Core/sm83_cpu.c` `stop()`, `enter_stop_mode()`;
  `Core/timing.c` `GB_advance_cycles()`. Its default CGB is revision E. Its
  own comments: "Speed switch timing needs far more tests. Double to single
  is wrong to avoid odd mode", and on the IME-off hold "I don't trust this
  if".
- **AGE** (Apache-2.0, read only) `gb_cpu` opcode 10, `gb_bus::execute_stop`,
  `gb_timer::after_div_reset`, `gb_sound::after_div_reset`. Its
  `speed-switch` test ROMs are verified on CGB B, C and E hardware, and the
  `-cgbBC` builds are the only M-cycle-exact evidence for the revision this
  core models (CGB C, ADR 0003).
- **Gambatte** (GPL, behavior only) `Memory::stop`, `PPU::speedChange`; its
  `cgb04c` hardware captures (`speedchange`, `lcd_offset`, `dma`).
- **Pan Docs** "Using the STOP Instruction" and FF4D: which STOP is one or
  two bytes, that DIV resets, that the timer runs through the switch. No
  M-cycle timing. GBCTR has none either.

The references disagree on one point that matters here: **when STOP resets
DIV**. SameBoy: at the end of STOP's fetch, before the second byte is read.
AGE: after the second byte (`READ_BYTE; TICK_MACHINE_CYCLE; execute_stop`),
for every STOP, whatever IME and IE & IF are.

## The model on `main`: piece by piece

| Piece | Follows | Pinned by |
|---|---|---|
| STOP is two bytes iff IE & IF = 0; a held button ends it | SameBoy `stop()`, Pan Docs chart | unit tests; every speed-switch ROM |
| DIV reset at the end of STOP's fetch | SameBoy `enter_stop_mode` | nothing by itself (see the experiment) |
| Timer 4 T-cycles late with IME off, nothing pending | SameBoy `div_cycles = -4` | with the row above: `spsw-tima-cgbBC`, gambatte `speedchange*_tima*` (they run with IME off) |
| No hold with IME on, nothing pending | SameBoy (CGB E) | **no ROM** |
| TIMA edge of a reset one M-cycle later, 262/65/16 kHz (`Timer::stop_edge`); none for 4 kHz | AGE `gb_timer::after_div_reset` ("speed change glitch", CGB B/C) | `spsw-tima-cgbBC`, 14 gambatte `speedchange*_tima0[123]*` |
| Halt of 0x8001 new-speed M-cycles from the second byte's end, timer running | SameBoy (0x20008 T-cycles, 5 frozen); Gambatte agrees (0x20004 from the second byte's start) | gambatte `speedchange/*`, AGE `spsw-*` |
| One clock short to single speed: the CPU grid moves against the PPU's | Gambatte `PPU::speedChange` (`p_.now -= isDoubleSpeed()`); SameBoy's count gives the same number | gambatte `lcd_offset/*`, `lycEnable/*lcdoffset*`, AGE `lcd-align-ly-cgbBC` |
| Interrupt pending: no halt; to double speed one idle M-cycle the timer doesn't count | SameBoy (`speed_switch_countdown = 6`, `speed_switch_freeze = 1`) | gambatte `speedchange/*` (pending rows), `spsw-interrupts-cgbBC` |
| Interrupt pending, IME on: timer 4 T-cycles late | AGE `spsw-interrupts-cgbBC` (`OFS_B = 1`; CGB E and SameBoy: 0) | `spsw-interrupts-cgbBC` |
| Frame-sequencer delay toggled by each switch to double speed | AGE `gb_sound::after_speed_change` | `spsw-ch2-lc-delay-cgbBCE`, gambatte `speedchange{,2,5}_ch2_nr52_*` |
| OAM DMA resumes one M-cycle later than after HALT; its end is held for the PPU | Gambatte captures | `oamdma/oamdmasrcC0_speedchange_readC000`, `dma/hdma_transition_speedchange_oamdma`, `oamdma_late_speedchange_stat_{1,2}` |
| Order: catch up under the old speed, switch, reschedule, then pass time | own (review A1-02, E8) | unit tests |

So the base is SameBoy's CGB E model, the PPU offset is Gambatte's, and
three rows are AGE's CGB B/C results laid on top. `stop_edge` exists only
because the reset is one M-cycle earlier than on the revision AGE measured.

## Experiment (M10 review A1-06, 2026-11-24): AGE's reset point throughout

Change, on branch `m10-spsw-div-code` (not merged, see "Status"):

1. STOP's DIV reset moves to **after STOP's last byte**: after the second
   byte when nothing is pending, at the end of the fetch when STOP is one
   byte. For the speed switch and for STOP mode alike.
2. `Timer::stop_edge` is deleted. The later reset makes its edge by itself;
   what is left of AGE's rule is the carve-out: at a two-byte STOP, a 4 kHz
   timer bit that rose on exactly the reset's M-cycle doesn't increment TIMA
   (`Timer::stop_div`).
3. The IME-off hold is deleted. With the reset one M-cycle later, "reset at
   the fetch, counter runs through the second byte, then holds 4 T-cycles"
   and "reset after the second byte, no hold" leave the same counter from
   the first post-switch M-cycle on, so the IME-off case is unchanged and
   the hold was the other half of the early reset.
4. The same carve-out for the APU: the reset of a two-byte STOP on exactly
   the M-cycle the DIV-APU bit rose doesn't step the frame sequencer (AGE
   `gb_sound::after_div_reset`, "sound STOP glitch", which cites gambatte's
   `speedchange_ch2_nr52_*`).
5. The one-byte (pending) rows are untouched: reset at the end of the
   fetch, 4 T-cycles late, plus the idle M-cycle to double speed.

What changes in behavior:

- **IME on, nothing pending:** the timer is 4 T-cycles behind `main` after
  the switch (the same as with IME off). AGE's model against SameBoy's.
- **Serial:** a clock bit that rises during the second byte now falls at
  the reset (one more flip-flop toggle); one that falls during the byte
  toggles there, an M-cycle before the reset. `main` patches only TIMA.
- **Frame sequencer:** a DIV-APU bit that falls during the second byte
  steps there, not at the fetch's end. Otherwise the same steps (the
  carve-out equals what the early reset did).
- **STOP mode:** the counter, the serial clock and the frame sequencer run
  through the second byte before they stop. A TIMA overflow at the end of
  STOP's fetch reloads at the reset; it was held through STOP mode.

### Measured

Full stage 1 manifest (3592 runs), before and after:

- Steps 1-3 only: **8 pass -> fail**, 0 fail -> pass:
  `age-test-roms/speed-switch/spsw-ch2-lc-delay-cgbBCE` and gambatte
  `speedchange/speedchange{,2,3,4,5}_ch2_nr52_1a_cgb04c_outF2`,
  `speedchange{,2}_ch2_nr52_ds_1a_cgb04c_outF2`. These are the ROMs that
  pin the frame-sequencer edge of the reset: on `main` the early reset sees
  the DIV-APU bit one M-cycle before it rises.
- Steps 1-5: **no status change in either direction**. Across the whole
  manifest one ROM's recorded result differs at all:
  `speed-switch/caution/spsw-interrupts-cgbBC` ends 18 clocks later and
  still passes (its IME-on, nothing-pending rows read the timer relative to
  the timer interrupt that ends the halt, so they don't see the 4 T-cycles).

- `scripts/diff-fingerprint.sh main --audio-buffer 7 --hash-samples`
  (4031 runs, frames and samples hashed): 4 ROMs differ, each in both of
  its runs, all of them speed-switching ROMs with IME on at a two-byte
  STOP: `age-test-roms/m3-bg-lcdc/m3-bg-lcdc-ds`, `m3-bg-scx/m3-bg-scx-ds`,
  `speed-switch/caution/spsw-interrupts-cgbBC` and `-cgbE`.
- Cost: total `.text` 115792 -> 115850 B (+58); `check-bench.sh` within
  0.00% of `main` on every line (largest difference 581 Ir);
  `check-sync-invariance.sh` ok.
- Unit tests seen failing on `main`'s code with the new expectations:
  `speed_switch_timer_is_the_same_with_ime_on` (counter 000C, want 0008),
  `serial_bit_in_flight_across_a_switch_to_single_speed_shifts_once` (the
  phase where the bit rises during the second byte),
  `stop_mode_holds_a_tima_reload_in_flight` (3 phases held, want 2). The
  4 kHz carve-out: `stop_div_reset_counts_a_timer_bit_rising_during_the_second_byte`
  fails with the carve-out removed.

What that shows:

- The TIMA rows (`spsw-tima-cgbBC` and the 14 gambatte `tima` ROMs) pass
  with no fix-up once the reset is where AGE has it: `stop_edge` is derived,
  not an independent fact. The 4 kHz and frame-sequencer carve-outs are the
  two fitted rows, each with hardware-verified ROMs behind it.
- **No ROM in the manifest decides the IME-on, nothing-pending timer
  phase.** `main` follows SameBoy (CGB E, flagged as untrusted in its
  source); the experiment follows AGE (verified on B/C, but by tests that
  run with IME off or read relative to an interrupt). A test that would
  decide it: `ei`, a two-byte speed-switch STOP with IE = 0, then DIV or a
  262 kHz TIMA read a fixed number of M-cycles after the halt.
- No ROM covers the serial edge, STOP mode, or the one-byte STOP's 4 kHz
  and frame-sequencer edges.

## Status

Not on `main`. The change is equally simple or simpler (one reset point,
no hold for two-byte STOPs, no `stop_edge`), has no regressions and one
cited reference, but it replaces sentences ADR 0020 decides, which needs
the human:

- Decision: "Otherwise DIV resets at the end of STOP's fetch."
- Decision: "With IME off it starts 4 T-cycles late (the held DIV reset)."
- Amendment "the TIMA edge of STOP's DIV reset": the rule
  (`Timer::stop_edge` ... "if the timer runs at 262, 65 or 16 kHz and
  c mod 2b = b - 4, TIMA increments").
- Amendment "STOP mode": "Entry (`Bus::stop_begin` -> `Ctx::stop_freeze`,
  at the end of STOP's fetch ...)" and "A TIMA reload in flight at the
  reset" (which overflow is in flight moves by one M-cycle).

The one-byte rows would stay a SameBoy/AGE composite either way: AGE runs
an M-cycle at the old speed after a pending STOP's fetch and resets DIV
after it, SameBoy and this core run none (to single speed) or one at the
new speed (to double), and the CPU/PPU phase there is pinned by gambatte's
pending-row captures.
