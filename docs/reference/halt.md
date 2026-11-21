# HALT: wake-up, return address, the HALT bug

Implemented in `crates/gbc-core/src/cpu/mod.rs` (`Cpu::halt`,
`Cpu::step_not_running`). The only model is CGB C (ADR 0003).

## HALT's own check
HALT checks `IE & IF & 1F` at the end of its fetch M-cycle (events that set
IF during that fetch count; ADR 0011 "Interrupt visibility").
- **Nothing pending:** the CPU halts.
- **Pending, IME = 1:** no halt. PC is rewound to the HALT, so the dispatch
  that follows pushes the HALT's own address and the HALT runs again after
  RETI. Source: SameBoy `Core/sm83_cpu.c` `halt()` (`gb->pc--`).
- **Pending, IME = 0: the HALT bug.** No halt, and the next opcode fetch
  doesn't advance PC, so the byte after HALT is read twice. An `RST` there
  pushes its own address. Source: Pan Docs "halt" (halt bug); SameBoy
  `halt()` (`halt_bug`, also on CGB in both modes).

## `EI; HALT`
EI's delay ends before the next instruction's fetch, so IME is already 1
when HALT checks: with an interrupt pending it is the IME = 1 case above.
The handler returns to the HALT, which then halts. Pan Docs describes this
as a HALT bug case (IME "still zero"), but the observable result is the
same: "the interrupt returns to the halt, which is executed again". Sources:
Pan Docs "halt"; SameSuite `interrupt/ei_delay_halt` (passes); SameBoy
`ei()` (`ime_toggle`, applied in `GB_cpu_run` before the fetch).

## Wake-up latency (CGB): +1 M-cycle, IME = 0 and IME = 1
While halted, SameBoy `GB_cpu_run` on CGB samples `IE & IF`, then advances 4
clocks, then either wakes without a dispatch (IME = 0) or dispatches
(IME = 1). So once IF is set, the CPU spends one more idle M-cycle before
the next fetch or the 5-M-cycle dispatch, in **both** IME cases. Our halted
step stops at the M-cycle whose end sets IF, then the wake-up step does one
`idle()` before the normal step.

DMG differs (SameBoy samples mid-M-cycle: 2 clocks, sample, 2 clocks) but
does not apply here: CGB hardware in DMG-compatibility mode is still CGB
(`GB_is_cgb` is the model, not the mode).

Test evidence: AGE `halt/ei-halt-dmgC-cgbBCE` and SameSuite
`interrupt/ei_delay_halt` pass after this change; Mooneye `halt_ime0_ei`,
`halt_ime0_nointr_timing`, `halt_ime1_timing` keep passing. Gekkio's GBCTR
gives no cycle-level HALT wake-up timing that we could use.
