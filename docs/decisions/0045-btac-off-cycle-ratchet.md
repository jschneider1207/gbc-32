# 0045: A second, tighter cycle ratchet with the branch predictor off

- Status: Accepted (agent decision; a stricter-only gate change, AGENTS.md
  "Autonomy contract")
- Date: 2026-11-27
- Builds on: ADR 0042 (core-first flash layout; the BTAC diagnosis and the
  canonical noise statement); plan `docs/plan/stage-2.md` M6 (the +3%
  on-board ratchet); review `docs/reviews/stage2-comprehensive.md` M1, M2

## Context
Gate B2's ratchet fails a line of `bench-itcm-dtcm-pll480` at more than +3%
of `expected-cycles.txt`. ADR 0042 showed that a core change moves the
counts by up to about +-1.7% (bound workloads) and +-4% (storms) through
code layout alone, because the Cortex-M7's branch target address cache
(BTAC) aliases on code addresses. So the +3% has a margin of only about 1.3
points on the bound set for a real cost increase, and a real increase of
1-3% hides inside the layout band. With the predictor off, the same
perturbations moved every workload by at most 0.11% (ADR 0042).

Measured 2026-11-27 (docs/perf.md "Session 30 (cont.): BTAC-off ratchet
(M1), layout sets (M2), align-loops (P6)"): five builds of the same source
at ITCM start pads 0/4/8/16/24 B (`tools/h7-bench/layout-set.sh`), each run
once, 30 lines (15 workloads, internal flash and QSPI), spread = (largest -
smallest) / set mean:

| | largest spread, mean= | largest spread, any field | bound set (worst-case, cgb-raster, dmg-raster, event-storm, cpu_instrs) |
|---|---|---|---|
| predictor on | 2.49% (flash storm-div) | 2.51% | up to 1.17% (cgb-raster) |
| predictor off | 0.18% (flash halt-irq) | 0.28% (qspi halt-irq max) | up to 0.08% (mean), 0.10% (max) |

With the predictor off the bench runs about 11% slower on worst-case
(3.68 M vs 3.30 M cycles per frame), still under half of one real-time
frame (7,902,557).

## Decision
- **A diagnostic image, `bench-itcm-dtcm-btacoff-pll480`** (h7-bench feature
  `btac-off`, implies `itcm`): ACTLR DISBTACREAD | DISBTACALLOC (bits 13 and
  14) set right after the lib's init, before any workload; banner
  `placement=itcm-dtcm-btacoff`, so its baseline can never be compared with
  the predictor-on image (check-cycles.sh compares the placement). build.sh
  builds it and the post-link ITCM check runs on it; gate H3 lints the
  feature set, H4 requires it rebuilt.
- **Gate B2 runs it too** (the same run-qspi.sh session: loader, pack,
  image, 100 s read; check-report.sh --qspi against `expected-hashes.txt`)
  and checks it with `check-cycles.sh --fresh --tolerance-bp 90` against
  `tools/h7-bench/expected-cycles-btac-off.txt`: every line's mean and max
  at most **+0.90%**. The existing +3% check on `expected-cycles.txt` is
  unchanged and runs first; its figures stay the reported ones.
- **Why 90 bp.** 3x the largest spread measured with the predictor off
  (0.279%) is 0.84%, rounded up to 0.90%. The factor covers a baseline and a
  later build that each sit anywhere in the band (one spread), boot-to-boot
  noise (inside the set, which is five boots), and a pad set that need not
  have hit the worst layout (one more). It is under the 1% the
  report-agreement check already allows between two repeats of one boot,
  which is a different test (a dropped digit), not a noise bound.
- **`--tolerance-bp <n>` can only tighten.** check-cycles.sh accepts 1 to
  300 basis points (300 = the +3% default) and refuses anything else (exit
  2), and only in check mode, so the option cannot loosen any check. The
  real-time bound (max < 7,902,557 on worst-case and cpu_instrs) applies to
  this pass as well.
- **The baseline rules are expected-cycles.txt's**: `--update` writes it,
  raising any line needs the human (`--absorb-regression`), measured in
  the main checkout (ADR 0042).

## Consequences
- Stricter only: every B2 run that fails today still fails (the first pass
  is unchanged); B2 now also fails when the predictor-off counts rise by
  more than 0.90%, and when the BTAC-off image, its baseline or its OK line
  is missing.
- A core change that costs 1-3% of real work, which the +3% band let
  through, now fails B2 and needs the human to absorb it. A change that
  moves only the predictor's behaviour (layout, branch shape) moves the
  BTAC-off counts little, so it trips the first pass or nothing.
- The BTAC-off figures measure the code without the predictor: they rank
  real cost, not shipped speed. Decisions about speed keep using the
  predictor-on figures, judged as layout sets (README.md "Layout sets").
- B2 takes one more board session, about 3.5 minutes.
- If a future change makes the BTAC-off spread itself grow (say a core
  whose hot loop starts to hit flash wait states by layout), the 90 bp
  could trip on layout: re-measure a set before touching the tolerance,
  and loosening it needs the human.
