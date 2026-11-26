# Stage 2 M6 milestone review (`stage2-m5..64b0045`, fixes to a18f914)

Two fresh read-only reviewers, 2026-11-26 (session 27): **test integrity +
platform correctness** (no emulation logic changed in M6, so the hardware
review was merged into it, AGENTS.md) and **performance and portability**.
No high findings. Outcomes below; "plan" = one-line task in
`docs/plan/stage-2.md`.

## Test integrity + platform correctness

| # | Sev | Where | Finding | Outcome |
|---|---|---|---|---|
| T1 | med | scripts/check-itcm-elf.sh:143 | elfsyms gained a 4th column (bind) but the ELF-extras loop still reads `_ _ sym`, so `sym` = "name BIND": downstream generic instances are no longer checked, report lists ~58 false "not in the link" | **fixed** a0d5324 (+ `scripts/test-check-itcm-elf.sh`, seen failing: 6 cases on the old loop; in gate H7) |
| T2 | med | tools/h7-bench/check-report.sh:62 | cycle fields are zeroed before the hash diff, so a VCP byte drop inside `max=` passes and check-cycles judges a corrupt number (an over-budget frame can pass) | **fixed** 10a8d65: two complete reports, last two agree (1%), min<=mean<=max; seen failing (16 cases on the old script) |
| T3 | low | check-cycles.sh `--update --force` | refresh never compares with the old baseline; repeated +2.9% refreshes ratchet up | **fixed** with P2 (341b471) |
| T4 | low | check-cycles.sh `--update` provenance | dirty check misses memory-x, perf/, .cargo; `H7_COMMIT` bypass; ITCM drop not in banner; capture not in repo | dirty check widened, `H7_COMMIT` bypass removed (341b471); banner drop field and capture archive: plan |
| T5 | low | check-cycles.sh | "wrong clock" is the compile-time `cpu_hz`, not a measurement | rejected for M6: gate B1 (M7) checks the measured clock |
| T6 | low | qspi.rs:87 | PLL2 input 16.0 MHz = the fPLL_IN maximum; with HSI tolerance up to ~16.24 MHz | **fixed** 5c66c0d: 12.8 MHz input, VCO 793.6, 99.2 MHz; const asserts over the HSI tolerance (seen failing on the old set); CSHT 6 cycles for the fastest HSI |
| T7 | low | qspi.rs:433 | `init` ignores `release_enhance`'s result; a timeout leaves the NOR in enhance mode and the loader fails late | **fixed** 2c66254: panics; seen on the board with a forced `Err(Busy)` |
| T8 | low | qspi-nor.md:248, perf.md "QSPI tuning" | the old-image boot panic is blamed on enhance mode, but the panicking step runs before any NOR command; mechanism unexplained | **confirmed, docs corrected** a494513: the old image panics with the NOR in normal mode too (3 boots), so enhance mode is not the cause; cause open (hypotheses in qspi-nor.md); current images not seen to hit it |
| T9 | low | perf.md:1345, plan:333 | "+3% is ~60x the spread" uses reflash spread; storm-spsw moved 0.67% between boots of one binary; "the gate fails" but no gate step runs check-cycles yet | **fixed** a9f2dde |

## Performance and portability

| # | Sev | Where | Finding | Outcome |
|---|---|---|---|---|
| P1 | med | ADR 0042, perf.md | noise estimates disagree (0.61% / 1.18% / ±1.5% / ±1.7% / "60x"); real margin for a core change on the bound set is ~1.3-1.8 points | **fixed** a9f2dde (ADR 0042 amendment 2026-11-26) |
| P2 | med | check-cycles.sh:136-148 | `--update --force` absorbs regressions on a "refresh after an improvement" without the human | **fixed** 341b471: `--force` refuses any slower or missing line unless `--absorb-regression` (human); seen failing (20 cases) |
| P3 | med | ratchet on storms | layout alone moves storms > 3% (storm-div +4.24%), so core edits will trip storm lines | needs the human (a separate tolerance or report-only storms loosens a check): STATUS "Needs human" |
| P4 | med | tools/memory-x (DTCM) | no link-time stack floor; scripted player uses 19,800 of 22,912 B | **fixed** 408a6cc: ASSERT stack >= 22 KiB in every firmware memory.x; images byte-identical; seen failing at 23 KiB |
| P5 | med | perf.md "M6 worst-case report" | the bench keeps `FRAME` in cached AXI, the player in DTCM; the report is not "the shipped placement" | **fixed** a9f2dde: relabelled; shipped-image figure from the soak (core max 3,390,856 = 42.9%) |
| P6 | low | ratchet guards the bench, not `player`; B2 unwired | plan (M7 gate) |
| P7 | low | ITCM 1,192 B free; 57 thunks (570 B) move with layout | plan |
| P8 | low | perf.md "Final chip" table still claims "AXI left 30,720 B" for the H735 | **fixed** a9f2dde |
| P9 | low | tools/memory-x/src/lib.rs:253 | `__aeabi_memmove*` not in the ITCM rule: ITCM -> flash -> ITCM ping-pong | plan (changes ITCM layout, so with the next baseline refresh) |
| P10 | low | perf.md LTO and tier A/B | fat LTO +2.25% and per-event swap +1.42% are near the noise band, one build each | accepted: decisions keep the default; wording softened (a9f2dde) |

## Re-review of the check-script fixes (one reviewer, AGENTS.md: the fixes changed pass/fail logic)

Scope `a0d5324`, `10a8d65`, `341b471` (pre-rebase b082d80, 16d1e6d,
6845a8a). No high or medium. Every change stricter only; each closes its
finding; tests mutation-checked by the reviewer too (37 failures on the
old check-cycles.sh, 6 on the old check-itcm-elf.sh); no false failure on
the real 49-report capture. Lows L1-L4 (dirty-check inputs, false
`+dirty`, guard skipped when the file is missing, absorb leaves no mark):
plan tasks.

## Board results after the fixes

QSPI at 99.2 MHz: `qspi-mmap` all PASS (miss 429 cycles); bench over QSPI
`check-cycles.sh` exit 0 against `expected-cycles.txt` (QSPI lines -0.05%
to +0.21%); `run-replay.sh` cgb-acid2 PASS; player Pokemon Red 0 over
budget. `gate-stage2.sh --host-only` exit 3 on each fix branch. The
10-minute soak (scripted and live) ran before the QSPI clock fix, at
100 MHz (`docs/reference/m6-evidence.md`); the fix lowers the clock by
0.8% and moves the QSPI worst case by <= 0.06%.
