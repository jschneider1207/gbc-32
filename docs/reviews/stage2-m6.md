# Stage 2 M6 milestone review (`stage2-m5..64b0045`)

Two fresh read-only reviewers, 2026-11-26 (session 27): **test integrity +
platform correctness** (no emulation logic changed in M6, so the hardware
review was merged into it, AGENTS.md) and **performance and portability**.
No high findings. Outcomes below; "plan" = one-line task in
`docs/plan/stage-2.md`.

## Test integrity + platform correctness

| # | Sev | Where | Finding | Outcome |
|---|---|---|---|---|
| T1 | med | scripts/check-itcm-elf.sh:143 | elfsyms gained a 4th column (bind) but the ELF-extras loop still reads `_ _ sym`, so `sym` = "name BIND": downstream generic instances are no longer checked, report lists ~58 false "not in the link" | confirmed (read the loop); fix in progress (branch m6-fix) |
| T2 | med | tools/h7-bench/check-report.sh:62 | cycle fields are zeroed before the hash diff, so a VCP byte drop inside `max=` passes and check-cycles judges a corrupt number (an over-budget frame can pass) | confirmed; fix in check-cycles.sh (min<=mean<=max, two agreeing reports), m6-fix |
| T3 | low | check-cycles.sh `--update --force` | refresh never compares with the old baseline; repeated +2.9% refreshes ratchet up | fixed with P2 (m6-fix) |
| T4 | low | check-cycles.sh `--update` provenance | dirty check misses memory-x, perf/, .cargo; `H7_COMMIT` bypass; ITCM drop not in banner; capture not in repo | dirty check + bypass in m6-fix; banner drop field and capture archive: plan |
| T5 | low | check-cycles.sh | "wrong clock" is the compile-time `cpu_hz`, not a measurement | rejected for M6: gate B1 (M7) checks the measured clock |
| T6 | low | qspi.rs:87 | PLL2 input 16.0 MHz = the fPLL_IN maximum; with HSI tolerance up to ~16.24 MHz | fixing (legal divider set), QSPI fix agent |
| T7 | low | qspi.rs:433 | `init` ignores `release_enhance`'s result; a timeout leaves the NOR in enhance mode and the loader fails late | fixing (panic), QSPI fix agent |
| T8 | low | qspi-nor.md:248, perf.md "QSPI tuning" | the old-image boot panic is blamed on enhance mode, but the panicking step runs before any NOR command; mechanism unexplained | QSPI fix agent investigates; docs corrected |
| T9 | low | perf.md:1345, plan:333 | "+3% is ~60x the spread" uses reflash spread; storm-spsw moved 0.67% between boots of one binary; "the gate fails" but no gate step runs check-cycles yet | docs fix |

## Performance and portability

| # | Sev | Where | Finding | Outcome |
|---|---|---|---|---|
| P1 | med | ADR 0042, perf.md | noise estimates disagree (0.61% / 1.18% / ±1.5% / ±1.7% / "60x"); real margin for a core change on the bound set is ~1.3-1.8 points | docs fix (one consistent statement) |
| P2 | med | check-cycles.sh:136-148 | `--update --force` absorbs regressions on a "refresh after an improvement" without the human | fixing: refuses any worse line unless `--absorb-regression` (human), m6-fix |
| P3 | med | ratchet on storms | layout alone moves storms > 3% (storm-div +4.24%), so core edits will trip storm lines | needs the human (a separate tolerance or report-only storms loosens a check): STATUS "Needs human" |
| P4 | med | tools/memory-x (DTCM) | no link-time stack floor; scripted player uses 19,800 of 22,912 B | fixing: linker ASSERT, m6-fix |
| P5 | med | perf.md "M6 worst-case report" | the bench keeps `FRAME` in cached AXI, the player in DTCM; the report is not "the shipped placement" | docs fix: relabel + quote the player-script soak `core` max |
| P6 | low | ratchet guards the bench, not `player`; B2 unwired | plan (M7 gate) |
| P7 | low | ITCM 1,192 B free; 57 thunks (570 B) move with layout | plan |
| P8 | low | perf.md "Final chip" table still claims "AXI left 30,720 B" for the H735 | docs fix |
| P9 | low | tools/memory-x/src/lib.rs:253 | `__aeabi_memmove*` not in the ITCM rule: ITCM -> flash -> ITCM ping-pong | plan (changes ITCM layout, so with the next baseline refresh) |
| P10 | low | perf.md LTO and tier A/B | fat LTO +2.25% and per-event swap +1.42% are near the noise band, one build each | accepted: decisions keep the default; wording softened in the docs fix |
